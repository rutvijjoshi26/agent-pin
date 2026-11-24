# agent-pin refactor audit, 2026-11-23

This audit did not change any code. Nothing was built or run, because the audit machine has no cargo, ruby or swift.

## Executive summary

Agent Pin's core idea is sound and keeps its stack small:

- Agent lifecycle hooks write a status line.
- An always-on-top Tauri window shows it.

But the repo is roughly 40% second copies, and four concerns exist twice:

- the reader (Swift and Tauri)
- the hook writer (Ruby and Rust)
- the hook definitions (JSON templates and Rust code)
- the installer (shell/Ruby and Rust)

The copies have already drifted. For example, running the README installer after the in-app Set up writes every event twice.

More important, three design decisions undercut the product's own pitch:

1. **A failing hook can block the agent it monitors.** On any failure the hook exits with code 2, which Claude Code treats as "block the tool call / prompt / stop".
2. **State is keyed by provider, not session.** A second Claude session hides the first one's "waiting for approval".
3. **Staleness rules drop a waiting agent after 15 minutes.** That is exactly when it matters most.

The Codex log scraper is fragile, and the Rust version overrides newer hook data with older states.

**Recommendation:**

- Keep one Tauri binary that is app, hook writer and installer.
- Delete the Swift app, the Ruby path, the Codex scraper and Antigravity.
- Make the store session-keyed, and put all display policy in one pure, tested function.

## Files

| File | Contents |
|---|---|
| `worklog.md` | Goal, constraints, chronological notes, resume point |
| `smells.md` | Architecture map; findings F1-F19 with evidence, verdicts and confidence; hand-rolled inventory; feature questions; legitimate vs accidental complexity |
| `target-architecture.md` | Target shape, patterns adopted and rejected, phased roadmap 0-6 |
| `research-notes.md` | Hook semantics checked against docs; prior-art overlays |
| `reports/` | The two critic subagents' outputs and their briefs |

## Open questions for the owner

1. Do you still run the Swift app? If so, which of its behaviors matter (no Dock icon, all Spaces)?
2. Does anyone consume `events.jsonl` besides the overlay? If not, it can become an optional debug log or be removed.
3. Do you need multiple sessions per provider shown separately, or is one row per provider with "2 sessions, 1 waiting" enough?
4. Is Antigravity a real near-term target, or should providers become free-form ids?
5. Several open-source tools now do this (Perch, Agent Signals, claude-status-bar, Open Island). Is Agent Pin's differentiator cross-platform (Windows), and is that worth the investment?
