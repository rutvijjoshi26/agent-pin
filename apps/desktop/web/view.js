// Pure display helpers for the overlay. No DOM or Tauri access, so
// `node --test apps/desktop/web/` can import this module directly.

const STATUS = {
  idle: { label: "Idle", symbol: "−", className: "is-idle" },
  running: { label: "Running", symbol: "▶", className: "is-running" },
  waiting_approval: { label: "Waiting", symbol: "!", className: "is-waiting" },
  completed: { label: "Done", symbol: "✓", className: "is-completed" },
  error: { label: "Error", symbol: "×", className: "is-error" }
};

const MAX_COMMANDS = 3;

/** The status's label, dot symbol and CSS class; unknown statuses read as idle. */
export function statusMeta(status) {
  return Object.prototype.hasOwnProperty.call(STATUS, status) ? STATUS[status] : STATUS.idle;
}

/** "just now" under a minute, then "Nm ago", then "Nh ago". */
export function ageText(updatedMs, nowMs) {
  const seconds = Math.floor(Math.max(0, nowMs - updatedMs) / 1000);
  if (seconds < 60) {
    return "just now";
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m ago`;
  }
  return `${Math.floor(seconds / 3600)}h ago`;
}

/**
 * The catch-up card's lines: the original ask, files changed since the user
 * looked, the most recent commands (at most 3, then "+K more") and the
 * current ask. Empty parts are left out.
 */
export function catchUpLines(catchUp) {
  if (!catchUp) {
    return [];
  }
  const lines = [];
  if (catchUp.original_ask) {
    lines.push(`You asked: ${catchUp.original_ask}`);
  }
  const files = catchUp.files_since_seen || [];
  if (files.length > 0) {
    lines.push(`Since you looked: ${files.length} ${files.length === 1 ? "file" : "files"} changed`);
  }
  const commands = catchUp.commands_since_seen || [];
  for (const run of commands.slice(-MAX_COMMANDS)) {
    lines.push(`Ran: ${run.command} ${run.ok ? "✓" : "✗"}`);
  }
  if (commands.length > MAX_COMMANDS) {
    lines.push(`+${commands.length - MAX_COMMANDS} more`);
  }
  if (catchUp.current_ask) {
    lines.push(`Now asking: ${catchUp.current_ask}`);
  }
  return lines;
}

/**
 * What an open card shows. Marking a row seen empties the backend's
 * since-seen lists, so the files and commands come from the snapshot taken
 * when the card opened, while the asks stay live.
 */
export function cardCatchUp(live, snapshot) {
  if (!snapshot) {
    return live;
  }
  return {
    ...(live || snapshot),
    files_since_seen: snapshot.files_since_seen,
    commands_since_seen: snapshot.commands_since_seen
  };
}

/** "⚠ a.rs also edited by Codex · api", plus " (+N)" for N more; null if none. */
export function collisionText(collisions) {
  if (!collisions || collisions.length === 0) {
    return null;
  }
  const first = collisions[0];
  const extra = collisions.length > 1 ? ` (+${collisions.length - 1})` : "";
  return `⚠ ${basename(first.path)} also edited by ${first.other_label}${extra}`;
}

function basename(path) {
  const parts = String(path).split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] || String(path);
}

/** Longest a pointer press may hold back row rebuilds. */
export const HOLD_MAX_MS = 1500;

/**
 * When a press starts holding rebuilds: only presses on interactive targets
 * (rows, cards, buttons) hold, so a press that starts a native window drag,
 * whose pointerup the OS may swallow, never does. Returns the start time or null.
 */
export function pressHold(interactive, nowMs) {
  return interactive ? nowMs : null;
}

/** True while a hold that started at `holdSince` is still in force. */
export function isHeld(holdSince, nowMs) {
  return holdSince !== null && nowMs - holdSince < HOLD_MAX_MS;
}

const NO_CONFIG = "No Claude Code or Codex config found (~/.claude or ~/.codex)";

// What to run after installing each provider's hooks.
const TRUST_LINES = {
  claude: "Claude: run /hooks in Claude Code to review and trust the hooks",
  codex: "Codex: run /hooks in Codex to trust the updated hooks"
};

/**
 * The setup note after an install, from the provider ids it installed: one
 * line per installed provider saying how to trust its hooks.
 */
export function installedNote(ids) {
  const list = Array.isArray(ids) ? ids : [];
  if (list.length === 0) {
    return { title: "No agents found", text: NO_CONFIG };
  }
  const lines = list.map((id) => TRUST_LINES[id] || `${capitalize(id)} hooks installed`);
  return { title: "Hooks installed", text: lines.join("\n") };
}

// A provider's hook status; one without a status reads from `installed`.
function hookStatus(provider) {
  if (provider.status === "missing" || provider.status === "stale" || provider.status === "current") {
    return provider.status;
  }
  return provider.installed ? "current" : "missing";
}

/**
 * What the setup panel shows, from read_integrations' result (null when it
 * failed) and the current note ({ title, text } or null). Missing hooks are
 * offered "Set up"; hooks from an older Agent Pin are offered "Update".
 */
export function setupView(integrations, note) {
  const providers = integrations && Array.isArray(integrations.providers) ? integrations.providers : null;
  const lines = [];
  let anyMissing = false;
  for (const provider of providers || []) {
    const status = hookStatus(provider);
    if (status === "missing") {
      anyMissing = true;
      lines.push(`${capitalize(provider.id)} hooks missing`);
    } else if (status === "stale") {
      lines.push(`${capitalize(provider.id)} hooks need updating`);
    }
  }
  const noConfig = providers !== null && providers.length === 0;
  const wayland = Boolean(integrations && integrations.wayland);
  let title = "Note";
  let detail = "";
  if (lines.length > 0) {
    title = anyMissing ? "Setup needed" : "Update needed";
    detail = note ? note.text : lines.join("\n");
  } else if (note) {
    title = note.title;
    detail = note.text;
  } else if (noConfig) {
    title = "No agents found";
    detail = NO_CONFIG;
  }
  return {
    visible: Boolean(detail) || wayland,
    title,
    detail,
    wayland,
    button: lines.length > 0,
    buttonLabel: lines.length > 0 && !anyMissing ? "Update" : "Set up"
  };
}

/**
 * The pending approval request for a row (key `provider__session_key`), or
 * null. With several, the newest by created_ms wins.
 */
export function approvalFor(rowKey, approvals) {
  let best = null;
  for (const req of Array.isArray(approvals) ? approvals : []) {
    if (`${req.provider}__${req.session_key}` !== rowKey) {
      continue;
    }
    if (best === null || req.created_ms > best.created_ms) {
      best = req;
    }
  }
  return best;
}

/**
 * The request a row offers Allow and Deny for. Waiting and running rows can
 * have one: a subagent or a parallel tool call can flip a waiting row to
 * running while its hook still waits.
 */
export function rowApproval(row, approvals) {
  const live = row.status === "waiting_approval" || row.status === "running";
  return live ? approvalFor(row.key, approvals) : null;
}

/**
 * Everything being approved: the request's summary (or tool name), then
 * every line of its detail. Lines are kept whole; the view scrolls and
 * wraps them rather than clipping.
 */
export function approvalLines(req) {
  if (!req) {
    return [];
  }
  const all = req.detail ? String(req.detail).split(/\r?\n/) : [];
  return [approvalTitle(req), ...all];
}

/**
 * Which buttons a request offers. A request whose detail was cut cannot be
 * judged from what is shown, so it is Deny only, with a note.
 */
export function approvalChoices(req) {
  if (req && req.truncated === true) {
    return { allow: false, deny: true, note: "Too long to show in full. Answer in the terminal." };
  }
  return { allow: true, deny: true, note: null };
}

/** "Allow Bash: rm -rf build" or "Deny Bash: rm -rf build". */
export function approvalButtonLabel(allow, req) {
  return `${allow ? "Allow" : "Deny"} ${approvalTitle(req)}`;
}

function approvalTitle(req) {
  return String(req.summary || req.tool_name || "");
}

/**
 * A click's decision on the row `rowKey`, kept until shortly after its
 * request leaves read_approvals. `settledMs` is null while `decide` is in
 * flight; `goneMs` is when its request was first seen gone, or null.
 */
export function decisionStarted(allow, nowMs, rowKey = null) {
  return { allow, rowKey, startedMs: nowMs, settledMs: null, failed: false, goneMs: null };
}

/** The decision once `decide` resolved. */
export function decisionSent(decision, nowMs) {
  return { ...decision, settledMs: nowMs };
}

/** The decision once `decide` rejected: the request is no longer pending. */
export function decisionFailed(decision, nowMs) {
  return { ...decision, settledMs: nowMs, failed: true };
}

/**
 * How a request's buttons look: enabled with no decision; once clicked,
 * disabled with a note. A failed decide never reads as success.
 */
export function decisionView(decision) {
  if (!decision) {
    return { disabled: false, note: null };
  }
  if (decision.failed) {
    return { disabled: true, note: "No longer pending" };
  }
  if (decision.settledMs === null) {
    return { disabled: true, note: "Sending..." };
  }
  return { disabled: true, note: decision.allow ? "Allowed" : "Denied" };
}

/** How long a decision's note stays after its request leaves the list. */
export const SETTLED_NOTE_MS = 1500;

/**
 * The decisions to keep after a poll at `nowMs` that listed `approvals`.
 * Each one stays, its buttons disabled and its outcome shown, while its
 * request is listed; once the request is gone its note alone stays for
 * SETTLED_NOTE_MS (see lingeringNote). A decision is never re-enabled.
 */
export function pruneDecisions(decisions, approvals, nowMs) {
  const listed = new Set((Array.isArray(approvals) ? approvals : []).map((req) => req.id));
  const kept = new Map();
  for (const [id, decision] of decisions) {
    if (listed.has(id)) {
      kept.set(id, decision);
      continue;
    }
    const goneMs = decision.goneMs ?? nowMs;
    if (nowMs - goneMs < SETTLED_NOTE_MS) {
      kept.set(id, decision.goneMs === goneMs ? decision : { ...decision, goneMs });
    }
  }
  return kept;
}

/**
 * The note a row shows, without buttons, for its newest decision whose
 * request has left the list; null when there is none.
 */
export function lingeringNote(rowKey, decisions) {
  let newest = null;
  for (const decision of decisions.values()) {
    if (decision.rowKey !== rowKey || decision.goneMs === null) {
      continue;
    }
    if (newest === null || decision.startedMs > newest.startedMs) {
      newest = decision;
    }
  }
  return newest ? decisionView(newest).note : null;
}

/**
 * True when a press at `offsetX` (from the element's padding edge) is on
 * its vertical scrollbar, right of its `clientWidth`. Such a press scrolls
 * and must not start a window drag.
 */
export function onScrollbar(offsetX, clientWidth) {
  return offsetX >= clientWidth;
}

function capitalize(id) {
  const text = String(id);
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** A decide click is ignored for this long after the layout last moved. */
export const CLICK_GUARD_MS = 500;

/**
 * Tracks when each shown request first appeared and when the approvals set
 * or row order last changed, so a click aimed at one button cannot land on a
 * different Allow that slid under the pointer. `previous` is the last
 * tracker (or null); `shownIds` are the request ids on screen and `rowKeys`
 * the rows in order. Returns a new tracker; the input is not mutated.
 */
export function trackLayout(previous, shownIds, rowKeys, nowMs) {
  const signature = JSON.stringify([[...shownIds].sort(), rowKeys]);
  const firstSeen = new Map();
  for (const id of shownIds) {
    firstSeen.set(id, previous && previous.firstSeen.has(id) ? previous.firstSeen.get(id) : nowMs);
  }
  const changedMs = previous && previous.signature === signature ? previous.changedMs : nowMs;
  return { firstSeen, signature, changedMs };
}

/**
 * Whether a click on a decide button for request `id` is accepted: not if
 * that request first rendered, or the layout last changed, less than
 * CLICK_GUARD_MS ago, and not if it is not shown at all.
 */
export function decideAccepted(tracker, id, nowMs) {
  if (!tracker || !tracker.firstSeen.has(id)) {
    return false;
  }
  return (
    nowMs - tracker.firstSeen.get(id) >= CLICK_GUARD_MS && nowMs - tracker.changedMs >= CLICK_GUARD_MS
  );
}
