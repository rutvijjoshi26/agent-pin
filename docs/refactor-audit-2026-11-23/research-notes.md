# Research notes

## Claude Code hooks (https://code.claude.com/docs/en/hooks, fetched 2026-11-23)

- **Exit code 2, per event:**
  - PreToolUse: "Blocks the tool call".
  - UserPromptSubmit: "Blocks the prompt".
  - Stop: "Prevents Claude from stopping, continues the conversation".
  - PermissionRequest: not read as a decision.
  - PostToolUse and SessionStart: stderr is shown only.
- **Other non-zero exit codes** are non-blocking errors.
- **Command hooks support `async: true`**, which "runs in the background without blocking". That removes the overlay from the critical path (F1).
- **PermissionRequest** fires "when a tool call needs a permission decision".
- **PostToolUseFailure** fires "after a tool call fails". It is routine and not an agent error (F4).
- **StopFailure** fires "when the turn ends due to an API error".
- **Matching hooks run in parallel**, so concurrent appends to the event log are real (F8).
- **Default command timeout** is 600 s (30 s on UserPromptSubmit).

## Codex hooks (https://developers.openai.com/codex/hooks, via search)

- Codex reads `~/.codex/hooks.json`, and PermissionRequest exists.
- Command hooks need review and trust before they run.
- Exit-2 semantics for Codex were not confirmed in official docs (secondary sources say exit 2 means block). Treat F1 as applying to Codex too.

## Prior art: status overlays and menu-bar apps

- **Perch** (github.com/ik-labs/perch): every Claude Code and Codex session, "sorted so whatever's blocked floats to the top". This is the per-session priority model (F2).
- **claude-status-bar** (github.com/m1ckc3s/claude-status-bar): "a session awaiting your permission is never hidden behind one that's thinking".
- **Agent Signals** (Product Hunt): reads JSON status files written by hooks, shows waiting/working/done counters, and focuses the tab. This is the state-file pattern (F7).
- **Others:** Clawdboard (installs hooks via a plugin), Open Island (GPL, approval cards), Agent Signal Bar, AgentPulse, Code Light, NotchPal.
- **Takeaway:** the field has converged on session-keyed state with blocked-first priority. Most of these tools are macOS-only, so Agent Pin's Windows support through Tauri is a plausible differentiator.

## Tauri 2

- **Capabilities.** Tauri 2 gates core window APIs, including the one `data-tauri-drag-region` needs (`core:window:allow-start-dragging`), behind capabilities. This repo has none (`gen/schemas/capabilities.json` = `{}`).
  - App-defined commands are allowed by default, which is why the custom drag command works.
  - Not verified on a running build.
- **Generated schemas.** Tauri's project template gitignores `/gen/schemas`.
- **Positioner plugin.** The `tauri-plugin-positioner` plugin offers a TopRight position but no margin, per the critic.
