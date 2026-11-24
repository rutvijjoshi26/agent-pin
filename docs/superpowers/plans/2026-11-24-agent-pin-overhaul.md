# Agent Pin Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make agent-pin safe for the agents it watches, show one row per session, and add two features no comparable tool has: a catch-up card and cross-session collision warnings. The app must run on macOS, Windows and Linux.

**Architecture:** A new pure-Rust crate, `crates/agent-pin-core`, has no Tauri dependency. It holds hook parsing, the session state machine, row aggregation, collision detection, the per-session file store and the hook installer, and it is fully unit-tested on Linux. The Tauri app (`apps/desktop/src-tauri`) becomes a thin shell:
- argv dispatch for the `hook`, `install-hooks` and `reset` subcommands;
- five Tauri commands.

The vanilla JS frontend splits into a pure `view.js`, tested with `node --test`, and DOM glue in `main.js`.

**Tech Stack:**
- Rust 1.89+ (`std::fs::File::lock`)
- serde and serde_json, with `preserve_order`
- tempfile (dev-only)
- Tauri 2
- vanilla ES modules
- Node 20 `node:test`

**Spec:** this plan's "Decisions" section, plus `docs/refactor-audit-2026-11-23/` (`smells.md`, `target-architecture.md`) and `/tmp/claude-1000/-home-ec2-user/53eaa5bf-eb58-4a19-ab41-a561cb7c3708/scratchpad/agent-pin-simplify-survey.md`.

## Decisions (from the user)

- The user uses none of these: the Swift app, the Ruby scripts, or `events.jsonl` outside the overlay. Delete them all.
- One row per session.
- Antigravity: "maybe". Accept any provider id and ship no Antigravity-specific code.
- Targets: macOS, Windows and Linux.
- No questions round. Implement everything in this plan on a local branch, `overhaul`, and do not push.

## Global Constraints

- **The hook never harms an agent.** `agent-pin hook …` always exits 0, never reads stdin for more than 2 s, and prints errors only to stderr.
- **Claude hook entries are installed with `"async": true`.** Codex entries are installed without `async`; that is unverified for Codex.
- **The installer protects the user's config.**
  - Back up the file to `<file>.agent-pin-backup-<unix_secs>` before writing.
  - Write atomically (temp file in the same directory, then rename).
  - Preserve key order (`serde_json` feature `preserve_order`).
  - Remove only agent-pin command objects and never a user's command. Drop a matcher group only when it becomes empty.
  - Only touch providers whose config directory already exists: `~/.claude/` for Claude, `~/.codex/` for Codex.
- **State directory:** `AGENT_PIN_HOME` if set, otherwise `<home>/.agent-pin`. Home is `USERPROFILE` on Windows and `HOME` elsewhere. Resolve it in one function, `store::state_dir()`, which every reader and writer uses.
- **Session files:** `<state_dir>/sessions/<provider>__<session_key>.json`. Each file is under 64 KB. Free text is truncated to 200 chars, and lists are capped at the last 20 items.
- **Provider ids** match `^[a-z0-9][a-z0-9-]{0,31}$`. Anything else becomes `agent`. Display names: `claude`→"Claude", `codex`→"Codex", `antigravity`→"Antigravity"; any other id is capitalized as-is.
- **Timestamps are Unix milliseconds** (`i64`) everywhere.
- **No new runtime dependencies** beyond serde and serde_json. No frontend framework. No network calls.
- **Commits** are local on branch `overhaul`, one per task, and end with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Empty or garbage stdin** for the hook, or a missing `session_id`: the hook still exits 0, and the session key falls back to `default`. Test owner: Task 1 / Task 5.
2. **Paths with spaces or backslashes** in the exe path, as on Windows (`C:\Program Files\Agent Pin\agent-pin.exe`): the installed command still runs. Test owner: Task 6.
3. **A user's own hook sharing a matcher group** with an agent-pin hook: the user's hook survives install and reinstall, and reinstalling twice yields exactly one agent-pin entry per event. Test owner: Task 6.
4. **Two hooks for the same session firing at once** (Claude runs matching hooks in parallel): neither update is lost. Test owner: Task 4.
5. **A huge prompt or tool input** (a 1 MB command string): the session file stays under 64 KB. Test owner: Task 2.

---

### Task 1: Core crate, provider ids, hook input parsing

**Files:**
- Create: `crates/agent-pin-core/Cargo.toml`, `crates/agent-pin-core/src/lib.rs`, `crates/agent-pin-core/src/input.rs`
- Test: `crates/agent-pin-core/src/input.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub enum HookKind { SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, Stop, StopFailure, SessionEnd, Other }`
  - `HookKind::parse(name: &str) -> HookKind`, which is case-sensitive, matches Claude's event names, and maps unknown names to `Other`.
  - `pub struct HookInput { pub session_id: Option<String>, pub cwd: Option<String>, pub prompt: Option<String>, pub tool_name: Option<String>, pub file_path: Option<String>, pub command: Option<String> }`
  - `HookInput::from_json_lossy(raw: &str) -> HookInput`:
    - Never fails; empty or invalid input gives all `None`.
    - `file_path` comes from `tool_input.file_path`, falling back to `tool_input.notebook_path`.
    - `command` comes from `tool_input.command`.
  - `pub fn normalize_provider(raw: &str) -> String`
  - `pub fn display_name(provider: &str) -> String`
  - `pub fn truncate(s: &str, max_chars: usize) -> String`, which cuts at a char boundary and appends `…` when cut.

- [ ] **Step 1:** Install the toolchain if it's missing: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy`, then `source ~/.cargo/env`. Expected: `cargo --version` ≥ 1.89.
- [ ] **Step 2:** Create the crate. Set `rust-version = "1.89"`, `edition = "2021"`, deps `serde = { version = "1", features = ["derive"] }` and `serde_json = { version = "1", features = ["preserve_order"] }`, and dev-dep `tempfile = "3"`. Write the failing tests:
  - `parses_claude_pretooluse`: `{"session_id":"s1","cwd":"/r","tool_name":"Edit","tool_input":{"file_path":"/r/a.rs"}}` gives `session_id=Some("s1")`, `tool_name=Some("Edit")` and `file_path=Some("/r/a.rs")`.
  - `parses_bash_command`: `tool_input.command="cargo test"` gives `command=Some("cargo test")`.
  - `empty_and_garbage_input_is_all_none`, for `""` and `"{not json"`.
  - `hook_kind_unknown_is_other`: `HookKind::parse("Notification") == Other`.
  - `provider_normalization`:
    - `"Claude"` gives `"claude"`;
    - `"my agent!"` gives `"agent"`;
    - `"antigravity"` is kept.
  - `display_names`:
    - `"codex"` gives `"Codex"`;
    - `"kimi"` gives `"Kimi"`.
  - `truncate_char_boundary`: `truncate("héllo wörld", 4) == "héll…"`.
- [ ] **Step 3:** Run `cargo test -p agent-pin-core` from `crates/agent-pin-core`. Expected: FAIL (items missing).
- [ ] **Step 4:** Implement the items in `input.rs` and re-export them from `lib.rs`.
- [ ] **Step 5:** Run the tests. Expected: PASS. Run `cargo clippy -- -D warnings`. Expected: clean.
- [ ] **Step 6:** Commit with `feat(core): add agent-pin-core crate with hook input parsing`.

### Task 2: Session state machine (`apply`)

**Files:**
- Create: `crates/agent-pin-core/src/session.rs`
- Test: same file

**Interfaces:**
- Consumes: `HookKind`, `HookInput`, `truncate` (Task 1).
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)] #[serde(rename_all = "snake_case")] pub enum Status { Idle, Running, WaitingApproval, Completed, Error }`
  - `pub struct CommandRun { pub command: String, pub ok: bool, pub at_ms: i64 }`
  - `pub struct FileTouch { pub path: String, pub at_ms: i64 }`
  - The session record:

    ```text
    pub struct SessionState {
        pub provider: String, pub session_key: String, pub cwd: Option<String>,
        pub status: Status, pub updated_ms: i64, pub seen_ms: i64,
        pub original_ask: Option<String>, pub last_ask: Option<String>,
        pub pending: Option<String>, pub files: Vec<FileTouch>, pub commands: Vec<CommandRun>
    }
    ```

    All fields are serde-serializable, and `seen_ms` defaults to 0.
  - `pub fn apply(prev: Option<SessionState>, provider: &str, kind: HookKind, input: &HookInput, now_ms: i64) -> Option<SessionState>`. `None` means "delete this session".
  - `pub fn session_key(input: &HookInput) -> String`: `session_id` sanitized to `[A-Za-z0-9_-]` (max 64 chars), or `"default"`.

**Transition table.** Every event except `Other` sets `updated_ms = now_ms`. `Other` changes nothing and returns `prev`.

| Kind | Status | Other effects |
|---|---|---|
| SessionStart | Idle | |
| UserPromptSubmit | Running | `original_ask` is set only if `None`; `last_ask` is set (both truncated to 200) |
| PreToolUse | Running | If `tool_name ∈ {Edit, Write, MultiEdit, NotebookEdit}` and `file_path` is set, push `FileTouch`. The path is made absolute by joining with `cwd` when relative. |
| PermissionRequest | WaitingApproval | `pending = "<tool_name>: <command or file_path>"`, truncated to 200 |
| PostToolUse | Running | Clear `pending`. If `tool_name == "Bash"` and `command` is set, push `CommandRun{ok: true}`. |
| PostToolUseFailure | Running | Clear `pending`. If `command` is set, push `CommandRun{ok: false}`. This is not an error status. |
| Stop | Completed | Clear `pending` |
| StopFailure | Error | |
| SessionEnd | — | Return `None` |

`files` and `commands` keep the last 20. `cwd` is updated whenever the input has one.

- [ ] **Step 1:** Write the failing tests:
  - `session_start_is_idle`
  - `first_prompt_becomes_original_ask_and_is_kept`: two prompts give `original_ask` = the first and `last_ask` = the second.
  - `permission_request_waits_with_pending_text`: Bash `rm -rf build` gives `WaitingApproval` with `pending = "Bash: rm -rf build"`.
  - `post_tool_use_clears_pending_and_records_ok_command`
  - `post_tool_use_failure_is_running_not_error`, with `ok == false`.
  - `edit_records_absolute_path`: `cwd=/r` and `file_path=src/a.rs` give `/r/src/a.rs`.
  - `stop_failure_is_error`
  - `session_end_deletes`: returns `None`.
  - `other_event_changes_nothing`
  - `lists_capped_at_20`
  - `huge_command_is_truncated`: a 1 MB command gives a `serde_json::to_string(&state)` length under 64 KB.
  - `session_key_fallback_and_sanitize`: `None` gives `"default"`; `"a/b c"` gives `"abc"`.
- [ ] **Step 2:** Run `cargo test -p agent-pin-core session`. Expected: FAIL.
- [ ] **Step 3:** Implement `session.rs`.
- [ ] **Step 4:** Run the tests and clippy. Expected: PASS and clean.
- [ ] **Step 5:** Commit with `feat(core): session state machine for hook events`.

### Task 3: Rows, catch-up card, collisions

**Files:**
- Create: `crates/agent-pin-core/src/rows.rs`
- Test: same file

**Interfaces:**
- Consumes: `SessionState`, `Status`, `display_name` (Tasks 1–2).
- Produces:
  - `pub const COMPLETED_VISIBLE_AFTER_SEEN_MS: i64 = 600_000; pub const IDLE_VISIBLE_MS: i64 = 120_000; pub const RUNNING_QUIET_MS: i64 = 1_800_000; pub const COLLISION_WINDOW_MS: i64 = 600_000;`
  - `#[derive(Serialize)] pub struct CatchUp { pub original_ask: Option<String>, pub current_ask: Option<String>, pub files_since_seen: Vec<String>, pub commands_since_seen: Vec<CommandRun>, pub unseen: bool }`
  - `#[derive(Serialize)] pub struct Collision { pub path: String, pub other_label: String }`
  - The row returned to the frontend:

    ```text
    #[derive(Serialize)] pub struct Row {
        pub key: String, pub label: String, pub project: Option<String>,
        pub status: Status, pub quiet: bool, pub updated_ms: i64,
        pub catch_up: CatchUp, pub collisions: Vec<Collision>
    }
    ```

    - `key` is `"<provider>__<session_key>"`.
    - `label` is the display name plus the project name, e.g. `"Claude · web-app"`.
    - `project` is the cwd's basename.
  - `pub fn build_rows(sessions: &[SessionState], now_ms: i64) -> Vec<Row>`

**Rules:**
- **Visibility:**
  - WaitingApproval is always shown and never expires.
  - Error is shown until seen, then for `COMPLETED_VISIBLE_AFTER_SEEN_MS` after `seen_ms`.
  - Completed follows the same rule as Error.
  - Running is always shown. It has `quiet = true` when `now - updated > RUNNING_QUIET_MS`.
  - Idle is shown only while `now - updated < IDLE_VISIBLE_MS`.
- **Sort:** WaitingApproval, then Error, Running, Completed, Idle. Within a status, newest `updated_ms` first.
- **Catch-up:**
  - `files_since_seen` holds the deduplicated paths with `at_ms > seen_ms`.
  - `commands_since_seen` holds the commands with `at_ms > seen_ms`.
  - `current_ask = pending`.
  - `unseen = updated_ms > seen_ms`.
- **Collisions:** for each row, report every path that this session touched within `COLLISION_WINDOW_MS` of `now` and that a different session touched within the same window. `other_label` is that session's label.

- [ ] **Step 1:** Write the failing tests:
  - `waiting_never_expires`: updated 3 days ago and still shown.
  - `sort_order_waiting_first`
  - `completed_hidden_ten_minutes_after_seen`
  - `completed_kept_while_unseen`
  - `idle_hidden_after_two_minutes`
  - `running_marked_quiet_after_30_minutes`
  - `catch_up_counts_only_since_seen`
  - `collision_reported_on_both_sessions`: A and B both edit `/r/a.rs` within 5 minutes, so each row lists the other.
  - `no_collision_outside_window_or_same_session`
  - `label_uses_project_basename`: Windows path `C:\code\web-app` gives `"Claude · web-app"`.
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3:** Implement `rows.rs`.
- [ ] **Step 4:** Run the tests and clippy. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(core): session rows with catch-up and collision warnings`.

### Task 4: Store

**Files:**
- Create: `crates/agent-pin-core/src/store.rs`
- Test: same file (with `tempfile`)

**Interfaces:**
- Consumes: `SessionState`, `apply`, `session_key` (Task 2).
- Produces:
  - `pub fn state_dir() -> Option<PathBuf>`, with `pub fn state_dir_from(env_home: Option<OsString>, user_home: Option<PathBuf>) -> Option<PathBuf>` as the testable core.
  - `pub fn update_session(dir: &Path, provider: &str, key: &str, f: impl FnOnce(Option<SessionState>) -> Option<SessionState>) -> io::Result<()>`
    - Takes an exclusive `File::lock()` on `<dir>/sessions/<provider>__<key>.lock`.
    - Reads the JSON, or `None` if it's missing or corrupt.
    - Writes `Some` atomically through a temp file and rename, or removes the file on `None`.
  - `pub fn list_sessions(dir: &Path) -> Vec<SessionState>`, which skips unreadable files.
  - `pub fn mark_seen(dir: &Path, row_key: &str, now_ms: i64) -> io::Result<()>`, which sets `seen_ms` under the same lock.
  - `pub fn reset(dir: &Path) -> io::Result<()>`, which removes `<dir>/sessions`.

- [ ] **Step 1:** Write the failing tests:
  - `state_dir_prefers_agent_pin_home`
  - `state_dir_defaults_to_home_dot_agent_pin`
  - `update_creates_then_updates`
  - `none_removes_file`
  - `corrupt_file_treated_as_missing`
  - `concurrent_updates_not_lost`: 8 threads each push a distinct `FileTouch` to the same session, and the final state has all 8.
  - `mark_seen_sets_seen_ms`
  - `reset_removes_sessions_dir`
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3:** Implement `store.rs`.
- [ ] **Step 4:** Run the tests and clippy. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(core): per-session file store with locking and atomic writes`.

### Task 5: Hook entry point that always exits 0

**Files:**
- Create: `crates/agent-pin-core/src/hook.rs`
- Test: same file

**Interfaces:**
- Consumes: Tasks 1, 2 and 4.
- Produces:
  - `pub fn run_hook(args: &[String], stdin: impl Read, state_dir: Option<PathBuf>, now_ms: i64) -> Result<(), String>`
  - `args` is `[provider, event_name]`.
  - Reads stdin up to 1 MB. Callers wrap the real stdin so that reading stops after 2 s.
  - Errors are returned, never panicked. `main.rs` prints them and still exits 0.

- [ ] **Step 1:** Write the failing tests:
  - `writes_session_from_stdin`
  - `missing_args_is_err_not_panic`
  - `garbage_stdin_still_writes_default_session`
  - `unwritable_dir_is_err_not_panic`: `state_dir` points at a regular file.
  - `session_end_removes_file`
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3:** Implement `hook.rs`.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(core): non-blocking hook entry point`.

### Task 6: Hook installer

**Files:**
- Create: `crates/agent-pin-core/src/install.rs`, `crates/agent-pin-core/tests/fixtures/*.json`
- Test: `crates/agent-pin-core/src/install.rs`

**Interfaces:**
- Produces:
  - `pub struct Provider { pub id: &'static str, pub config_rel: &'static str, pub events: &'static [&'static str], pub async_hooks: bool }`
  - `pub const PROVIDERS: &[Provider]`:
    - `claude`: `.claude/settings.json`, events `[SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, Stop, StopFailure, SessionEnd]`, `async_hooks: true`.
    - `codex`: `.codex/hooks.json`, events `[SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, Stop]`, `async_hooks: false`.
  - `pub fn hook_command(exe: &Path, provider: &str, event: &str) -> String`. It produces `"<exe>" hook <provider> <event>`: the exe is wrapped in double quotes, and inner `"` is escaped. Backslashes are not doubled.
  - `pub fn merge(config: &mut serde_json::Value, provider: &Provider, exe: &Path)`. For every event, it:
    - removes agent-pin command objects inside every group, detected by `is_agent_pin_command`;
    - drops groups whose `hooks` array became empty;
    - appends one group per event, `{"matcher":"*","hooks":[{"type":"command","command":…,"async":true?}]}`. Omit `matcher` for `UserPromptSubmit` and `Stop`.

    A non-object `hooks` is replaced with `{}`.
  - `pub fn is_agent_pin_command(cmd: &str) -> bool`: contains `agent-pin-status`, or contains `agent-pin` together with ` hook `.
  - `pub fn install(home: &Path, exe: &Path, now_secs: i64) -> Result<Vec<String>, String>`
    - Returns the provider ids it installed.
    - Skips a provider whose config directory doesn't exist.
    - Fails, writing nothing, on unparseable JSON.
    - Backs up, then writes atomically.
  - `pub fn installed(home: &Path) -> Vec<(String, bool)>`, one entry per provider whose config directory exists.

- [ ] **Step 1:** Write the failing tests, using fixture files:
  - `keeps_user_hook_in_shared_group`: the fixture group's hooks are `[userCmd, oldAgentPinCmd]`. After merge, the user's command is still there.
  - `reinstall_is_idempotent`: merging twice gives exactly one agent-pin command per event.
  - `removes_legacy_ruby_entries`: commands containing `agent-pin-status` are removed.
  - `preserves_key_order`: the top-level keys `["model","hooks","permissions"]` keep that order.
  - `claude_entries_are_async`
  - `codex_entries_not_async`
  - `windows_exe_path_quoted`: `C:\Program Files\Agent Pin\agent-pin.exe` gives `"C:\Program Files\Agent Pin\agent-pin.exe" hook claude Stop`.
  - `skips_missing_provider_dir`
  - `invalid_json_aborts_without_writing`
  - `backup_written`
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3:** Implement `install.rs`.
- [ ] **Step 4:** Run the tests and clippy. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(core): safe hook installer`.

### Task 7: Thin Tauri app and CLI

**Files:**
- Modify:
  - `apps/desktop/src-tauri/Cargo.toml`: remove `[lib] crate-type` (keep `rlib` only by deleting the line), add `agent-pin-core = { path = "../../../crates/agent-pin-core" }`, add `serde_json` with `preserve_order`, and set `description` without Antigravity.
  - `apps/desktop/src-tauri/src/main.rs`
  - `apps/desktop/src-tauri/src/lib.rs` (full rewrite)
  - `apps/desktop/src-tauri/tauri.conf.json`
- Create: `apps/desktop/src-tauri/capabilities/default.json`

**Interfaces:**
- Consumes: everything in Tasks 1–6.
- `main.rs` argv dispatch:
  - `hook <provider> <event>`: run `run_hook` with a stdin reader that gives up after 2 s (read on a thread and join with a timeout), then `exit(0)` no matter what.
  - `install-hooks`: print the installed providers.
  - `reset`
  - Anything else: run the app.
- Tauri commands, the only ones:
  - `read_rows() -> Vec<Row>`
  - `mark_seen(key: String)`
  - `read_integrations() -> Vec<(String,bool)>`
  - `install_integrations() -> Result<Vec<String>,String>`, which keeps the existing macOS translocation guard.
  - `fit_window(width: f64, height: f64)`
  - `start_window_drag`
- Delete: the `events.jsonl` reading and writing, `infer_codex_from_log`, `read_tail`, `normalize`, the size constants except `SCREEN_MARGIN`, and the Antigravity special cases.
- Window:
  - Keep the top-right positioning.
  - Set `skipTaskbar: true` and `visibleOnAllWorkspaces: true` in `tauri.conf.json`.
  - On Linux, if `WAYLAND_DISPLAY` is set, include `wayland: true` in `read_integrations` so the UI can tell the user that always-on-top may not be honored.
- `capabilities/default.json` grants only `core:default`. Dragging uses the custom command.

- [ ] **Step 1:** Implement the changes above.
- [ ] **Step 2:** Run `cargo test -p agent-pin-core` (it must stay green). Then try a local check: `sudo dnf -y install webkit2gtk4.1-devel libsoup3-devel gtk3-devel 2>/dev/null && (cd apps/desktop/src-tauri && cargo check)`.
  - If the packages aren't available, record "app crate compile-verified only in CI (Task 10)" in the task report.
  - Do not claim it compiles.
- [ ] **Step 3:** Commit with `refactor(app): thin Tauri shell over agent-pin-core; add CLI subcommands`.

### Task 8: Frontend rows, catch-up card, collision badge

**Files:**
- Create: `apps/desktop/web/view.js` (pure ES module), `apps/desktop/web/view.test.js`
- Modify: `apps/desktop/web/main.js`, `apps/desktop/web/index.html`, `apps/desktop/web/styles.css`

**Interfaces:**
- Consumes: the `Row` JSON from Task 7.
- Produces, in `view.js`:
  - `export function ageText(updatedMs, nowMs) -> string`: `"just now"` under 60 s, `"Nm ago"`, `"Nh ago"`.
  - `export function catchUpLines(catchUp) -> string[]`, in this order:
    - `"You asked: <original_ask>"`
    - `"Since you looked: N files changed"` when N > 0
    - `"Ran: <command> ✓/✗"` for each command, up to 3, then `"+K more"`
    - `"Now asking: <current_ask>"`
  - `export function collisionText(collisions) -> string|null`: `"⚠ a.rs also edited by Codex · api"`, plus `" (+N)"` when there are several.
  - `export function statusMeta(status) -> {label, symbol, className}`, the existing `STATUS` table.

**`main.js` behavior:**
- Poll `read_rows` every 800 ms and render one row per session.
- A row shows the status dot, `label`, status label or "quiet", and the age.
- Show a small dot when `catch_up.unseen`.
- Show the collision line when present.
- Clicking a row toggles its catch-up card and calls `mark_seen(key)` when opening it.
- Remove every `data-tauri-drag-region` attribute and the duplicate pointerdown guard. Drag stays on the pin background via `start_window_drag`.
- A `ResizeObserver` on `#pin` calls `fit_window` with the content size.
- Check integrations on start and after install, not on an interval.
- The setup panel lists only providers from `read_integrations`, and shows the Wayland note when flagged.

- [ ] **Step 1:** Write the failing tests in `view.test.js` with `node:test`:
  - `ageText` boundaries at 59 s, 60 s and 3600 s;
  - `catchUpLines` ordering and the `+K more` cap;
  - `collisionText` returns null for an empty array;
  - `collisionText` uses the basename of the path;
  - the `statusMeta` fallback for unknown statuses.
- [ ] **Step 2:** Run `node --test apps/desktop/web/`. Expected: FAIL.
- [ ] **Step 3:** Implement `view.js`. Then rewrite `main.js`, `index.html` and `styles.css`. Give `index.html` `<script type="module">`, and add card and collision styles that are readable at a 220 px width.
- [ ] **Step 4:** Run `node --test apps/desktop/web/`. Expected: PASS.
- [ ] **Step 5:** Commit with `feat(web): per-session rows with catch-up card and collision warnings`.

### Task 9: Delete duplicates and leftovers; rewrite the README

**Files:**
- Delete:
  - `Sources/`, `Package.swift`
  - `bin/`, `scripts/`, `hooks/`
  - `apps/desktop/src-tauri/gen/schemas/` (untrack it and add it to `.gitignore`)
  - `apps/desktop/src-tauri/icons/android/`, `apps/desktop/src-tauri/icons/ios/`
  - `apps/desktop/src-tauri/icons/Square*Logo.png`, `apps/desktop/src-tauri/icons/StoreLogo.png`
  - `apps/desktop/web/icon.svg`
  - the `.agent-pin-test/` and `.install-test/` lines in `.gitignore`
- Keep: `apps/desktop/assets/agent-pin-logo.png`, as the source for `tauri icon`, and every icon referenced in `tauri.conf.json`.
- Modify: `README.md`.

**README contents:**
- **What it does:** one row per session, waiting first, the catch-up card and collision warnings.
- **Install:** download a release for macOS, Windows or Linux, then click **Set up** or run `agent-pin install-hooks`.
- **CLI:** `agent-pin hook <provider> <event>`, `install-hooks` and `reset`.
- **How it works:** hooks write `~/.agent-pin/sessions/*.json`. Hooks are async and always exit 0, so the overlay can never block an agent.
- **Adding another agent:** any provider id works. Point the agent's hooks at `agent-pin hook <id> <EventName>`.
- **Linux note:** on Wayland, always-on-top may not be honored.
- **Remove:** every mention of Ruby, Swift and the Codex log fallback, and the "Eight Claude Code lifecycle events" line.

- [ ] **Step 1:** Run the deletions with `git rm` and apply the edits.
- [ ] **Step 2:** Verify:
  - `grep -rn -i -E "ruby|swift|agent-pin-status|codex-tui|antigravity" --exclude-dir=docs --exclude-dir=target --exclude-dir=node_modules .` → no hits except the README sentence on "any provider id (e.g. antigravity)".
  - `cargo test -p agent-pin-core` → PASS.
  - `node --test apps/desktop/web/` → PASS.
- [ ] **Step 3:** Commit with `chore: delete Swift prototype, Ruby CLI, templates and template leftovers; rewrite README`.

### Task 10: CI for tests and three-OS builds

**Files:**
- Modify: `.github/workflows/build-desktop.yml`. Add `ubuntu-22.04` to the build matrix with an apt step: `sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`. Upload `**/*.deb` and `**/*.AppImage`, and include them in the release `find`.
- Create: `.github/workflows/test.yml`, triggered on `push` and `pull_request`, with one job on `ubuntu-latest`:
  - `cargo test --manifest-path crates/agent-pin-core/Cargo.toml`
  - `cargo clippy --manifest-path crates/agent-pin-core/Cargo.toml -- -D warnings`
  - `node --test apps/desktop/web/`

- [ ] **Step 1:** Make the edits.
- [ ] **Step 2:** Validate the YAML with `python3 -c "import yaml,sys;[yaml.safe_load(open(f)) for f in sys.argv[1:]]" .github/workflows/*.yml`. Expected: no error. If `yaml` isn't installed, run `python3 -m pip install --user pyyaml` first.
- [ ] **Step 3:** Commit with `ci: run core and web tests on push; build Linux bundles`.

---

## After all tasks

- Do a whole-branch review: superpowers:requesting-code-review against the Decisions, the Global Constraints and the Review Focus.
- Report to the user:
  - what is verified locally (core and web tests);
  - what is CI-only (the app crate compiling on macOS, Windows and Linux);
  - that nothing has been pushed;
  - that a push and a CI run need the user's approval.
