//! Per-session state record and the hook-event state machine.

use serde::{Deserialize, Serialize};

use crate::input::{truncate, HookInput, HookKind};

/// The status a session row shows.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Running,
    WaitingApproval,
    Completed,
    Error,
}

/// A shell command the agent ran.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CommandRun {
    pub command: String,
    pub ok: bool,
    pub at_ms: i64,
}

/// A file the agent edited.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct FileTouch {
    pub path: String,
    pub at_ms: i64,
}

/// Everything agent-pin remembers about one agent session.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SessionState {
    pub provider: String,
    pub session_key: String,
    pub cwd: Option<String>,
    pub status: Status,
    pub updated_ms: i64,
    #[serde(default)]
    pub seen_ms: i64,
    pub original_ask: Option<String>,
    pub last_ask: Option<String>,
    pub pending: Option<String>,
    pub files: Vec<FileTouch>,
    pub commands: Vec<CommandRun>,
}

/// Max chars kept for any free-text or path field.
const TEXT_MAX: usize = 200;
/// Max chars kept for an edited file path (long enough that collision keys
/// stay faithful).
const PATH_MAX: usize = 400;
/// Max items kept in `files` and `commands` (the most recent ones).
const LIST_MAX: usize = 20;
/// Max chars in a session key.
const KEY_MAX: usize = 64;

/// Applies one hook event to a session. `None` means "delete this session".
pub fn apply(
    prev: Option<SessionState>,
    provider: &str,
    kind: HookKind,
    input: &HookInput,
    now_ms: i64,
) -> Option<SessionState> {
    match kind {
        HookKind::Other => return prev,
        HookKind::SessionEnd => return None,
        _ => {}
    }
    let mut s = prev.unwrap_or_else(|| SessionState {
        provider: provider.to_owned(),
        session_key: session_key(input),
        cwd: None,
        status: Status::Idle,
        updated_ms: now_ms,
        seen_ms: 0,
        original_ask: None,
        last_ask: None,
        pending: None,
        files: Vec::new(),
        commands: Vec::new(),
    });
    s.updated_ms = now_ms;
    // An empty cwd counts as absent: it neither replaces the stored cwd
    // nor serves as a join base.
    let input_cwd = input.cwd.as_deref().filter(|c| !c.is_empty());
    if let Some(cwd) = input_cwd {
        s.cwd = Some(clean(cwd, TEXT_MAX));
    }
    let tool = input.tool_name.as_deref();
    match kind {
        HookKind::SessionStart => s.status = Status::Idle,
        HookKind::UserPromptSubmit => {
            s.status = Status::Running;
            if let Some(prompt) = &input.prompt {
                let ask = clean(prompt, TEXT_MAX);
                if s.original_ask.is_none() {
                    s.original_ask = Some(ask.clone());
                }
                s.last_ask = Some(ask);
            }
        }
        HookKind::PreToolUse => {
            s.status = Status::Running;
            let edits = matches!(tool, Some("Edit" | "Write" | "MultiEdit" | "NotebookEdit"));
            if let Some(path) = input.file_path.as_deref().filter(|_| edits) {
                // Join with the raw input cwd: the stored one may be truncated.
                let cwd = input_cwd.or(s.cwd.as_deref());
                let path = absolute(cwd, path);
                let touch = FileTouch { path: clean(&path, PATH_MAX), at_ms: now_ms };
                push_capped(&mut s.files, touch);
            }
        }
        HookKind::PermissionRequest => {
            s.status = Status::WaitingApproval;
            let name = tool.unwrap_or("Tool");
            let pending = match input.command.as_ref().or(input.file_path.as_ref()) {
                Some(detail) => format!("{name}: {detail}"),
                None => name.to_owned(),
            };
            s.pending = Some(clean(&pending, TEXT_MAX));
        }
        HookKind::PostToolUse | HookKind::PostToolUseFailure => {
            s.status = Status::Running;
            s.pending = None;
            let ok = kind == HookKind::PostToolUse;
            let record = !ok || tool == Some("Bash");
            if let Some(command) = input.command.as_deref().filter(|_| record) {
                let run = CommandRun { command: clean(command, TEXT_MAX), ok, at_ms: now_ms };
                push_capped(&mut s.commands, run);
            }
        }
        HookKind::Stop => {
            s.status = Status::Completed;
            s.pending = None;
        }
        HookKind::StopFailure => s.status = Status::Error,
        // Returned early above; kept non-panicking so the hook never panics.
        HookKind::SessionEnd | HookKind::Other => {}
    }
    Some(s)
}

/// The session's file key: `session_id` sanitized to `[A-Za-z0-9_-]`
/// (max 64 chars), or `"default"`.
pub fn session_key(input: &HookInput) -> String {
    let key: String = input
        .session_id
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(KEY_MAX)
        .collect();
    if key.is_empty() {
        "default".to_owned()
    } else {
        key
    }
}

/// Truncates to `max_chars` and replaces control characters with spaces,
/// so stored text stays byte-bounded (no 6-byte `\uXXXX` JSON escapes).
fn clean(s: &str, max_chars: usize) -> String {
    truncate(s, max_chars)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Appends `item`, keeping only the last `LIST_MAX` items.
fn push_capped<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > LIST_MAX {
        list.drain(..list.len() - LIST_MAX);
    }
}

/// Makes `path` absolute by joining it onto `cwd` when it is relative.
/// String-level only: `/`-rooted and Windows drive paths (`C:\`, `C:/`)
/// count as absolute on every platform, and the filesystem is not touched.
fn absolute(cwd: Option<&str>, path: &str) -> String {
    match cwd {
        Some(cwd) if !is_absolute(path) => {
            std::path::Path::new(cwd).join(path).to_string_lossy().into_owned()
        }
        _ => path.to_owned(),
    }
}

fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b[2], b'\\' | b'/');
    path.starts_with('/') || drive
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> HookInput {
        HookInput {
            session_id: Some("s1".to_owned()),
            cwd: Some("/r".to_owned()),
            ..HookInput::default()
        }
    }

    fn tool(name: &str, command: Option<&str>, file_path: Option<&str>) -> HookInput {
        HookInput {
            tool_name: Some(name.to_owned()),
            command: command.map(str::to_owned),
            file_path: file_path.map(str::to_owned),
            ..input()
        }
    }

    fn prompt(text: &str) -> HookInput {
        HookInput {
            prompt: Some(text.to_owned()),
            ..input()
        }
    }

    fn started() -> SessionState {
        apply(None, "claude", HookKind::SessionStart, &input(), 1).unwrap()
    }

    #[test]
    fn session_start_is_idle() {
        let s = started();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.provider, "claude");
        assert_eq!(s.session_key, "s1");
        assert_eq!(s.cwd.as_deref(), Some("/r"));
        assert_eq!(s.updated_ms, 1);
        assert_eq!(s.seen_ms, 0);
    }

    #[test]
    fn first_prompt_becomes_original_ask_and_is_kept() {
        let s = apply(Some(started()), "claude", HookKind::UserPromptSubmit, &prompt("first"), 2);
        let s = apply(s, "claude", HookKind::UserPromptSubmit, &prompt("second"), 3).unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.original_ask.as_deref(), Some("first"));
        assert_eq!(s.last_ask.as_deref(), Some("second"));
        assert_eq!(s.updated_ms, 3);
    }

    #[test]
    fn permission_request_waits_with_pending_text() {
        let req = tool("Bash", Some("rm -rf build"), None);
        let s = apply(Some(started()), "claude", HookKind::PermissionRequest, &req, 2).unwrap();
        assert_eq!(s.status, Status::WaitingApproval);
        assert_eq!(s.pending.as_deref(), Some("Bash: rm -rf build"));
    }

    #[test]
    fn post_tool_use_clears_pending_and_records_ok_command() {
        let req = tool("Bash", Some("cargo test"), None);
        let s = apply(Some(started()), "claude", HookKind::PermissionRequest, &req, 2);
        let s = apply(s, "claude", HookKind::PostToolUse, &req, 3).unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.pending, None);
        assert_eq!(
            s.commands,
            vec![CommandRun { command: "cargo test".to_owned(), ok: true, at_ms: 3 }]
        );
    }

    #[test]
    fn post_tool_use_non_bash_records_no_command() {
        let req = tool("Read", Some("x"), None);
        let s = apply(Some(started()), "claude", HookKind::PostToolUse, &req, 2).unwrap();
        assert!(s.commands.is_empty());
    }

    #[test]
    fn post_tool_use_failure_is_running_not_error() {
        let req = tool("Bash", Some("false"), None);
        let s = apply(Some(started()), "claude", HookKind::PermissionRequest, &req, 2);
        let s = apply(s, "claude", HookKind::PostToolUseFailure, &req, 3).unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.pending, None);
        assert_eq!(
            s.commands,
            vec![CommandRun { command: "false".to_owned(), ok: false, at_ms: 3 }]
        );
    }

    #[test]
    fn edit_records_absolute_path() {
        let req = tool("Edit", None, Some("src/a.rs"));
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.files, vec![FileTouch { path: "/r/src/a.rs".to_owned(), at_ms: 2 }]);
    }

    #[test]
    fn edit_keeps_already_absolute_paths() {
        for p in ["/x/a.rs", "C:\\x\\a.rs", "C:/x/a.rs", "d:\\a.rs"] {
            let req = tool("Write", None, Some(p));
            let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
            assert_eq!(s.files[0].path, p);
        }
    }

    #[test]
    fn relative_path_joins_full_long_cwd() {
        let cwd = format!("/{}", "d".repeat(300));
        let req = HookInput { cwd: Some(cwd.clone()), ..tool("Edit", None, Some("a.rs")) };
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.files[0].path, format!("{cwd}/a.rs"));
    }

    #[test]
    fn relative_path_falls_back_to_stored_cwd() {
        let req = HookInput { cwd: None, ..tool("Edit", None, Some("a.rs")) };
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.files[0].path, "/r/a.rs");
    }

    #[test]
    fn file_path_capped_at_400_chars() {
        let long = format!("/{}", "p".repeat(350));
        let req = tool("Edit", None, Some(&long));
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.files[0].path, long);
        let huge = format!("/{}", "p".repeat(2000));
        let req = tool("Edit", None, Some(&huge));
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.files[0].path.chars().count(), 401);
        assert!(s.files[0].path.ends_with('…'));
    }

    #[test]
    fn empty_cwd_falls_back_to_stored_cwd() {
        let req = HookInput { cwd: Some(String::new()), ..tool("Edit", None, Some("a.rs")) };
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert_eq!(s.files[0].path, "/r/a.rs");
        assert_eq!(s.cwd.as_deref(), Some("/r"));
    }

    #[test]
    fn control_chars_replaced_with_space() {
        let s = apply(Some(started()), "claude", HookKind::UserPromptSubmit, &prompt("a\nb\tc\u{7}"), 2);
        let cwd = HookInput { cwd: Some("/r\n/x".to_owned()), ..tool("Edit", None, Some("a\rb.rs")) };
        let s = apply(s, "claude", HookKind::PreToolUse, &cwd, 3).unwrap();
        assert_eq!(s.cwd.as_deref(), Some("/r /x"));
        assert_eq!(s.files[0].path, "/r /x/a b.rs");
        let bash = tool("Bash", Some("ls\n-la\u{1b}"), None);
        let s = apply(Some(s), "claude", HookKind::PermissionRequest, &bash, 4).unwrap();
        assert_eq!(s.original_ask.as_deref(), Some("a b c "));
        assert_eq!(s.last_ask.as_deref(), Some("a b c "));
        assert_eq!(s.pending.as_deref(), Some("Bash: ls -la "));
        let s = apply(Some(s), "claude", HookKind::PostToolUse, &bash, 5).unwrap();
        assert_eq!(s.commands[0].command, "ls -la ");
    }

    #[test]
    fn worst_case_session_stays_under_64kb() {
        // 4-byte chars mixed with chars JSON escapes ('\\' and '"').
        let nasty: String = "😀😀😀\\\"".repeat(2000);
        let mut s = Some(started());
        s = apply(s, "claude", HookKind::UserPromptSubmit, &prompt(&nasty), 1);
        for i in 0..LIST_MAX as i64 {
            let path = format!("/{i}{nasty}");
            let bash = tool("Bash", Some(&format!("{i}{nasty}")), None);
            let edit = HookInput { cwd: Some(nasty.clone()), ..tool("Edit", None, Some(&path)) };
            s = apply(s, "claude", HookKind::PreToolUse, &edit, i);
            s = apply(s, "claude", HookKind::PostToolUse, &bash, i);
        }
        let s = apply(s, "claude", HookKind::UserPromptSubmit, &prompt(&nasty), 98);
        let req = HookInput { cwd: Some(nasty.clone()), ..tool("Bash", Some(&nasty), None) };
        let s = apply(s, "claude", HookKind::PermissionRequest, &req, 99).unwrap();
        let at_cap = |v: &str, max: usize| v.chars().count() == max + 1;
        assert_eq!(s.files.len(), LIST_MAX);
        assert_eq!(s.commands.len(), LIST_MAX);
        assert!(s.files.iter().all(|f| at_cap(&f.path, PATH_MAX)));
        assert!(s.commands.iter().all(|c| at_cap(&c.command, TEXT_MAX)));
        for text in [&s.original_ask, &s.last_ask, &s.pending, &s.cwd] {
            assert!(at_cap(text.as_deref().unwrap(), TEXT_MAX));
        }
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.len() < 64 * 1024, "json is {} bytes", json.len());
    }

    #[test]
    fn non_edit_tool_records_no_file() {
        let req = tool("Read", None, Some("src/a.rs"));
        let s = apply(Some(started()), "claude", HookKind::PreToolUse, &req, 2).unwrap();
        assert!(s.files.is_empty());
    }

    #[test]
    fn stop_completes_and_clears_pending() {
        let req = tool("Bash", Some("ls"), None);
        let s = apply(Some(started()), "claude", HookKind::PermissionRequest, &req, 2);
        let s = apply(s, "claude", HookKind::Stop, &input(), 3).unwrap();
        assert_eq!(s.status, Status::Completed);
        assert_eq!(s.pending, None);
    }

    #[test]
    fn stop_failure_is_error() {
        let s = apply(Some(started()), "claude", HookKind::StopFailure, &input(), 2).unwrap();
        assert_eq!(s.status, Status::Error);
        assert_eq!(s.updated_ms, 2);
    }

    #[test]
    fn session_end_deletes() {
        assert_eq!(apply(Some(started()), "claude", HookKind::SessionEnd, &input(), 2), None);
        assert_eq!(apply(None, "claude", HookKind::SessionEnd, &input(), 2), None);
    }

    #[test]
    fn other_event_changes_nothing() {
        let prev = started();
        let other = HookInput { cwd: Some("/elsewhere".to_owned()), ..input() };
        assert_eq!(apply(Some(prev.clone()), "claude", HookKind::Other, &other, 9), Some(prev));
        assert_eq!(apply(None, "claude", HookKind::Other, &other, 9), None);
    }

    #[test]
    fn cwd_updates_when_input_has_one() {
        let moved = HookInput { cwd: Some("/new".to_owned()), ..input() };
        let s = apply(Some(started()), "claude", HookKind::Stop, &moved, 2).unwrap();
        assert_eq!(s.cwd.as_deref(), Some("/new"));
        let no_cwd = HookInput { cwd: None, ..input() };
        let s = apply(Some(s), "claude", HookKind::Stop, &no_cwd, 3).unwrap();
        assert_eq!(s.cwd.as_deref(), Some("/new"));
    }

    #[test]
    fn lists_capped_at_20() {
        let mut s = Some(started());
        for i in 0..25 {
            let cmd = format!("cmd{i}");
            let file = format!("f{i}.rs");
            let bash = tool("Bash", Some(&cmd), None);
            let edit = tool("Edit", None, Some(&file));
            s = apply(s, "claude", HookKind::PostToolUse, &bash, i);
            s = apply(s, "claude", HookKind::PreToolUse, &edit, i);
        }
        let s = s.unwrap();
        assert_eq!(s.commands.len(), 20);
        assert_eq!(s.files.len(), 20);
        assert_eq!(s.commands[0].command, "cmd5");
        assert_eq!(s.commands[19].command, "cmd24");
        assert_eq!(s.files[0].path, "/r/f5.rs");
        assert_eq!(s.files[19].path, "/r/f24.rs");
    }

    #[test]
    fn huge_command_is_truncated() {
        let huge = "x".repeat(1 << 20);
        let mut s = Some(started());
        s = apply(s, "claude", HookKind::UserPromptSubmit, &prompt(&huge), 2);
        for i in 0..25 {
            let bash = tool("Bash", Some(&huge), Some(&huge));
            s = apply(s, "claude", HookKind::PermissionRequest, &bash, i);
            s = apply(s, "claude", HookKind::PostToolUse, &bash, i);
            let edit = HookInput { cwd: Some(huge.clone()), ..tool("Edit", None, Some(&huge)) };
            s = apply(s, "claude", HookKind::PreToolUse, &edit, i);
        }
        let s = apply(s, "claude", HookKind::PermissionRequest, &tool("Bash", Some(&huge), None), 99);
        let json = serde_json::to_string(&s.unwrap()).unwrap();
        assert!(json.len() < 64 * 1024, "json is {} bytes", json.len());
    }

    #[test]
    fn session_key_fallback_and_sanitize() {
        assert_eq!(session_key(&HookInput::default()), "default");
        let k = |id: &str| session_key(&HookInput { session_id: Some(id.to_owned()), ..HookInput::default() });
        assert_eq!(k("a/b c"), "abc");
        assert_eq!(k("A-z_09"), "A-z_09");
        assert_eq!(k("../.."), "default");
        assert_eq!(k(""), "default");
        assert_eq!(k(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn status_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Status::WaitingApproval).unwrap(), "\"waiting_approval\"");
    }

    #[test]
    fn seen_ms_defaults_to_zero() {
        let mut v = serde_json::to_value(started()).unwrap();
        v.as_object_mut().unwrap().remove("seen_ms");
        let s: SessionState = serde_json::from_value(v).unwrap();
        assert_eq!(s.seen_ms, 0);
    }
}
