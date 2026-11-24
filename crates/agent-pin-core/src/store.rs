//! On-disk store: one JSON file per agent session under
//! `<state_dir>/sessions/<provider>__<session_key>.json`.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Read-modify-writes one session under an exclusive lock on
/// `<dir>/sessions/<provider>__<key>.lock`. A missing or corrupt file reads
/// as `None`; returning `None` removes the session file.
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
        .filter_map(|p| read_state(&p))
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
    lock.lock()?;
    let path = sessions.join(format!("{stem}.json"));
    match f(read_state(&path)) {
        Some(state) => write_atomic(&sessions, stem, &path, &state),
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
    // `lock` drops here, releasing the lock.
}

/// Rejects stems that could escape the sessions dir.
fn check_stem(stem: &str) -> io::Result<()> {
    if stem.is_empty() || stem.contains(['/', '\\']) || stem.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid session key: {stem:?}"),
        ));
    }
    Ok(())
}

fn read_state(path: &Path) -> Option<SessionState> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
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
        for bad in ["../x", "a/b", "a\\b", "..", ""] {
            let err = mark_seen(tmp.path(), bad, 1).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
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
