import { test } from "node:test";
import assert from "node:assert/strict";

import {
  HOLD_MAX_MS,
  ageText,
  cardCatchUp,
  catchUpLines,
  collisionText,
  installedNote,
  isHeld,
  pressHold,
  setupView,
  statusMeta
} from "./view.js";

const NOW = 10_000_000_000;

test("ageText boundaries", () => {
  assert.equal(ageText(NOW, NOW), "just now");
  assert.equal(ageText(NOW - 59_999, NOW), "just now");
  assert.equal(ageText(NOW - 59_000, NOW), "just now");
  assert.equal(ageText(NOW - 60_000, NOW), "1m ago");
  assert.equal(ageText(NOW - 3_599_999, NOW), "59m ago");
  assert.equal(ageText(NOW - 3_600_000, NOW), "1h ago");
  assert.equal(ageText(NOW - 3 * 3_600_000 - 5, NOW), "3h ago");
});

test("ageText clamps a future timestamp to just now", () => {
  assert.equal(ageText(NOW + 5_000, NOW), "just now");
});

test("catchUpLines in order", () => {
  const lines = catchUpLines({
    original_ask: "build it",
    current_ask: "Bash: rm -rf build",
    files_since_seen: ["/r/a.rs", "/r/b.rs"],
    commands_since_seen: [
      { command: "cargo test", ok: true, at_ms: 1 },
      { command: "make", ok: false, at_ms: 2 }
    ],
    unseen: true
  });
  assert.deepEqual(lines, [
    "You asked: build it",
    "Since you looked: 2 files changed",
    "Ran: cargo test ✓",
    "Ran: make ✗",
    "Now asking: Bash: rm -rf build"
  ]);
});

test("catchUpLines caps commands at 3 then +K more", () => {
  const commands = ["c1", "c2", "c3", "c4", "c5"].map((command, i) => ({
    command,
    ok: true,
    at_ms: i
  }));
  const lines = catchUpLines({
    original_ask: null,
    current_ask: null,
    files_since_seen: [],
    commands_since_seen: commands,
    unseen: false
  });
  assert.deepEqual(lines, ["Ran: c3 ✓", "Ran: c4 ✓", "Ran: c5 ✓", "+2 more"]);
});

test("catchUpLines skips empty parts", () => {
  assert.deepEqual(
    catchUpLines({
      original_ask: null,
      current_ask: null,
      files_since_seen: [],
      commands_since_seen: [],
      unseen: false
    }),
    []
  );
  assert.deepEqual(catchUpLines(null), []);
});

test("collisionText returns null for no collisions", () => {
  assert.equal(collisionText([]), null);
  assert.equal(collisionText(undefined), null);
});

test("collisionText uses the path basename", () => {
  assert.equal(
    collisionText([{ path: "/repo/src/a.rs", other_label: "Codex · api", other_key: "codex__x" }]),
    "⚠ a.rs also edited by Codex · api"
  );
  assert.equal(
    collisionText([{ path: "C:\\repo\\src\\a.rs", other_label: "Codex · api", other_key: "codex__x" }]),
    "⚠ a.rs also edited by Codex · api"
  );
});

test("collisionText counts the extra collisions", () => {
  assert.equal(
    collisionText([
      { path: "/r/a.rs", other_label: "Codex · api", other_key: "codex__x" },
      { path: "/r/b.rs", other_label: "Codex · api", other_key: "codex__x" },
      { path: "/r/a.rs", other_label: "Claude · r #abcd", other_key: "claude__abcd" }
    ]),
    "⚠ a.rs also edited by Codex · api (+2)"
  );
});

test("statusMeta known statuses", () => {
  assert.deepEqual(statusMeta("running"), { label: "Running", symbol: "▶", className: "is-running" });
  assert.deepEqual(statusMeta("waiting_approval"), { label: "Waiting", symbol: "!", className: "is-waiting" });
  assert.deepEqual(statusMeta("completed"), { label: "Done", symbol: "✓", className: "is-completed" });
  assert.deepEqual(statusMeta("error"), { label: "Error", symbol: "×", className: "is-error" });
});

test("statusMeta falls back to idle for unknown statuses", () => {
  const idle = { label: "Idle", symbol: "−", className: "is-idle" };
  assert.deepEqual(statusMeta("idle"), idle);
  assert.deepEqual(statusMeta("bogus"), idle);
  assert.deepEqual(statusMeta(undefined), idle);
});

test("cardCatchUp keeps snapshot since-seen lists and live asks", () => {
  const snapshot = {
    original_ask: "old ask",
    current_ask: "old pending",
    files_since_seen: ["/r/a.rs"],
    commands_since_seen: [{ command: "make", ok: true, at_ms: 1 }],
    unseen: true
  };
  const live = {
    original_ask: "build it",
    current_ask: "Bash: ls",
    files_since_seen: [],
    commands_since_seen: [],
    unseen: false
  };
  assert.deepEqual(cardCatchUp(live, snapshot), {
    original_ask: "build it",
    current_ask: "Bash: ls",
    files_since_seen: ["/r/a.rs"],
    commands_since_seen: [{ command: "make", ok: true, at_ms: 1 }],
    unseen: false
  });
});

test("cardCatchUp without a snapshot is the live catch-up", () => {
  const live = {
    original_ask: "a",
    current_ask: null,
    files_since_seen: ["/x"],
    commands_since_seen: [],
    unseen: true
  };
  assert.deepEqual(cardCatchUp(live, undefined), live);
});

test("a non-interactive press never holds renders", () => {
  assert.equal(pressHold(false, NOW), null);
  assert.equal(isHeld(pressHold(false, NOW), NOW), false);
  assert.equal(isHeld(null, NOW), false);
});

test("an interactive press holds renders until it expires after 1.5 s", () => {
  assert.equal(HOLD_MAX_MS, 1500);
  const since = pressHold(true, NOW);
  assert.equal(since, NOW);
  assert.equal(isHeld(since, NOW), true);
  assert.equal(isHeld(since, NOW + 1499), true);
  assert.equal(isHeld(since, NOW + 1500), false);
  assert.equal(isHeld(since, NOW + 60_000), false);
});

test("catchUpLines says 1 file vs N files", () => {
  const base = { original_ask: null, current_ask: null, commands_since_seen: [], unseen: true };
  assert.deepEqual(catchUpLines({ ...base, files_since_seen: ["/r/a.rs"] }), [
    "Since you looked: 1 file changed"
  ]);
  assert.deepEqual(catchUpLines({ ...base, files_since_seen: ["/a", "/b", "/c"] }), [
    "Since you looked: 3 files changed"
  ]);
});

const NO_CONFIG = "No Claude Code or Codex config found (~/.claude or ~/.codex)";

test("installedNote reflects what was installed", () => {
  const trust = "Run /hooks in Claude Code to review and trust the new hooks";
  assert.deepEqual(installedNote(["claude"]), { title: "Hooks installed", text: trust });
  assert.deepEqual(installedNote(["claude", "codex"]), { title: "Hooks installed", text: trust });
  assert.deepEqual(installedNote(["codex"]), { title: "Hooks installed", text: "Codex hooks installed" });
  assert.deepEqual(installedNote([]), { title: "No agents found", text: NO_CONFIG });
  assert.deepEqual(installedNote(undefined), { title: "No agents found", text: NO_CONFIG });
});

test("setupView asks for setup while hooks are missing", () => {
  const view = setupView(
    { providers: [{ id: "claude", installed: false }, { id: "codex", installed: true }], wayland: false },
    null
  );
  assert.deepEqual(view, {
    visible: true,
    title: "Setup needed",
    detail: "Claude hooks missing",
    wayland: false,
    button: true
  });
});

test("setupView explains when no agent config exists", () => {
  assert.deepEqual(setupView({ providers: [], wayland: false }, null), {
    visible: true,
    title: "No agents found",
    detail: NO_CONFIG,
    wayland: false,
    button: false
  });
});

test("setupView shows a note once everything is installed, else hides", () => {
  const done = { providers: [{ id: "codex", installed: true }], wayland: false };
  assert.equal(setupView(done, null).visible, false);
  assert.deepEqual(setupView(done, installedNote(["codex"])), {
    visible: true,
    title: "Hooks installed",
    detail: "Codex hooks installed",
    wayland: false,
    button: false
  });
  const wayland = setupView({ ...done, wayland: true }, null);
  assert.equal(wayland.visible, true);
  assert.equal(wayland.wayland, true);
  assert.equal(wayland.title, "Note");
  // An unreadable integrations result shows only the note, if any.
  assert.equal(setupView(null, null).visible, false);
  assert.deepEqual(setupView(null, { title: "Setup failed", text: "boom" }), {
    visible: true,
    title: "Setup failed",
    detail: "boom",
    wayland: false,
    button: false
  });
});
