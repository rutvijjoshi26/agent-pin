//! Parsing of hook stdin payloads, provider ids and small string helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hook event kind, matching Claude Code's event names (case-sensitive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    StopFailure,
    SessionEnd,
    Other,
}

impl HookKind {
    /// Parses a Claude hook event name. Unknown names map to `Other`.
    pub fn parse(name: &str) -> HookKind {
        match name {
            "SessionStart" => HookKind::SessionStart,
            "UserPromptSubmit" => HookKind::UserPromptSubmit,
            "PreToolUse" => HookKind::PreToolUse,
            "PermissionRequest" => HookKind::PermissionRequest,
            "PostToolUse" => HookKind::PostToolUse,
            "PostToolUseFailure" => HookKind::PostToolUseFailure,
            "Stop" => HookKind::Stop,
            "StopFailure" => HookKind::StopFailure,
            "SessionEnd" => HookKind::SessionEnd,
            _ => HookKind::Other,
        }
    }
}

/// The fields agent-pin reads from a hook's stdin JSON payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub file_path: Option<String>,
    pub command: Option<String>,
}

impl HookInput {
    /// Parses a hook payload. Never fails: empty, invalid or non-object
    /// input, and fields of the wrong type, all yield `None`.
    pub fn from_json_lossy(raw: &str) -> HookInput {
        HookInput::from_json(raw).unwrap_or_default()
    }

    /// Parses a hook payload that must be a JSON object; anything else
    /// (empty, truncated, invalid, or a non-object) is `None`. Fields of the
    /// wrong type still yield `None` fields.
    pub fn from_json(raw: &str) -> Option<HookInput> {
        let root = match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(map)) => map,
            _ => return None,
        };
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        let tool_input = root.get("tool_input");
        let tool_field = |key: &str| text(tool_input.and_then(|t| t.get(key)));
        Some(HookInput {
            session_id: text(root.get("session_id")),
            cwd: text(root.get("cwd")),
            prompt: text(root.get("prompt")),
            tool_name: text(root.get("tool_name")),
            file_path: tool_field("file_path").or_else(|| tool_field("notebook_path")),
            command: tool_field("command"),
        })
    }
}

/// Normalizes a provider id: lowercased, and it must match
/// `^[a-z0-9][a-z0-9-]{0,31}$`; anything else becomes `agent`.
pub fn normalize_provider(raw: &str) -> String {
    let id = raw.trim().to_ascii_lowercase();
    let valid = (1..=32).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        id
    } else {
        "agent".to_owned()
    }
}

/// Human-readable name for a provider id. Known ids use a fixed table;
/// any other id is shown with its first character capitalized.
pub fn display_name(provider: &str) -> String {
    match provider {
        "claude" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity" => "Antigravity".to_owned(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// Truncates to at most `max_chars` chars (at a char boundary), appending
/// `…` when anything was cut.
pub fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => format!("{}…", &s[..byte_idx]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_pretooluse() {
        let raw = r#"{"session_id":"s1","cwd":"/r","tool_name":"Edit","tool_input":{"file_path":"/r/a.rs"}}"#;
        let input = HookInput::from_json_lossy(raw);
        assert_eq!(input.session_id.as_deref(), Some("s1"));
        assert_eq!(input.cwd.as_deref(), Some("/r"));
        assert_eq!(input.tool_name.as_deref(), Some("Edit"));
        assert_eq!(input.file_path.as_deref(), Some("/r/a.rs"));
        assert_eq!(input.command, None);
    }

    #[test]
    fn parses_notebook_path_fallback() {
        let raw = r#"{"tool_name":"NotebookEdit","tool_input":{"notebook_path":"/r/n.ipynb"}}"#;
        let input = HookInput::from_json_lossy(raw);
        assert_eq!(input.file_path.as_deref(), Some("/r/n.ipynb"));
    }

    #[test]
    fn parses_bash_command() {
        let raw = r#"{"session_id":"s1","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
        let input = HookInput::from_json_lossy(raw);
        assert_eq!(input.command.as_deref(), Some("cargo test"));
        assert_eq!(input.file_path, None);
    }

    #[test]
    fn parses_prompt() {
        let raw = r#"{"session_id":"s1","prompt":"fix the bug"}"#;
        let input = HookInput::from_json_lossy(raw);
        assert_eq!(input.prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn empty_and_garbage_input_is_all_none() {
        for raw in ["", "{not json", "[1,2]", "\"str\"", "null"] {
            assert_eq!(HookInput::from_json_lossy(raw), HookInput::default(), "input {raw:?}");
        }
    }

    #[test]
    fn wrong_field_types_are_none() {
        let raw = r#"{"session_id":5,"cwd":null,"tool_input":"x"}"#;
        assert_eq!(HookInput::from_json_lossy(raw), HookInput::default());
    }

    #[test]
    fn hook_kind_parses_claude_names() {
        assert_eq!(HookKind::parse("SessionStart"), HookKind::SessionStart);
        assert_eq!(HookKind::parse("UserPromptSubmit"), HookKind::UserPromptSubmit);
        assert_eq!(HookKind::parse("PreToolUse"), HookKind::PreToolUse);
        assert_eq!(HookKind::parse("PermissionRequest"), HookKind::PermissionRequest);
        assert_eq!(HookKind::parse("PostToolUse"), HookKind::PostToolUse);
        assert_eq!(HookKind::parse("PostToolUseFailure"), HookKind::PostToolUseFailure);
        assert_eq!(HookKind::parse("Stop"), HookKind::Stop);
        assert_eq!(HookKind::parse("StopFailure"), HookKind::StopFailure);
        assert_eq!(HookKind::parse("SessionEnd"), HookKind::SessionEnd);
    }

    #[test]
    fn hook_kind_unknown_is_other() {
        assert_eq!(HookKind::parse("Notification"), HookKind::Other);
        assert_eq!(HookKind::parse("stop"), HookKind::Other);
        assert_eq!(HookKind::parse(""), HookKind::Other);
    }

    #[test]
    fn provider_normalization() {
        assert_eq!(normalize_provider("Claude"), "claude");
        assert_eq!(normalize_provider("my agent!"), "agent");
        assert_eq!(normalize_provider("antigravity"), "antigravity");
        assert_eq!(normalize_provider("codex"), "codex");
        assert_eq!(normalize_provider("gemini-cli"), "gemini-cli");
        assert_eq!(normalize_provider(""), "agent");
        assert_eq!(normalize_provider("-lead"), "agent");
        assert_eq!(normalize_provider(&"a".repeat(32)), "a".repeat(32));
        assert_eq!(normalize_provider(&"a".repeat(33)), "agent");
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("claude"), "Claude");
        assert_eq!(display_name("codex"), "Codex");
        assert_eq!(display_name("antigravity"), "Antigravity");
        assert_eq!(display_name("kimi"), "Kimi");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn truncate_char_boundary() {
        assert_eq!(truncate("héllo wörld", 4), "héll…");
        assert_eq!(truncate("héllo", 5), "héllo");
        assert_eq!(truncate("", 3), "");
        assert_eq!(truncate("abc", 0), "…");
    }
}
