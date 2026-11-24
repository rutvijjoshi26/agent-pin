# Findings (smells, feature questions, hand-rolled inventory)

Paths are relative to the repo root. `lib.rs` = `apps/desktop/src-tauri/src/lib.rs`, `main.rs` = `apps/desktop/src-tauri/src/main.rs`, `main.js` / `styles.css` / `index.html` = `apps/desktop/web/...`, `claude.json` = `hooks/claude/settings.json`, `codex.json` = `hooks/codex/hooks.json`.

Nothing was built or run, because the audit machine has no cargo, ruby or swift. Every finding comes from reading the code.

Verdicts: Keep / Simplify / Delete candidate / Replace with library / Redesign / Unknown.

---

## Current architecture map

```
Claude Code / Codex hook fires (one process per event, synchronous)
   |                                   |
   | README path                       | in-app "Set up" path
   v                                   v
bin/agent-pin-status (Ruby)        agent-pin hook ... (Tauri binary, main.rs:573-584 -> lib.rs:110-182)
   \                                   /
    `--> append ~/.agent-pin/events.jsonl (never truncated) <--'
                         |
          every 800 ms, whole file re-read
          /                                 \
Tauri: read_status (lib.rs:45-77)       Swift prototype: AgentStatusStore.refresh (main.swift:112-148)
  + Codex TUI log scrape (lib.rs:210)     + Codex TUI log scrape (main.swift:164)
  + staleness rules (lib.rs:461)          + staleness rules (main.swift:235)
          |                                   |
     main.js render (vanilla DOM)         SwiftUI PinView
```

There are two installers:

- `scripts/install-hooks.sh` renders the `hooks/*.json` templates (the Ruby path).
- `lib.rs:262-443` builds the same JSON in code (the Rust path).

Both merge into `~/.claude/settings.json` and `~/.codex/hooks.json`.

The repo has roughly 1,650 hand-written lines. About 40% of them are second copies of something else.

---

## F1. The hook writer can block the agent it monitors. Redesign (high)

**Evidence.**

- `main.rs:576-579` exits with code 2 on any `write_hook_event` error, including I/O failures (`lib.rs:128`, `176-181`).
- `bin/agent-pin-status:17` exits 2 on a usage error. An unhandled `JSON.parse` exception (`:21`) exits 1.
- Claude Code hooks reference, per event:
  - `PreToolUse`: exit 2 "Blocks the tool call".
  - `UserPromptSubmit`: "Blocks the prompt".
  - `Stop`: "Prevents Claude from stopping, continues the conversation".
- All three events are wired (`lib.rs:382-387`, `claude.json:14-77`).

**Why it exists.** Exiting 2 on a usage error is a Unix habit.

**Cost.** A passive status light can block every tool call, swallow prompts, or keep a turn running after `Stop`. Triggers include a full disk, an unwritable `~/.agent-pin`, a moved app bundle, or a bad argument.

**Recommendation.**

- The hook entry point should always exit 0 and report problems on stderr only.
- Mark the hooks `"async": true`. Claude Code documents this as "runs in the background without blocking", which removes the overlay from the agent's critical path entirely.
- Check that Codex offers the same before relying on it there.

**Risk.** None.

**Confidence.** High for Claude Code. The Codex exit-2 behavior is unverified (secondary sources only).

## F2. State is keyed by provider, not by session. Redesign (high)

**Evidence.**

- The writers record `session_id` (`agent-pin-status:43`, `lib.rs:171`), but no reader uses it.
- `latest_from_events` keeps only the last line per provider (`lib.rs:184-208`). Swift does the same with a dictionary keyed by provider (`main.swift:117-143`).

**Why it matters.**

- README:9-11 sells the product on running "more than one coding agent at a time", and calls `waiting_approval` "the expensive state".
- With two Claude sessions, session B's `PreToolUse` and `PostToolUse` lines overwrite session A's `waiting_approval` within seconds.
- So the headline use case gives the wrong answer.
- The deletion advocate reached the same conclusion independently.

**Prior art.**

- Perch sorts sessions "so whatever's blocked floats to the top".
- claude-status-bar promises a waiting session "is never hidden behind one that's thinking".
- Agent Signals reads per-session JSON status files written by hooks.

**Recommendation.**

- Key state by `(provider, session_id)`.
- Show one row per provider, using the highest-priority state across its live sessions: waiting > error > running > completed > idle.
- Add a count, for example "Claude 2: 1 waiting".
- Per-session rows can be added later.

## F3. The staleness rules hide the state the product exists for. Redesign (high)

**Evidence.**

- `lib.rs:462-480` and `main.swift:242-266` turn `running` and `waiting_approval` into `idle` after 900 s, then hide idle rows older than 20 s.
- `completed` becomes idle after 600 s.
- `error` never expires.
- The thresholds are duplicated, unnamed magic numbers.

**The contradiction.** README:11 says an agent waiting for approval "does nothing at all until you happen to look at it". After 15 minutes of waiting, the overlay stops showing it, which is exactly when it matters most.

**Why it exists.** There is no liveness signal, so without expiry a crashed session would show `running` forever. `bin/agent-pin-reset` is the manual escape hatch for the same problem.

**Recommendation.**

- Never expire `waiting_approval`. Show its age instead ("waiting 23m").
- Clear sessions on `SessionEnd`. Claude already wires it (`claude.json:89-98`).
- Keep a long timeout only for `running`.
- Put the whole policy in one named function with named constants.

## F4. Two hook-to-state mappings are wrong. Simplify (high / medium)

- **`SessionStart -> running`** (`claude.json:9`, `codex.json:10`, `lib.rs:381,397`).
  - A new, resumed or cleared session waiting for its first prompt shows "Running" for up to 15 minutes.
  - It should map to `idle`.
  - Confidence: high.
- **`PostToolUseFailure -> error`** (`claude.json:63`, `lib.rs:386`).
  - A failed `grep` or test run is routine for an agent, yet the row turns red until the next tool event.
  - Only `StopFailure` (an API error that ends the turn, per the hooks docs) is a real agent error.
  - It should map to `running`.
  - Confidence: medium.
- **Keep `PostToolUse -> running`.**
  - It is the only event that clears `waiting_approval` after the user approves, because `PreToolUse` fires before `PermissionRequest`. The deletion advocate made this correction.
  - Once failures map to `running`, `PreToolUse` becomes redundant, since `UserPromptSubmit` and `PostToolUse` already set `running`. Dropping it would halve the hook spawns per tool call.
  - Confidence: medium. Verify against real traces first.
- **Keep `PermissionRequest` rather than `Notification` for approvals** (README:111). This is a good, documented decision.

## F5. The Rust Codex log fallback overrides newer hook events. Delete candidate (high)

**Evidence.**

- `infer_codex_from_log` takes the state implied by the last matching line and stamps it with the log file's modification time (`lib.rs:212`, `233-241`).
- `read_status` lets that inferred state win whenever the modification time is newer than the last hook event (`lib.rs:54-58`).
- Codex writes this log continuously, so any later write makes an old inferred state look current.
- Example: a `running` line from before an approval prompt overrides the newer `waiting_approval` hook event.
- Swift uses each line's own timestamp instead (`main.swift:172`), so the two copies disagree.

**Fragility.** It matches internal tracing strings such as `codex_core::tasks: new` and `"sandbox_permissions":"require_escalated"` (`lib.rs:217-229`). These are not a public interface and can change between Codex versions. It also reads 260 KB every 0.8 s.

**The stated reason does not hold.**

- README:20 says the fallback covers sessions "already running before you launched the app".
- But hooks write to `events.jsonl` whether or not the app is running.
- The only real gap is sessions started before the hooks were installed or trusted (README:79), which is a one-time setup window.

**Recommendation.** Delete it: about 35 lines in Rust and 80 in Swift. If it stays, use it only when there is no Codex hook event at all, and use the per-line timestamps.

## F6. Four concerns are implemented twice and have drifted. Delete candidate (high)

| Duplicate | Drift found |
|---|---|
| Swift prototype (`main.swift` 426 lines, `Package.swift`, `scripts/build-app.sh`) vs Tauri | Swift drops antigravity (`main.swift:130`), has no setup UI, uses different Codex timestamps (F5), and has a dead `lastFileSize` (`:90,113-114`). The package is named `"claude-helper"` (`Package.swift:5`). |
| Ruby `agent-pin-status` vs `agent-pin hook` | Ruby locks with flock (`:47-49`); Rust does not lock. Ruby crashes on non-JSON stdin; Rust silently ignores it (`lib.rs:135`). |
| `hooks/*.json` templates vs `claude_hooks`/`codex_hooks` (`lib.rs:378-405`) | Identical today. Nothing keeps them in sync. |
| `install-hooks.sh` vs `install_hooks` | The Ruby installer only strips `agent-pin-status` entries (`install-hooks.sh:39`). Running it after the in-app Set up keeps the Rust `agent-pin ... hook` entries and adds a second copy, so every event is written twice by two different writers. |

**Recommendation.**

- Keep one binary. The Tauri executable is already both the app and the hook writer (`main.rs:575`).
- Add `agent-pin install-hooks` and `agent-pin reset` subcommands.
- Delete the Swift app, the Ruby scripts, the templates and the shell installer, about 650 lines.
- This also removes the Ruby requirement (README:26). That requirement is a real burden on Windows, where `#!/usr/bin/env ruby` hook commands cannot run anyway.

**Open question.** Does the user rely on the Swift app? It is lighter, uses the `.accessory` activation policy, and joins all Spaces (`main.swift:388,416`). If so, port those window behaviors to Tauri first (see F10).

## F7. The event log is re-read three times per tick and never trimmed. Simplify (high)

**Evidence.**

- `read_status` calls `latest_from_events` once per provider, and each call runs `fs::read_to_string` on the whole file (`lib.rs:50-52`, `185`).
- This happens every 800 ms (`main.js:220`).
- Each tool call appends two lines, and nothing ever truncates the file.
- The Swift copy also re-reads the whole file (`main.swift:115`).

**Recommendation.**

- Have the hook writer keep a small current-state file per session at `~/.agent-pin/sessions/<provider>-<session>.json`, written via temp file and rename.
- The reader then just lists a directory.
- Keep `events.jsonl` as an optional audit trail with size-based rotation, or drop it (see open questions).
- Polling every 800 ms is fine once each read is cheap. A file watcher (the `notify` crate) is not worth the extra complexity here.

## F8. The Rust append can interleave lines, and the installer rewrites user config. Simplify (medium-high)

- **Interleaved appends.**
  - `writeln!(file, "{event}")` on an unbuffered `File` (`lib.rs:176-181`) formats a `serde_json::Value` through `Display`, which issues many small writes.
  - Claude runs matching hooks in parallel, per the docs, so concurrent appends can interleave.
  - The reader then silently drops the corrupt lines (`lib.rs:189`).
  - Fix: serialize to a `String` and call `write_all` once, or adopt F7's per-session files.
- **The installer rewrites the user's `~/.claude/settings.json`.**
  - It goes through `serde_json` without `preserve_order` (`Cargo.toml:23`), which alphabetizes the user's keys.
  - It replaces a non-object `hooks` value with `{}` without warning (`lib.rs:310-312`).
  - `fs::write` is not atomic (`lib.rs:288`).
  - It creates `~/.codex/hooks.json` for people who don't use Codex (`lib.rs:276-278`).
  - Every click adds another timestamped backup (`lib.rs:280-283`).
  - Fix: enable `preserve_order`, write via temp file plus rename, and fail rather than overwrite on unexpected shapes.
- **Over-broad removal.**
  - `agent_pin_entry` removes an entire matcher group if any string anywhere inside it contains "agent-pin" and " hook " (`lib.rs:363-376`). That can delete the user's own hooks in the same group.
  - Fix: match only each hook's `command` field and remove only that hook.

## F9. The drag attributes are probably inert, and drag is hand-rolled. Simplify (medium-high)

**Evidence.**

- There are no capability files: `gen/schemas/capabilities.json` is `{}`.
- So the 13 `data-tauri-drag-region` attributes (5 in `index.html`, 8 in `main.js`) most likely do nothing, because they need `core:window:allow-start-dragging`.
- A custom `start_window_drag` command (`lib.rs:79-82`) plus a `pointerdown` handler (`main.js:20-34`) does the actual dragging.

**Recommendation.** Pick one mechanism.

- **Preferred:** add `src-tauri/capabilities/default.json` granting `core:window:allow-start-dragging` (and `allow-set-size` for F10). Keep one `data-tauri-drag-region` on `#pin`, and delete the command, the handler and the other 12 attributes.
- **Alternative (the deletion advocate's choice):** keep the command and delete all the attributes.

Either way, also set a real CSP instead of `"csp": null` (`tauri.conf.json:28`).

## F10. Window sizing is split across three files, and compact mode clips. Simplify (medium-high)

**Evidence.**

- Sizes are defined in three places: `lib.rs:9-14`, `tauri.conf.json:15-18` and `styles.css`.
- Compact mode is 70 px tall (`lib.rs:12`). Two compact rows need 20 (padding) + 24 (header) + 18 + 6 + 18 = 86 px (`styles.css:24,45-46,64-65,124`), so the second agent is cut off.
- Expanded height is fixed at 164 or 214 px regardless of the number of rows.

**Recommendation.**

- Measure the content in JS with `ResizeObserver` and call `getCurrentWindow().setSize()`. This deletes `set_compact`, the size constants and the `setupVisible` round-trip.
- Position the window with the official `tauri-plugin-positioner` (`TopRight`) instead of `position_top_right` (`lib.rs:552-572`).

**Missing behaviors compared with Swift.**

- `skipTaskbar: false` (`tauri.conf.json:23`) means the overlay appears in the taskbar.
- No `visibleOnAllWorkspaces` setting.
- `main.rs` has no `#![windows_subsystem = "windows"]`, so the release app probably opens a console window on Windows.
  - Adding the attribute needs care: the same binary is the hook CLI, and its stdin/stderr behavior under the GUI subsystem must be tested.
  - Status: Unknown.

## F11. Antigravity is a phantom provider. Redesign (high)

**Evidence.**

- It is hard-coded into validation (`agent-pin-status:8`, `lib.rs:115`), reset (`agent-pin-reset:15`) and display (`lib.rs:52,69-74,527`).
- It is advertised in the bundle descriptions (`tauri.conf.json:42-43`) but not in `Cargo.toml:4`.
- Swift ignores it.
- No adapter exists (README:123, 140).

**The contradiction.** README:5 and 115 promise a format open to "any other agent", but the hard-coded list rejects every other name.

**Recommendation.** Accept any provider id matching `[a-z0-9-]+` and title-case it for display. That removes the special case and delivers what the README promises. Otherwise, delete antigravity everywhere.

## F12. The setup panel nags about agents the user may not have. Simplify (high)

**Evidence.**

- `read_integrations` runs every 5 s and re-parses `~/.claude/settings.json` each time (`main.js:221`, `lib.rs:244-260`).
- The panel shows whenever either Claude or Codex hooks are missing (`main.js:106-123`).
- So a Claude-only user permanently sees "Codex hooks missing", and the window stays 214 px tall (`lib.rs:14`).

**Recommendation.**

- Offer only the agents whose config directory exists.
- Check once at startup and again after installing.
- Allow dismissing the panel.

## F13. Status is stringly typed in Rust. Simplify (medium)

**Evidence.**

- `AgentSnapshot.status: String` (`lib.rs:30`).
- Status is compared as raw strings, e.g. `== "completed"` (`lib.rs:463-472`).
- The allow-lists are duplicated (`lib.rs:115-116`).

**Recommendation.** Define one `#[serde(rename_all = "snake_case")] enum Status` and use it in the writer, the reader and validation.

## F14. Repo hygiene and leftovers from the Tauri template. Delete candidate (high confidence, low impact)

**Not needed for a desktop-only app:**

- `crate-type = ["staticlib","cdylib","rlib"]` (`Cargo.toml:12`), left over from the mobile template.
- `icons/android/` and `icons/ios/`.
- The Windows Store `Square*Logo.png` and `StoreLogo.png` icons.

**Generated files that are committed:**

- `gen/schemas/` (304 KB) is generated by `tauri-build`, and Tauri's own template gitignores `/gen/schemas`.

**Unreferenced files:**

- `web/icon.svg`.
- `assets/agent-pin-logo.png` (900 KB). Keep it only if it is the source for `tauri icon`, and say so.

**Small leftovers:**

- `Package.swift` is named `"claude-helper"`.
- `.gitignore` lists `.agent-pin-test/` and `.install-test/`, but those tests don't exist.
- `Cargo.toml` has `repository = ""`.

**Keep** `Cargo.lock` and `package-lock.json`. This is an application, and CI runs `npm ci`.

## F15. There are no tests and no CI check on code. Redesign the test seam (high)

**Evidence.**

- README:140 says there are no tests.
- CI runs only on tags or manual dispatch (`build-desktop.yml:3-7`).

**Recommendation.**

- The logic is already almost pure. Once `normalize`, `latest_from_events`, `merge_hooks` and the hook-to-state table move out of `lib.rs`, they can be unit-tested without Tauri.
- Add `cargo test` and `cargo clippy` on every push.
- Keep the release workflow itself; it is lean.

## F16. The writers honor `AGENT_PIN_HOME`, but the reader ignores it. Simplify (high)

**Evidence.**

- The writers honor `AGENT_PIN_HOME`: `lib.rs:124-127`, `agent-pin-status:23`, `agent-pin-reset:7`.
- The readers hard-code `~/.agent-pin` (`lib.rs:47-48`, `main.swift:99`).
- So setting the variable makes the overlay go blank.

**Recommendation.** Resolve the state directory in one function, used by both writer and reader. Found by the hand-rolled critic.

## F17. The setup check reports hooks as installed even when they point at an old app path. Redesign (medium-high)

**Evidence.**

- `json_contains_provider_hook` (`lib.rs:347-361`) treats any Agent Pin command string as installed.
- That includes one pointing at a previous app location or the Ruby script.
- After the app moves or updates, every hook fails silently (and with exit 2, see F1), while the panel still says hooks are installed.

**Recommendation.** Compare the command's executable path with `current_exe()`, and offer "Repair" when they differ.

## F18. The Codex log tail read fails on a UTF-8 boundary. Delete with F5 (high)

**Evidence.**

- `read_tail` seeks to `size - 260000` and calls `read_to_string` (`lib.rs:483-491`).
- If the seek lands inside a multi-byte character, the read fails.
- Inference then silently disappears (`.ok()?` at `lib.rs:213`).

Moot if F5 is accepted. Found by the hand-rolled critic.

## F19. Timestamps are whole seconds. Simplify (medium)

**Evidence.**

- `lib.rs:504-509` and `agent-pin-status:44` record seconds.
- The log-versus-hook comparison (`lib.rs:56`) ties within the same second.

**Recommendation.** Store milliseconds. This becomes unimportant once state is per-session files (F7).

## Critic disagreements, recorded

- **Positioner.** The hand-rolled critic says `tauri-plugin-positioner` has no margin option, so `position_top_right` can stay custom. I accept that, and downgrade F10's positioner suggestion to "optional".
- **Drag.** I prefer the capability plus one attribute. The deletion advocate prefers the command and no attributes. Both remove one of the two mechanisms. Verify on a real build which one works.

---

## Hand-rolled inventory

| Hand-rolled | Library / framework alternative | Decision |
|---|---|---|
| Window drag command + pointerdown (`lib.rs:79`, `main.js:20`) | Tauri capability + `data-tauri-drag-region` | Replace (F9) |
| `position_top_right` (`lib.rs:552`) | `tauri-plugin-positioner` (no margin option, per critic) | Keep custom |
| `set_compact` + size constants (`lib.rs:9-14,96`) | Content measurement + `window.setSize` from JS | Replace (F10) |
| `home_dir` (`lib.rs:511-521`) + state dir | `std::env::home_dir` / `dirs` | Replace, and use one state-dir resolver (F16) |
| JSONL whole-file replay (`lib.rs:184`) | Per-session state files (atomic rename) | Redesign (F7) |
| Codex log tail scraper (`lib.rs:210`, `483`) | Codex hooks | Delete (F5) |
| JSON merge into user config (`lib.rs:300`) | serde_json with `preserve_order`; custom merge is justified | Keep custom, fix (F8) |
| `shell_quote_path` (`lib.rs:440`) | `shell-words` / platform-specific quoting | Keep, but test on Windows paths and on paths containing `$` or backticks. Double quotes do not stop `$` expansion in sh. Unknown. |
| File append without lock (`lib.rs:176`) | One `write_all`, or `fs4`/`fd-lock` | Simplify (F8) |
| Staleness policy (`lib.rs:461`) | none; this is product logic | Keep custom, redesign (F3) |
| Polling loop (`main.js:220`) | `notify` crate + Tauri events | Keep polling; make each poll cheap |
| Vanilla DOM rendering (`main.js:143-191`) | React/Svelte | Keep vanilla. A framework is not justified for 3 rows. |
| Ruby installer (`install-hooks.sh`) | the Rust installer already exists | Delete (F6) |

## Feature questions

| Feature | Value | Cost | Call |
|---|---|---|---|
| Swift macOS prototype | Native and lighter | 490 lines, drifted copy | Delete unless the user runs it daily |
| Ruby CLI + shell installer | Works without building the app | Requires Ruby; doubles hooks with the app installer | Delete |
| Codex log fallback | Covers Codex sessions without hooks | Overrides correct hook data; brittle | Delete |
| Antigravity provider | none (no adapter) | Special cases in 6 places | Replace with an open provider id |
| `events.jsonl` audit log | "tail or build on" (README:21) | Unbounded growth; it is also the read path | Keep as optional and rotated; stop reading it for state |
| Compact / expanded toggle (double-click) | Takes less screen space | Size constants; compact mode clips | Keep; size by content |
| In-app Set up | Onboarding without a terminal | Polls every 5 s; nags Claude-only users | Keep; detect installed agents |
| `agent-pin-reset` | Clears stuck states | Exists because there is no liveness | Becomes `agent-pin reset`; less needed after F3 |
| Codex `statusMessage: "Updating Agent Pin"` (`codex.json:111`) | none for the user | A visible message in Codex on every tool call (assumed) | Delete candidate, low confidence |

## Legitimate complexity worth keeping

- **Hooks as the source of truth** rather than terminal scraping (README:19).
- **`PermissionRequest` rather than `Notification` for approval** (README:111).
- **`PostToolUse -> running`**: it is load-bearing for leaving `waiting_approval`.
- **One binary that doubles as the hook CLI** (`main.rs:575`). This is the right instinct and the base of the target design.
- **Idempotent merge with backup** when installing into user config, and refusing to overwrite unparsable JSON (`lib.rs:275-339`, `297`).
- **`validate_install_exe_path`**, which refuses to install from AppTranslocation or a mounted DMG (`lib.rs:445-454`). This is a real macOS pitfall.
- **Tauri over Electron, and vanilla JS with no bundler.**
- **Polling** (once each poll is cheap).
- **A lean release workflow.**

## Accidental complexity, likely from incremental or AI-assisted work

Each of these looks like something added and never cleaned up:

- The Swift prototype was not deleted after the Tauri rewrite.
- The Rust hook writer and installer were added alongside the Ruby ones instead of replacing them.
- The drag attributes were left in place after a command worked around the missing capability.
- Antigravity was declared before any adapter existed.
- The Codex log scraper's justification does not match how hooks actually work.
- Template icons and `crate-type` leftovers remain.
- Swift has a dead `lastFileSize` field.
