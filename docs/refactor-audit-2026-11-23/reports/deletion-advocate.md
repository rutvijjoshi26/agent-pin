# Deletion advocate / feature skeptic (subagent output, condensed verbatim points)

The verdict and confidence follow each item.

## Problems

1. **One row per provider, last write wins.** If session A is waiting for approval, session B's PreToolUse hides it. `session_id` is written (agent-pin-status:43, lib.rs:171) but never read (lib.rs:184-208, main.swift:129-142).
   - Redesign. High confidence.
2. **A hook error can block the agent.** main.rs exits 2 on any `write_hook_event` error (lib.rs:128,180). The Ruby CLI also exits 2 on bad usage (agent-pin-status:17).
   - Redesign: always exit 0. High confidence.
3. **The Rust Codex log fallback uses the file's modification time** (lib.rs:212), while Swift uses each line's timestamp (main.swift:172). Any later log write makes an old matched state look newer than the hook event (lib.rs:56). It matches internal tracing strings (lib.rs:217-229) and reads 260 KB every 0.8 s. README:20's justification is weak, because hooks write `events.jsonl` whether or not the app is running.
   - Delete candidate: about 35 lines in Rust and 80 in Swift. High confidence.
4. **The Swift prototype duplicates the Tauri app and has drifted.** It ignores Antigravity, timestamps differently, has a dead `lastFileSize`, and its package is named "claude-helper".
   - Delete candidate: about 490 lines. High confidence.
5. **There are two writers, two installers and two hook definitions.**
   - The Ruby installer recognizes only `agent-pin-status` entries (install-hooks.sh:39), so after the in-app Set up every event is written twice.
   - Keep the Rust path; delete install-hooks.sh (91 lines), the templates (175 lines) and the Ruby CLI (50 lines).
   - Delete candidate. High confidence.
6. **The Rust installer can damage config.**
   - It alphabetizes keys because `preserve_order` is off (Cargo.toml:23).
   - It replaces a non-object `hooks` value with `{}` without warning (lib.rs:310-312).
   - The write is not atomic (lib.rs:288).
   - Removal of old entries matches loosely (lib.rs:317,374).
   - It creates a Codex config file for people who don't use Codex (lib.rs:271,277).
   - Simplify. Medium-high confidence.
7. **SessionStart maps to running** (hooks/claude/settings.json:9, lib.rs:381,397).
   - Simplify: map it to idle. High confidence.
8. **PostToolUseFailure maps to error** (hooks/claude/settings.json:63).
   - Simplify: map it to running. Medium confidence.
9. **Antigravity is hard-coded in 4+ places**, which contradicts the README's open-format promise.
   - Redesign: accept any provider id. High confidence.
10. **The setup banner never goes away for single-agent users**, and the window stays 214 px tall (main.js:111-122,150; lib.rs:14).
    - Simplify. High confidence.
11. **Drag is implemented twice.**
    - Keep the command and delete the attributes; a naive cleanup would do the opposite.
    - Medium confidence.
12. **events.jsonl is unbounded and is re-read three times every 0.8 s** (lib.rs:50-52,185).
    - Simplify. High confidence.
13. **Template leftovers.**
    - Mobile crate-type entries, android/ios icons, unused icon.svg and logo.png, test-dir .gitignore entries.
    - `skipTaskbar: false`.
    - No `windows_subsystem` in main.rs, so a console window probably opens on Windows.
    - Delete candidate. Medium confidence.
14. **`agent-pin-reset` should become a subcommand.**
    - Simplify. Low confidence.

## Keep

- **PostToolUse → running** (hooks/claude/settings.json:52). It is the only event that clears waiting_approval after approval, because PreToolUse fires before PermissionRequest.
- **validate_install_exe_path** (lib.rs:445-454).
- **load_json refusing to overwrite unparsable files** (lib.rs:297).
- **The lean CI workflow.**

## Net

About 900 of roughly 2,180 lines could be removed. The items with real user impact are 1, 2, 3 and 6.
