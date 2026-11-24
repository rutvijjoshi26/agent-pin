# Agent Pin

An always-on-top desktop overlay that shows, at a glance, what each of your coding-agent sessions is doing right now: idle, running, waiting for your approval, completed, or errored.

Supports Claude Code and Codex out of the box. Any other agent that can run a hook command can report into it too.

## The problem

Once you run more than one coding agent at a time, you lose track of them. The agents work in separate terminals or windows, so the only way to know where each one stands is to cycle through them and read the last few lines.

The expensive state is **waiting for approval**. An agent that has stopped to ask permission does nothing at all until you happen to look at it. Time spent blocked is invisible, so you find out about it late. The same applies to an agent that finished four minutes ago, or one that errored out early and has been sitting there since.

Agent Pin pulls that state out of the terminals and into one small window that stays on top of whatever you're working in. You glance at it instead of hunting for it.

## What it does

- **One row per session.** Rows are sorted waiting → error → running → completed → idle, so the sessions that need you are always on top. A session waiting for approval never expires from the list. Double-click the pin to collapse it to bare status dots.
- **A catch-up card.** Click a row to see what happened while you were away:

  ```text
  You asked: <your original prompt>
  Since you looked: 4 files changed
  Ran: cargo test ✓
  Now asking: <the current approval request>
  ```

- **Collision warnings.** When two sessions edited the same file within 10 minutes, both rows say so. Sessions of the same agent in the same project are told apart with a ` #xxxx` suffix (the first characters of the session id).

## Install

1. Download a release for your platform: macOS `.dmg`, Windows `.msi` or `.exe`, Linux `.deb` or `.AppImage`.
2. Open Agent Pin and click **Set up**, or run:

   ```sh
   agent-pin install-hooks
   ```

   Hooks are only installed for agents whose config directory already exists (`~/.claude` for Claude Code, `~/.codex` for Codex). They are merged into `~/.claude/settings.json` and `~/.codex/hooks.json`.
3. **Claude Code:** run `/hooks` to review and trust the new hooks. **Codex:** restart it, since it only loads hooks at session start.

The installer is careful with your config. It backs the file up to `<file>.agent-pin-backup-<timestamp>` before writing, writes atomically, keeps your key order, never removes a hook it didn't install, and skips the write entirely when nothing would change. Re-running it is safe.

The release builds are unsigned, so macOS and Windows will warn before running them. Code signing needs paid developer accounts and isn't set up here.

## CLI

The app binary doubles as the CLI:

```sh
agent-pin hook <provider> <EventName>   # record one hook event (what the installed hooks run)
agent-pin install-hooks                 # install or update hooks for Claude Code and Codex
agent-pin reset                         # clear all stored session state
```

The `agent-pin` CLI binary isn't on your `PATH` by default (the `.deb`, which installs `/usr/bin/agent-pin`, is the exception). It lives inside the app: `Agent Pin.app/Contents/MacOS/agent-pin` on macOS, the `.AppImage` file itself on Linux (for example `./Agent\ Pin_<version>_<arch>.AppImage install-hooks`), and `agent-pin.exe` in the install folder on Windows. Run it by full path or symlink it onto your `PATH`.

On Windows, release builds have no console window, so the CLI prints nothing there. Use the **Set up** button instead of `install-hooks`.

## How it works

```text
Claude Code / Codex / any agent
      |  lifecycle hooks
      v
agent-pin hook <provider> <EventName>
      |
      v
~/.agent-pin/sessions/<provider>__<session>.json
      |
      v
Agent Pin overlay
```

Each hook event updates one small JSON file per session under `~/.agent-pin/sessions/`. Set `AGENT_PIN_HOME` to use a different directory. While running, the app deletes session files that haven't changed in 3 days.

The hook can never block an agent. Claude Code hooks are installed with `"async": true`, and `agent-pin hook` always exits 0: it waits at most about 2 seconds for its input, and reports any error only on stderr. Codex hooks are installed without `async`.

One deliberate choice worth noting: Claude's `Notification` hook is **not** used to detect approval waits, because Claude also fires notifications after a run completes, which makes the signal ambiguous. Approval waits come from `PermissionRequest` instead.

## Adding another agent

Any provider id works (lowercase letters, digits and dashes). Point the agent's hooks at:

```sh
agent-pin hook <id> <EventName>
```

using Claude-style event names (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `StopFailure`, `SessionEnd`), with the hook's JSON payload on stdin. For example, `agent-pin hook antigravity PermissionRequest`.

## Linux

On Wayland, the compositor may not honor always-on-top. The app detects a Wayland session and says so in its setup panel.

## Building from source

You need **Node.js 18+** and the **Rust toolchain** ([rustup](https://rustup.rs/)). On Linux you also need the webkit2gtk-4.1 development packages (on Debian or Ubuntu: `libwebkit2gtk-4.1-dev`).

```sh
# Core tests
cargo test --manifest-path crates/agent-pin-core/Cargo.toml

# Frontend tests
node --test apps/desktop/web/*.test.js

# The app
cd apps/desktop && npm ci && npm run build
```

Bundles land under `apps/desktop/src-tauri/target/release/bundle/`. Use `npm run dev` to develop against the app instead of building a bundle.

## Building a release

Pushing a tag builds the bundles through GitHub Actions and creates a release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The `Build desktop app` workflow can also be run manually, in which case the bundles land as workflow artifacts rather than on a release.

## License

MIT. See [LICENSE](LICENSE).
