# Approve from Agent Pin: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a Claude Code or Codex session asks for permission, its Agent Pin row shows what it wants to run and offers **Allow** and **Deny** buttons. The agent continues without the user going back to the terminal.

**Architecture:** The existing `agent-pin hook <provider> PermissionRequest` process records the waiting state as it does today. It then hands the request to the running app through files in the state dir and waits for a decision file, polling every 200 ms. It prints the documented decision JSON on stdout. If the app isn't running, approvals are off, the wait times out, or anything fails, it prints nothing and exits 0. Claude Code and Codex then fall back to their normal terminal prompt. Nothing listens on the network.

**Tech Stack:** Rust (agent-pin-core, Tauri 2 app), vanilla JS (view.js + node:test). No new dependencies.

**Spec:**
- **Claude Code hooks reference** (https://code.claude.com/docs/en/hooks), PermissionRequest:
  - input: `tool_name`, `tool_input`, `session_id`, `cwd`, optional `permission_suggestions`;
  - output: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`, or `"deny"` with `"message"`;
  - a hook `allow` can't override a `deny` rule;
  - async hooks can't return decisions;
  - the default command-hook timeout is 600 s.
- **Codex hooks** (https://learn.chatgpt.com/docs/hooks): the same allow/deny shape. "If no matching hook decides, Codex uses normal approval flow." `updatedInput`/`updatedPermissions`/`interrupt` aren't allowed.
- **User request:** Allow/Deny from the overlay, as in suren19113/agent-huddle. Push and merge when done.

## Global Constraints

- **The hook never harms an agent.**
  - Exit code is always 0.
  - On every non-decision path, stdout is empty: app not running, approvals off, timeout, I/O error, malformed decision.
  - It never prints a partial JSON object.
  - The only decisions it ever emits are `allow` and `deny`, and only ones the user clicked.
- **App liveness:** the app writes `<state_dir>/app.alive`, containing its unix-ms timestamp and pid, every 1 s while running. The hook treats the app as alive only if that timestamp is < 3000 ms old. Otherwise it returns at once with no output.
- **Approvals switch:** `<state_dir>/approvals.off` exists means approvals are disabled, and the hook returns immediately. Default is enabled.
- **Request file:** `<state_dir>/approvals/<id>.req.json`
  - `id` = `<provider>__<session_key>__<unix_ms>__<4 random hex>`, matching `[A-Za-z0-9_-]+`.
  - Contents: `{"id","provider","session_key","label_hint","tool_name","summary","detail","created_ms","deadline_ms"}`.
  - `summary`: the existing pending text, e.g. `Bash: rm -rf build`, ≤ 200 chars.
  - `detail`: pretty `tool_input` JSON, ≤ 4000 chars, cleaned of control chars.
  - Written atomically (temp file + rename).
- **Decision file:** `<state_dir>/approvals/<id>.dec.json`, containing `{"behavior":"allow"|"deny"}`. Written atomically by the app. The hook deletes both the request and decision files when it finishes, on every path.
- **Wait:** the hook polls every 200 ms until it finds a decision or `deadline_ms = created_ms + 590_000`. Each poll re-checks liveness: if the app dies, the hook returns with no output.
- **Installed hook timeout:** for PermissionRequest only, `"timeout": 600` and no `"async"` for Claude; `"timeout": 600` for Codex. Every other event keeps its current entries. Reinstalling migrates an old async PermissionRequest entry.
- **Deny message:** `"Denied from Agent Pin"`.
- **Stale requests:** the app ignores and deletes request files whose `deadline_ms` has passed, or whose session row is no longer WaitingApproval.
- **Platforms:** macOS, Windows, Linux. Use files only, no sockets or signals.
- **Commits:** on local branch `approvals`. Each ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The controller pushes and merges after CI passes.

## Review Focus

1. The app is not running, or crashes mid-wait: the agent gets its normal terminal prompt within about 3 s, and nothing is printed. Tests: Task 1 and Task 2.
2. The user answers in the terminal instead: the hook is killed or keeps waiting, and the card must not keep offering a decision that no longer matters. Tests: Task 3 (stale cleanup when the row isn't WaitingApproval) and Task 1 (deadline).
3. Two sessions wait at once: each card's buttons resolve only that session's request. Tests: Task 1 (ids) and Task 4 (view mapping).
4. A malformed or foreign decision file, e.g. `{"behavior":"always"}` or garbage: no output, and the normal prompt appears. Test: Task 1.
5. Reinstalling over an existing install converts the old async PermissionRequest entry to a sync one with a 600 s timeout, and keeps the user's hooks. Test: Task 2.

---

### Task 1: Core approvals module

**Files:**
- Create: `crates/agent-pin-core/src/approvals.rs`
- Modify: `crates/agent-pin-core/src/lib.rs` (`pub mod approvals`)

**Interfaces:**
- Produces:
  - `pub struct Request { pub id, provider, session_key, tool_name, summary, detail: String; pub created_ms, deadline_ms: i64 }`, with serde.
  - `pub enum Behavior { Allow, Deny }`, serde lowercase.
  - `pub const POLL_MS: u64 = 200; pub const ALIVE_MAX_AGE_MS: i64 = 3000; pub const WAIT_MS: i64 = 590_000;`
  - `pub fn write_alive(dir: &Path, now_ms: i64) -> io::Result<()>`
  - `pub fn app_alive(dir: &Path, now_ms: i64) -> bool`
  - `pub fn enabled(dir: &Path) -> bool`
  - `pub fn set_enabled(dir: &Path, on: bool) -> io::Result<()>`
  - `pub fn new_request(provider, session_key, input: &HookInput, now_ms) -> Request`
  - `pub fn post(dir: &Path, r: &Request) -> io::Result<()>`
  - `pub fn list_pending(dir: &Path, now_ms: i64) -> Vec<Request>`: skips expired requests and deletes them.
  - `pub fn decide(dir: &Path, id: &str, b: Behavior) -> io::Result<()>`: rejects an id outside `[A-Za-z0-9_-]` and a missing request.
  - `pub fn wait(dir: &Path, r: &Request, now: impl FnMut() -> i64, sleep: impl FnMut(Duration)) -> Option<Behavior>`: polls, re-checks `app_alive`, and stops at the deadline. It always removes the request and decision files before returning.
  - `pub fn decision_json(b: Behavior) -> String`: exactly the documented objects.

- [ ] **Step 1: Write failing tests** with a fake clock and sleep, so no test takes real minutes:
  - `alive_fresh_and_stale`
  - `enabled_toggle`
  - `post_then_list_roundtrip`
  - `expired_request_listed_never_and_deleted`
  - `decide_writes_decision_wait_returns_it`
  - `wait_times_out_none_and_cleans_files`
  - `wait_returns_none_when_app_dies_midwait`
  - `malformed_decision_is_none`, using `{"behavior":"always"}` and `garbage`
  - `two_requests_independent`
  - `decide_rejects_bad_id_and_missing`
  - `decision_json_exact`, which asserts the exact strings for allow and for deny with the message
  - `detail_truncated_and_control_chars_cleaned`
- [ ] **Step 2:** Run `cargo test --manifest-path crates/agent-pin-core/Cargo.toml approvals`. Expected: FAIL.
- [ ] **Step 3:** Implement. Reuse the store's atomic-write helper if one exists; otherwise use temp file + rename. Create the `approvals/` dir on demand.
- [ ] **Step 4:** Run the tests and clippy `-D warnings`. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(core): file-based approval requests and decisions`.

### Task 2: Hook path and installer

**Files:**
- Modify: `crates/agent-pin-core/src/hook.rs` (the PermissionRequest branch), `crates/agent-pin-core/src/install.rs` (PermissionRequest entries), `apps/desktop/src-tauri/src/main.rs` (print the returned decision on stdout)

**Interfaces:**
- Produces:
  - `pub fn run_hook(...) -> Result<Option<String>, String>`. It returns `Some(json)` only for a clicked decision.
  - All other events behave as today and return `Ok(None)`.
  - `main.rs` prints `Some` with a trailing newline, then exits 0.
  - The existing 2 s stdin deadline is unchanged. The approval wait starts only after stdin has been read and the state recorded.
- **Behavior:**
  - On PermissionRequest, after `update_session` (the existing WaitingApproval logic), the hook calls `approvals::enabled` and `approvals::app_alive`.
  - If both are true, it posts a request and waits.
  - On Allow or Deny, it returns the JSON. Otherwise it returns `None`.

- [ ] **Step 1: Write failing tests:**
  - `permission_request_without_app_returns_none_fast`: completes in under 1 s.
  - `permission_request_with_decision_returns_json`: simulate the app with a helper thread that writes alive every 200 ms and decides the posted request.
  - `permission_request_disabled_returns_none`
  - `other_events_never_print`
  - Installer: `claude_permission_request_is_sync_with_timeout_600`, `codex_permission_request_timeout_600`, `reinstall_migrates_async_permission_entry`, `other_events_unchanged`.
  - CLI: `hook_prints_decision_only_when_decided`, through `run()`.
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the full core suite with clippy. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(hook): wait for an Agent Pin decision on PermissionRequest`.

### Task 3: App side

**Files:**
- Modify: `apps/desktop/src-tauri/src/lib.rs`

**Interfaces:**
- New Tauri commands:
  - `read_approvals() -> Vec<approvals::Request>`. Returns only requests whose session row is currently WaitingApproval. It deletes the others, which covers Review Focus 2.
  - `decide(id: String, allow: bool) -> Result<(), String>`
  - `approvals_enabled() -> bool`
  - `set_approvals_enabled(on: bool) -> Result<(), String>`
- **Heartbeat:** a thread spawned in `setup` calls `approvals::write_alive` every 1 s and never panics. On normal exit, delete `app.alive`, best effort.
- **Pure core helper:** `approvals::pending_for_rows(pending: Vec<Request>, rows: &[Row]) -> (Vec<Request>, Vec<String /*ids to delete*/>)`. Unit-test it in core.

- [ ] **Step 1:** Write failing core tests for `pending_for_rows`: keep a request whose row key is waiting; drop one whose row is running, missing, or completed.
- [ ] **Step 2:** Implement the helper and wire up the commands and the heartbeat.
- [ ] **Step 3:** Run the core tests and `GOOS`-equivalent checks. The app crate is checked in CI (app-check job). Run `cargo check` locally only if webkit is available; otherwise say it is CI-only.
- [ ] **Step 4:** Commit with `feat(app): approval commands and liveness heartbeat`.

### Task 4: Frontend

**Files:**
- Modify: `apps/desktop/web/view.js`, `view.test.js`, `main.js`, `styles.css`, `index.html`

**Interfaces:**
- `view.js` exports `approvalFor(rowKey, approvals) -> Request|null`. It matches by `provider__session_key`, taking the newest by `created_ms`.
- `approvalLines(req) -> string[]`: `[summary, ...first 6 lines of detail]`.
- **Row behavior:** a WaitingApproval row with an approval shows the summary plus **Allow** and **Deny** buttons.
  - Clicking a button calls `decide`, disables both buttons until the next poll, and never starts a drag.
  - The detail is visible in the row's card.
- **Setup panel:** an "Approve from Agent Pin" checkbox wired to `approvals_enabled`/`set_approvals_enabled`.
- **Safety:** text is set with `textContent` only.
- **Polling:** `read_approvals` is polled with `read_rows` (800 ms).

- [ ] **Step 1:** Write failing node tests:
  - `approvalFor` matching and the newest-wins rule;
  - `approvalFor` returns null for another session's request;
  - `approvalLines` truncation.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Run `node --test apps/desktop/web/*.test.js`. Expected: PASS.
- [ ] **Step 4:** Commit with `feat(web): Allow and Deny on waiting rows`.

### Task 5: README

**Files:**
- Modify: `README.md`

Add an "Approve from Agent Pin" section that covers:
- **How it works:** files in `~/.agent-pin`, no network.
- **What the terminal does:** in an interactive session it still shows its own prompt, and whichever answers first wins. Headless `claude -p` runs and background sessions wait for the click.
- **Timing:** falls back to the terminal prompt after about 10 minutes, or at once if Agent Pin isn't running.
- **Limits:** an Allow never overrides a `deny` rule.
- **Off switch:** how to turn it off (the setup panel checkbox), and that turning it off makes headless runs fall back to their normal denial.
- **Codex:** supported, per its hooks docs.

Claim only what the docs or tests support, and say which behaviours were not tested end to end.

- [ ] Commit with `docs: approve from Agent Pin`.
