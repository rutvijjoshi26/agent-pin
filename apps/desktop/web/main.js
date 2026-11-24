// DOM glue for the overlay. Display logic lives in view.js; all text from
// rows is untrusted (prompts, commands, paths) and is set via textContent.
import { ageText, catchUpLines, collisionText, statusMeta } from "./view.js";

const POLL_MS = 800;
const INSTALLED_MESSAGE = "Run /hooks in Claude Code to review and trust the new hooks";

const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);

const pin = document.querySelector("#pin");
const agents = document.querySelector("#agents");
const setup = document.querySelector("#setup");
const setupTitle = document.querySelector("#setup-title");
const setupDetail = document.querySelector("#setup-detail");
const setupWayland = document.querySelector("#setup-wayland");
const setupButton = document.querySelector("#setup-button");

let expanded = true;
let rows = [];
let rowsError = null;
let renderedSignature = "";
const openKeys = new Set();
let integrations = null;
let setupMessage = "";
let installing = false;
let fittedSize = "";

// Drag from the pin background only; rows, cards and buttons handle clicks.
pin.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || event.detail > 1) {
    return;
  }
  if (isInteractive(event.target)) {
    return;
  }
  invoke("start_window_drag").catch((error) => console.warn("Unable to start window drag", error));
});

pin.addEventListener("dblclick", (event) => {
  if (isInteractive(event.target)) {
    return;
  }
  expanded = !expanded;
  pin.classList.toggle("is-collapsed", !expanded);
  renderSetup();
});

agents.addEventListener("click", (event) => {
  const row = event.target.closest(".agent[data-key]");
  if (!expanded || !row) {
    return;
  }
  const key = row.dataset.key;
  if (openKeys.has(key)) {
    openKeys.delete(key);
  } else {
    openKeys.add(key);
    invoke("mark_seen", { key })
      .then(refresh)
      .catch((error) => console.warn("Unable to mark seen", error));
  }
  renderRows(true);
});

setupButton.addEventListener("click", async () => {
  installing = true;
  setupMessage = "Installing hooks...";
  renderSetup();
  try {
    await invoke("install_integrations");
    setupMessage = INSTALLED_MESSAGE;
  } catch (error) {
    setupMessage = String(error);
  } finally {
    installing = false;
    await refreshIntegrations();
  }
});

new ResizeObserver(fitWindow).observe(pin);

// Buttons always take clicks; rows and cards only while expanded, since the
// collapsed pin shows bare dots and is dragged or double-clicked anywhere.
function isInteractive(target) {
  return Boolean(target.closest("button") || (expanded && target.closest(".session")));
}

function fitWindow() {
  const rect = pin.getBoundingClientRect();
  const width = Math.ceil(rect.width);
  const height = Math.ceil(rect.height);
  const size = `${width}x${height}`;
  if (width <= 0 || height <= 0 || size === fittedSize) {
    return;
  }
  fittedSize = size;
  invoke("fit_window", { width, height }).catch((error) => {
    fittedSize = "";
    console.warn("Unable to resize window", error);
  });
}

async function refresh() {
  try {
    rows = await invoke("read_rows");
    rowsError = null;
  } catch (error) {
    rows = [];
    rowsError = String(error);
  }
  renderRows(false);
}

function renderRows(force) {
  const now = Date.now();
  const liveKeys = new Set(rows.map((row) => row.key));
  for (const key of openKeys) {
    if (!liveKeys.has(key)) {
      openKeys.delete(key);
    }
  }

  // Rebuild only when something visible changed, so a click is never
  // interrupted by a poll replacing the row under the pointer.
  const signature = JSON.stringify([
    rows,
    rows.map((row) => ageText(row.updated_ms, now)),
    [...openKeys],
    rowsError
  ]);
  if (!force && signature === renderedSignature) {
    return;
  }
  renderedSignature = signature;

  if (rowsError) {
    agents.replaceChildren(placeholder("error", "Agent Pin", rowsError));
    return;
  }
  if (!rows.length) {
    agents.replaceChildren(placeholder("idle", "No active sessions", ""));
    return;
  }
  agents.replaceChildren(...rows.map((row) => session(row, now)));
}

function session(row, now) {
  const node = el("div", "session");
  const open = openKeys.has(row.key);
  const status = row.quiet ? "quiet" : statusMeta(row.status).label;
  const head = agentRow(row.status, row.label, status, ageText(row.updated_ms, now));
  head.dataset.key = row.key;
  head.classList.toggle("is-open", open);
  head.classList.toggle("is-quiet", row.quiet);
  if (row.catch_up && row.catch_up.unseen) {
    const unseen = el("span", "agent__unseen");
    unseen.title = "New since you looked";
    head.querySelector(".agent__top").insertBefore(unseen, head.querySelector(".agent__status"));
  }
  node.appendChild(head);

  const collision = collisionText(row.collisions);
  if (collision) {
    const line = el("div", "agent__collision", collision);
    line.setAttribute("data-expanded-only", "");
    line.title = row.collisions.map((c) => `${c.path} — ${c.other_label}`).join("\n");
    node.appendChild(line);
  }

  if (open) {
    node.appendChild(card(row.catch_up));
  }
  return node;
}

function card(catchUp) {
  const node = el("div", "card");
  node.setAttribute("data-expanded-only", "");
  const lines = catchUpLines(catchUp);
  if (!lines.length) {
    node.appendChild(el("div", "card__line card__line--empty", "Nothing new since you looked"));
  }
  for (const line of lines) {
    node.appendChild(el("div", "card__line", line));
  }
  return node;
}

function placeholder(statusName, label, secondary) {
  const node = el("div", "session");
  node.appendChild(agentRow(statusName, label, "", secondary));
  return node;
}

function agentRow(statusName, label, statusText, secondary) {
  const meta = statusMeta(statusName);
  const node = el("div", `agent ${meta.className}`);
  node.appendChild(el("span", "agent__dot", meta.symbol));

  const body = el("div", "agent__body");
  body.setAttribute("data-expanded-only", "");
  const top = el("div", "agent__top");
  top.appendChild(el("span", "agent__name", label));
  top.appendChild(el("span", "agent__status", statusText));
  body.appendChild(top);
  body.appendChild(el("div", "agent__secondary", secondary));
  node.appendChild(body);
  return node;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

function missingProviders() {
  if (!integrations || !Array.isArray(integrations.providers)) {
    return [];
  }
  return integrations.providers
    .filter((provider) => !provider.installed)
    .map((provider) => provider.id.charAt(0).toUpperCase() + provider.id.slice(1));
}

function renderSetup() {
  const missing = missingProviders();
  const wayland = Boolean(integrations && integrations.wayland);
  const visible = expanded && (missing.length > 0 || Boolean(setupMessage) || wayland);
  setup.hidden = !visible;
  if (!visible) {
    return;
  }

  setupTitle.textContent = missing.length ? "Setup needed" : setupMessage ? "Hooks installed" : "Note";
  const detail = setupMessage || (missing.length ? `${missing.join(" + ")} hooks missing` : "");
  setupDetail.textContent = detail;
  setupDetail.hidden = !detail;
  setupWayland.hidden = !wayland;
  setupButton.hidden = !missing.length;
  setupButton.disabled = installing;
  setupButton.textContent = installing ? "..." : "Set up";
}

async function refreshIntegrations() {
  try {
    integrations = await invoke("read_integrations");
  } catch (error) {
    integrations = null;
    setupMessage = String(error);
  }
  renderSetup();
}

refresh();
refreshIntegrations();
setInterval(refresh, POLL_MS);
