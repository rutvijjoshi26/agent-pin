//! The `agent-pin hook <provider> <event>` entry point. It never panics and
//! never blocks for long: errors are returned for the caller to print to
//! stderr before exiting 0.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::input::{normalize_provider, HookInput, HookKind};
use crate::session::{apply, session_key};
use crate::store::update_session;

/// Most bytes of stdin a hook parses. A larger payload is drained and only
/// its `session_id` and `cwd` are salvaged from the first `STDIN_MAX` bytes.
pub const STDIN_MAX: u64 = 1 << 20;

/// Runs one hook event. `args` is `[provider, event_name]`; `stdin` is the
/// hook's JSON payload. A payload that is not a complete JSON object (empty,
/// truncated, garbage, or over 1 MB) keeps only `session_id` and `cwd`
/// salvaged from its raw bytes so the status still moves; without a
/// salvageable `session_id` the event is skipped. The `default` session key
/// is used only for a valid JSON object with no `session_id`. The caller bounds how
/// long reading `stdin` can take. Errors are returned, never panicked.
pub fn run_hook(
    args: &[String],
    mut stdin: impl Read,
    state_dir: Option<PathBuf>,
    now_ms: i64,
) -> Result<(), String> {
    let [provider, event, ..] = args else {
        return Err("usage: agent-pin hook <provider> <event>".to_owned());
    };
    let mut raw = Vec::new();
    (&mut stdin)
        .take(STDIN_MAX + 1)
        .read_to_end(&mut raw)
        .map_err(|e| format!("reading hook stdin: {e}"))?;
    let oversized = raw.len() as u64 > STDIN_MAX;
    if oversized {
        // Drain the rest so the writer never sees a broken pipe.
        let _ = io::copy(&mut stdin, &mut io::sink());
    }
    let dir = state_dir.ok_or("no state directory: set AGENT_PIN_HOME or the home directory")?;
    let provider = normalize_provider(provider);
    let kind = HookKind::parse(event);
    if oversized {
        raw.truncate(STDIN_MAX as usize);
    }
    let parsed = if oversized {
        None
    } else {
        HookInput::from_json(&String::from_utf8_lossy(&raw))
    };
    let input = match parsed {
        Some(input) => input,
        // Not a complete JSON object (empty, truncated, garbage or too
        // big): salvage session_id and cwd, or record nothing.
        None => {
            let Some(session_id) = salvage_string(&raw, "session_id") else {
                return Ok(());
            };
            HookInput {
                session_id: Some(session_id),
                cwd: salvage_string(&raw, "cwd"),
                ..HookInput::default()
            }
        }
    };
    let key = session_key(&input);
    update_session(&dir, &provider, &key, |prev| apply(prev, &provider, kind, &input, now_ms))
        .map_err(|e| format!("updating session {provider}__{key}: {e}"))
}

/// Finds the first `"<key>"`, optional whitespace, `:`, optional whitespace
/// and a complete JSON string in `raw`, and returns the decoded string.
/// Used on a truncated payload, so an unterminated value yields `None`.
fn salvage_string(raw: &[u8], key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let needle = needle.as_bytes();
    let mut from = 0;
    while let Some(pos) = find(&raw[from..], needle) {
        let after = from + pos + needle.len();
        if let Some(value) = string_after_colon(&raw[after..]) {
            return Some(value);
        }
        from = after;
    }
    None
}

/// Parses `ws : ws "..."` at the start of `rest`.
fn string_after_colon(rest: &[u8]) -> Option<String> {
    let rest = rest.trim_ascii_start().strip_prefix(b":")?.trim_ascii_start();
    if rest.first() != Some(&b'"') {
        return None;
    }
    let mut escaped = false;
    for (i, &b) in rest.iter().enumerate().skip(1) {
        match b {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => return serde_json::from_slice(&rest[..=i]).ok(),
            _ => {}
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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

    fn json_count(dir: &Path) -> usize {
        fs::read_dir(dir.join("sessions"))
            .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0)
    }

    #[test]
    fn garbage_stdin_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let garbage: &[u8] = b"\xff\xfe not json {{{";
        run_hook(&args("codex", "SessionStart"), garbage, Some(dir.clone()), 5).unwrap();
        // Empty stdin (e.g. a reader timeout) records nothing either.
        run_hook(&args("codex", "Stop"), &b""[..], Some(dir.clone()), 6).unwrap();
        assert_eq!(json_count(&dir), 0);
        assert!(!session_file(&dir, "codex__default").exists());
    }

    #[test]
    fn valid_json_without_session_id_uses_default() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        run_hook(&args("codex", "Stop"), &br#"{"cwd":"/repo"}"#[..], Some(dir.clone()), 7).unwrap();
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_key, "default");
        assert_eq!(all[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(all[0].updated_ms, 7);
    }

    #[test]
    fn partial_json_salvages_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let start = br#"{"session_id":"s1","cwd":"/repo"}"#;
        run_hook(&args("claude", "PermissionRequest"), &start[..], Some(dir.clone()), 1).unwrap();
        // Truncated mid-payload, well under 1 MB.
        let partial = br#"{"session_id":"s1","cwd":"/repo","tool_name":"Bash","tool_input":{"command":"ca"#;
        run_hook(&args("claude", "PostToolUse"), &partial[..], Some(dir.clone()), 2).unwrap();
        assert!(!session_file(&dir, "claude__default").exists());
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_key, "s1");
        assert_eq!(all[0].status, Status::Running);
        assert_eq!(all[0].updated_ms, 2);
        assert_eq!(all[0].cwd.as_deref(), Some("/repo"));
    }

    /// A payload of more than 1 MB: `head` then a huge `tool_response`.
    fn oversized(head: &str) -> String {
        format!(r#"{{{head}"tool_response":"{}"}}"#, "x".repeat(3 << 20))
    }

    #[test]
    fn oversized_payload_updates_real_session() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let start = br#"{"session_id":"s1","cwd":"/repo"}"#;
        run_hook(&args("claude", "PermissionRequest"), &start[..], Some(dir.clone()), 1).unwrap();
        assert_eq!(list_sessions(&dir)[0].status, Status::WaitingApproval);
        let big = oversized(r#""session_id" : "s1", "cwd":"/repo","tool_name":"Bash","#);
        run_hook(&args("claude", "PostToolUse"), big.as_bytes(), Some(dir.clone()), 2).unwrap();
        assert!(!session_file(&dir, "claude__default").exists());
        let all = list_sessions(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_key, "s1");
        assert_eq!(all[0].status, Status::Running);
        assert_eq!(all[0].updated_ms, 2);
        assert_eq!(all[0].cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn oversized_payload_without_session_id_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let big = oversized(r#""cwd":"/repo","#);
        run_hook(&args("claude", "PostToolUse"), big.as_bytes(), Some(dir.clone()), 1).unwrap();
        let sessions = dir.join("sessions");
        let jsons = fs::read_dir(&sessions)
            .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0);
        assert_eq!(jsons, 0);
    }

    #[test]
    fn salvage_handles_escaped_quote() {
        let raw = br#"{"session_id":"a\"b\\","cwd":"C:\\dir \"q\"","x":1"#;
        assert_eq!(salvage_string(raw, "session_id").as_deref(), Some("a\"b\\"));
        assert_eq!(salvage_string(raw, "cwd").as_deref(), Some("C:\\dir \"q\""));
        // An unterminated value (cut off by the cap) is not salvaged.
        assert_eq!(salvage_string(br#"{"session_id":"abc"#, "session_id"), None);
        assert_eq!(salvage_string(br#"{"other":"v"}"#, "session_id"), None);
        assert_eq!(salvage_string(br#"{"session_id":42}"#, "session_id"), None);
        // Via the hook: the escaped quote is dropped by key sanitizing.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let big = oversized(r#""session_id":"a\"b","#);
        run_hook(&args("claude", "PostToolUse"), big.as_bytes(), Some(dir.clone()), 1).unwrap();
        assert!(session_file(&dir, "claude__ab").is_file());
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
