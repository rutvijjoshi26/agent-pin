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
- **Allow and Deny from the pin.** When Claude Code or Codex asks for permission, the session's row offers **Allow** and **Deny**. See [Approve from Agent Pin](#approve-from-agent-pin).

## Install

1. Download a release for your platform: macOS `.dmg`, Windows `.msi` or `.exe`, Linux `.deb` or `.AppImage`.
2. Open Agent Pin and click **Set up**, or run:

   ```sh
   agent-pin install-hooks
   ```

   Hooks are only installed for agents whose config directory already exists (`~/.claude` for Claude Code, `~/.codex` for Codex). They are merged into `~/.claude/settings.json` and `~/.codex/hooks.json`.
3. **Claude Code:** run `/hooks` to review and trust the new hooks. **Codex:** restart it, since it only loads hooks at session start, and run `/hooks` to trust the new hooks.

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

`agent-pin hook` always exits 0: it waits at most about 2 seconds for its input, and reports any error only on stderr. Claude Code hooks are installed with `"async": true`, except `PermissionRequest`, which is synchronous with a 600 s timeout so it can return an Allow or Deny (see [Approve from Agent Pin](#approve-from-agent-pin)). Codex hooks are installed without `async`, and its `PermissionRequest` hook has the same 600 s timeout. Every other event returns as soon as it is recorded.

One deliberate choice worth noting: Claude's `Notification` hook is **not** used to detect approval waits, because Claude also fires notifications after a run completes, which makes the signal ambiguous. Approval waits come from `PermissionRequest` instead.

## Approve from Agent Pin

When Claude Code or Codex asks for permission, the session's row shows the request with **Allow** and **Deny** buttons. It shows the request's summary, for example `Bash: rm -rf build`, and below the buttons the tool's input as formatted JSON in a scrollable box, every line of it. Input longer than 4000 characters is cut short; a request cut short cannot be allowed from Agent Pin. Its card says "Too long to show in full. Answer in the terminal." and offers **Deny** only. The buttons stay if the row flips back to running while the request waits, which subagents and parallel tool calls can cause. They are hidden while the pin is collapsed to status dots.

### How it works

- The installed `PermissionRequest` hook writes a request file to `~/.agent-pin/approvals/` (or `$AGENT_PIN_HOME/approvals/`) and waits for the app. A click writes a decision file next to it, which the hook reads within 200 ms and passes to the agent. A Deny carries the message "Denied from Agent Pin".
- Nothing listens on the network. The hook and the app share files only.
- The first click is final. Both buttons are disabled once you click, and the app never replaces a decision that has been written.
- A click affects only the request shown on that card. Each request has its own id. If one session has several requests at once, its row shows the newest.

### When the agent asks in its own prompt instead

The hook prints nothing, and the agent falls back to its normal permission prompt, when:

- **Agent Pin isn't running.** The app refreshes `app.alive` in the state directory every second, and the hook treats a file 3 s old as a stopped app. A hook started without the app returns at once. One that is waiting when the app quits or crashes returns within about 3 s.
- **Approvals are switched off.** Untick **Approve from Agent Pin** in the setup panel. This takes effect at once: a hook that is already waiting checks the switch on every 200 ms poll, returns with no decision and removes its request, so its card goes away and the agent shows its normal prompt.
- **No click within about 10 minutes.** The hook gives up after 590 s, inside its 600 s timeout.
- **The card is dropped.** The app drops a request when its session has completed, gone idle or errored, and when its hook has stopped refreshing the request file for about 5 s (the hook refreshes it every 200 ms while it waits). A waiting hook whose request is dropped returns with no decision.
- **Anything goes wrong**, such as a file that can't be written or read, or a decision file that isn't exactly an Allow or a Deny.

The hook never decides on its own. It only passes on a click.

### Interactive sessions and headless runs

In an interactive terminal session, the terminal also shows its own permission prompt, and whichever is answered first is used. This is how similar tools report Claude Code's behaviour; it has not been verified here.

After you answer in the terminal, the card may stay until the session finishes. Clicking it then has no effect.

Headless runs (`claude -p`) and background sessions have no prompt to answer, so they wait for a click whenever the app is running and approvals are on. To get their normal behaviour back, which in headless mode is a denial, switch approvals off; a run already waiting is released at once.

The Claude Code headless docs ([Run Claude Code programmatically](https://code.claude.com/docs/en/headless)) say: "In a Manual mode `claude -p` run, a call that needs approval is denied unless something approves it, such as a `PermissionRequest` hook or a `--permission-prompt-tool`." The hooks reference ([Hooks](https://code.claude.com/docs/en/hooks)) says the same for a `-p` run with no permission host: nobody can answer the prompt, so a call that would prompt is denied unless something, such as a `PermissionRequest` hook, approves it. That is the behaviour Agent Pin relies on. Other permission modes (for example `dontAsk` or `acceptEdits`) change which calls prompt at all, so a click is only asked for calls that would otherwise prompt.

### Settings and Codex

- An Allow never overrides a `deny` rule in your Claude Code settings. The hooks reference says a `PermissionRequest` hook cannot approve a call that a deny rule refuses, unless an installed Claude Code mod overrides the rule ([Hooks](https://code.claude.com/docs/en/hooks); see also [Precedence among rules, hooks, and mods](https://code.claude.com/docs/en/permissions#precedence-among-rules-hooks-and-mods)).
- Codex is supported through its `PermissionRequest` hook. The Codex hooks docs ([Hooks](https://learn.chatgpt.com/docs/hooks), which `https://developers.openai.com/codex/hooks` redirects to) say that non-managed hooks must be reviewed and trusted before they run, and that new or changed hooks are marked for review and skipped until trusted. After **Set up**, run `/hooks` in Codex to trust the updated hook. Until you do, Codex skips it and prompts as usual.

### Upgrading

Hooks installed by an older Agent Pin show "<Agent> hooks need updating" in the setup panel until you click **Update**. The update makes Claude Code's `PermissionRequest` hook synchronous with a 600 s timeout (an async hook can't return a decision), gives Codex's the same timeout, and keeps your own hooks. Claude Code users then run `/hooks` to review the change; Codex users run `/hooks` to trust it.

### Platforms

Approvals use only files, no sockets or signals, so they work the same way on macOS, Windows and Linux. The hook, the request and decision files, and the installer have tests that run in CI on all three. These parts have not been tested end to end on a real machine:

- the Tauri app itself (its approval commands are only compile-checked in CI);
- the hook's Allow or Deny output on Windows from the release build, which has no console window;
- what happens to a card after you answer in an interactive terminal instead.

## Adding another agent

Any provider id works (lowercase letters, digits and dashes). Point the agent's hooks at:

```sh
agent-pin hook <id> <EventName>
```

using Claude-style event names (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `StopFailure`, `SessionEnd`), with the hook's JSON payload on stdin. For example, `agent-pin hook antigravity PermissionRequest`.

A `PermissionRequest` that names a tool waits for Allow or Deny for any provider, as described in [Approve from Agent Pin](#approve-from-agent-pin), and prints the decision in Claude Code's format. Give that hook a timeout of at least 600 s, or switch approvals off if your agent can't use the decision.

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
