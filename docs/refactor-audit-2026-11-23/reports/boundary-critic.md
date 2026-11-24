# Boundary and hand-rolled machinery critic (subagent output, condensed)

1. **Exit 2 from a passive hook can block the agent.** Seen at main.rs:6 and agent-pin-status:16-17.
   - Codex exit-2 semantics are unverified.
   - Recommendation: always exit 0 and log errors to a file.
   - Verdict: Redesign. Confidence: high for Claude, medium for Codex.
2. **Concurrent appends can interleave.** The writer does an unlocked, unbuffered `writeln!` of a serde Value (lib.rs:176-181), and the reader silently skips bad lines (lib.rs:189-191). Ruby uses flock.
   - Recommendation: serialize with `to_vec`, append `\n`, and do a single `write_all`, or use fs4.
   - Verdict: Simplify. Confidence: high.
3. **The event log grows without limit and is fully re-read three times per poll.**
   - Recommendation: per-session state files, or offset tracking with `notify` plus Tauri emit. A 1 s timer is still needed for age-out.
   - Verdict: Redesign. Confidence: high.
4. **Multiple sessions collapse into one row.** `session_id` is never deserialized (`AgentEvent`, lib.rs:16-24).
   - Verdict: Redesign. Confidence: high.
5. **The Codex log scrape is fragile.**
   - It stamps state with the file's modification time, not the line time (lib.rs:212), which is a regression from Swift (main.swift:172,226-233).
   - The 260 KB tail read fails if it starts mid-character in UTF-8 (lib.rs:486-490).
   - Verdict: Delete candidate, or switch to `from_utf8_lossy` and line timestamps. Confidence: high.
6. **The installer reorders keys and writes non-atomically.**
   - `preserve_order` is off: serde_json has no indexmap dependency in Cargo.lock.
   - It uses `fs::write`.
   - Two installs in the same second overwrite the backup (lib.rs:281).
   - Verdict: Replace with library (`preserve_order` plus tempfile and rename). Confidence: high.
7. **Hook matching is too loose in both directions.**
   - Removal drops whole matcher groups (lib.rs:363-370).
   - The installed check counts hooks pointing at a stale exe path (lib.rs:347-351).
   - Verdict: Redesign. Confidence: high.
8. **Shell quoting is wrong.**
   - Double quotes still let `$` and backticks expand (lib.rs:440-443).
   - Backslash doubling assumes bash; the Windows hook shell is unverified.
   - The Ruby templates insert `__REPO__` unquoted, so paths with spaces break.
   - Verdict: Replace with library (shlex). Confidence: medium-high.
9. **Two drag mechanisms; the attribute one is likely dead without a capability.**
   - Recommendation: add the capability and delete the command.
   - Confidence: medium.
10. **Window sizing lives in the wrong layer.**
    - Recommendation: measure in JS and call setSize; the resize is also re-sent every 5 s.
    - `position_top_right` can stay, because tauri-plugin-positioner has no margin option.
    - Verdict: Redesign.
11. **Display rules are buried in `normalize`.**
    - 600/900/20 s are magic numbers duplicated in Swift, and errors never expire.
    - Verdict: Simplify.
12. **Status and provider strings are defined in four places**, and error text is pushed into the `cwd` field (main.js:198-203).
    - Verdict: Simplify, using a serde enum.
13. **`AGENT_PIN_HOME` is honored by the writer (lib.rs:124) but not the reader (lib.rs:48)**, and home lookup is hand-rolled.
    - Verdict: Replace with library. Confidence: high.
14. **Swift, Ruby and Rust triplicate the same logic and disagree.**
    - Verdict: Delete candidate. Confidence: medium.
15. **Timestamps are whole seconds.**
    - Verdict: Simplify, store milliseconds. Confidence: medium.

**Keep custom:** hook stdin field extraction (lib.rs:138-162) and the AppTranslocation guard (lib.rs:445-454).

**Sources cited by the critic:**
- https://developers.openai.com/codex/hooks
- https://flaviocopes.com/ai-glossary/hooks/
- https://raw.githubusercontent.com/Dicklesworthstone/destructive_command_guard/main/docs/codex-integration.md
