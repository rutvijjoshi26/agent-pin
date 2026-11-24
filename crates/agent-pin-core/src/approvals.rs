//! File-based approval requests: the hook posts
//! `<state_dir>/approvals/<id>.req.json` and waits for the app to write
//! `<state_dir>/approvals/<id>.dec.json`. The app proves it is running by
//! refreshing `<state_dir>/app.alive`; `<state_dir>/approvals.off` turns
//! the feature off.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::input::{truncate, HookInput};
use crate::rows::project;
use crate::session::{clean, pending_text, TEXT_MAX};
use crate::store::{check_stem, write_bytes_atomic};

/// How often [`wait`] polls for a decision.
pub const POLL_MS: u64 = 200;
/// `app.alive` this old or older means the app is not running.
pub const ALIVE_MAX_AGE_MS: i64 = 3000;
/// How long a request waits for a decision.
pub const WAIT_MS: i64 = 590_000;

/// Max chars in a request's `detail` (including the trailing `…`).
const DETAIL_MAX: usize = 4000;
/// The message Claude Code and Codex show for a deny.
const DENY_MESSAGE: &str = "Denied from Agent Pin";

/// One pending permission request, as stored in `<id>.req.json`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// `<provider>__<session_key>__<unix_ms>__<4 hex>`, `[A-Za-z0-9_-]+`.
    pub id: String,
    pub provider: String,
    pub session_key: String,
    /// The project basename of the session's cwd, or empty.
    #[serde(default)]
    pub label_hint: String,
    pub tool_name: String,
    /// The session's pending text, e.g. `Bash: rm -rf build`.
    pub summary: String,
    /// Pretty-printed `tool_input`, at most 4000 chars, no control chars
    /// other than newlines.
    pub detail: String,
    pub created_ms: i64,
    pub deadline_ms: i64,
}

/// A decision the user clicked.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Behavior {
    Allow,
    Deny,
}

/// The decision file's contents.
#[derive(Serialize, Deserialize)]
struct Decision {
    behavior: Behavior,
}

fn approvals_dir(dir: &Path) -> PathBuf {
    dir.join("approvals")
}

fn req_path(dir: &Path, id: &str) -> PathBuf {
    approvals_dir(dir).join(format!("{id}.req.json"))
}

fn dec_path(dir: &Path, id: &str) -> PathBuf {
    approvals_dir(dir).join(format!("{id}.dec.json"))
}

/// Atomically writes `<dir>/app.alive` as `"<now_ms> <pid>"`.
pub fn write_alive(dir: &Path, now_ms: i64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let body = format!("{now_ms} {}", std::process::id());
    write_bytes_atomic(dir, "app.alive", &dir.join("app.alive"), body.as_bytes())
}

/// True when `app.alive` holds a timestamp less than [`ALIVE_MAX_AGE_MS`]
/// from `now_ms` (either way, so a stale file with a future stamp from a
/// clock jump does not count as alive forever).
pub fn app_alive(dir: &Path, now_ms: i64) -> bool {
    let Ok(body) = fs::read_to_string(dir.join("app.alive")) else {
        return false;
    };
    body.split_whitespace()
        .next()
        .and_then(|t| t.parse::<i64>().ok())
        .is_some_and(|t| now_ms.saturating_sub(t).saturating_abs() < ALIVE_MAX_AGE_MS)
}

/// Approvals are on unless `<dir>/approvals.off` exists.
pub fn enabled(dir: &Path) -> bool {
    !dir.join("approvals.off").exists()
}

/// Creates or removes `<dir>/approvals.off`.
pub fn set_enabled(dir: &Path, on: bool) -> io::Result<()> {
    let off = dir.join("approvals.off");
    if on {
        match fs::remove_file(&off) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    } else {
        fs::create_dir_all(dir)?;
        fs::write(&off, b"")
    }
}

/// Builds the request for a PermissionRequest hook event.
pub fn new_request(provider: &str, session_key: &str, input: &HookInput, now_ms: i64) -> Request {
    let id = format!(
        "{}__{}__{now_ms}__{:04x}",
        id_part(provider),
        id_part(session_key),
        random_u16()
    );
    let detail = input
        .tool_input
        .as_ref()
        .and_then(|v| serde_json::to_string_pretty(v).ok())
        .unwrap_or_default();
    Request {
        id,
        provider: provider.to_owned(),
        session_key: session_key.to_owned(),
        label_hint: project(input.cwd.as_deref())
            .map(|p| clean(&p, TEXT_MAX))
            .unwrap_or_default(),
        tool_name: clean(input.tool_name.as_deref().unwrap_or("Tool"), TEXT_MAX),
        summary: pending_text(input),
        detail: clean_keep_newlines(&detail, DETAIL_MAX),
        created_ms: now_ms,
        deadline_ms: now_ms + WAIT_MS,
    }
}

/// Keeps only `[A-Za-z0-9_-]`, so ids are always valid file stems.
fn id_part(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// 16 bits that differ between calls in one process (for 65536 calls) and
/// probably between processes. Not cryptographic.
fn random_u16() -> u16 {
    static SEED: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = *SEED.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let x = nanos ^ (u64::from(std::process::id()) << 16);
        // Fold the high bits down so the low 16 depend on all of them.
        x ^ (x >> 16) ^ (x >> 32) ^ (x >> 48)
    });
    // An odd step makes consecutive counters distinct modulo 2^16.
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    seed.wrapping_add(n.wrapping_mul(0x9E37)) as u16
}

/// Truncates to at most `max_chars` (counting the `…`) and replaces control
/// characters other than `\n` with spaces.
fn clean_keep_newlines(s: &str, max_chars: usize) -> String {
    let cut = if s.chars().count() > max_chars {
        truncate(s, max_chars.saturating_sub(1))
    } else {
        s.to_owned()
    };
    cut.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// Atomically writes `<dir>/approvals/<id>.req.json`, creating the
/// approvals dir if needed. Rejects an id outside `[A-Za-z0-9_-]`.
pub fn post(dir: &Path, r: &Request) -> io::Result<()> {
    check_stem(&r.id)?;
    let approvals = approvals_dir(dir);
    fs::create_dir_all(&approvals)?;
    let body = serde_json::to_vec(r).map_err(io::Error::other)?;
    let name = format!("{}.req.json", r.id);
    write_bytes_atomic(&approvals, &name, &approvals.join(&name), &body)
}

/// Every readable, unexpired request, oldest first. Requests whose
/// `deadline_ms` has passed are deleted (with any decision file).
/// Unparsable files are skipped.
pub fn list_pending(dir: &Path, now_ms: i64) -> Vec<Request> {
    let Ok(entries) = fs::read_dir(approvals_dir(dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Request> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".req.json"))
        .filter_map(|e| fs::read(e.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Request>(&bytes).ok())
        .filter(|r| {
            if now_ms < r.deadline_ms {
                return true;
            }
            if check_stem(&r.id).is_ok() {
                remove_files(dir, &r.id);
            }
            false
        })
        .collect();
    out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Atomically writes the decision for request `id`. Errors with
/// `InvalidInput` for an id outside `[A-Za-z0-9_-]` and `NotFound` when no
/// such request is pending.
pub fn decide(dir: &Path, id: &str, b: Behavior) -> io::Result<()> {
    check_stem(id)?;
    if !req_path(dir, id).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no pending approval request {id:?}"),
        ));
    }
    let approvals = approvals_dir(dir);
    let body = serde_json::to_vec(&Decision { behavior: b }).map_err(io::Error::other)?;
    let name = format!("{id}.dec.json");
    write_bytes_atomic(&approvals, &name, &approvals.join(&name), &body)
}

/// Best-effort removal of a request's request and decision files.
fn remove_files(dir: &Path, id: &str) {
    let _ = fs::remove_file(req_path(dir, id));
    let _ = fs::remove_file(dec_path(dir, id));
}

/// Removes the request's files when dropped, so every return path of
/// [`wait`] (and a panic) cleans up.
struct Cleanup<'a> {
    dir: &'a Path,
    id: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if check_stem(self.id).is_ok() {
            remove_files(self.dir, self.id);
        }
    }
}

/// Waits for the decision on `r`, polling every [`POLL_MS`] with the
/// injected clock and sleep. Each poll first reads the decision file, then
/// gives up at `r.deadline_ms` or as soon as the app is not alive. A
/// decision file that is not exactly an allow or deny yields `None`.
/// Removes the request and decision files before returning.
pub fn wait(
    dir: &Path,
    r: &Request,
    mut now: impl FnMut() -> i64,
    mut sleep: impl FnMut(Duration),
) -> Option<Behavior> {
    let _cleanup = Cleanup { dir, id: &r.id };
    check_stem(&r.id).ok()?;
    let dec = dec_path(dir, &r.id);
    loop {
        match fs::read(&dec) {
            Ok(bytes) => {
                return serde_json::from_slice::<Decision>(&bytes)
                    .ok()
                    .map(|d| d.behavior);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        let t = now();
        if t >= r.deadline_ms || !app_alive(dir, t) {
            return None;
        }
        sleep(Duration::from_millis(POLL_MS));
    }
}

/// The hook's stdout for a decision, as documented for Claude Code's and
/// Codex's PermissionRequest hooks.
pub fn decision_json(b: Behavior) -> String {
    let decision = match b {
        Behavior::Allow => serde_json::json!({"behavior": "allow"}),
        Behavior::Deny => serde_json::json!({"behavior": "deny", "message": DENY_MESSAGE}),
    };
    serde_json::json!({
        "hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;

    const T0: i64 = 1_700_000_000_000;

    fn bash(cmd: &str) -> HookInput {
        let raw = serde_json::json!({
            "session_id": "s1",
            "cwd": "/r/web-app",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
        })
        .to_string();
        HookInput::from_json(&raw).unwrap()
    }

    fn req_path(dir: &Path, id: &str) -> PathBuf {
        dir.join("approvals").join(format!("{id}.req.json"))
    }

    fn dec_path(dir: &Path, id: &str) -> PathBuf {
        dir.join("approvals").join(format!("{id}.dec.json"))
    }

    /// A fake clock and a sleep that advances it, optionally keeping the
    /// app alive and running `on_sleep` after each tick.
    fn run_wait(
        dir: &Path,
        r: &Request,
        start: i64,
        keep_alive: bool,
        mut on_sleep: impl FnMut(i64),
    ) -> (Option<Behavior>, usize) {
        let clock = Cell::new(start);
        let mut sleeps = 0;
        let got = wait(dir, r, || clock.get(), |d| {
            sleeps += 1;
            assert_eq!(d, Duration::from_millis(POLL_MS));
            clock.set(clock.get() + d.as_millis() as i64);
            if keep_alive {
                write_alive(dir, clock.get()).unwrap();
            }
            on_sleep(clock.get());
        });
        (got, sleeps)
    }

    #[test]
    fn alive_fresh_and_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert!(!app_alive(dir, T0), "no file means not alive");
        write_alive(dir, T0).unwrap();
        let body = fs::read_to_string(dir.join("app.alive")).unwrap();
        assert!(body.contains(&std::process::id().to_string()), "{body}");
        assert!(app_alive(dir, T0));
        assert!(app_alive(dir, T0 + ALIVE_MAX_AGE_MS - 1));
        assert!(!app_alive(dir, T0 + ALIVE_MAX_AGE_MS));
        assert!(!app_alive(dir, T0 + 60_000));
        fs::write(dir.join("app.alive"), "garbage").unwrap();
        assert!(!app_alive(dir, T0));
    }

    #[test]
    fn enabled_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("fresh");
        assert!(enabled(&dir), "enabled by default");
        set_enabled(&dir, false).unwrap();
        assert!(dir.join("approvals.off").exists());
        assert!(!enabled(&dir));
        set_enabled(&dir, false).unwrap();
        set_enabled(&dir, true).unwrap();
        assert!(enabled(&dir));
        set_enabled(&dir, true).unwrap();
    }

    #[test]
    fn post_then_list_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let r = new_request("claude", "s1", &bash("rm -rf build"), T0);
        assert!(r.id.starts_with(&format!("claude__s1__{T0}__")), "{}", r.id);
        let hex = r.id.rsplit("__").next().unwrap();
        assert_eq!(hex.len(), 4);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{hex}");
        assert_eq!(r.provider, "claude");
        assert_eq!(r.session_key, "s1");
        assert_eq!(r.label_hint, "web-app");
        assert_eq!(r.tool_name, "Bash");
        assert_eq!(r.summary, "Bash: rm -rf build");
        assert_eq!(r.detail, "{\n  \"command\": \"rm -rf build\"\n}");
        assert_eq!(r.created_ms, T0);
        assert_eq!(r.deadline_ms, T0 + WAIT_MS);
        post(dir, &r).unwrap();
        assert!(req_path(dir, &r.id).is_file());
        let names: Vec<String> = fs::read_dir(dir.join("approvals"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [format!("{}.req.json", r.id)], "no temp files left");
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(req_path(dir, &r.id)).unwrap()).unwrap();
        for key in [
            "id", "provider", "session_key", "label_hint", "tool_name", "summary", "detail",
            "created_ms", "deadline_ms",
        ] {
            assert!(raw.get(key).is_some(), "missing {key}");
        }
        assert_eq!(list_pending(dir, T0 + 1), vec![r]);
        assert!(list_pending(&dir.join("nope"), T0).is_empty());
    }

    #[test]
    fn expired_request_listed_never_and_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        assert!(list_pending(dir, r.deadline_ms).is_empty());
        assert!(!req_path(dir, &r.id).exists(), "expired request deleted");
        assert!(list_pending(dir, T0).is_empty());
    }

    #[test]
    fn decide_writes_decision_wait_returns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        // Decided before the first poll.
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        decide(dir, &r.id, Behavior::Allow).unwrap();
        let raw = fs::read_to_string(dec_path(dir, &r.id)).unwrap();
        assert_eq!(raw, r#"{"behavior":"allow"}"#);
        let (got, sleeps) = run_wait(dir, &r, T0, true, |_| {});
        assert_eq!(got, Some(Behavior::Allow));
        assert_eq!(sleeps, 0);
        assert!(!req_path(dir, &r.id).exists());
        assert!(!dec_path(dir, &r.id).exists());
        // Decided mid-wait, after a few polls.
        let r = new_request("codex", "s2", &bash("ls"), T0);
        post(dir, &r).unwrap();
        let id = r.id.clone();
        let (got, sleeps) = run_wait(dir, &r, T0, true, |now| {
            if now == T0 + 3 * POLL_MS as i64 {
                decide(dir, &id, Behavior::Deny).unwrap();
            }
        });
        assert_eq!(got, Some(Behavior::Deny));
        assert_eq!(sleeps, 3);
        assert!(!req_path(dir, &r.id).exists());
        assert!(!dec_path(dir, &r.id).exists());
    }

    #[test]
    fn wait_times_out_none_and_cleans_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let mut r = new_request("claude", "s1", &bash("ls"), T0);
        r.deadline_ms = T0 + 1000;
        post(dir, &r).unwrap();
        let (got, sleeps) = run_wait(dir, &r, T0, true, |_| {});
        assert_eq!(got, None);
        assert_eq!(sleeps, 5, "stops at the deadline");
        assert!(!req_path(dir, &r.id).exists());
        assert!(!dec_path(dir, &r.id).exists());
    }

    #[test]
    fn wait_returns_none_when_app_dies_midwait() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        // The app never refreshes app.alive after T0.
        let (got, sleeps) = run_wait(dir, &r, T0, false, |_| {});
        assert_eq!(got, None);
        assert_eq!(sleeps as i64, ALIVE_MAX_AGE_MS / POLL_MS as i64);
        assert!(!req_path(dir, &r.id).exists());
        // Not alive at all: returns before sleeping.
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        let (got, sleeps) = run_wait(dir, &r, T0 + 60_000, false, |_| {});
        assert_eq!((got, sleeps), (None, 0));
        assert!(!req_path(dir, &r.id).exists());
    }

    #[test]
    fn malformed_decision_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        for body in [r#"{"behavior":"always"}"#, "garbage", ""] {
            let r = new_request("claude", "s1", &bash("ls"), T0);
            post(dir, &r).unwrap();
            fs::write(dec_path(dir, &r.id), body).unwrap();
            let (got, _) = run_wait(dir, &r, T0, true, |_| {});
            assert_eq!(got, None, "{body:?}");
            assert!(!req_path(dir, &r.id).exists());
            assert!(!dec_path(dir, &r.id).exists());
        }
    }

    #[test]
    fn two_requests_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let a = new_request("claude", "a", &bash("ls"), T0);
        let b = new_request("claude", "b", &bash("ls"), T0);
        // Same session, same millisecond: ids still differ.
        let a2 = new_request("claude", "a", &bash("ls"), T0);
        assert_ne!(a.id, a2.id);
        assert_ne!(a.id, b.id);
        post(dir, &a).unwrap();
        post(dir, &b).unwrap();
        let mut ids: Vec<String> = list_pending(dir, T0).into_iter().map(|r| r.id).collect();
        ids.sort();
        let mut want = vec![a.id.clone(), b.id.clone()];
        want.sort();
        assert_eq!(ids, want);
        decide(dir, &b.id, Behavior::Deny).unwrap();
        decide(dir, &a.id, Behavior::Allow).unwrap();
        assert_eq!(run_wait(dir, &b, T0, true, |_| {}).0, Some(Behavior::Deny));
        assert!(req_path(dir, &a.id).exists(), "a's files untouched by b's wait");
        assert!(dec_path(dir, &a.id).exists());
        assert_eq!(run_wait(dir, &a, T0, true, |_| {}).0, Some(Behavior::Allow));
    }

    #[test]
    fn decide_rejects_bad_id_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for bad in ["../x", "a/b", "a\\b", "..", "", "a.b", "a b", "C:x"] {
            let err = decide(dir, bad, Behavior::Allow).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
        let err = decide(dir, "claude__s1__1__abcd", Behavior::Allow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!dec_path(dir, "claude__s1__1__abcd").exists());
    }

    #[test]
    fn decision_json_exact() {
        assert_eq!(
            decision_json(Behavior::Allow),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
        );
        assert_eq!(
            decision_json(Behavior::Deny),
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Agent Pin"}}}"#
        );
    }

    #[test]
    fn detail_truncated_and_control_chars_cleaned() {
        let raw = serde_json::json!({
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/r/a.rs", "content": "x".repeat(10_000)},
        })
        .to_string();
        let r = new_request("claude", "s1", &HookInput::from_json(&raw).unwrap(), T0);
        assert_eq!(r.detail.chars().count(), 4000);
        assert!(r.detail.ends_with('…'));
        assert!(r.detail.starts_with("{\n  \"file_path\": \"/r/a.rs\","), "{}", r.detail);
        assert_eq!(r.summary, "Write: /r/a.rs");
        // Control chars from the payload (here in a tool name and a key)
        // become spaces; pretty-print newlines are kept.
        let raw = serde_json::json!({
            "tool_name": "Ba\u{7}sh",
            "tool_input": {"k\u{1b}ey": "v", "command": "a\tb"},
        })
        .to_string();
        let r = new_request("claude", "s1", &HookInput::from_json(&raw).unwrap(), T0);
        assert!(!r.detail.chars().any(|c| c.is_control() && c != '\n'), "{:?}", r.detail);
        assert!(!r.tool_name.chars().any(char::is_control), "{:?}", r.tool_name);
        assert!(!r.summary.chars().any(char::is_control), "{:?}", r.summary);
        // No tool_input: empty detail; no tool name: "Tool".
        let r = new_request("claude", "s1", &HookInput::default(), T0);
        assert_eq!(r.detail, "");
        assert_eq!(r.tool_name, "Tool");
        assert_eq!(r.label_hint, "");
    }
}
