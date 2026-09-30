# Agent Pin

An always-on-top desktop overlay that shows, at a glance, what each of your coding agents is doing right now: idle, running, waiting for your approval, completed, or errored.

Supports Claude Code and Codex today, with an event format open enough for any other agent to report into.

## The problem

Once you run more than one coding agent at a time, you lose track of them. The agents work in separate terminals or windows, so the only way to know where each one stands is to cycle through them and read the last few lines.

The expensive state is **waiting for approval**. An agent that has stopped to ask permission does nothing at all until you happen to look at it. Time spent blocked is invisible, so you find out about it late. The same applies to an agent that finished four minutes ago, or one that errored out early and has been sitting there since.

Agent Pin pulls that state out of the terminals and into one small window that stays on top of whatever you're working in. You glance at it instead of hunting for it.

## What it does

- Shows each provider's current state in a compact overlay you can collapse to a single pin.
- Tracks five states: `idle`, `running`, `waiting_approval`, `completed`, `error`.
- Reads state from agent lifecycle hooks, so it reflects what the agent is actually doing rather than polling.
- Falls back to reading the Codex TUI log, so Codex sessions that were already running before you launched the app still resolve to a correct state.
- Appends every state change to `~/.agent-pin/events.jsonl`, which you can tail or build on.

## Requirements

- macOS or Windows.
- **Ruby**, for the status CLI and the hook commands. Preinstalled on macOS; on Windows install it from [rubyinstaller.org](https://rubyinstaller.org/).
- **Node.js 18+** and the **Rust toolchain** ([rustup](https://rustup.rs/)), to build the desktop app. Tauri needs Rust to compile.
- **Swift 5.9+ / Xcode**, only if you want to run the macOS-native prototype instead of the Tauri app. Optional.

## Setup

### 1. Clone the repo

```sh
git clone https://github.com/rutvijjoshi26/agent-pin.git
cd agent-pin
```

### 2. Build the desktop app

```sh
cd apps/desktop
npm install
npm run build
cd ../..
```

On macOS this produces:

```text
apps/desktop/src-tauri/target/release/bundle/macos/Agent Pin.app
apps/desktop/src-tauri/target/release/bundle/dmg/Agent Pin_0.1.0_aarch64.dmg
```

On Windows the same command writes installer artifacts under `apps/desktop/src-tauri/target/release/bundle/`.

To develop against it instead of building a bundle, use `npm run dev`. On macOS you can also run the Swift prototype with `swift run AgentPin` from the repo root.

### 3. Install the agent hooks

```sh
scripts/install-hooks.sh
```

This merges Agent Pin's hook entries into:

```text
~/.claude/settings.json
~/.codex/hooks.json
```

It makes a timestamped backup of each file before touching it, and it removes any previous Agent Pin entries rather than duplicating them, so the script is safe to re-run.

### 4. Trust the hooks

Both agents require you to approve hook commands before they will run.

- **Claude Code:** run `/hooks` and trust the Agent Pin commands.
- **Codex:** restart Codex first, since it only loads hooks at session start and silently skips untrusted ones. Then run `/hooks` and trust the commands.

### 5. Verify it works

With the app open, send some events by hand and watch the overlay change:

```sh
bin/agent-pin-status claude running
bin/agent-pin-status codex waiting_approval
bin/agent-pin-status claude completed
bin/agent-pin-reset
```

If the overlay reacts to these but not to real agent activity, the hooks aren't installed or aren't trusted yet. Revisit steps 3 and 4.

## How it works

```text
Claude Code / Codex
      |  lifecycle hooks
      v
bin/agent-pin-status  (Ruby CLI, validates provider + status)
      |
      v
~/.agent-pin/events.jsonl  <--- also fed by the Codex log reader
      |
      v
Agent Pin overlay
```

Eight Claude Code lifecycle events map onto the five states, covering session start, prompt submission, tool use, approval requests, tool failures, and stop. The CLI validates both the provider and the status before writing, so a malformed hook can't put the overlay into an undefined state.

One deliberate choice worth noting: Claude's `Notification` hook is **not** used to detect approval waits, because Claude also fires notifications after a run completes, which makes the signal ambiguous. Approval waits come from `PermissionRequest` instead.

## Adding another agent

Anything that can append a line of JSON can report into Agent Pin. Either call the CLI:

```sh
bin/agent-pin-status <claude|codex|antigravity> <idle|running|waiting_approval|completed|error> [detail]
```

or write the same event shape to `~/.agent-pin/events.jsonl` directly.

Antigravity is accepted as a provider, but it does not currently expose a stable hook stream comparable to Claude Code's or Codex's, so its state has to be reported this way until an adapter exists.

## Building a release

Pushing a tag builds macOS and Windows bundles through GitHub Actions and creates a release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The `Build desktop app` workflow can also be run manually, in which case the bundles land as workflow artifacts rather than on a release.

These builds are unsigned, so macOS and Windows will warn before running them. Code signing needs paid developer accounts and isn't set up here.

## Status of the project

An early prototype that I use daily, not a polished product. There is no automated test suite yet, the Antigravity path depends on an adapter that doesn't exist, and the release builds are unsigned. Issues and pull requests are welcome.

## License

MIT. See [LICENSE](LICENSE).
