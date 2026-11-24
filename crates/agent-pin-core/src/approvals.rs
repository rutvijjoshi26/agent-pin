//! File-based approval requests: the hook posts
//! `<state_dir>/approvals/<id>.req.json` and waits for the app to write
//! `<state_dir>/approvals/<id>.dec.json`. The app proves it is running by
//! refreshing `<state_dir>/app.alive`; `<state_dir>/approvals.off` turns
//! the feature off.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::BuildHasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::input::{truncate, HookInput};
use crate::rows::{project, Row};
use crate::session::{pending_raw, Status};
use crate::store::{check_stem, older_than, publish_new, write_bytes_atomic};

/// How often [`wait`] polls for a decision.
pub const POLL_MS: u64 = 200;
/// `app.alive` this old or older means the app is not running.
pub const ALIVE_MAX_AGE_MS: i64 = 3000;
/// A request file whose mtime is older than this has no live hook behind
/// it: a waiting hook refreshes it every [`POLL_MS`].
pub const HOOK_STALE_MS: i64 = 5000;
/// The app's liveness file in the state dir.
pub const ALIVE_FILE: &str = "app.alive";
/// How long a request waits for a decision.
pub const WAIT_MS: i64 = 590_000;

/// Max chars in a request's `detail` (including the trailing `…`).
const DETAIL_MAX: usize = 4000;
/// Max chars in a request's other text fields (including the trailing `…`).
const TEXT_MAX: usize = 200;
/// Temp files older than this are left over from a crash and swept.
const TEMP_MAX_AGE: Duration = Duration::from_secs(60);
/// An unparsable request file younger than this may still be being
/// written, so the sweep keeps it.
const UNPARSABLE_GRACE: Duration = Duration::from_secs(5);
/// The message Claude Code and Codex show for a deny.
const DENY_MESSAGE: &str = "Denied from Agent Pin";

/// One pending permission request, as stored in `<id>.req.json`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// `<provider>__<session_key>__<unix_ms>__<16 lowercase hex>`,
    /// `[A-Za-z0-9_-]+`.
    pub id: String,
    pub provider: String,
    pub session_key: String,
    /// The project basename of the session's cwd, or empty. This and the
    /// other text fields are at most 200 chars, with control and invisible
    /// format characters removed.
    #[serde(default)]
    pub label_hint: String,
    pub tool_name: String,
    /// The session's pending text, e.g. `Bash: rm -rf build`.
    pub summary: String,
    /// Pretty-printed `tool_input`, at most 4000 chars, no control chars
    /// other than newlines and no invisible format characters.
    pub detail: String,
    /// True when `detail` was cut at the 4000-char cap. Such a request can
    /// only be denied from Agent Pin. Absent in older files: false.
    #[serde(default)]
    pub truncated: bool,
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

/// The decision file's contents. Anything else, extra fields included, is
/// not a decision.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    write_bytes_atomic(dir, ALIVE_FILE, &dir.join(ALIVE_FILE), body.as_bytes())
}

/// Removes `<dir>/app.alive` only when it records this process's pid, so
/// an exiting instance never removes another running instance's file. A
/// missing or unparsable file is left alone and is not an error.
pub fn remove_alive(dir: &Path) -> io::Result<()> {
    let path = dir.join(ALIVE_FILE);
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let pid = body.split_whitespace().nth(1).and_then(|p| p.parse::<u32>().ok());
    if pid != Some(std::process::id()) {
        return Ok(());
    }
    match fs::remove_file(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// True when `app.alive` holds a timestamp less than [`ALIVE_MAX_AGE_MS`]
/// from `now_ms` (either way, so a stale file with a future stamp from a
/// clock jump does not count as alive forever).
pub fn app_alive(dir: &Path, now_ms: i64) -> bool {
    let Ok(body) = fs::read_to_string(dir.join(ALIVE_FILE)) else {
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
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let suffix = random_suffix(
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        &RandomState::new(),
    );
    let id = format!(
        "{}__{}__{now_ms}__{suffix:016x}",
        id_part(provider),
        id_part(session_key)
    );
    let detail = input
        .tool_input
        .as_ref()
        .and_then(|v| serde_json::to_string_pretty(v).ok())
        .unwrap_or_default();
    let (detail, truncated) = sanitize_flagged(&detail, DETAIL_MAX, true);
    Request {
        id,
        provider: provider.to_owned(),
        session_key: session_key.to_owned(),
        label_hint: project(input.cwd.as_deref())
            .map(|p| sanitize(&p, TEXT_MAX, false))
            .unwrap_or_default(),
        tool_name: sanitize(input.tool_name.as_deref().unwrap_or("Tool"), TEXT_MAX, false),
        summary: sanitize(&pending_raw(input), TEXT_MAX, false),
        detail,
        truncated,
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

/// 64 bits for an id suffix, std only and not cryptographic. `state` is a
/// fresh `RandomState`, whose keys are randomly seeded per process (and
/// differ per call within it), so two hook processes with the same clock,
/// pid and counter still get different suffixes. A stack address adds
/// ASLR entropy.
fn random_suffix(nanos: u64, pid: u32, counter: u64, state: &RandomState) -> u64 {
    let stack = &nanos as *const u64 as usize;
    state.hash_one((nanos, pid, counter, stack))
}

/// Invisible Unicode format characters (zero-width and bidi controls) that
/// could make the text shown on a card differ from what will run.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}'
        | '\u{feff}')
}

/// Removes invisible format characters, replaces control characters with
/// spaces (keeping `\n` when `keep_newlines`), then truncates to at most
/// `max_chars` counting the trailing `…`.
fn sanitize(s: &str, max_chars: usize, keep_newlines: bool) -> String {
    sanitize_flagged(s, max_chars, keep_newlines).0
}

/// [`sanitize`], also reporting whether the text was cut.
fn sanitize_flagged(s: &str, max_chars: usize, keep_newlines: bool) -> (String, bool) {
    let cleaned: String = s
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() && !(keep_newlines && c == '\n') { ' ' } else { c })
        .collect();
    if cleaned.chars().count() > max_chars {
        (truncate(&cleaned, max_chars.saturating_sub(1)), true)
    } else {
        (cleaned, false)
    }
}

/// Proof that [`post`] published a request. Only `post` makes one, and
/// [`wait`] consumes it, so a wait can never run for a request this process
/// did not publish (whose files, or planted decision, belong to someone
/// else).
///
/// ```compile_fail
/// use agent_pin_core::approvals::{new_request, Posted};
/// let r = new_request("claude", "s1", &Default::default(), 0);
/// let forged = Posted { request: r };
/// ```
#[derive(Debug)]
pub struct Posted {
    request: Request,
}

impl Posted {
    /// The request that was published.
    pub fn request(&self) -> &Request {
        &self.request
    }
}

/// Publishes `<dir>/approvals/<id>.req.json`, creating the approvals dir
/// if needed. Never replaces anything: fails with `AlreadyExists` when the
/// request or a decision for its id already exists, so a stale decision can
/// never answer a new request. Rejects an id outside `[A-Za-z0-9_-]`. Only
/// the returned [`Posted`] lets the caller [`wait`].
pub fn post(dir: &Path, r: &Request) -> io::Result<Posted> {
    check_stem(&r.id)?;
    let approvals = approvals_dir(dir);
    fs::create_dir_all(&approvals)?;
    if fs::symlink_metadata(dec_path(dir, &r.id)).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a decision already exists for this approval id",
        ));
    }
    let body = serde_json::to_vec(r).map_err(io::Error::other)?;
    publish_new(&approvals, &format!("{}.req.json", r.id), &body)?;
    Ok(Posted { request: r.clone() })
}

/// Every valid, unexpired, undecided request, oldest first. Also sweeps
/// `<dir>/approvals`, best effort:
/// - expired requests (`deadline_ms` passed), and requests whose file mtime
///   is more than [`HOOK_STALE_MS`] old (their hook is gone, e.g. killed
///   after the user answered in the terminal), each with any decision file;
/// - request files whose content `id` is not their file stem, and ones
///   that do not parse once they are 5 s old (a younger one may still be
///   being written);
/// - decision files with no request that are older than [`WAIT_MS`];
/// - temp files older than 60 s, and `<dir>/app.alive.tmp-*` likewise.
///
/// File ages are mtimes measured against the system clock.
pub fn list_pending(dir: &Path, now_ms: i64) -> Vec<Request> {
    sweep_alive_temps(dir);
    let approvals = approvals_dir(dir);
    let Ok(entries) = fs::read_dir(&approvals) else {
        return Vec::new();
    };
    let names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut out = Vec::new();
    for name in &names {
        let path = approvals.join(name);
        if let Some(stem) = name.strip_suffix(".req.json") {
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            match serde_json::from_slice::<Request>(&bytes) {
                Ok(r) if r.id == stem && check_stem(stem).is_ok() => {
                    let hook_gone =
                        older_than(&path, Duration::from_millis(HOOK_STALE_MS as u64));
                    let decided = approvals.join(format!("{stem}.dec.json")).exists();
                    if now_ms < r.deadline_ms && !hook_gone {
                        // A decided request waits only for its hook to read
                        // the decision; it is not offered again.
                        if !decided {
                            out.push(r);
                        }
                    } else {
                        remove_files(dir, stem);
                    }
                }
                Ok(_) => {
                    let _ = fs::remove_file(&path);
                }
                // Possibly still being written: swept only once old.
                Err(_) if older_than(&path, UNPARSABLE_GRACE) => {
                    let _ = fs::remove_file(&path);
                }
                Err(_) => {}
            }
        } else if let Some(stem) = name.strip_suffix(".dec.json") {
            let orphan = !approvals.join(format!("{stem}.req.json")).exists();
            if orphan && older_than(&path, Duration::from_millis(WAIT_MS as u64)) {
                let _ = fs::remove_file(&path);
            }
        } else if name.contains(".tmp-") && older_than(&path, TEMP_MAX_AGE) {
            let _ = fs::remove_file(&path);
        }
    }
    out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Decides which pending requests the overlay shows, by their session row
/// (`<provider>__<session_key>`). Returns `(keep, delete)`:
/// - row WaitingApproval or Running: kept. Running still counts because
///   subagents and parallel tool calls report under the parent's session
///   and can flip the row to Running while the request is live;
/// - row Completed, Idle or Error: its id is returned to delete, as the
///   session has moved on;
/// - no row (hidden, or a transient read failure): in neither list, so it
///   is hidden but not deleted.
///
/// Rows are not what dismisses a request whose hook has gone: the hook
/// refreshes its request file's mtime while it waits, and [`list_pending`]
/// deletes requests it has stopped refreshing ([`HOOK_STALE_MS`]).
pub fn pending_for_rows(pending: Vec<Request>, rows: &[Row]) -> (Vec<Request>, Vec<String>) {
    let mut keep = Vec::new();
    let mut delete = Vec::new();
    for r in pending {
        let key = format!("{}__{}", r.provider, r.session_key);
        match rows.iter().find(|row| row.key == key).map(|row| row.status) {
            Some(Status::WaitingApproval | Status::Running) => keep.push(r),
            Some(Status::Completed | Status::Idle | Status::Error) => delete.push(r.id),
            None => {}
        }
    }
    (keep, delete)
}

/// Removes the request and decision files for each id, best effort. Ids
/// outside `[A-Za-z0-9_-]` are skipped.
pub fn remove(dir: &Path, ids: &[String]) {
    for id in ids.iter().filter(|id| check_stem(id).is_ok()) {
        remove_files(dir, id);
    }
}

/// Removes `<dir>/app.alive.tmp-*` files older than 60 s.
fn sweep_alive_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let is_temp = e.file_name().to_str().is_some_and(|n| n.starts_with("app.alive.tmp-"));
        if is_temp && older_than(&e.path(), TEMP_MAX_AGE) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Writes the decision for request `id`, once: an existing decision is
/// never replaced, so a click can't be changed before the hook reads it.
/// Errors with `InvalidInput` for an id outside `[A-Za-z0-9_-]`, `NotFound`
/// when no such request is pending and `AlreadyExists` when it is already
/// decided.
pub fn decide(dir: &Path, id: &str, b: Behavior) -> io::Result<()> {
    check_stem(id)?;
    if !req_path(dir, id).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no pending approval request {id:?}"),
        ));
    }
    if b == Behavior::Allow {
        let truncated = fs::read(req_path(dir, id))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Request>(&bytes).ok())
            .is_some_and(|r| r.truncated);
        if truncated {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("approval request {id:?} has truncated detail: it can only be denied"),
            ));
        }
    }
    let body = serde_json::to_vec(&Decision { behavior: b }).map_err(io::Error::other)?;
    publish_new(&approvals_dir(dir), &format!("{id}.dec.json"), &body).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("approval request {id:?} is already decided"),
            )
        } else {
            error
        }
    })
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

/// Waits for the decision on the posted request, polling every [`POLL_MS`] with the
/// injected clock and sleep. Each poll first reads the decision file, then
/// gives up if its own request file is gone (the app dismissed it), else
/// refreshes the request file's mtime so the app knows this hook is still
/// waiting (see [`HOOK_STALE_MS`]), then gives up at its `deadline_ms` or
/// as soon as the app is not alive or approvals are switched off. A
/// decision file that is not exactly an allow or deny yields `None`.
/// Removes the request and decision files before returning.
pub fn wait(
    dir: &Path,
    posted: Posted,
    mut now: impl FnMut() -> i64,
    mut sleep: impl FnMut(Duration),
) -> Option<Behavior> {
    let r = &posted.request;
    let _cleanup = Cleanup { dir, id: &r.id };
    check_stem(&r.id).ok()?;
    let dec = dec_path(dir, &r.id);
    let req = req_path(dir, &r.id);
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
        if fs::symlink_metadata(&req).is_err() {
            return None;
        }
        let _ = fs::File::options()
            .write(true)
            .open(&req)
            .and_then(|f| f.set_modified(SystemTime::now()));
        let t = now();
        if t >= r.deadline_ms || !app_alive(dir, t) || !enabled(dir) {
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
        posted: Posted,
        start: i64,
        keep_alive: bool,
        mut on_sleep: impl FnMut(i64),
    ) -> (Option<Behavior>, usize) {
        let clock = Cell::new(start);
        let mut sleeps = 0;
        let got = wait(dir, posted, || clock.get(), |d| {
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
        assert_eq!(hex.len(), 16);
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
        let posted = post(dir, &r).unwrap();
        assert_eq!(posted.request(), &r);
        decide(dir, &r.id, Behavior::Allow).unwrap();
        let raw = fs::read_to_string(dec_path(dir, &r.id)).unwrap();
        assert_eq!(raw, r#"{"behavior":"allow"}"#);
        let (got, sleeps) = run_wait(dir, posted, T0, true, |_| {});
        assert_eq!(got, Some(Behavior::Allow));
        assert_eq!(sleeps, 0);
        assert!(!req_path(dir, &r.id).exists());
        assert!(!dec_path(dir, &r.id).exists());
        // Decided mid-wait, after a few polls.
        let r = new_request("codex", "s2", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        let id = r.id.clone();
        let (got, sleeps) = run_wait(dir, posted, T0, true, |now| {
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
        let posted = post(dir, &r).unwrap();
        let (got, sleeps) = run_wait(dir, posted, T0, true, |_| {});
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
        let posted = post(dir, &r).unwrap();
        // The app never refreshes app.alive after T0.
        let (got, sleeps) = run_wait(dir, posted, T0, false, |_| {});
        assert_eq!(got, None);
        assert_eq!(sleeps as i64, ALIVE_MAX_AGE_MS / POLL_MS as i64);
        assert!(!req_path(dir, &r.id).exists());
        // Not alive at all: returns before sleeping.
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        let (got, sleeps) = run_wait(dir, posted, T0 + 60_000, false, |_| {});
        assert_eq!((got, sleeps), (None, 0));
        assert!(!req_path(dir, &r.id).exists());
    }

    #[test]
    fn truncated_flag_set_only_when_detail_cut() {
        let big = serde_json::json!({
            "tool_name": "Write",
            "tool_input": {"content": "x".repeat(10_000)},
        })
        .to_string();
        let r = new_request("claude", "s1", &HookInput::from_json(&big).unwrap(), T0);
        assert!(r.truncated);
        let r = new_request("claude", "s1", &bash("ls"), T0);
        assert!(!r.truncated);
        // Old request files have no such field.
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("truncated");
        let old: Request = serde_json::from_value(v).unwrap();
        assert!(!old.truncated);
    }

    #[test]
    fn wait_returns_none_once_approvals_switched_off() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        let (got, sleeps) = run_wait(dir, posted, T0, true, |t| {
            if t >= T0 + 400 {
                set_enabled(dir, false).unwrap();
            }
        });
        assert_eq!(got, None);
        assert_eq!(sleeps, 2, "returns on the poll after the switch");
        assert!(!req_path(dir, &r.id).exists());
        assert!(!dec_path(dir, &r.id).exists());
    }

    #[test]
    fn malformed_decision_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        for body in [r#"{"behavior":"always"}"#, "garbage", ""] {
            let r = new_request("claude", "s1", &bash("ls"), T0);
            let posted = post(dir, &r).unwrap();
            fs::write(dec_path(dir, &r.id), body).unwrap();
            let (got, _) = run_wait(dir, posted, T0, true, |_| {});
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
        let posted_a = post(dir, &a).unwrap();
        let posted_b = post(dir, &b).unwrap();
        let mut ids: Vec<String> = list_pending(dir, T0).into_iter().map(|r| r.id).collect();
        ids.sort();
        let mut want = vec![a.id.clone(), b.id.clone()];
        want.sort();
        assert_eq!(ids, want);
        decide(dir, &b.id, Behavior::Deny).unwrap();
        decide(dir, &a.id, Behavior::Allow).unwrap();
        assert_eq!(run_wait(dir, posted_b, T0, true, |_| {}).0, Some(Behavior::Deny));
        assert!(req_path(dir, &a.id).exists(), "a's files untouched by b's wait");
        assert!(dec_path(dir, &a.id).exists());
        assert_eq!(run_wait(dir, posted_a, T0, true, |_| {}).0, Some(Behavior::Allow));
    }

    #[test]
    fn decide_rejects_bad_id_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for bad in ["../x", "a/b", "a\\b", "..", "", "a.b", "a b", "C:x"] {
            let err = decide(dir, bad, Behavior::Allow).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
            assert_eq!(err.to_string(), "invalid id", "{bad}");
        }
        let err = decide(dir, "claude__s1__1__abcd", Behavior::Allow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!dec_path(dir, "claude__s1__1__abcd").exists());
    }

    #[test]
    fn decide_refuses_allow_for_truncated_request() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let input = bash(&"x".repeat(DETAIL_MAX + 500));
        let r = new_request("claude", "s1", &input, T0);
        assert!(r.truncated);
        post(dir, &r).unwrap();
        let err = decide(dir, &r.id, Behavior::Allow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(!dec_path(dir, &r.id).exists());
        decide(dir, &r.id, Behavior::Deny).unwrap();
        assert!(dec_path(dir, &r.id).exists());
    }

    #[test]
    fn decide_is_create_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        decide(dir, &r.id, Behavior::Allow).unwrap();
        let first = fs::read(dec_path(dir, &r.id)).unwrap();
        let err = decide(dir, &r.id, Behavior::Deny).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dec_path(dir, &r.id)).unwrap(), first);
        let err = decide(dir, &r.id, Behavior::Allow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dec_path(dir, &r.id)).unwrap(), first);
    }

    #[test]
    fn list_pending_skips_decided_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let open = new_request("claude", "s1", &bash("ls"), T0);
        let decided = new_request("claude", "s2", &bash("ls"), T0);
        post(dir, &open).unwrap();
        post(dir, &decided).unwrap();
        decide(dir, &decided.id, Behavior::Deny).unwrap();
        assert_eq!(list_pending(dir, T0), vec![open.clone()]);
        // Skipped, not swept: the hook still has to read its decision.
        assert!(req_path(dir, &decided.id).exists());
        assert!(dec_path(dir, &decided.id).exists());
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

    fn set_age(path: &Path, age: Duration) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age).unwrap();
    }

    #[test]
    fn post_refuses_existing_request() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(dir, &r).unwrap();
        let mut other = r.clone();
        other.summary = "Bash: rm -rf /".to_owned();
        let err = post(dir, &other).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(list_pending(dir, T0), vec![r.clone()], "first request kept");
        let names: Vec<String> = fs::read_dir(dir.join("approvals"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [format!("{}.req.json", r.id)], "no temp files left");
    }

    #[test]
    fn post_refuses_preexisting_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        fs::create_dir_all(dir.join("approvals")).unwrap();
        fs::write(dec_path(dir, &r.id), r#"{"behavior":"deny"}"#).unwrap();
        let err = post(dir, &r).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(!req_path(dir, &r.id).exists());
    }

    /// A decision written before the request was posted (an orphan from a
    /// `decide` that raced a finished wait) must never release a hook.
    #[test]
    fn planted_decision_before_post_never_allows() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        fs::create_dir_all(dir.join("approvals")).unwrap();
        fs::write(dec_path(dir, &r.id), r#"{"behavior":"allow"}"#).unwrap();
        assert!(post(dir, &r).is_err(), "the hook gives up when post fails");
        assert!(list_pending(dir, T0).is_empty(), "the app never shows it");
    }

    #[test]
    fn ids_differ_across_fresh_process_state() {
        use std::collections::hash_map::RandomState;
        // Same clock, pid and a fresh counter, as two hook processes
        // started in the same nanosecond would have.
        let a = random_suffix(T0 as u64 * 1_000_000, 4242, 0, &RandomState::new());
        let b = random_suffix(T0 as u64 * 1_000_000, 4242, 0, &RandomState::new());
        assert_ne!(a, b);
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let hex = r.id.rsplit("__").next().unwrap();
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hex}");
    }

    #[test]
    fn list_pending_rejects_content_id_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let victim = new_request("claude", "victim", &bash("ls"), T0);
        post(dir, &victim).unwrap();
        // A file whose content claims another request's id, and one whose
        // content id is not a valid stem.
        let mut forged = new_request("claude", "s1", &bash("ls"), T0);
        let forged_name = forged.id.clone();
        forged.id = victim.id.clone();
        fs::write(req_path(dir, &forged_name), serde_json::to_vec(&forged).unwrap()).unwrap();
        let mut bad = new_request("claude", "s2", &bash("ls"), T0);
        let bad_name = bad.id.clone();
        bad.id = "../escape".to_owned();
        fs::write(req_path(dir, &bad_name), serde_json::to_vec(&bad).unwrap()).unwrap();
        assert_eq!(list_pending(dir, T0), vec![victim.clone()]);
        assert!(!req_path(dir, &forged_name).exists());
        assert!(!req_path(dir, &bad_name).exists());
        assert!(req_path(dir, &victim.id).exists());
    }

    #[test]
    fn list_pending_sweeps_orphans_garbage_and_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let approvals = dir.join("approvals");
        let live = new_request("claude", "live", &bash("ls"), T0);
        post(dir, &live).unwrap();
        decide(dir, &live.id, Behavior::Allow).unwrap();
        let old = Duration::from_millis(WAIT_MS as u64 + 60_000);
        set_age(&dec_path(dir, &live.id), old);
        let orphan_old = dec_path(dir, "claude__gone__1__00000000000000aa");
        let orphan_new = dec_path(dir, "claude__gone__2__00000000000000bb");
        fs::write(&orphan_old, r#"{"behavior":"allow"}"#).unwrap();
        fs::write(&orphan_new, r#"{"behavior":"allow"}"#).unwrap();
        set_age(&orphan_old, old);
        let garbage = req_path(dir, "claude__junk__1__00000000000000cc");
        fs::write(&garbage, "{not json").unwrap();
        set_age(&garbage, Duration::from_secs(6));
        let tmp_old = approvals.join("x.req.json.tmp-1-0");
        let tmp_new = approvals.join("x.req.json.tmp-1-1");
        let alive_tmp_old = dir.join("app.alive.tmp-1-0");
        let alive_tmp_new = dir.join("app.alive.tmp-1-1");
        for p in [&tmp_old, &tmp_new, &alive_tmp_old, &alive_tmp_new] {
            fs::write(p, "").unwrap();
        }
        set_age(&tmp_old, Duration::from_secs(61));
        set_age(&alive_tmp_old, Duration::from_secs(61));
        // Decided, so not listed; its files stay for the hook to read.
        assert!(list_pending(dir, T0).is_empty());
        assert!(req_path(dir, &live.id).exists());
        assert!(!orphan_old.exists(), "old orphan decision removed");
        assert!(orphan_new.exists(), "recent orphan decision kept");
        assert!(dec_path(dir, &live.id).exists(), "a live request's decision is kept");
        assert!(!garbage.exists(), "unparsable request removed");
        assert!(!tmp_old.exists() && tmp_new.exists());
        assert!(!alive_tmp_old.exists() && alive_tmp_new.exists());
    }

    /// A request file still being written (the `create_new` fallback of
    /// `publish_new`) is not swept until it is at least 5 s old.
    #[test]
    fn young_unparsable_request_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("approvals")).unwrap();
        let partial = req_path(dir, "claude__s1__1__00000000000000dd");
        fs::write(&partial, r#"{"id":"claude__s1"#).unwrap();
        assert!(list_pending(dir, T0).is_empty(), "not listed");
        assert!(partial.exists(), "a young unparsable request is kept");
        set_age(&partial, Duration::from_secs(4));
        list_pending(dir, T0);
        assert!(partial.exists(), "4 s old is still kept");
        set_age(&partial, Duration::from_secs(6));
        list_pending(dir, T0);
        assert!(!partial.exists(), "swept once 5 s old");
    }

    #[test]
    fn decision_with_unknown_fields_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        fs::write(dec_path(dir, &r.id), r#"{"behavior":"allow","updatedInput":{}}"#).unwrap();
        assert_eq!(run_wait(dir, posted, T0, true, |_| {}).0, None);
        assert!(!dec_path(dir, &r.id).exists());
    }

    #[test]
    fn invisible_format_chars_stripped() {
        let raw = serde_json::json!({
            "cwd": "/r/we\u{202e}b",
            "tool_name": "Ba\u{200b}sh",
            "tool_input": {"command": "rm \u{202e}fdp.exe\u{2066}x\u{feff}"},
        })
        .to_string();
        let r = new_request("claude", "s1", &HookInput::from_json(&raw).unwrap(), T0);
        let invisible = |c: char| {
            matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        };
        for (name, text) in [
            ("summary", &r.summary),
            ("detail", &r.detail),
            ("label_hint", &r.label_hint),
            ("tool_name", &r.tool_name),
        ] {
            assert!(!text.chars().any(invisible), "{name}: {text:?}");
        }
        assert_eq!(r.summary, "Bash: rm fdp.exex");
        assert_eq!(r.label_hint, "web");
        assert_eq!(r.tool_name, "Bash");
    }

    #[test]
    fn text_fields_at_most_200_chars() {
        let long = "y".repeat(500);
        let raw = serde_json::json!({
            "cwd": format!("/r/{long}"),
            "tool_name": long,
            "tool_input": {"command": long},
        })
        .to_string();
        let r = new_request("claude", "s1", &HookInput::from_json(&raw).unwrap(), T0);
        for (name, text) in [("summary", &r.summary), ("label_hint", &r.label_hint), ("tool_name", &r.tool_name)] {
            assert_eq!(text.chars().count(), 200, "{name}");
            assert!(text.ends_with('…'), "{name}");
        }
        // Exactly 200 chars is kept whole.
        let exact = "z".repeat(200);
        let input = HookInput { tool_name: Some(exact.clone()), ..HookInput::default() };
        assert_eq!(new_request("claude", "s1", &input, T0).tool_name, exact);
    }

    fn row(key: &str, status: crate::session::Status) -> Row {
        Row {
            key: key.to_owned(),
            label: String::new(),
            project: None,
            status,
            quiet: false,
            updated_ms: T0,
            catch_up: crate::rows::CatchUp {
                original_ask: None,
                current_ask: None,
                files_since_seen: Vec::new(),
                commands_since_seen: Vec::new(),
                unseen: false,
            },
            collisions: Vec::new(),
        }
    }

    fn req(provider: &str, session_key: &str, n: i64) -> Request {
        let mut r = new_request(provider, session_key, &bash("ls"), T0 + n);
        r.id = format!("{provider}__{session_key}__{n}");
        r
    }

    #[test]
    fn pending_for_rows_keeps_waiting_and_running_drops_ended_hides_missing() {
        use crate::session::Status;
        let rows = [
            row("claude__wait", Status::WaitingApproval),
            row("claude__run", Status::Running),
            row("claude__done", Status::Completed),
            row("claude__idle", Status::Idle),
            row("claude__err", Status::Error),
            row("codex__wait", Status::WaitingApproval),
        ];
        let pending = vec![
            req("claude", "wait", 1),
            req("claude", "run", 2),
            req("claude", "done", 3),
            req("claude", "gone", 4),
            req("codex", "wait", 5),
            req("claude", "idle", 6),
            req("claude", "err", 7),
            req("claude", "wait", 8),
        ];
        let (keep, drop) = pending_for_rows(pending, &rows);
        let kept: Vec<&str> = keep.iter().map(|r| r.id.as_str()).collect();
        // A Running row may be a subagent or parallel tool call reporting
        // under the parent's session while the request is still live.
        assert_eq!(kept, ["claude__wait__1", "claude__run__2", "codex__wait__5", "claude__wait__8"]);
        // A missing row (gone__4) is neither kept nor deleted.
        assert_eq!(drop, ["claude__done__3", "claude__idle__6", "claude__err__7"]);
    }

    #[test]
    fn pending_for_rows_matches_provider_and_key_exactly() {
        use crate::session::Status;
        // Same session key under another provider is a different session,
        // so claude__s1 has no row: hidden, not deleted.
        let rows = [row("codex__s1", Status::Completed)];
        let (keep, drop) = pending_for_rows(vec![req("claude", "s1", 1)], &rows);
        assert!(keep.is_empty() && drop.is_empty());
        let (keep, drop) = pending_for_rows(vec![req("codex", "s1", 2)], &rows);
        assert!(keep.is_empty());
        assert_eq!(drop, ["codex__s1__2"]);
        let (keep, drop) = pending_for_rows(Vec::new(), &rows);
        assert!(keep.is_empty() && drop.is_empty());
    }

    #[test]
    fn wait_returns_none_when_request_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        let path = req_path(dir, &r.id);
        let (got, sleeps) = run_wait(dir, posted, T0, true, |_| {
            let _ = fs::remove_file(&path);
        });
        assert_eq!((got, sleeps), (None, 1));
        assert!(!dec_path(dir, &r.id).exists());
    }

    #[test]
    fn wait_refreshes_request_mtime_each_poll() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_alive(dir, T0).unwrap();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        let posted = post(dir, &r).unwrap();
        let path = req_path(dir, &r.id);
        let id = r.id.clone();
        let mut polls = 0;
        let (got, _) = run_wait(dir, posted, T0, true, |_| {
            polls += 1;
            if polls > 1 {
                // The poll after the last sleep refreshed the mtime.
                assert!(!older_than(&path, Duration::from_secs(2)), "poll {polls}");
            }
            if polls == 3 {
                decide(dir, &id, Behavior::Allow).unwrap();
            } else {
                set_age(&path, Duration::from_secs(10));
            }
        });
        assert_eq!(got, Some(Behavior::Allow));
        assert_eq!(polls, 3);
    }

    #[test]
    fn list_pending_deletes_request_whose_hook_stopped_refreshing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let live = new_request("claude", "s1", &bash("ls"), T0);
        let dead = new_request("claude", "s2", &bash("ls"), T0);
        post(dir, &live).unwrap();
        post(dir, &dead).unwrap();
        decide(dir, &dead.id, Behavior::Allow).unwrap();
        // A hook that paused 4 s is still alive; 6 s without a refresh is gone.
        assert_eq!(HOOK_STALE_MS, 5000);
        set_age(&req_path(dir, &live.id), Duration::from_millis(4000));
        set_age(&req_path(dir, &dead.id), Duration::from_millis(6000));
        assert_eq!(list_pending(dir, T0), vec![live.clone()]);
        assert!(!req_path(dir, &dead.id).exists());
        assert!(!dec_path(dir, &dead.id).exists());
        assert!(req_path(dir, &live.id).exists());
    }

    #[test]
    fn remove_alive_only_removes_own_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        remove_alive(dir).unwrap();
        write_alive(dir, T0).unwrap();
        remove_alive(dir).unwrap();
        assert!(!dir.join(ALIVE_FILE).exists());
        let other = std::process::id().wrapping_add(1);
        fs::write(dir.join(ALIVE_FILE), format!("{T0} {other}")).unwrap();
        remove_alive(dir).unwrap();
        assert!(dir.join(ALIVE_FILE).exists(), "another instance's file stays");
        fs::write(dir.join(ALIVE_FILE), "garbage").unwrap();
        remove_alive(dir).unwrap();
        assert!(dir.join(ALIVE_FILE).exists());
    }

    #[test]
    fn remove_deletes_request_and_decision_and_skips_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let r = new_request("claude", "s1", &bash("ls"), T0);
        post(d, &r).unwrap();
        decide(d, &r.id, Behavior::Allow).unwrap();
        let outside = d.join("x.req.json");
        fs::write(&outside, b"{}").unwrap();
        remove(d, &[r.id.clone(), "../x".to_owned(), "missing".to_owned()]);
        assert!(!req_path(d, &r.id).exists());
        assert!(!dec_path(d, &r.id).exists());
        assert!(outside.exists());
    }
}
