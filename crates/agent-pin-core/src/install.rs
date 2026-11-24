//! Hook installer: merges agent-pin hook entries into the user's provider
//! config files without touching the user's own hooks.

use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A coding agent whose config file agent-pin can install hooks into.
pub struct Provider {
    pub id: &'static str,
    /// Config file path relative to the user's home directory.
    pub config_rel: &'static str,
    pub events: &'static [&'static str],
    pub async_hooks: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "claude",
        config_rel: ".claude/settings.json",
        events: &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "Stop",
            "StopFailure",
            "SessionEnd",
        ],
        async_hooks: true,
    },
    Provider {
        id: "codex",
        config_rel: ".codex/hooks.json",
        events: &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
        ],
        async_hooks: false,
    },
];

/// The shell command installed for one hook: `"<exe>" hook <provider> <event>`.
/// The exe is wrapped in double quotes with inner `"` escaped; backslashes are
/// left alone so Windows paths stay valid.
pub fn hook_command(exe: &Path, provider: &str, event: &str) -> String {
    format!("{}{provider} {event}", exe_prefix(exe))
}

/// True for commands agent-pin installed: the legacy Ruby `agent-pin-status`
/// script, an `agent-pin` binary invoked with the `hook` subcommand, or a
/// command of exactly the [`hook_command`] shape whose quoted exe is named
/// like Agent Pin in any case or separator style (an AppImage is named
/// `Agent Pin_<version>_<arch>.AppImage`).
pub fn is_agent_pin_command(cmd: &str) -> bool {
    cmd.contains("agent-pin-status")
        || (cmd.contains("agent-pin") && cmd.contains(" hook "))
        || quoted_hook_exe(cmd).is_some_and(|exe| {
            let name = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
            let squashed: String = name
                .chars()
                .filter(|c| !matches!(c, ' ' | '_' | '-' | '.'))
                .flat_map(char::to_lowercase)
                .collect();
            squashed.contains("agentpin")
        })
}

/// The exe of a command shaped exactly like [`hook_command`] output:
/// `"<exe>" hook <provider> <event>` with nothing after the event.
fn quoted_hook_exe(cmd: &str) -> Option<&str> {
    let rest = cmd.strip_prefix('"')?;
    let end = rest.find("\" hook ")?;
    let args: Vec<&str> = rest[end + "\" hook ".len()..].split(' ').collect();
    let word = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (args.len() == 2 && args.iter().all(|w| word(w))).then_some(&rest[..end])
}

/// True for an entry agent-pin owns: [`is_agent_pin_command`], or a hook
/// command for the exe being installed (`"<exe>" hook …`).
fn is_ours(cmd: &str, exe: &Path) -> bool {
    is_agent_pin_command(cmd) || cmd.starts_with(&exe_prefix(exe))
}

/// `"<exe>" hook `, quoted exactly as [`hook_command`] quotes it.
fn exe_prefix(exe: &Path) -> String {
    let exe = exe.display().to_string().replace('"', "\\\"");
    format!("\"{exe}\" hook ")
}

/// Removes every agent-pin command object from `config.hooks`, then appends
/// one fresh agent-pin group per provider event. User commands, including ones
/// sharing a group with an agent-pin command, are left untouched; a group is
/// dropped only when removing agent-pin commands left it empty.
pub fn merge(config: &mut Value, provider: &Provider, exe: &Path) {
    if !config.is_object() {
        *config = Value::Object(Default::default());
    }
    let root = config.as_object_mut().expect("object");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()));
    if !hooks.is_object() {
        *hooks = Value::Object(Default::default());
    }
    let hooks = hooks.as_object_mut().expect("object");

    hooks.retain(|_, groups| {
        let Some(groups) = groups.as_array_mut() else {
            return true;
        };
        let before = groups.len();
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let had = entries.len();
            entries.retain(|entry| {
                !entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|cmd| is_ours(cmd, exe))
            });
            !(entries.is_empty() && had > 0)
        });
        // Drop an event key only when our removals emptied it.
        !(groups.is_empty() && before > 0)
    });

    for event in provider.events {
        let mut entry = serde_json::Map::new();
        entry.insert("type".into(), "command".into());
        entry.insert(
            "command".into(),
            hook_command(exe, provider.id, event).into(),
        );
        if provider.async_hooks {
            entry.insert("async".into(), true.into());
        }
        let mut group = serde_json::Map::new();
        if *event != "UserPromptSubmit" && *event != "Stop" {
            group.insert("matcher".into(), "*".into());
        }
        group.insert("hooks".into(), Value::Array(vec![Value::Object(entry)]));

        let slot = hooks
            .entry(*event)
            .or_insert_with(|| Value::Array(Vec::new()));
        if !slot.is_array() {
            *slot = Value::Array(Vec::new());
        }
        slot.as_array_mut()
            .expect("array")
            .push(Value::Object(group));
    }
}

/// The exe to install as the hook command. Inside an AppImage, `current`
/// lives on a temporary mount, so the AppImage file (`APPIMAGE`) is used
/// instead, but only when `current` really is inside this AppImage's mount
/// (`APPDIR`): an `APPIMAGE` inherited from another AppImage, such as an
/// editor whose terminal leaks it, is ignored. Empty values count as unset.
pub fn choose_exe(current: PathBuf, appimage: Option<OsString>, appdir: Option<OsString>) -> PathBuf {
    match (
        appimage.filter(|v| !v.is_empty()),
        appdir.filter(|v| !v.is_empty()),
    ) {
        (Some(image), Some(dir)) if current.starts_with(Path::new(&dir)) => PathBuf::from(image),
        _ => current,
    }
}

/// Reads and parses a provider config. A missing or blank file is `{}`.
/// Anything that is not a JSON object is an error.
fn read_config(path: &Path) -> Result<Option<Value>, String> {
    Ok(read_config_text(path)?.map(|(_, value)| value))
}

/// Like [`read_config`], also returning the file's original text.
fn read_config_text(path: &Path) -> Result<Option<(String, Value)>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some((text, Value::Object(Default::default()))));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{}: top level is not a JSON object", path.display()));
    }
    Ok(Some((text, value)))
}

/// Installs agent-pin hooks for every provider whose config directory exists
/// under `home`, returning the provider ids installed.
///
/// All configs are read, parsed and merged before anything is written, so an
/// unparseable file aborts the whole install with no file touched. Each
/// existing file is backed up to `<file>.agent-pin-backup-<now_secs>` and then
/// replaced atomically. A file whose merged text equals its current text is
/// left alone: no backup and no write.
pub fn install(home: &Path, exe: &Path, now_secs: i64) -> Result<Vec<String>, String> {
    let mut planned = Vec::new();
    for provider in PROVIDERS {
        let path = home.join(provider.config_rel);
        if !provider_dir_exists(home, provider) {
            continue;
        }
        let (original, mut config) = match read_config_text(&path)? {
            Some((text, value)) => (Some(text), value),
            None => (None, Value::Object(Default::default())),
        };
        merge(&mut config, provider, exe);
        let text = serde_json::to_string_pretty(&config).map_err(|err| err.to_string())? + "\n";
        planned.push((provider, path, original, text));
    }

    let mut ids = Vec::new();
    for (provider, path, original, text) in planned {
        if original.as_deref() == Some(text.as_str()) {
            ids.push(provider.id.to_string());
            continue;
        }
        if original.is_some() {
            backup(&path, now_secs).map_err(|err| format!("backup {}: {err}", path.display()))?;
        }
        write_atomic(&path, text.as_bytes())
            .map_err(|err| format!("write {}: {err}", path.display()))?;
        ids.push(provider.id.to_string());
    }
    Ok(ids)
}

/// One entry per provider whose config directory exists: true when its config
/// holds exactly the command [`hook_command`] builds for `exe` for every event
/// the provider uses. Hooks pointing at a moved or older exe read as false.
pub fn installed(home: &Path, exe: &Path) -> Vec<(String, bool)> {
    PROVIDERS
        .iter()
        .filter(|provider| provider_dir_exists(home, provider))
        .map(|provider| {
            let config = read_config(&home.join(provider.config_rel)).ok().flatten();
            let ok = config.is_some_and(|config| {
                provider.events.iter().all(|event| {
                    let want = hook_command(exe, provider.id, event);
                    event_commands(&config, event).any(|cmd| cmd == want)
                })
            });
            (provider.id.to_string(), ok)
        })
        .collect()
}

fn event_commands<'a>(config: &'a Value, event: &str) -> impl Iterator<Item = &'a str> {
    config["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter_map(|entry| entry["command"].as_str())
}

fn provider_dir_exists(home: &Path, provider: &Provider) -> bool {
    Path::new(provider.config_rel)
        .parent()
        .is_some_and(|dir| home.join(dir).is_dir())
}

/// Copies `path` to `<path>.agent-pin-backup-<now_secs>`, never overwriting an
/// earlier backup (a numeric suffix is added on collision).
fn backup(path: &Path, now_secs: i64) -> io::Result<PathBuf> {
    let base = format!("{}.agent-pin-backup-{now_secs}", path.display());
    let mut candidate = PathBuf::from(&base);
    let mut n = 1;
    while fs::symlink_metadata(&candidate).is_ok() {
        candidate = PathBuf::from(format!("{base}-{n}"));
        n += 1;
    }
    fs::copy(path, &candidate)?;
    Ok(candidate)
}

/// Writes `bytes` to a temp file beside the real target, then renames it into
/// place. A symlinked config is resolved first so the link itself survives,
/// and an existing file's permissions are carried over.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.agent-pin-tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::ffi::OsString;
    use std::path::PathBuf;

    const SHARED_GROUP: &str = include_str!("../tests/fixtures/claude_shared_group.json");
    const LEGACY_RUBY: &str = include_str!("../tests/fixtures/legacy_ruby.json");
    const KEY_ORDER: &str = include_str!("../tests/fixtures/key_order.json");
    const INVALID: &str = include_str!("../tests/fixtures/invalid.json");

    const USER_CMD: &str = "/usr/local/bin/my-audit-hook --strict";

    fn claude() -> &'static Provider {
        &PROVIDERS[0]
    }

    fn codex() -> &'static Provider {
        &PROVIDERS[1]
    }

    fn exe() -> PathBuf {
        PathBuf::from("/opt/Agent Pin/agent-pin")
    }

    /// Every command string under `hooks.<event>[*].hooks[*].command`.
    fn commands(config: &Value, event: &str) -> Vec<String> {
        config["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect()
    }

    fn agent_pin_entries(config: &Value, event: &str) -> Vec<Value> {
        config["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter(|h| h["command"].as_str().is_some_and(is_agent_pin_command))
            .cloned()
            .collect()
    }

    #[test]
    fn keeps_user_hook_in_shared_group() {
        let mut config: Value = serde_json::from_str(SHARED_GROUP).unwrap();
        merge(&mut config, claude(), &exe());

        let cmds = commands(&config, "PreToolUse");
        assert!(cmds.contains(&USER_CMD.to_string()), "user hook lost: {cmds:?}");
        // The shared group survives with only the user's command left in it.
        let first = &config["hooks"]["PreToolUse"][0];
        assert_eq!(first["matcher"], "*");
        assert_eq!(first["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(first["hooks"][0]["command"], USER_CMD);
        // The old agent-pin command is gone; the new one is appended.
        assert!(!cmds.iter().any(|c| c.ends_with("running tool")));
        let expected = hook_command(&exe(), "claude", "PreToolUse");
        assert_eq!(cmds.iter().filter(|c| **c == expected).count(), 1);
        // The Stop group held only agent-pin, so it is dropped and replaced.
        let stop = config["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], hook_command(&exe(), "claude", "Stop"));
    }

    #[test]
    fn reinstall_is_idempotent() {
        for provider in PROVIDERS {
            let mut config: Value = serde_json::from_str(SHARED_GROUP).unwrap();
            merge(&mut config, provider, &exe());
            let once = config.clone();
            merge(&mut config, provider, &exe());
            assert_eq!(config, once, "{} second merge changed config", provider.id);
            for event in provider.events {
                assert_eq!(agent_pin_entries(&config, event).len(), 1, "{} {event}", provider.id);
            }
            assert!(commands(&config, "PreToolUse").contains(&USER_CMD.to_string()));
        }
    }

    #[test]
    fn removes_legacy_ruby_entries() {
        let mut config: Value = serde_json::from_str(LEGACY_RUBY).unwrap();
        merge(&mut config, claude(), &exe());
        let text = serde_json::to_string(&config).unwrap();
        assert!(!text.contains("agent-pin-status"), "{text}");
        // The user's command sharing the legacy Notification group survives.
        assert_eq!(commands(&config, "Notification"), vec!["notify-send 'Claude needs you'"]);
        assert_eq!(agent_pin_entries(&config, "SessionStart").len(), 1);
    }

    #[test]
    fn preserves_key_order() {
        let mut config: Value = serde_json::from_str(KEY_ORDER).unwrap();
        merge(&mut config, claude(), &exe());
        let keys: Vec<&str> = config.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["model", "hooks", "permissions"]);
        let perm_keys: Vec<&str> =
            config["permissions"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(perm_keys, ["deny", "allow"]);
        let events: Vec<&str> =
            config["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(events, claude().events);
    }

    #[test]
    fn claude_entries_are_async() {
        let mut config: Value = serde_json::from_str(KEY_ORDER).unwrap();
        merge(&mut config, claude(), &exe());
        for event in claude().events {
            let group = &config["hooks"][*event][0];
            let entry = &group["hooks"][0];
            assert_eq!(entry["type"], "command");
            assert_eq!(entry["command"], hook_command(&exe(), "claude", event));
            assert_eq!(entry["async"], Value::Bool(true), "{event}");
            if *event == "UserPromptSubmit" || *event == "Stop" {
                assert!(group.get("matcher").is_none(), "{event} must not have a matcher");
            } else {
                assert_eq!(group["matcher"], "*", "{event}");
            }
        }
    }

    #[test]
    fn codex_entries_not_async() {
        let mut config = Value::Object(Default::default());
        merge(&mut config, codex(), &exe());
        let events: Vec<&str> =
            config["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(events, codex().events);
        for event in codex().events {
            let entry = &config["hooks"][*event][0]["hooks"][0];
            assert_eq!(entry["command"], hook_command(&exe(), "codex", event));
            assert!(entry.get("async").is_none(), "{event}: {entry}");
        }
    }

    #[test]
    fn non_object_hooks_replaced() {
        let mut config = serde_json::json!({"model": "x", "hooks": "oops"});
        merge(&mut config, codex(), &exe());
        assert_eq!(agent_pin_entries(&config, "Stop").len(), 1);
        assert_eq!(config["model"], "x");
    }

    #[test]
    fn windows_exe_path_quoted() {
        let exe = PathBuf::from(r"C:\Program Files\Agent Pin\agent-pin.exe");
        assert_eq!(
            hook_command(&exe, "claude", "Stop"),
            r#""C:\Program Files\Agent Pin\agent-pin.exe" hook claude Stop"#
        );
        let quoted = PathBuf::from(r#"/tmp/we"ird/agent-pin"#);
        assert_eq!(
            hook_command(&quoted, "codex", "Stop"),
            r#""/tmp/we\"ird/agent-pin" hook codex Stop"#
        );
        assert!(is_agent_pin_command(&hook_command(&exe, "claude", "Stop")));
    }

    #[test]
    fn detects_agent_pin_commands() {
        assert!(is_agent_pin_command("ruby agent-pin-status claude running"));
        assert!(is_agent_pin_command("\"/x/agent-pin\" hook claude Stop"));
        assert!(!is_agent_pin_command("/usr/bin/agent-pin --version"));
        assert!(!is_agent_pin_command("my-hook hook claude Stop"));
        // AppImage names, in any case or separator style, in the exact shape.
        assert!(is_agent_pin_command("\"/h/Agent Pin_0.1.0_amd64.AppImage\" hook claude Stop"));
        assert!(is_agent_pin_command(r#""C:\Apps\Agent_Pin.exe" hook codex PreToolUse"#));
        assert!(!is_agent_pin_command("\"/h/Agent Pin.AppImage\" hook claude Stop --x"));
        assert!(!is_agent_pin_command("\"/h/Agent Pin/other\" hook claude Stop"));
        assert!(!is_agent_pin_command("/h/AgentPin hook claude Stop"));
    }

    #[test]
    fn skips_missing_provider_dir() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".codex")).unwrap();

        let ids = install(home.path(), &exe(), 1_700_000_000).unwrap();
        assert_eq!(ids, vec!["codex".to_string()]);
        assert!(!home.path().join(".claude").exists(), "must not create ~/.claude");
        // The codex dir existed but the file did not: it is created.
        let text = fs::read_to_string(home.path().join(".codex/hooks.json")).unwrap();
        let config: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(agent_pin_entries(&config, "Stop").len(), 1);
        assert_eq!(installed(home.path(), &exe()), vec![("codex".to_string(), true)]);
    }

    #[test]
    fn invalid_json_aborts_without_writing() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::create_dir(home.path().join(".codex")).unwrap();
        fs::write(home.path().join(".claude/settings.json"), INVALID).unwrap();

        let err = install(home.path(), &exe(), 1_700_000_000).unwrap_err();
        assert!(err.contains("settings.json"), "{err}");
        assert_eq!(fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(), INVALID);
        // Nothing else is written either: no backup, no temp, no codex file.
        let claude_dir: Vec<_> = fs::read_dir(home.path().join(".claude")).unwrap().collect();
        assert_eq!(claude_dir.len(), 1);
        assert_eq!(fs::read_dir(home.path().join(".codex")).unwrap().count(), 0);
        assert_eq!(
            installed(home.path(), &exe()),
            vec![("claude".to_string(), false), ("codex".to_string(), false)]
        );
    }

    #[test]
    fn non_object_root_aborts_without_writing() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::write(home.path().join(".claude/settings.json"), "[1, 2]").unwrap();
        assert!(install(home.path(), &exe(), 1).is_err());
        assert_eq!(fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(), "[1, 2]");
    }

    #[test]
    fn backup_written() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        let settings = home.path().join(".claude/settings.json");
        fs::write(&settings, SHARED_GROUP).unwrap();

        let ids = install(home.path(), &exe(), 1_700_000_000).unwrap();
        assert_eq!(ids, vec!["claude".to_string()]);
        let backup = home.path().join(".claude/settings.json.agent-pin-backup-1700000000");
        assert_eq!(fs::read_to_string(&backup).unwrap(), SHARED_GROUP);

        let written: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(commands(&written, "PreToolUse").contains(&USER_CMD.to_string()));
        let keys: Vec<&str> = written.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["model", "hooks", "permissions"]);
        // No temp files left behind; only settings + backup.
        assert_eq!(fs::read_dir(home.path().join(".claude")).unwrap().count(), 2);
        assert_eq!(installed(home.path(), &exe()), vec![("claude".to_string(), true)]);

        // A second install in the same second keeps the first backup intact.
        install(home.path(), &exe(), 1_700_000_000).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), SHARED_GROUP);
    }

    #[test]
    fn reinstall_unchanged_writes_no_backup() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        let settings = home.path().join(".claude/settings.json");
        fs::write(&settings, SHARED_GROUP).unwrap();

        install(home.path(), &exe(), 100).unwrap();
        let after_first = fs::read_to_string(&settings).unwrap();
        let files_after_first = fs::read_dir(home.path().join(".claude")).unwrap().count();

        let ids = install(home.path(), &exe(), 200).unwrap();
        assert_eq!(ids, vec!["claude".to_string()]);
        assert!(!home.path().join(".claude/settings.json.agent-pin-backup-200").exists());
        assert_eq!(
            fs::read_dir(home.path().join(".claude")).unwrap().count(),
            files_after_first
        );
        assert_eq!(fs::read_to_string(&settings).unwrap(), after_first);
    }

    #[test]
    fn installed_false_for_legacy_only() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::write(home.path().join(".claude/settings.json"), SHARED_GROUP).unwrap();
        assert_eq!(installed(home.path(), &exe()), vec![("claude".to_string(), false)]);
    }

    fn appimage() -> PathBuf {
        PathBuf::from("/home/u/Agent Pin_0.1.0_amd64.AppImage")
    }

    #[test]
    fn appimage_reinstall_keeps_one_entry_per_event() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::create_dir(home.path().join(".codex")).unwrap();
        install(home.path(), &appimage(), 1).unwrap();
        install(home.path(), &appimage(), 2).unwrap();
        for provider in PROVIDERS {
            let text = fs::read_to_string(home.path().join(provider.config_rel)).unwrap();
            let config: Value = serde_json::from_str(&text).unwrap();
            for event in provider.events {
                let want = hook_command(&appimage(), provider.id, event);
                let cmds = commands(&config, event);
                assert_eq!(cmds, vec![want], "{} {event}", provider.id);
            }
        }
        assert_eq!(
            installed(home.path(), &appimage()),
            vec![("claude".to_string(), true), ("codex".to_string(), true)]
        );
    }

    #[test]
    fn appimage_merge_keeps_user_hooks() {
        let mut config: Value = serde_json::from_str(SHARED_GROUP).unwrap();
        merge(&mut config, claude(), &appimage());
        merge(&mut config, claude(), &appimage());
        let cmds = commands(&config, "PreToolUse");
        assert!(cmds.contains(&USER_CMD.to_string()), "{cmds:?}");
        let want = hook_command(&appimage(), "claude", "PreToolUse");
        assert_eq!(cmds.iter().filter(|c| **c == want).count(), 1, "{cmds:?}");
    }

    #[test]
    fn moved_exe_is_not_installed() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        install(home.path(), &appimage(), 1).unwrap();
        let moved = PathBuf::from("/home/u/Applications/Agent Pin_0.1.0_amd64.AppImage");
        assert_eq!(installed(home.path(), &moved), vec![("claude".to_string(), false)]);
        // Reinstalling from the new place replaces the old entries.
        install(home.path(), &moved, 2).unwrap();
        let text = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        let config: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(commands(&config, "Stop"), vec![hook_command(&moved, "claude", "Stop")]);
        assert_eq!(installed(home.path(), &moved), vec![("claude".to_string(), true)]);
    }

    #[test]
    fn foreign_quoted_hook_command_is_kept() {
        let mut config = serde_json::json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "\"/opt/other tool\" hook claude Stop"}
        ]}]}});
        merge(&mut config, claude(), &appimage());
        assert_eq!(
            commands(&config, "Stop"),
            vec![
                "\"/opt/other tool\" hook claude Stop".to_string(),
                hook_command(&appimage(), "claude", "Stop")
            ]
        );
    }

    #[test]
    fn choose_exe_uses_appimage_only_inside_its_mount() {
        let cur = PathBuf::from("/tmp/.mount_AgentPabc/usr/bin/agent-pin");
        let img = Some(OsString::from("/home/u/Agent Pin.AppImage"));
        let dir = Some(OsString::from("/tmp/.mount_AgentPabc"));
        assert_eq!(choose_exe(cur.clone(), img.clone(), dir.clone()), PathBuf::from("/home/u/Agent Pin.AppImage"));
        // APPIMAGE leaked from another AppImage (e.g. an editor's terminal).
        let other_dir = Some(OsString::from("/tmp/.mount_Cursorxyz"));
        let plain = PathBuf::from("/usr/bin/agent-pin");
        assert_eq!(choose_exe(plain.clone(), img.clone(), other_dir), plain);
        assert_eq!(choose_exe(plain.clone(), img.clone(), None), plain);
        // A prefix that is not a path-component prefix does not count.
        let near = PathBuf::from("/tmp/.mount_AgentPabcdef/usr/bin/agent-pin");
        assert_eq!(choose_exe(near.clone(), img.clone(), dir.clone()), near);
        // Empty or missing APPIMAGE.
        assert_eq!(choose_exe(cur.clone(), Some(OsString::new()), dir.clone()), cur);
        assert_eq!(choose_exe(cur.clone(), None, dir.clone()), cur);
        assert_eq!(choose_exe(cur.clone(), img, Some(OsString::new())), cur);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_keeps_link() {
        let home = tempfile::tempdir().unwrap();
        let dotfiles = home.path().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::write(dotfiles.join("settings.json"), KEY_ORDER).unwrap();
        let link = home.path().join(".claude/settings.json");
        std::os::unix::fs::symlink(dotfiles.join("settings.json"), &link).unwrap();

        install(home.path(), &exe(), 5).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let target: Value =
            serde_json::from_str(&fs::read_to_string(dotfiles.join("settings.json")).unwrap()).unwrap();
        assert_eq!(agent_pin_entries(&target, "Stop").len(), 1);
    }
}
