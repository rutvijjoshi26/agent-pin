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

const MAX_DETAIL_LINES = 6;

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

/** The request a row offers Allow and Deny for: only waiting rows have one. */
export function rowApproval(row, approvals) {
  return row.status === "waiting_approval" ? approvalFor(row.key, approvals) : null;
}

/** The request's summary (or tool name), then the first 6 lines of its detail. */
export function approvalLines(req) {
  if (!req) {
    return [];
  }
  const detail = req.detail ? String(req.detail).split(/\r?\n/).slice(0, MAX_DETAIL_LINES) : [];
  return [approvalTitle(req), ...detail];
}

/** "Allow Bash: rm -rf build" or "Deny Bash: rm -rf build". */
export function approvalButtonLabel(allow, req) {
  return `${allow ? "Allow" : "Deny"} ${approvalTitle(req)}`;
}

function approvalTitle(req) {
  return String(req.summary || req.tool_name || "");
}

/**
 * A click's decision, from click until the poll after `decide` settles.
 * `settledMs` is null while `decide` is in flight.
 */
export function decisionStarted(allow, nowMs) {
  return { allow, startedMs: nowMs, settledMs: null, failed: false };
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

/**
 * The decisions to keep after a poll that started at `pollStartedMs` and
 * listed `approvals`. A decision goes once its request is no longer listed;
 * a sent one also goes at the first poll that started after `decide`
 * settled, so its buttons stay disabled until then. A failed one stays
 * while its request is listed, so nothing is re-enabled.
 */
export function pruneDecisions(decisions, approvals, pollStartedMs) {
  const listed = new Set((Array.isArray(approvals) ? approvals : []).map((req) => req.id));
  const kept = new Map();
  for (const [id, decision] of decisions) {
    if (!listed.has(id)) {
      continue;
    }
    const done = !decision.failed && decision.settledMs !== null && pollStartedMs > decision.settledMs;
    if (!done) {
      kept.set(id, decision);
    }
  }
  return kept;
}

function capitalize(id) {
  const text = String(id);
  return text.charAt(0).toUpperCase() + text.slice(1);
}
