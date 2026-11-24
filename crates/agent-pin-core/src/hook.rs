//! The `agent-pin hook <provider> <event>` entry point. It never panics and
//! never blocks for long, except that a PermissionRequest may wait for a
//! click in the running app: errors are returned for the caller to print to
//! stderr before exiting 0.

use std::io::{self, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::approvals::{self, Request};
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
///
/// A PermissionRequest whose payload parsed as JSON and names a tool (a
/// salvaged payload never names one) is, after its state is recorded and
/// the session lock released, posted to the app when approvals are on and
/// the app is alive. The hook then waits for the user's click and returns
/// the decision JSON for stdout. Every other path, including any approval
/// failure, returns `Ok(None)`.
pub fn run_hook(
    args: &[String],
    mut stdin: impl Read,
    state_dir: Option<PathBuf>,
    now_ms: i64,
) -> Result<Option<String>, String> {
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
                return Ok(None);
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
        .map_err(|e| format!("updating session {provider}__{key}: {e}"))?;
    // The session lock is released: waiting here blocks no other hook.
    // Input reaching this point parsed as JSON or had a session_id salvaged;
    // a salvaged one carries no tool name, so it never waits.
    let has_tool = input.tool_name.as_deref().is_some_and(|t| !t.trim().is_empty());
    if kind != HookKind::PermissionRequest || !has_tool {
        return Ok(None);
    }
    let t = unix_ms();
    if !approvals::enabled(&dir) || !approvals::app_alive(&dir, t) {
        return Ok(None);
    }
    let request = approvals::new_request(&provider, &key, &input, t);
    Ok(post_and_wait(&dir, &request, unix_ms, std::thread::sleep).map(approvals::decision_json))
}

/// Posts `r` and waits for its decision. A failed post means no wait at
/// all: the id's files, or a planted decision, are not this hook's.
fn post_and_wait(
    dir: &Path,
    r: &Request,
    now: impl FnMut() -> i64,
    sleep: impl FnMut(Duration),
) -> Option<approvals::Behavior> {
    let posted = approvals::post(dir, r).ok()?;
    approvals::wait(dir, posted, now, sleep)
}

/// The hook subcommand once stdin is read: runs [`run_hook`], writes a
/// decision with a trailing newline to `out` in one write and flushes it,
/// and writes errors (and a caught panic) to `err` only. Write failures are
/// ignored; it never panics.
pub fn run(
    args: &[String],
    input: &[u8],
    state_dir: Option<PathBuf>,
    now_ms: i64,
    out: &mut impl Write,
    err: &mut impl Write,
) {
    let result = catch_unwind(AssertUnwindSafe(|| run_hook(args, input, state_dir, now_ms)));
    let _ = match result {
        Ok(Ok(Some(json))) => out.write_all(format!("{json}\n").as_bytes()).and_then(|()| out.flush()),
        Ok(Ok(None)) => Ok(()),
        Ok(Err(error)) => writeln!(err, "agent-pin: {error}"),
        Err(_) => writeln!(err, "agent-pin: hook panicked"),
    };
}

/// Wall-clock unix milliseconds (0 if the clock is before 1970).
fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

    // --- PermissionRequest approvals -------------------------------------

    use crate::approvals::{self, decision_json, Behavior};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const PERMISSION: &[u8] =
        br#"{"session_id":"s1","cwd":"/r/web","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#;

    fn files_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }

    /// A stand-in for the app: refreshes `app.alive` every 200 ms and, once
    /// a request is pending, runs `on_request` on it and then answers with
    /// `decision` (if any). Stops when the returned flag is set.
    fn fake_app(
        dir: &Path,
        decision: Option<Behavior>,
        on_request: impl Fn(&approvals::Request) + Send + 'static,
    ) -> (Arc<AtomicBool>, std::thread::JoinHandle<Vec<approvals::Request>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let dir = dir.to_path_buf();
        approvals::write_alive(&dir, unix_ms()).unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                approvals::write_alive(&dir, unix_ms()).unwrap();
                for r in approvals::list_pending(&dir, unix_ms()) {
                    if !seen.contains(&r) {
                        on_request(&r);
                        if let Some(b) = decision {
                            approvals::decide(&dir, &r.id, b).unwrap();
                        }
                        seen.push(r);
                    }
                }
                std::thread::sleep(Duration::from_millis(approvals::POLL_MS));
            }
            seen
        });
        (stop, handle)
    }

    #[test]
    fn permission_request_without_app_returns_none_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let start = Instant::now();
        let out = run_hook(&args("claude", "PermissionRequest"), PERMISSION, Some(dir.clone()), 1);
        assert_eq!(out, Ok(None));
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
        assert_eq!(list_sessions(&dir)[0].status, Status::WaitingApproval);
        assert!(!dir.join("approvals").exists(), "no request posted");
        // A stale app.alive (app gone) is the same as none.
        approvals::write_alive(&dir, unix_ms() - 60_000).unwrap();
        let start = Instant::now();
        let out = run_hook(&args("claude", "PermissionRequest"), PERMISSION, Some(dir.clone()), 2);
        assert_eq!(out, Ok(None));
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
        assert!(!dir.join("approvals").exists(), "no request posted");
    }

    #[test]
    fn permission_request_with_decision_returns_json() {
        for (provider, b) in [("claude", Behavior::Allow), ("codex", Behavior::Deny)] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            // While the hook waits, its session is recorded as waiting.
            let check_dir = dir.clone();
            let (stop, app) = fake_app(&dir, Some(b), move |_| {
                let s = list_sessions(&check_dir);
                assert_eq!(s[0].status, Status::WaitingApproval);
            });
            let start = Instant::now();
            let out = run_hook(&args(provider, "PermissionRequest"), PERMISSION, Some(dir.clone()), 1);
            stop.store(true, Ordering::SeqCst);
            let seen = app.join().unwrap();
            assert_eq!(out, Ok(Some(decision_json(b))), "{provider}");
            assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
            assert_eq!(seen.len(), 1, "{provider}");
            let r = &seen[0];
            assert_eq!(r.provider, provider);
            assert_eq!(r.session_key, "s1");
            assert_eq!(r.tool_name, "Bash");
            assert_eq!(r.summary, "Bash: rm -rf build");
            assert_eq!(r.label_hint, "web");
            let left: Vec<String> =
                files_in(&dir.join("approvals")).into_iter().filter(|n| !n.contains(".tmp-")).collect();
            assert!(left.is_empty(), "request and decision removed: {left:?}");
        }
    }

    /// The wait runs after the session lock is released: another hook for
    /// the same session can update it while this one waits.
    #[test]
    fn wait_does_not_hold_session_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let other = dir.clone();
        let (stop, app) = fake_app(&dir, Some(Behavior::Allow), move |_| {
            let start = Instant::now();
            update_session(&other, "claude", "s1", |prev| prev).unwrap();
            assert!(start.elapsed() < Duration::from_millis(500), "{:?}", start.elapsed());
        });
        let out = run_hook(&args("claude", "PermissionRequest"), PERMISSION, Some(dir.clone()), 1);
        stop.store(true, Ordering::SeqCst);
        assert_eq!(app.join().unwrap().len(), 1);
        assert_eq!(out, Ok(Some(decision_json(Behavior::Allow))));
    }

    #[test]
    fn permission_request_disabled_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        approvals::set_enabled(&dir, false).unwrap();
        let (stop, app) = fake_app(&dir, Some(Behavior::Allow), |_| {});
        let start = Instant::now();
        let out = run_hook(&args("claude", "PermissionRequest"), PERMISSION, Some(dir.clone()), 1);
        let elapsed = start.elapsed();
        std::thread::sleep(Duration::from_millis(3 * approvals::POLL_MS));
        stop.store(true, Ordering::SeqCst);
        assert!(app.join().unwrap().is_empty(), "no request posted");
        assert_eq!(out, Ok(None));
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        assert_eq!(list_sessions(&dir)[0].status, Status::WaitingApproval);
    }

    #[test]
    fn other_events_never_print() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let (stop, app) = fake_app(&dir, Some(Behavior::Allow), |_| {});
        for event in [
            "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
            "Stop", "StopFailure", "Notification", "SessionEnd",
        ] {
            let start = Instant::now();
            let out = run_hook(&args("claude", event), PERMISSION, Some(dir.clone()), 1);
            assert_eq!(out, Ok(None), "{event}");
            assert!(start.elapsed() < Duration::from_secs(1), "{event}");
        }
        stop.store(true, Ordering::SeqCst);
        assert!(app.join().unwrap().is_empty(), "no request posted");
    }

    /// Only a real payload with a tool name starts a wait: empty, garbage,
    /// salvaged (partial or oversized) and tool-less input never post.
    #[test]
    fn permission_request_needs_real_input_with_tool_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let (stop, app) = fake_app(&dir, Some(Behavior::Allow), |_| {});
        let big = oversized(r#""session_id":"s1","tool_name":"Bash","tool_input":{"command":"x"},"#);
        let inputs: [&[u8]; 7] = [
            b"",
            b"\xff garbage {{",
            br#"{"session_id":"s1","tool_name":"Bash","tool_input":{"command":"rm"#,
            big.as_bytes(),
            br#"{"session_id":"s1","tool_input":{"command":"ls"}}"#,
            br#"{"session_id":"s1","tool_name":"","tool_input":{"command":"ls"}}"#,
            b"{}",
        ];
        for input in inputs {
            let start = Instant::now();
            let out = run_hook(&args("claude", "PermissionRequest"), input, Some(dir.clone()), 1);
            assert_eq!(out, Ok(None), "{:?}", String::from_utf8_lossy(&input[..input.len().min(80)]));
            assert!(start.elapsed() < Duration::from_secs(1));
        }
        stop.store(true, Ordering::SeqCst);
        assert!(app.join().unwrap().is_empty(), "no request posted");
    }

    /// A decision planted for the id before the hook posts makes `post`
    /// fail; the hook then never waits, prints nothing and leaves the
    /// planted file alone.
    #[test]
    fn planted_decision_and_failed_post_gives_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        approvals::write_alive(&dir, unix_ms()).unwrap();
        let input = HookInput::from_json(std::str::from_utf8(PERMISSION).unwrap()).unwrap();
        let r = approvals::new_request("claude", "s1", &input, unix_ms());
        fs::create_dir_all(dir.join("approvals")).unwrap();
        let planted = dir.join("approvals").join(format!("{}.dec.json", r.id));
        fs::write(&planted, r#"{"behavior":"allow"}"#).unwrap();
        let start = Instant::now();
        let got = post_and_wait(&dir, &r, unix_ms, std::thread::sleep);
        assert_eq!(got, None);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(planted.exists(), "no wait ran, so nothing was cleaned up");
        // Through the hook: an approvals path that cannot be a directory
        // makes every post fail.
        fs::remove_dir_all(dir.join("approvals")).unwrap();
        fs::write(dir.join("approvals"), b"not a dir").unwrap();
        let start = Instant::now();
        let out = run_hook(&args("claude", "PermissionRequest"), PERMISSION, Some(dir.clone()), 1);
        assert_eq!(out, Ok(None));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn hook_prints_decision_only_when_decided() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let run_cli = |a: &[String], input: &[u8]| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            run(a, input, Some(dir.clone()), 1, &mut out, &mut err);
            (String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
        };
        // No app: nothing printed.
        assert_eq!(run_cli(&args("claude", "PermissionRequest"), PERMISSION), (String::new(), String::new()));
        // An error goes to stderr only.
        let (out, err) = run_cli(&[], PERMISSION);
        assert_eq!(out, "");
        assert!(err.starts_with("agent-pin: usage"), "{err}");
        // A clicked decision: the JSON and a newline.
        let (stop, app) = fake_app(&dir, Some(Behavior::Deny), |_| {});
        let (out, err) = run_cli(&args("claude", "PermissionRequest"), PERMISSION);
        let (out2, _) = run_cli(&args("claude", "PostToolUse"), PERMISSION);
        stop.store(true, Ordering::SeqCst);
        app.join().unwrap();
        assert_eq!(out, format!("{}\n", decision_json(Behavior::Deny)));
        assert_eq!(err, "");
        assert_eq!(out2, "", "other events print nothing");
    }

    #[test]
    fn bad_provider_becomes_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        run_hook(&args("../evil", "SessionStart"), &b"{}"[..], Some(dir.clone()), 1).unwrap();
        assert!(session_file(&dir, "agent__default").is_file());
    }
}
