# Worklog: agent-pin deep refactor audit (2026-11-23)

## Goal

Critically audit the whole agent-pin repository (Swift prototype, Tauri app, Ruby CLI, hook templates, installers, CI) to determine which architecture, features, boundaries, names, abstractions, dependencies and workflows should be preserved, simplified, removed or redesigned.

Core questions:
- Why does this exist? Is it still needed?
- Is this the simplest shape that can support "glance at one overlay to see which agent needs me"?
- Did this complexity come from real constraints or accidental accumulation?
- Is hand-rolled code justified, or should it use a standard library / framework capability?
- What would the architecture look like if designed cleanly today?

Non-goals: no production code changes, no shallow smell list, no assumption that the current shape is justified, no pattern name-dropping without concrete movement. No git commit, push, PR, or remote access.

Success criteria: architecture mapped; boundaries judged; questionable features called out; hand-rolled machinery compared to libraries/framework features; accidental vs legitimate complexity separated; prior art considered; target architecture and roadmap written.

## Constraints observed

- Repo is a single squashed commit (`f0b5a29 Agent Pin: status overlay for coding agents`), so there is no history to mine for why things exist.
- No cargo, rustc, swift or ruby on this machine. Nothing can be built or tested offline. All findings are from reading. Node/npm exist but `npm run build` needs Rust, so it was not run.
- Ignored: icons, gen/schemas, Cargo.lock, package-lock.json (only judged whether they belong in the repo).

## Session 1 log

1. Read the skill in full.
2. Inventoried files. Code by size: lib.rs 572, main.swift 426, main.js 221, styles.css 217, README 144, hooks/claude 101, install-hooks.sh 91, hooks/codex 74, CI 72, agent-pin-status 50, build-app.sh 46, agent-pin-reset 26.
3. Read every non-generated file.

### Key early insight: three things exist twice

| Concern | Copy A | Copy B |
|---|---|---|
| Overlay reader + staleness + Codex log parser | Sources/AgentPin/main.swift | apps/desktop/src-tauri/src/lib.rs + web/main.js |
| Hook writer (event to events.jsonl) | bin/agent-pin-status (Ruby) | lib.rs `write_hook_event` via `agent-pin hook` (main.rs) |
| Hook definitions | hooks/claude/settings.json, hooks/codex/hooks.json (templates) | lib.rs `claude_hooks`/`codex_hooks` (code) |
| Hook installer | scripts/install-hooks.sh (embedded Ruby) | lib.rs `install_hooks` (Set up button) |

The copies already disagree (see smells). README documents the Ruby path; the app's Set up button uses the Rust path.

### Findings noted while reading (detail in findings.md)

- Hook writer exit code 2 on failure (main.rs:578, agent-pin-status:17). Exit 2 is the blocking code for Claude Code hooks. A monitoring sidecar can block the monitored agent.
- State keyed by provider only; session_id recorded (agent-pin-status:43, lib.rs:171) but never read. Two concurrent Claude sessions overwrite each other, which hides `waiting_approval`, the README's headline state (README:11).
- `waiting_approval` decays to idle after 900s and disappears (lib.rs:463-480, main.swift:244). Directly contradicts README:11.
- SessionStart maps to `running` (hooks/claude/settings.json:9, lib.rs:381, 397). A fresh session waiting for a prompt shows Running.
- PostToolUseFailure maps to `error` (lib.rs:386). Routine failed tool calls flash red.
- Rust Codex log fallback stamps the inferred state with log file mtime (lib.rs:212), not the line time. Any later log write makes an old inferred state look newer than the latest hook event and overrides it (lib.rs:54-58). Swift parses the line timestamp instead (main.swift:172).
- read_status re-reads and re-parses the entire unbounded events.jsonl three times every 800 ms (lib.rs:50-52, main.js:220). Swift reads it once per 800 ms; `lastFileSize` is dead (main.swift:90, 113-114).
- Rust writer: no lock, and `writeln!(file, "{event}")` on an unbuffered File with serde_json Value Display issues many small writes, so parallel hooks can interleave (lib.rs:176-181). Ruby uses flock.
- Rust installer re-serializes ~/.claude/settings.json through serde_json without `preserve_order` (Cargo.toml:23), so it alphabetizes the user's keys. Non-atomic fs::write (lib.rs:288).
- Ruby installer only strips `agent-pin-status` entries (install-hooks.sh:39); Rust installer entries (`agent-pin ... hook`) survive, so running the README installer after the app's Set up doubles every hook.
- No Tauri capability files (gen/schemas/capabilities.json is `{}`), so the 13 `data-tauri-drag-region` attributes are inert; a custom `start_window_drag` command (lib.rs:79-82, main.js:20-34) does the job.
- Compact window is 54x70 (lib.rs:11-12) but two compact rows need about 86px (styles.css:24,45-46,64,124), so compact mode clips the second agent.
- Antigravity: accepted, displayed by Tauri, advertised in bundle descriptions (tauri.conf.json:42-43), ignored by Swift (main.swift:130), no adapter (README:123).
- Template leftovers: crate-type staticlib/cdylib (Cargo.toml:12), Android/iOS icons, Windows Store Square*Logo icons, gen/schemas committed, Package.swift name "claude-helper", unreferenced web/icon.svg and 900KB assets/agent-pin-logo.png, .gitignore entries for test dirs that do not exist.

## Next steps

- [x] Spawn up to 3 critics (deletion advocate, hand-rolled/boundary critic, migration realist on draft target) with code, not conclusions.
- [x] Prior-art check: Claude Code hook exit-code semantics; Codex hooks; similar status overlays.
- [x] Write findings.md, target-and-roadmap.md, README.md (index + exec summary).

## Session 1, later

- Both critics returned (reports/deletion-advocate.md, reports/boundary-critic.md). They independently confirmed F1, F2, F5, F6, F7 and F8.
- Corrections accepted:
  - PostToolUse is load-bearing.
  - main.rs has no windows_subsystem.
- New findings: F16 (AGENT_PIN_HOME writer-only), F17 (stale exe path counted as installed), F18 (UTF-8 tail read), F19 (second-resolution timestamps).
- Disagreement recorded: positioner plugin has no margin, so position_top_right stays custom. Which drag mechanism to keep is undecided.
- Verified hook semantics against the Claude Code docs (research-notes.md).
- Note: a Write of `findings.md` was refused by the harness as a "report file". The content went into `smells.md`, the name the skill prescribes.
- Artifacts done: README.md, smells.md, target-architecture.md (includes roadmap), research-notes.md, reports/.

**Resume point:** the audit is complete. If continuing, verify F9 (drag) and F10 (compact clipping) on a real macOS/Windows build, and confirm Codex exit-2 semantics.
