//! The `agent-pin hook <provider> <event>` entry point. It never panics and
//! never blocks for long: errors are returned for the caller to print to
//! stderr before exiting 0.

use std::io::Read;
use std::path::PathBuf;

use crate::input::{normalize_provider, HookInput, HookKind};
use crate::session::{apply, session_key};
use crate::store::update_session;

/// Most bytes of stdin a hook reads; anything beyond is ignored.
const STDIN_MAX: u64 = 1 << 20;

/// Runs one hook event. `args` is `[provider, event_name]`; `stdin` is the
/// hook's JSON payload, of which at most 1 MB is read. The caller bounds
/// how long reading `stdin` can take. Errors are returned, never panicked.
pub fn run_hook(
    args: &[String],
    stdin: impl Read,
    state_dir: Option<PathBuf>,
    now_ms: i64,
) -> Result<(), String> {
    let [provider, event, ..] = args else {
        return Err("usage: agent-pin hook <provider> <event>".to_owned());
    };
    let mut raw = Vec::new();
    stdin
        .take(STDIN_MAX)
        .read_to_end(&mut raw)
        .map_err(|e| format!("reading hook stdin: {e}"))?;
    let dir = state_dir.ok_or("no state directory: set AGENT_PIN_HOME or HOME")?;
    let provider = normalize_provider(provider);
    let kind = HookKind::parse(event);
    let input = HookInput::from_json_lossy(&String::from_utf8_lossy(&raw));
    let key = session_key(&input);
    update_session(&dir, &provider, &key, |prev| apply(prev, &provider, kind, &input, now_ms))
        .map_err(|e| format!("updating session {provider}__{key}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Status;
    use crate::store::list_sessions;
    use std::fs;
    use std::path::Path;

    fn args(provider: &str, event: &str) -> Vec<String> {
        vec![provider.to_owned(), event.to_owned()]
    }

    fn session_file(dir: &Path, stem: &str) -> PathBuf {
        dir.join("sessions").join(format!("{stem}.json"))
    }

    #[test]
    fn writes_session_from_stdin() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let stdin = br#"{"session_id":"s1","cwd":"/repo","prompt":"fix it"}"#;
        run_hook(&args("Claude", "UserPromptSubmit"), &stdin[..], Some(dir.clone()), 1000).unwrap();
        assert!(session_file(&dir, "claude__s1").is_file());
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].provider, "claude");
        assert_eq!(all[0].session_key, "s1");
        assert_eq!(all[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(all[0].status, Status::Running);
        assert_eq!(all[0].updated_ms, 1000);
    }

    #[test]
    fn missing_args_is_err_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        assert!(run_hook(&[], &b"{}"[..], Some(dir.clone()), 1).is_err());
        assert!(run_hook(&["claude".to_owned()], &b"{}"[..], Some(dir.clone()), 1).is_err());
        assert!(!dir.join("sessions").exists());
        // No state dir is an error too.
        assert!(run_hook(&args("claude", "Stop"), &b"{}"[..], None, 1).is_err());
    }

    #[test]
    fn garbage_stdin_still_writes_default_session() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let garbage: &[u8] = b"\xff\xfe not json {{{";
        run_hook(&args("codex", "SessionStart"), garbage, Some(dir.clone()), 5).unwrap();
        assert!(session_file(&dir, "codex__default").is_file());
        // Empty stdin works the same way.
        run_hook(&args("codex", "Stop"), &b""[..], Some(dir.clone()), 6).unwrap();
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_key, "default");
        assert_eq!(all[0].updated_ms, 6);
    }

    #[test]
    fn oversized_stdin_is_capped_not_err() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // 3 MB of stdin: only 1 MB is read, which is not valid JSON, so
        // the event lands on the default session.
        let big = format!(r#"{{"session_id":"big","prompt":"{}"}}"#, "x".repeat(3 << 20));
        run_hook(&args("claude", "UserPromptSubmit"), big.as_bytes(), Some(dir.clone()), 1).unwrap();
        assert!(session_file(&dir, "claude__default").is_file());
    }

    #[test]
    fn unwritable_dir_is_err_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        let err = run_hook(&args("claude", "SessionStart"), &b"{}"[..], Some(file), 1);
        assert!(err.is_err());
    }

    #[test]
    fn session_end_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let stdin = br#"{"session_id":"s9"}"#;
        run_hook(&args("claude", "SessionStart"), &stdin[..], Some(dir.clone()), 1).unwrap();
        assert!(session_file(&dir, "claude__s9").is_file());
        run_hook(&args("claude", "SessionEnd"), &stdin[..], Some(dir.clone()), 2).unwrap();
        assert!(!session_file(&dir, "claude__s9").exists());
    }

    #[test]
    fn bad_provider_becomes_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        run_hook(&args("../evil", "SessionStart"), &b"{}"[..], Some(dir.clone()), 1).unwrap();
        assert!(session_file(&dir, "agent__default").is_file());
    }
}
