# Target architecture and refactor roadmap

## Shape: one binary, one contract, one reader

```
agent-pin (single Tauri executable)
├── src/main.rs        argv dispatch: (none) -> app | hook | install-hooks | reset
├── src/state.rs       pure core: Status enum, SessionState, hook->state table,
│                      aggregate(sessions, now) -> Vec<ProviderRow>   <- all display policy here
├── src/store.rs       state-dir resolution (AGENT_PIN_HOME honored once),
│                      write_session(atomic temp+rename), list_sessions(), remove_session()
│                      optional events.jsonl append (single write_all, rotated)
├── src/hook.rs        hook entry: parse stdin -> SessionState -> store; ALWAYS exit 0
├── src/install.rs     merge into ~/.claude/settings.json, ~/.codex/hooks.json
│                      (preserve_order, atomic write, exact-command matching, stale-path repair,
│                       only for agents whose config dir exists)
├── src/app.rs         Tauri shell: one command read_rows(); window flags
└── capabilities/default.json   start-dragging, set-size
web/  index.html, main.js (render rows + ResizeObserver -> setSize), styles.css
```

How the main flows read after the refactor:

```rust
// hook (spawned by Claude Code / Codex, async: true)
let ev = HookInput::from_stdin_lossy();
let s = state::apply(provider, hook_kind, ev, now_ms());   // table: SessionStart->idle, Prompt->running, PermissionRequest->waiting, PostToolUse(+Failure)->running, Stop->completed, StopFailure->error, SessionEnd->remove
store::write_session(&s).ok();                            // errors to stderr only
std::process::exit(0);

// app poll (800 ms)
let rows = state::aggregate(store::list_sessions(), now_ms());  // waiting > error > running > completed > idle; waiting never expires
```

### Boundaries

| Concern | Rule |
|---|---|
| Functional core (`state.rs`) | No I/O. Unit-test it with tables. |
| Imperative shell (`store`, `hook`, `install`, `app`) | Thin. |
| Provider adapter | Just the hook-to-state table per provider. Claude and Codex are data, not code paths. Any provider id is accepted. |

### Deleted or collapsed concepts

| Concept | Status after refactor |
|---|---|
| Swift app | Deleted |
| Ruby CLI and installer | Deleted |
| JSON templates | Deleted |
| Codex log scraping | Deleted |
| Antigravity special case | Deleted |
| `set_compact` and size constants | Deleted |
| `start_window_drag` | Deleted, or its attributes deleted instead |
| Provider-keyed state | Becomes session-keyed |
| `normalize` | Becomes `aggregate` with named policy constants |
| `agent-pin-reset` | Becomes `agent-pin reset`, which deletes the session files |

### Patterns

- **Adopted: functional core, imperative shell.** It gives the test seam.
- **Adopted, lightweight: anti-corruption layer.** Hook stdin is mapped once into `SessionState`.
- **Adopted: state-file-per-entity (the "pidfile/status file" pattern).** Agent Signals uses the same approach.
- **Rejected: plugin registry, event sourcing and file watching.**
  - Replaying `events.jsonl` to get current state is event sourcing nobody needs.
  - There are two providers and they are data, so a plugin registry adds nothing.
  - Polling is cheap enough.
- **Rejected: a frontend framework.** Three rows do not need one.

### What gets easier, and what becomes unsupported

**Easier:**
- Multiple sessions.
- New agents: one table entry, or zero via the CLI.
- Windows: no Ruby.
- Tests.

**Intentionally unsupported:**
- Codex sessions without hooks.
- Running from a repo checkout without building.
- A native Swift build.

---

## Roadmap

### Phase 0: stop the harm and freeze the surface (small PRs)

**Goal.** No overlay failure can affect an agent, and no new duplication is added.

**Changes.**
- The hook writer always exits 0, in both Rust and Ruby, until the Ruby one is deleted.
- Add `"async": true` to the Claude hooks.
- SessionStart maps to idle.
- PostToolUseFailure maps to running.
- Single `write_all` per append.
- Enable `preserve_order`; write atomically.
- Delete the Codex log fallback (F5, F18).

**Why first.** These are the only findings that can hurt the user's agents or config today. Each one is a few lines.

**Risk.** Low. Async hooks might reorder events. Mitigate with millisecond timestamps (F19).

**Verification.** Manual: make `~/.agent-pin` unwritable and confirm Claude tool calls still run. Diff `settings.json` key order before and after install.

### Phase 1: delete the duplicates

**Goal.** One implementation of each concern.

**Changes.**
- Remove the Swift app, `Package.swift` and `build-app.sh`.
- Remove the Ruby CLI and reset script, `install-hooks.sh` and `hooks/`.
- Add `install-hooks` and `reset` subcommands.
- Make the installer strip legacy `agent-pin-status` entries (it already does: lib.rs:373).
- Rewrite README setup around the app binary.
- Remove the template leftovers (F14) and gitignore `gen/schemas`.

**Why now.** Every later change would otherwise be made twice.

**Risk.** Users on the README Ruby path. The installer's legacy-entry removal covers migration.

**Verification.** Install on a config that already contains Ruby-path hooks and confirm exactly one Agent Pin entry per event remains.

### Phase 2: honest names and types

**Changes.**
- A `Status` enum.
- An open provider id, with Antigravity removed (F11).
- `normalize` renamed to `aggregate`, with named constants.
- A single state-dir resolver (F16).

### Phase 3: move decisions out of mechanisms

**Changes.**
- Split `lib.rs` into `state`, `store`, `hook`, `install` and `app`.
- Display policy lives only in `state::aggregate`.
- Size policy lives only in the frontend, which measures its own content (F10).
- Use the drag capability (F9).

### Phase 4: the session-keyed store

**Goal.** Make the README's multi-agent promise true.

**Changes.**
- Per-session state files written via temp file and rename.
- `SessionEnd` removes the session file.
- Aggregation by priority; waiting never expires.
- Rows show a count and age.
- `events.jsonl` becomes optional and rotated.

**Risk.** Orphaned session files from crashed agents. Handle them with a long running-TTL and `agent-pin reset`.

**Verification.** Two concurrent Claude sessions. One waits on approval while the other runs tools. The row must show waiting until approved.

### Phase 5: setup UX

**Changes.**
- Offer only installed agents.
- Detect stale exe paths and offer Repair (F17).
- Check on start and after install, not every 5 s.
- Window flags: skip the taskbar, show on all workspaces, and use the Windows subsystem after testing hook-mode stdin.

### Phase 6: tests and CI

**Changes.**
- Table tests for the hook-to-state mapping and `aggregate`.
- Merge tests on fixture configs: user hooks in the same group, non-object `hooks`, legacy entries.
- `cargo test` and `clippy` on push.

**Verification.** CI is green on macOS and Windows.
