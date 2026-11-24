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
    // The lock file is never removed: deleting it would let two writers
    // lock different inodes for the same session.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sessions.join(format!("{stem}.lock")))?;
    lock_bounded(&lock)?;
    let path = sessions.join(format!("{stem}.json"));
    match f(read_state(&path)?) {
        Some(state) => write_atomic(&sessions, stem, &path, &state),
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
    // `lock` drops here, releasing the lock.
}

/// Takes an exclusive lock on `file`, retrying until [`LOCK_TIMEOUT`].
fn lock_bounded(file: &File) -> io::Result<()> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
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

/// Accepts only non-empty stems of `[A-Za-z0-9_-]`, so a stem can never
/// name a path outside the sessions dir on any platform.
fn check_stem(stem: &str) -> io::Result<()> {
    let ok = !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid session key: {stem:?}"),
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
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = sessions.join(format!("{stem}.json.tmp-{}-{n}", std::process::id()));
    let result = (|| {
        let body = serde_json::to_vec(state).map_err(io::Error::other)?;
        let mut file = File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
        assert!(waited < std::time::Duration::from_secs(3), "{waited:?}");
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
