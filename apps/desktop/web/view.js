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
    lines.push(`Since you looked: ${files.length} files changed`);
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
