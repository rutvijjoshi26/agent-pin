// DOM glue for the overlay. Display logic lives in view.js; all text from
// rows is untrusted (prompts, commands, paths) and is set via textContent.
import {
  HOLD_MAX_MS,
  ageText,
  approvalButtonLabel,
  approvalLines,
  cardCatchUp,
  catchUpLines,
  collisionText,
  decisionFailed,
  decisionSent,
  decisionStarted,
  decisionView,
  installedNote,
  isHeld,
  pressHold,
  pruneDecisions,
  rowApproval,
  setupView,
  statusMeta
} from "./view.js";

const POLL_MS = 800;

const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);

const pin = document.querySelector("#pin");
const agents = document.querySelector("#agents");
const setup = document.querySelector("#setup");
const setupNoteBox = document.querySelector("#setup-note");
const approvalsToggle = document.querySelector("#approvals-toggle");
const setupTitle = document.querySelector("#setup-title");
const setupDetail = document.querySelector("#setup-detail");
const setupWayland = document.querySelector("#setup-wayland");
const setupButton = document.querySelector("#setup-button");

let expanded = true;
let rows = [];
let rowsError = null;
// Pending approval requests from the last poll (see read_approvals).
let approvals = [];
// Clicked Allow/Deny decisions by request id (see view.js decisionStarted).
let decisions = new Map();
let renderedSignature = "";
// Open cards, keyed by row key, mapped to the row's catch_up as it was just
// before mark_seen emptied the backend's since-seen lists.
const openCards = new Map();
// Start time of a press that holds row rebuilds (see pressHold), or null.
let holdSince = null;
let holdTimer = null;
let renderPending = false;
let integrations = null;
// The setup panel's note ({ title, text }) after an install or error, or null.
let setupNote = null;
let installing = false;
let fittedSize = "";

// Drag from the pin background only; rows, cards and buttons handle clicks.
// A press on those holds row rebuilds, so a poll can't replace the element
// under the pointer and swallow the click. A press that starts a native drag
// never holds: the OS drag loop may swallow its pointerup.
pin.addEventListener("pointerdown", (event) => {
  if (event.button !== 0) {
    return;
  }
  holdSince = pressHold(isInteractive(event.target), Date.now());
  if (holdSince !== null) {
    clearTimeout(holdTimer);
    holdTimer = setTimeout(releaseHold, HOLD_MAX_MS);
    return;
  }
  releaseHold();
  if (event.detail > 1) {
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
  const button = event.target.closest("button[data-decide]");
  if (button) {
    decideFrom(button);
    return;
  }
  const row = event.target.closest(".agent[data-key]");
  if (!expanded || !row) {
    return;
  }
  const key = row.dataset.key;
  if (openCards.has(key)) {
    openCards.delete(key);
  } else {
    const live = rows.find((r) => r.key === key);
    openCards.set(key, live ? live.catch_up : null);
    invoke("mark_seen", { key })
      .then(refresh)
      .catch((error) => console.warn("Unable to mark seen", error));
  }
  renderRows(true);
});

// Sends one decision. Both buttons stay disabled, with the outcome shown,
// until the request leaves read_approvals; a rejected decide shows "No
// longer pending". The backend never replaces a decision either.
function decideFrom(button) {
  const id = button.dataset.id;
  if (!id || decisions.has(id)) {
    return;
  }
  const allow = button.dataset.decide === "allow";
  for (const sibling of button.parentElement.querySelectorAll("button")) {
    sibling.disabled = true;
  }
  decisions.set(id, decisionStarted(allow, Date.now()));
  renderRows(true);
  invoke("decide", { id, allow })
    .then(() => settle(id, decisionSent))
    .catch((error) => {
      console.warn("Unable to decide", error);
      settle(id, decisionFailed);
    });
}

function settle(id, next) {
  const decision = decisions.get(id);
  if (decision) {
    decisions.set(id, next(decision, Date.now()));
    renderRows(true);
  }
}

for (const type of ["pointerup", "pointercancel", "blur"]) {
  window.addEventListener(type, releaseHold);
}

// Ends a hold; a held rebuild runs after the click has been dispatched.
function releaseHold() {
  holdSince = null;
  clearTimeout(holdTimer);
  if (renderPending) {
    setTimeout(() => renderRows(true), 0);
  }
}

approvalsToggle.addEventListener("change", async () => {
  const on = approvalsToggle.checked;
  approvalsToggle.disabled = true;
  try {
    await invoke("set_approvals_enabled", { on });
  } catch (error) {
    setupNote = { title: "Approvals switch failed", text: String(error) };
    renderSetup();
  } finally {
    await refreshApprovalsEnabled();
    approvalsToggle.disabled = false;
  }
});

setupButton.addEventListener("click", async () => {
  installing = true;
  setupNote = { title: "Setting up", text: "Installing hooks..." };
  renderSetup();
  try {
    setupNote = installedNote(await invoke("install_integrations"));
  } catch (error) {
    setupNote = { title: "Setup failed", text: String(error) };
  } finally {
    installing = false;
    await refreshIntegrations();
  }
});

new ResizeObserver(fitWindow).observe(pin);

// Buttons and the approvals switch always take clicks; rows and cards only
// while expanded, since the collapsed pin shows bare dots and is dragged or
// double-clicked anywhere.
function isInteractive(target) {
  return Boolean(target.closest("button, input, label") || (expanded && target.closest(".session")));
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
  const [rowsResult, approvalsResult] = await Promise.allSettled([
    invoke("read_rows"),
    invoke("read_approvals")
  ]);
  if (rowsResult.status === "fulfilled") {
    rows = rowsResult.value;
    rowsError = null;
  } else {
    rows = [];
    rowsError = String(rowsResult.reason);
  }
  if (approvalsResult.status === "fulfilled") {
    approvals = approvalsResult.value;
  } else {
    approvals = [];
    console.warn("Unable to read approvals", approvalsResult.reason);
  }
  decisions = pruneDecisions(decisions, approvals);
  renderRows(false);
}

function renderRows(force) {
  if (isHeld(holdSince, Date.now())) {
    renderPending = true;
    return;
  }
  renderPending = false;
  const now = Date.now();
  const liveKeys = new Set(rows.map((row) => row.key));
  for (const key of openCards.keys()) {
    if (!liveKeys.has(key)) {
      openCards.delete(key);
    }
  }

  // Rebuild only when something visible changed.
  const signature = JSON.stringify([
    rows,
    rows.map((row) => ageText(row.updated_ms, now)),
    [...openCards.keys()],
    rowsError,
    approvals,
    [...decisions]
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
  // A rebuild replaces the buttons, so keep keyboard focus on the same one.
  const focused = document.activeElement && document.activeElement.dataset
    ? document.activeElement.dataset.focusId
    : undefined;
  agents.replaceChildren(...rows.map((row) => session(row, now)));
  if (focused) {
    const again = [...agents.querySelectorAll("[data-focus-id]")].find((node) => node.dataset.focusId === focused);
    if (again && !again.disabled) {
      again.focus();
    }
  }
}

function session(row, now) {
  const node = el("div", "session");
  const open = openCards.has(row.key);
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

  const request = rowApproval(row, approvals);
  if (request) {
    node.appendChild(approvalStrip(request));
  }

  if (open) {
    node.appendChild(card(cardCatchUp(row.catch_up, openCards.get(row.key))));
  }
  return node;
}

// Everything being approved (summary, then detail, wrapped, never clipped)
// with Allow and Deny. Not inside .agent[data-key], so a click on it never
// toggles the card.
function approvalStrip(request) {
  const view = decisionView(decisions.get(request.id));
  const node = el("div", "approval");
  node.setAttribute("data-expanded-only", "");
  const [summary, ...detail] = approvalLines(request);
  node.appendChild(el("div", "approval__summary", summary));
  if (detail.length) {
    const block = el("div", "approval__detail");
    for (const line of detail) {
      block.appendChild(el("div", "approval__detail-line", line));
    }
    node.appendChild(block);
  }
  const actions = el("div", "approval__actions");
  for (const allow of [true, false]) {
    const button = el(
      "button",
      `approval__button ${allow ? "approval__button--allow" : "approval__button--deny"}`,
      allow ? "Allow" : "Deny"
    );
    button.type = "button";
    button.dataset.decide = allow ? "allow" : "deny";
    button.dataset.id = request.id;
    button.dataset.focusId = `${request.id}:${button.dataset.decide}`;
    button.setAttribute("aria-label", approvalButtonLabel(allow, request));
    button.disabled = view.disabled;
    actions.appendChild(button);
  }
  node.appendChild(actions);
  if (view.note) {
    const note = el("div", "approval__note", view.note);
    note.setAttribute("role", "status");
    node.appendChild(note);
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

// Not a .session, so dragging works on the placeholder row.
function placeholder(statusName, label, secondary) {
  const node = el("div", "placeholder");
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

// The setup section always holds the approvals switch; its note shows only
// when there is something to say.
function renderSetup() {
  const view = setupView(integrations, setupNote);
  setup.hidden = !expanded;
  setupNoteBox.hidden = !view.visible;
  if (!expanded || !view.visible) {
    return;
  }

  setupTitle.textContent = view.title;
  setupDetail.textContent = view.detail;
  setupDetail.hidden = !view.detail;
  setupWayland.hidden = !view.wayland;
  setupButton.hidden = !view.button;
  setupButton.disabled = installing;
  setupButton.textContent = installing ? "..." : view.buttonLabel;
}

async function refreshApprovalsEnabled() {
  try {
    approvalsToggle.checked = Boolean(await invoke("approvals_enabled"));
  } catch (error) {
    console.warn("Unable to read the approvals switch", error);
  }
}

async function refreshIntegrations() {
  try {
    integrations = await invoke("read_integrations");
  } catch (error) {
    integrations = null;
    setupNote = { title: "Setup check failed", text: String(error) };
  }
  renderSetup();
}

refresh();
refreshIntegrations();
refreshApprovalsEnabled();
setInterval(refresh, POLL_MS);
