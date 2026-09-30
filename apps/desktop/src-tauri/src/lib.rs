use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{LogicalPosition, LogicalSize, Manager, Position, Size, WebviewWindow};

const EXPANDED_WIDTH: f64 = 180.0;
const EXPANDED_HEIGHT: f64 = 164.0;
const COMPACT_WIDTH: f64 = 54.0;
const COMPACT_HEIGHT: f64 = 70.0;
const SCREEN_MARGIN: f64 = 20.0;
const EXPANDED_SETUP_HEIGHT: f64 = 214.0;

#[derive(Debug, Deserialize)]
struct AgentEvent {
    provider: String,
    status: String,
    cwd: Option<String>,
    detail: Option<String>,
    event: Option<String>,
    timestamp: i64,
}

#[derive(Clone, Debug, Serialize)]
struct AgentSnapshot {
    id: String,
    name: String,
    status: String,
    cwd: Option<String>,
    detail: Option<String>,
    updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
struct IntegrationState {
    claude_installed: bool,
    codex_installed: bool,
    claude_path: String,
    codex_path: String,
    hook_command_path: String,
}

#[tauri::command]
fn read_status() -> Result<Vec<AgentSnapshot>, String> {
    let home = home_dir().ok_or_else(|| "Could not resolve home directory".to_string())?;
    let event_file = home.join(".agent-pin").join("events.jsonl");

    let mut claude = latest_from_events(&event_file, "claude");
    let mut codex = latest_from_events(&event_file, "codex");
    let mut antigravity = latest_from_events(&event_file, "antigravity");

    if let Some(inferred) = infer_codex_from_log(&home) {
        let current = codex.as_ref().map(|snapshot| snapshot.updated_at).unwrap_or(0);
        if inferred.updated_at > current {
            codex = Some(inferred);
        }
    }

    let now = now_epoch();
    let mut snapshots = Vec::new();
    if let Some(snapshot) = claude.take().and_then(|snapshot| normalize(snapshot, now)) {
        snapshots.push(snapshot);
    }
    if let Some(snapshot) = codex.take().and_then(|snapshot| normalize(snapshot, now)) {
        snapshots.push(snapshot);
    }
    if let Some(snapshot) = antigravity
        .take()
        .and_then(|snapshot| normalize(snapshot, now))
    {
        snapshots.push(snapshot);
    }

    Ok(snapshots)
}

#[tauri::command]
fn start_window_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|error| error.to_string())
}

#[tauri::command]
fn read_integrations() -> Result<IntegrationState, String> {
    integration_state()
}

#[tauri::command]
fn install_integrations() -> Result<IntegrationState, String> {
    install_hooks()?;
    integration_state()
}

#[tauri::command]
fn set_compact(window: WebviewWindow, compact: bool, setup_visible: Option<bool>) -> Result<(), String> {
    let size = if compact {
        LogicalSize::new(COMPACT_WIDTH, COMPACT_HEIGHT)
    } else if setup_visible.unwrap_or(false) {
        LogicalSize::new(EXPANDED_WIDTH, EXPANDED_SETUP_HEIGHT)
    } else {
        LogicalSize::new(EXPANDED_WIDTH, EXPANDED_HEIGHT)
    };

    window
        .set_size(Size::Logical(size))
        .map_err(|error| error.to_string())
}

pub fn write_hook_event(args: &[String]) -> Result<(), String> {
    let provider = args.first().map(String::as_str).unwrap_or("");
    let status = args.get(1).map(String::as_str).unwrap_or("");
    let detail = args.get(2).cloned().unwrap_or_default();

    if !["claude", "codex", "antigravity"].contains(&provider)
        || !["idle", "running", "waiting_approval", "completed", "error"].contains(&status)
    {
        return Err(
            "usage: agent-pin hook <claude|codex|antigravity> <idle|running|waiting_approval|completed|error> [detail]"
                .to_string(),
        );
    }

    let state_dir = std::env::var_os("AGENT_PIN_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".agent-pin")))
        .ok_or_else(|| "Could not resolve Agent Pin state directory".to_string())?;
    fs::create_dir_all(&state_dir).map_err(|error| error.to_string())?;

    let mut stdin = String::new();
    let _ = io::stdin().read_to_string(&mut stdin);
    let input = if stdin.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str::<Value>(&stdin).unwrap_or_else(|_| Value::Object(Map::new()))
    };

    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| std::env::current_dir().ok().map(|path| path.display().to_string()));
    let hook_event = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| std::env::var("HOOK_EVENT_NAME").ok())
        .or_else(|| std::env::var("CLAUDE_HOOK_EVENT_NAME").ok());
    let tool_name = input
        .get("tool_name")
        .and_then(Value::as_str)
        .or_else(|| input.pointer("/tool/name").and_then(Value::as_str))
        .or_else(|| input.pointer("/request/tool_name").and_then(Value::as_str))
        .or_else(|| input.pointer("/tool_input/name").and_then(Value::as_str))
        .map(ToString::to_string);

    let detail = [Some(detail.as_str()), hook_event.as_deref(), tool_name.as_deref()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let event = json!({
        "provider": provider,
        "status": status,
        "cwd": cwd,
        "detail": detail,
        "event": hook_event,
        "tool_name": tool_name,
        "session_id": input.get("session_id").and_then(Value::as_str),
        "timestamp": now_epoch()
    });

    let event_file = state_dir.join("events.jsonl");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(event_file)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{event}").map_err(|error| error.to_string())
}

fn latest_from_events(path: &Path, provider: &str) -> Option<AgentSnapshot> {
    let contents = fs::read_to_string(path).ok()?;
    let mut latest = None;

    for line in contents.lines() {
        let Ok(event) = serde_json::from_str::<AgentEvent>(line) else {
            continue;
        };

        if event.provider.to_lowercase() != provider {
            continue;
        }

        latest = Some(AgentSnapshot {
            id: provider.to_string(),
            name: display_name(provider).to_string(),
            status: event.status,
            cwd: event.cwd,
            detail: event.detail.or(event.event),
            updated_at: event.timestamp,
        });
    }

    latest
}

fn infer_codex_from_log(home: &Path) -> Option<AgentSnapshot> {
    let log_path = home.join(".codex").join("log").join("codex-tui.log");
    let updated_at = modified_epoch(&log_path)?;
    let log = read_tail(&log_path, 260_000).ok()?;
    let mut state = None;

    for line in log.lines() {
        if line.contains("codex_core::tasks: new")
            || (line.contains("model_client.stream_responses_websocket")
                && line.contains(": codex_core::client: new"))
        {
            state = Some(("running", "log activity"));
        } else if line.contains("ToolCall:")
            && line.contains("\"sandbox_permissions\":\"require_escalated\"")
        {
            state = Some(("waiting_approval", "approval requested"));
        } else if line.contains("codex.op=\"exec_approval\"") {
            state = Some(("running", "approval handled"));
        } else if line.contains("codex_core::tasks: close") {
            state = Some(("completed", "turn complete"));
        }
    }

    let (status, detail) = state?;
    Some(AgentSnapshot {
        id: "codex".to_string(),
        name: "Codex".to_string(),
        status: status.to_string(),
        cwd: None,
        detail: Some(detail.to_string()),
        updated_at,
    })
}

fn integration_state() -> Result<IntegrationState, String> {
    let home = home_dir().ok_or_else(|| "Could not resolve home directory".to_string())?;
    let claude_path = home.join(".claude").join("settings.json");
    let codex_path = home.join(".codex").join("hooks.json");
    let hook_command_path = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .display()
        .to_string();

    Ok(IntegrationState {
        claude_installed: has_agent_pin_hooks(&claude_path, "claude"),
        codex_installed: has_agent_pin_hooks(&codex_path, "codex"),
        claude_path: claude_path.display().to_string(),
        codex_path: codex_path.display().to_string(),
        hook_command_path,
    })
}

fn install_hooks() -> Result<(), String> {
    let home = home_dir().ok_or_else(|| "Could not resolve home directory".to_string())?;
    let claude_path = home.join(".claude").join("settings.json");
    let codex_path = home.join(".codex").join("hooks.json");
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    validate_install_exe_path(&exe)?;
    let timestamp = now_epoch();

    install_hook_file(&claude_path, claude_hooks(&exe), timestamp)?;
    install_hook_file(&codex_path, codex_hooks(&exe), timestamp)?;
    Ok(())
}

fn install_hook_file(path: &Path, incoming: Value, timestamp: i64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    if path.exists() {
        let backup_path = PathBuf::from(format!("{}.agent-pin-backup-{timestamp}", path.display()));
        fs::copy(path, backup_path).map_err(|error| error.to_string())?;
    }

    let mut target = load_json(path)?;
    merge_hooks(&mut target, incoming)?;
    let formatted = serde_json::to_string_pretty(&target).map_err(|error| error.to_string())?;
    fs::write(path, format!("{formatted}\n")).map_err(|error| error.to_string())
}

fn load_json(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }

    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&contents).map_err(|error| format!("{}: {error}", path.display()))
}

fn merge_hooks(target: &mut Value, incoming: Value) -> Result<(), String> {
    if !target.is_object() {
        *target = json!({});
    }

    let target_hooks = target
        .as_object_mut()
        .expect("target object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !target_hooks.is_object() {
        *target_hooks = json!({});
    }

    let hooks_object = target_hooks.as_object_mut().expect("hooks object");
    for entries in hooks_object.values_mut() {
        if let Some(entries_array) = entries.as_array_mut() {
            entries_array.retain(|entry| !agent_pin_entry(entry));
        }
    }
    hooks_object.retain(|_, entries| entries.as_array().map(|array| !array.is_empty()).unwrap_or(true));

    let incoming_hooks = incoming
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or_else(|| "Incoming hook template is missing hooks object".to_string())?;
    for (event_name, entries) in incoming_hooks {
        let entries_array = entries
            .as_array()
            .ok_or_else(|| format!("Incoming hook {event_name} is not an array"))?;
        hooks_object
            .entry(event_name.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("Existing hook {event_name} is not an array"))?
            .extend(entries_array.iter().cloned());
    }

    Ok(())
}

fn has_agent_pin_hooks(path: &Path, provider: &str) -> bool {
    load_json(path)
        .map(|value| json_contains_provider_hook(&value, provider))
        .unwrap_or(false)
}

fn json_contains_provider_hook(value: &Value, provider: &str) -> bool {
    match value {
        Value::String(text) => {
            agent_pin_command_text(text)
                && (text.contains(&format!(" {provider} ")) || text.contains(&format!(" {provider}\n")))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_provider_hook(value, provider)),
        Value::Object(values) => values
            .values()
            .any(|value| json_contains_provider_hook(value, provider)),
        _ => false,
    }
}

fn agent_pin_entry(value: &Value) -> bool {
    match value {
        Value::String(text) => agent_pin_command_text(text),
        Value::Array(values) => values.iter().any(agent_pin_entry),
        Value::Object(values) => values.values().any(agent_pin_entry),
        _ => false,
    }
}

fn agent_pin_command_text(text: &str) -> bool {
    text.contains("agent-pin-status")
        || (text.contains("agent-pin") && text.contains(" hook "))
        || (text.contains("agent-pin.exe") && text.contains(" hook "))
}

fn claude_hooks(exe: &Path) -> Value {
    json!({
        "hooks": {
            "SessionStart": [matcher_hook("*", hook_command(exe, "claude", "running", "session-start"), false)],
            "UserPromptSubmit": [plain_hook(hook_command(exe, "claude", "running", "prompt"), false)],
            "PreToolUse": [matcher_hook("*", hook_command(exe, "claude", "running", "tool"), false)],
            "PermissionRequest": [matcher_hook("*", hook_command(exe, "claude", "waiting_approval", "approval"), false)],
            "PostToolUse": [matcher_hook("*", hook_command(exe, "claude", "running", "post-tool"), false)],
            "PostToolUseFailure": [matcher_hook("*", hook_command(exe, "claude", "error", "tool-failure"), false)],
            "Stop": [plain_hook(hook_command(exe, "claude", "completed", "stop"), false)],
            "StopFailure": [matcher_hook("*", hook_command(exe, "claude", "error", "stop-failure"), false)],
            "SessionEnd": [matcher_hook("*", hook_command(exe, "claude", "idle", "session-end"), false)]
        }
    })
}

fn codex_hooks(exe: &Path) -> Value {
    json!({
        "hooks": {
            "SessionStart": [matcher_hook("*", hook_command(exe, "codex", "running", "session-start"), true)],
            "UserPromptSubmit": [plain_hook(hook_command(exe, "codex", "running", "prompt"), true)],
            "PreToolUse": [matcher_hook("*", hook_command(exe, "codex", "running", "tool"), true)],
            "PermissionRequest": [matcher_hook("*", hook_command(exe, "codex", "waiting_approval", "approval"), true)],
            "PostToolUse": [matcher_hook("*", hook_command(exe, "codex", "running", "post-tool"), true)],
            "Stop": [plain_hook(hook_command(exe, "codex", "completed", "stop"), true)]
        }
    })
}

fn hook_command(exe: &Path, provider: &str, status: &str, detail: &str) -> String {
    format!(
        "{} hook {provider} {status} {detail}",
        shell_quote_path(exe)
    )
}

fn plain_hook(command: String, status_message: bool) -> Value {
    json!({ "hooks": [command_object(command, status_message)] })
}

fn matcher_hook(matcher: &str, command: String, status_message: bool) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [command_object(command, status_message)]
    })
}

fn command_object(command: String, status_message: bool) -> Value {
    if status_message {
        json!({
            "type": "command",
            "command": command,
            "statusMessage": "Updating Agent Pin"
        })
    } else {
        json!({
            "type": "command",
            "command": command
        })
    }
}

fn shell_quote_path(path: &Path) -> String {
    let text = path.display().to_string();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn validate_install_exe_path(path: &Path) -> Result<(), String> {
    let text = path.display().to_string();
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        return Err(
            "Move Agent Pin to Applications, reopen it, then run setup again.".to_string(),
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn validate_install_exe_path(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn normalize(mut snapshot: AgentSnapshot, now: i64) -> Option<AgentSnapshot> {
    let age = now.saturating_sub(snapshot.updated_at);
    let stale_terminal = snapshot.status == "completed" && age > 600;
    let stale_active =
        (snapshot.status == "running" || snapshot.status == "waiting_approval") && age > 900;

    if stale_terminal || stale_active {
        snapshot.status = "idle".to_string();
        snapshot.detail = Some("stale".to_string());
    }

    if snapshot.status != "idle" {
        return Some(snapshot);
    }

    if age < 20 {
        Some(snapshot)
    } else {
        None
    }
}

fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    let start = size.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;

    let mut buffer = String::new();
    file.read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn modified_epoch(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }

    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn display_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "antigravity" => "Antigravity",
        _ => "Agent",
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            read_status,
            read_integrations,
            install_integrations,
            start_window_drag,
            set_compact
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(true);
                let _ = position_top_right(&window);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Agent Pin");
}

fn position_top_right(window: &WebviewWindow) -> Result<(), String> {
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "No monitor available".to_string())?;
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let scale_factor = monitor.scale_factor();

    let width = EXPANDED_WIDTH * scale_factor;
    let margin = SCREEN_MARGIN * scale_factor;
    let x = monitor_position.x as f64 + monitor_size.width as f64 - width - margin;
    let y = monitor_position.y as f64 + margin;

    window
        .set_position(Position::Logical(LogicalPosition::new(
            x / scale_factor,
            y / scale_factor,
        )))
        .map_err(|error| error.to_string())
}
