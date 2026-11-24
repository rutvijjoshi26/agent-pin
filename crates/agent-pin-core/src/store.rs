//! On-disk store: one JSON file per agent session under
//! `<state_dir>/sessions/<provider>__<session_key>.json`.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::session::SessionState;

/// The state directory: `AGENT_PIN_HOME` if set, otherwise
/// `<home>/.agent-pin` (home is `USERPROFILE` on Windows, `HOME` elsewhere).
pub fn state_dir() -> Option<PathBuf> {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let user_home = std::env::var_os(home_var).filter(|h| !h.is_empty()).map(PathBuf::from);
    state_dir_from(std::env::var_os("AGENT_PIN_HOME"), user_home)
}

/// Testable core of [`state_dir`]. An empty `env_home` counts as unset.
pub fn state_dir_from(env_home: Option<OsString>, user_home: Option<PathBuf>) -> Option<PathBuf> {
    match env_home.filter(|h| !h.is_empty()) {
        Some(h) => Some(PathBuf::from(h)),
        None => user_home.map(|h| h.join(".agent-pin")),
    }
}

/// How long [`update_session`] waits for a session's lock before giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// Sleep between lock attempts.
const LOCK_POLL: Duration = Duration::from_millis(15);

/// Read-modify-writes one session under an exclusive lock on
/// `<dir>/sessions/<provider>__<key>.lock`. A missing or corrupt file reads
/// as `None`; any other read error is returned and `f` is not called.
/// Returning `None` removes the session file. If the lock is not acquired
/// within 2 s, returns an error of kind `TimedOut`.
pub fn update_session(
    dir: &Path,
    provider: &str,
    key: &str,
    f: impl FnOnce(Option<SessionState>) -> Option<SessionState>,
) -> io::Result<()> {
    update_stem(dir, &format!("{provider}__{key}"), f)
}

/// Every readable session file in `<dir>/sessions`. Skips lock files, temp
/// files and anything that fails to read or parse.
pub fn list_sessions(dir: &Path) -> Vec<SessionState> {
    let Ok(entries) = fs::read_dir(dir.join("sessions")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|p| read_state(&p).ok().flatten())
        .collect()
}

/// Sets `seen_ms` on the session whose row key is `row_key`
/// (`<provider>__<session_key>`), under the same lock as [`update_session`].
/// A missing session is left missing.
pub fn mark_seen(dir: &Path, row_key: &str, now_ms: i64) -> io::Result<()> {
    update_stem(dir, row_key, |prev| {
        prev.map(|mut s| {
            s.seen_ms = now_ms;
            s
        })
    })
}

/// Deletes stale files in `<dir>/sessions`, returning how many went:
/// - session `*.json` files whose `updated_ms` is older than `max_age_ms`
///   before `now_ms` (unparseable ones go by file mtime), together with their
///   lock file on Unix, all under the session's lock (bounded, as for
///   writers); a session whose lock stays busy is skipped;
/// - `*.json.tmp-*` files whose mtime is older than `max_age_ms`;
/// - on Unix only, orphan `*.lock` files (no session file) whose mtime is
///   older than `max_age_ms`; a lock someone holds is kept.
///
/// Lock files are never deleted on Windows (see [`prune_session`]).
///
/// mtime ages are measured against the system clock. Per-file errors are
/// skipped; only failing to list the directory is an error.
pub fn prune(dir: &Path, now_ms: i64, max_age_ms: i64) -> io::Result<usize> {
    let sessions = dir.join("sessions");
    let names: Vec<String> = match fs::read_dir(&sessions) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let max_age = Duration::from_millis(max_age_ms.max(0) as u64);
    let mut removed = 0;
    for stem in names.iter().filter_map(|n| n.strip_suffix(".json")) {
        removed += prune_session(&sessions, stem, now_ms, max_age_ms, max_age).unwrap_or(0);
    }
    for name in &names {
        let path = sessions.join(name);
        if name.contains(".json.tmp-") {
            if older_than(&path, max_age) && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        } else if let Some(stem) = name.strip_suffix(".lock") {
            #[cfg(unix)]
            if prune_orphan_lock(&sessions, stem, &path, max_age) {
                removed += 1;
            }
            #[cfg(not(unix))]
            let _ = stem;
        }
    }
    Ok(removed)
}

/// Prunes one session under its lock. Returns how many files went.
fn prune_session(
    sessions: &Path,
    stem: &str,
    now_ms: i64,
    max_age_ms: i64,
    max_age: Duration,
) -> io::Result<usize> {
    check_stem(stem)?;
    let lock_path = sessions.join(format!("{stem}.lock"));
    let lock = lock_session(&lock_path)?;
    let path = sessions.join(format!("{stem}.json"));
    let stale = match read_state(&path)? {
        Some(state) => state.updated_ms < now_ms.saturating_sub(max_age_ms),
        None => path.exists() && older_than(&path, max_age),
    };
    if !stale {
        return Ok(0);
    }
    fs::remove_file(&path)?;
    // Unix only: writers waiting on this lock file see (by dev and inode)
    // that it was unlinked and relock. On Windows `still_linked` can only
    // check that the path exists, so a waiter could keep the old lock while
    // another writer locks a new file at the same path; there the 0-byte
    // lock file is left in place.
    #[cfg(unix)]
    let lock_gone = fs::remove_file(&lock_path).is_ok();
    #[cfg(not(unix))]
    let lock_gone = false;
    drop(lock);
    Ok(1 + usize::from(lock_gone))
}

/// Removes a lock file with no session file beside it, if it is old and
/// nobody holds it. Unix only, for the reason given in [`prune_session`].
#[cfg(unix)]
fn prune_orphan_lock(sessions: &Path, stem: &str, path: &Path, max_age: Duration) -> bool {
    if check_stem(stem).is_err()
        || sessions.join(format!("{stem}.json")).exists()
        || !older_than(path, max_age)
    {
        return false;
    }
    let Ok(lock) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    if lock.try_lock().is_err() || !still_linked(&lock, path) {
        return false;
    }
    // Re-check under the lock: a writer may have created the session since.
    if sessions.join(format!("{stem}.json")).exists() {
        return false;
    }
    fs::remove_file(path).is_ok()
}

/// True when `path`'s mtime is more than `max_age` before the system clock.
pub(crate) fn older_than(path: &Path, max_age: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > max_age)
}

/// Removes `<dir>/sessions` and everything in it.
pub fn reset(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir.join("sessions")) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn update_stem(
    dir: &Path,
    stem: &str,
    f: impl FnOnce(Option<SessionState>) -> Option<SessionState>,
) -> io::Result<()> {
    check_stem(stem)?;
    let sessions = dir.join("sessions");
    fs::create_dir_all(&sessions)?;
    let _lock = lock_session(&sessions.join(format!("{stem}.lock")))?;
    let path = sessions.join(format!("{stem}.json"));
    match f(read_state(&path)?) {
        Some(state) => write_atomic(&sessions, stem, &path, &state),
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
    // `_lock` drops here, releasing the lock.
}

/// Opens (creating if needed) and exclusively locks the lock file at
/// `path`, retrying until [`LOCK_TIMEOUT`]. Only [`prune`] deletes lock files,
/// only on Unix, and only while holding them; a caller that then wins the
/// unlinked file sees it is no longer at `path` and locks the current file
/// instead, so two writers never hold different lock files for one session.
fn lock_session(path: &Path) -> io::Result<File> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        lock_until(&lock, deadline)?;
        if still_linked(&lock, path) {
            return Ok(lock);
        }
    }
}

/// Takes an exclusive lock on `file`, retrying until `deadline`.
fn lock_until(file: &File, deadline: Instant) -> io::Result<()> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::Error(e)) => return Err(e),
            Err(TryLockError::WouldBlock) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for session lock",
                    ));
                }
                std::thread::sleep(LOCK_POLL.min(deadline - now));
            }
        }
    }
}

/// True when `file` is still the file at `path`.
#[cfg(unix)]
fn still_linked(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Best effort where file identity is not available on stable Rust: the
/// path still exists.
#[cfg(not(unix))]
fn still_linked(_file: &File, path: &Path) -> bool {
    path.exists()
}

/// Accepts only non-empty stems of `[A-Za-z0-9_-]`, so a stem can never
/// name a path outside the sessions dir on any platform.
pub(crate) fn check_stem(stem: &str) -> io::Result<()> {
    let ok = !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid id",
        ));
    }
    Ok(())
}

/// `Ok(None)` for a missing or unparsable file; other I/O errors propagate.
fn read_state(path: &Path) -> io::Result<Option<SessionState>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

/// Writes `state` to a unique temp file in `sessions`, then renames it over
/// `path`.
fn write_atomic(sessions: &Path, stem: &str, path: &Path, state: &SessionState) -> io::Result<()> {
    let body = serde_json::to_vec(state).map_err(io::Error::other)?;
    write_bytes_atomic(sessions, &format!("{stem}.json"), path, &body)
}

/// Writes `body` to a unique temp file `<dir>/<name>.tmp-<pid>-<n>`, syncs
/// it, then renames it over `path` (which must be in `dir`). The temp file
/// is removed on failure.
pub(crate) fn write_bytes_atomic(dir: &Path, name: &str, path: &Path, body: &[u8]) -> io::Result<()> {
    let tmp = temp_path(dir, name);
    let result = write_synced(&mut File::create(&tmp)?, body).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Publishes `body` as `<dir>/<name>` only if nothing is there yet: writes a
/// synced temp file and hard-links it into place (failing with
/// `AlreadyExists` if the target exists), then removes the temp. Where hard
/// links fail for another reason, falls back to `create_new` and writes the
/// target directly (not atomic for readers).
pub(crate) fn publish_new(dir: &Path, name: &str, body: &[u8]) -> io::Result<()> {
    let path = dir.join(name);
    let tmp = temp_path(dir, name);
    let linked = write_synced(&mut File::create(&tmp)?, body).and_then(|()| fs::hard_link(&tmp, &path));
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
            let result = write_synced(&mut file, body);
            if result.is_err() {
                drop(file);
                let _ = fs::remove_file(&path);
            }
            result
        }
    }
}

/// A unique temp path `<dir>/<name>.tmp-<pid>-<n>`.
fn temp_path(dir: &Path, name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{name}.tmp-{}-{n}", std::process::id()))
}

fn write_synced(file: &mut File, body: &[u8]) -> io::Result<()> {
    file.write_all(body)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{FileTouch, Status};
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    fn state(provider: &str, key: &str) -> SessionState {
        SessionState {
            provider: provider.to_owned(),
            session_key: key.to_owned(),
            cwd: None,
            status: Status::Idle,
            updated_ms: 1,
            seen_ms: 0,
            original_ask: None,
            last_ask: None,
            pending: None,
            pending_ms: 0,
            files: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn json_path(dir: &Path, stem: &str) -> PathBuf {
        dir.join("sessions").join(format!("{stem}.json"))
    }

    #[test]
    fn state_dir_prefers_agent_pin_home() {
        let got = state_dir_from(Some("/custom/pin".into()), Some(PathBuf::from("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/custom/pin")));
    }

    #[test]
    fn state_dir_defaults_to_home_dot_agent_pin() {
        let got = state_dir_from(None, Some(PathBuf::from("/home/u")));
        assert_eq!(got, Some(Path::new("/home/u").join(".agent-pin")));
        // An empty AGENT_PIN_HOME counts as unset.
        let got = state_dir_from(Some(OsString::new()), Some(PathBuf::from("/home/u")));
        assert_eq!(got, Some(Path::new("/home/u").join(".agent-pin")));
        assert_eq!(state_dir_from(None, None), None);
    }

    #[test]
    fn update_creates_then_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        update_session(dir, "claude", "s1", |prev| {
            assert!(prev.is_none());
            Some(state("claude", "s1"))
        })
        .unwrap();
        assert!(json_path(dir, "claude__s1").is_file());
        update_session(dir, "claude", "s1", |prev| {
            let mut s = prev.expect("existing state");
            s.updated_ms = 42;
            Some(s)
        })
        .unwrap();
        let all = list_sessions(dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].updated_ms, 42);
        assert_eq!(all[0].session_key, "s1");
    }

    #[test]
    fn none_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        update_session(dir, "codex", "s", |_| Some(state("codex", "s"))).unwrap();
        update_session(dir, "codex", "s", |_| None).unwrap();
        assert!(!json_path(dir, "codex__s").exists());
        assert!(list_sessions(dir).is_empty());
        // Removing a missing session is fine too.
        update_session(dir, "codex", "s", |_| None).unwrap();
    }

    #[test]
    fn corrupt_file_treated_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("sessions")).unwrap();
        fs::write(json_path(dir, "claude__bad"), b"{not json").unwrap();
        assert!(list_sessions(dir).is_empty());
        update_session(dir, "claude", "bad", |prev| {
            assert!(prev.is_none());
            Some(state("claude", "bad"))
        })
        .unwrap();
        assert_eq!(list_sessions(dir).len(), 1);
    }

    #[test]
    fn list_skips_lock_and_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        update_session(dir, "claude", "a", |_| Some(state("claude", "a"))).unwrap();
        let body = serde_json::to_string(&state("claude", "z")).unwrap();
        fs::write(dir.join("sessions").join("claude__z.json.tmp-1-0"), &body).unwrap();
        fs::write(dir.join("sessions").join("claude__z.lock"), &body).unwrap();
        let all = list_sessions(dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_key, "a");
        // A missing state dir lists nothing.
        assert!(list_sessions(&dir.join("nope")).is_empty());
    }

    #[test]
    fn concurrent_updates_not_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Arc::new(tmp.path().to_path_buf());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = Arc::clone(&dir);
                thread::spawn(move || {
                    update_session(&dir, "claude", "shared", |prev| {
                        let mut s = prev.unwrap_or_else(|| state("claude", "shared"));
                        // Widen the read-modify-write window.
                        thread::sleep(std::time::Duration::from_millis(5));
                        s.files.push(FileTouch { path: format!("/f{i}"), at_ms: i });
                        Some(s)
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        let mut paths: Vec<_> = all[0].files.iter().map(|f| f.path.clone()).collect();
        paths.sort();
        let want: Vec<_> = (0..8).map(|i| format!("/f{i}")).collect();
        assert_eq!(paths, want);
    }

    #[test]
    fn mark_seen_sets_seen_ms() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        update_session(dir, "claude", "s1", |_| Some(state("claude", "s1"))).unwrap();
        mark_seen(dir, "claude__s1", 777).unwrap();
        assert_eq!(list_sessions(dir)[0].seen_ms, 777);
        // A missing session stays missing.
        mark_seen(dir, "claude__gone", 5).unwrap();
        assert!(!json_path(dir, "claude__gone").exists());
    }

    #[test]
    fn mark_seen_rejects_escaping_keys() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["../x", "a/b", "a\\b", "..", "", "C:x", "a:b", "a.b", "a b", "a\0b"] {
            let err = mark_seen(tmp.path(), bad, 1).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
    }

    #[test]
    fn held_lock_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let holder = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(sessions.join("claude__held.lock"))
            .unwrap();
        holder.lock().unwrap();
        let start = std::time::Instant::now();
        let err = update_session(dir, "claude", "held", |_| Some(state("claude", "held"))).unwrap_err();
        let waited = start.elapsed();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(waited >= std::time::Duration::from_millis(1900), "{waited:?}");
        assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
        assert!(!json_path(dir, "claude__held").exists());
        // Once released, the session can be written again.
        drop(holder);
        update_session(dir, "claude", "held", |_| Some(state("claude", "held"))).unwrap();
    }

    #[test]
    fn read_error_propagates_without_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A directory where the session file should be makes the read fail
        // with an error other than NotFound.
        fs::create_dir_all(json_path(dir, "claude__s1")).unwrap();
        let mut called = false;
        let err = update_session(dir, "claude", "s1", |_| {
            called = true;
            Some(state("claude", "s1"))
        });
        assert!(err.is_err());
        assert!(!called, "f must not run after a read error");
        assert!(json_path(dir, "claude__s1").is_dir());
    }

    const DAY: i64 = 86_400_000;

    fn set_age(path: &Path, age: Duration) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age).unwrap();
    }

    fn put(dir: &Path, key: &str, updated_ms: i64) {
        update_session(dir, "claude", key, |_| {
            let mut s = state("claude", key);
            s.updated_ms = updated_ms;
            Some(s)
        })
        .unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.join("sessions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn prune_removes_old_sessions_and_keeps_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = 10 * DAY;
        put(dir, "old", now - 3 * DAY - 1);
        put(dir, "fresh", now - 3 * DAY + 1000);
        let removed = prune(dir, now, 3 * DAY).unwrap();
        assert_eq!(removed, 2, "old json and its lock");
        assert_eq!(names(dir), ["claude__fresh.json", "claude__fresh.lock"]);
    }

    #[test]
    fn prune_judges_unparseable_sessions_by_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("claude__oldbad.json"), b"{nope").unwrap();
        fs::write(sessions.join("claude__newbad.json"), b"{nope").unwrap();
        set_age(&sessions.join("claude__oldbad.json"), Duration::from_secs(4 * 86_400));
        prune(dir, 10 * DAY, 3 * DAY).unwrap();
        assert!(!sessions.join("claude__oldbad.json").exists());
        assert!(sessions.join("claude__newbad.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_removes_orphan_tmp_and_lock_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        for name in [
            "claude__a.json.tmp-1-0",
            "claude__b.lock",
            "claude__c.json.tmp-2-0",
            "claude__d.lock",
        ] {
            fs::write(sessions.join(name), b"").unwrap();
        }
        let old = Duration::from_secs(4 * 86_400);
        set_age(&sessions.join("claude__a.json.tmp-1-0"), old);
        set_age(&sessions.join("claude__b.lock"), old);
        let removed = prune(dir, 10 * DAY, 3 * DAY).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(names(dir), ["claude__c.json.tmp-2-0", "claude__d.lock"]);
        // A missing sessions dir prunes nothing.
        assert_eq!(prune(&dir.join("nope"), 10 * DAY, 3 * DAY).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn prune_skips_a_session_whose_lock_is_held() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        put(dir, "busy", 1);
        let holder = OpenOptions::new()
            .write(true)
            .open(dir.join("sessions").join("claude__busy.lock"))
            .unwrap();
        holder.lock().unwrap();
        assert_eq!(prune(dir, 10 * DAY, 3 * DAY).unwrap(), 0);
        assert!(json_path(dir, "claude__busy").exists());
        drop(holder);
        assert_eq!(prune(dir, 10 * DAY, 3 * DAY).unwrap(), 2);
    }

    /// On Windows prune deletes stale session files but never lock files.
    #[cfg(windows)]
    #[test]
    fn prune_keeps_lock_files_on_windows() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = 10 * DAY;
        put(dir, "old", now - 3 * DAY - 1);
        let sessions = dir.join("sessions");
        fs::write(sessions.join("claude__orphan.lock"), b"").unwrap();
        set_age(&sessions.join("claude__orphan.lock"), Duration::from_secs(4 * 86_400));
        assert_eq!(prune(dir, now, 3 * DAY).unwrap(), 1, "only the old json");
        assert_eq!(names(dir), ["claude__old.lock", "claude__orphan.lock"]);
    }

    /// A writer that was waiting on a lock file prune then deleted must not
    /// keep using that unlinked file, or it could run beside a writer that
    /// locked the new file at the same path.
    #[cfg(unix)]
    #[test]
    fn writer_relocks_after_lock_file_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let lock_path = sessions.join("claude__s.lock");
        let open = |p: &Path| {
            OpenOptions::new().create(true).truncate(false).write(true).open(p).unwrap()
        };
        let first = open(&lock_path);
        first.lock().unwrap();
        let writer_dir = dir.clone();
        let writer = thread::spawn(move || {
            update_session(&writer_dir, "claude", "s", |_| Some(state("claude", "s")))
        });
        thread::sleep(Duration::from_millis(150));
        fs::remove_file(&lock_path).unwrap();
        let second = open(&lock_path);
        second.lock().unwrap();
        drop(first);
        thread::sleep(Duration::from_millis(150));
        assert!(!json_path(&dir, "claude__s").exists(), "writer ran beside the new lock holder");
        drop(second);
        writer.join().unwrap().unwrap();
        assert!(json_path(&dir, "claude__s").exists());
    }

    #[test]
    fn reset_removes_sessions_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        update_session(dir, "claude", "s1", |_| Some(state("claude", "s1"))).unwrap();
        reset(dir).unwrap();
        assert!(!dir.join("sessions").exists());
        assert!(dir.exists());
        // Resetting an already-empty dir is fine.
        reset(dir).unwrap();
    }
}
