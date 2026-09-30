const STATUS = {
  idle: { label: "Idle", symbol: "−", className: "is-idle" },
  running: { label: "Running", symbol: "▶", className: "is-running" },
  waiting_approval: { label: "Waiting", symbol: "!", className: "is-waiting" },
  completed: { label: "Done", symbol: "✓", className: "is-completed" },
  error: { label: "Error", symbol: "×", className: "is-error" }
};

const pin = document.querySelector("#pin");
const agents = document.querySelector("#agents");
const setup = document.querySelector("#setup");
const setupTitle = document.querySelector("#setup-title");
const setupDetail = document.querySelector("#setup-detail");
const setupButton = document.querySelector("#setup-button");
let expanded = true;
let integrationState = null;
let setupMessage = "";
let installing = false;

pin.addEventListener("pointerdown", async (event) => {
  if (event.target.closest("button")) {
    return;
  }

  if (event.button !== 0 || event.detail > 1) {
    return;
  }

  try {
    await window.__TAURI__.core.invoke("start_window_drag");
  } catch (error) {
    console.warn("Unable to start window drag", error);
  }
});

pin.addEventListener("dblclick", async () => {
  expanded = !expanded;
  pin.classList.toggle("is-collapsed", !expanded);
  renderSetup();
});

setupButton.addEventListener("pointerdown", (event) => {
  event.stopPropagation();
});

setupButton.addEventListener("click", async (event) => {
  event.stopPropagation();
  installing = true;
  setupMessage = "Installing hooks...";
  renderSetup();

  try {
    integrationState = await window.__TAURI__.core.invoke("install_integrations");
    setupMessage = "Run /hooks in Claude and Codex to trust.";
  } catch (error) {
    setupMessage = String(error);
  } finally {
    installing = false;
    renderSetup();
  }
});

async function resizePin() {
  try {
    await window.__TAURI__.core.invoke("set_compact", {
      compact: !expanded,
      setupVisible: setupVisible()
    });
  } catch (error) {
    console.warn("Unable to resize window", error);
  }
}

function formatSecondary(snapshot) {
  if (snapshot.cwd) {
    const parts = snapshot.cwd.split(/[\\/]/).filter(Boolean);
    return parts[parts.length - 1] || snapshot.cwd;
  }

  if (snapshot.updated_at) {
    const ageSeconds = Math.max(0, Math.floor(Date.now() / 1000 - snapshot.updated_at));
    return ageSeconds < 60 ? "just now" : `${Math.floor(ageSeconds / 60)}m ago`;
  }

  return "active";
}

function render(snapshots) {
  agents.replaceChildren();

  if (!snapshots.length) {
    agents.appendChild(row({
      name: "Idle",
      status: "idle",
      cwd: "",
      updated_at: 0
    }, true));
    return;
  }

  for (const snapshot of snapshots) {
    agents.appendChild(row(snapshot, false));
  }
}

function missingIntegrations() {
  if (!integrationState) {
    return [];
  }

  const missing = [];
  if (!integrationState.claude_installed) {
    missing.push("Claude");
  }
  if (!integrationState.codex_installed) {
    missing.push("Codex");
  }
  return missing;
}

function setupVisible() {
  return Boolean(expanded && (missingIntegrations().length > 0 || setupMessage));
}

function renderSetup() {
  const missing = missingIntegrations();
  const visible = setupVisible();
  setup.hidden = !visible;

  if (!visible) {
    resizePin();
    return;
  }

  setupTitle.textContent = missing.length ? "Setup needed" : "Hooks installed";
  setupDetail.textContent = setupMessage || `${missing.join(" + ")} hooks missing`;
  setupButton.hidden = !missing.length;
  setupButton.disabled = installing;
  setupButton.textContent = installing ? "..." : "Set up";
  resizePin();
}

function row(snapshot, generic) {
  const status = STATUS[snapshot.status] || STATUS.idle;
  const node = document.createElement("div");
  node.className = `agent ${status.className}`;
  node.setAttribute("data-tauri-drag-region", "");

  const dot = document.createElement("span");
  dot.className = "agent__dot";
  dot.textContent = status.symbol;
  dot.setAttribute("data-tauri-drag-region", "");
  node.appendChild(dot);

  const initial = document.createElement("span");
  initial.className = "agent__initial";
  initial.textContent = generic ? "" : snapshot.name.slice(0, 1);
  initial.setAttribute("data-tauri-drag-region", "");
  node.appendChild(initial);

  const body = document.createElement("div");
  body.className = "agent__body";
  body.setAttribute("data-expanded-only", "");
  body.setAttribute("data-tauri-drag-region", "");

  const top = document.createElement("div");
  top.className = "agent__top";
  top.setAttribute("data-tauri-drag-region", "");

  const name = document.createElement("span");
  name.className = "agent__name";
  name.textContent = generic ? "Idle" : snapshot.name;
  name.setAttribute("data-tauri-drag-region", "");
  top.appendChild(name);

  const label = document.createElement("span");
  label.className = "agent__status";
  label.textContent = generic ? "" : status.label;
  label.setAttribute("data-tauri-drag-region", "");
  top.appendChild(label);
  body.appendChild(top);

  const secondary = document.createElement("div");
  secondary.className = "agent__secondary";
  secondary.textContent = generic ? "" : formatSecondary(snapshot);
  secondary.setAttribute("data-tauri-drag-region", "");
  body.appendChild(secondary);

  node.appendChild(body);
  return node;
}

async function refresh() {
  try {
    const snapshots = await window.__TAURI__.core.invoke("read_status");
    render(snapshots);
  } catch (error) {
    render([{
      name: "Agent Pin",
      status: "error",
      cwd: String(error),
      updated_at: Math.floor(Date.now() / 1000)
    }]);
  }
}

async function refreshIntegrations() {
  try {
    integrationState = await window.__TAURI__.core.invoke("read_integrations");
    renderSetup();
  } catch (error) {
    integrationState = null;
    setupMessage = String(error);
    renderSetup();
  }
}

refresh();
refreshIntegrations();
setInterval(refresh, 800);
setInterval(refreshIntegrations, 5000);
