import { listConnections, startServer, stopServer, deleteConnection, startOAuthFlow, getGlobalPort, setGlobalPort, getOAuthCredentials, rotateOAuthSecret } from "./api.js";

const dashboard       = document.getElementById("dashboard");
const emptyState      = document.getElementById("empty-state");
const connectionCount = document.getElementById("connection-count");
const tmpl            = document.getElementById("tmpl-connection-card");
const settingsModal   = document.getElementById("modal-settings");
const settingsBg      = document.getElementById("settings-bg");
const settingsClose   = document.getElementById("settings-close");
const settingsClose2  = document.getElementById("btn-settings-close2");
const settingsPort    = document.getElementById("settings-port");
const btnSavePort     = document.getElementById("btn-save-port");
const settingsStatus  = document.getElementById("settings-port-status");
const btnSettings     = document.getElementById("btn-settings");

const TYPE_LABELS = {
  Filesystem:         "Folder",
  ObsidianFilesystem: "Obsidian Vault",
  RemoteProxy:        "Remote API",
};

const STATUS_CLASSES = ["stopped", "running", "starting", "error"];

// Current global port — starts at default, loaded async on init
let globalPort = 51552;
// Public base URL from Cloudflare tunnel (null when tunnel is down)
let tunnelBaseUrl = null;

function statusClass(status) {
  if (!status) return "stopped";
  if (typeof status === "string") return status.toLowerCase();
  return Object.keys(status)[0].toLowerCase();
}

function refreshCount() {
  const running = document.querySelectorAll(".connection-card.running").length;
  connectionCount.textContent = `${running} active`;
  connectionCount.className   = `tag ${running > 0 ? "is-success" : "is-light"}`;
}

function buildUrl(conn) {
  return `http://127.0.0.1:${globalPort}${conn.mcp_path || "/mcp"}`;
}

function buildPublicUrl(conn) {
  if (!tunnelBaseUrl) return null;
  return `${tunnelBaseUrl}${conn.mcp_path || "/mcp"}`;
}

/** Refresh public URL display on all cards when tunnel state changes */
function updateAllPublicUrls() {
  document.querySelectorAll(".connection-card").forEach(card => {
    if (!card._conn) return;
    updatePublicUrlOnCard(card, card._conn);
  });
}

function updatePublicUrlOnCard(card, conn) {
  const publicUrl = buildPublicUrl(conn);
  const publicRow = card.querySelector(".public-url-row");
  const publicEl  = card.querySelector(".public-url-display");
  const copyPublicBtn = card.querySelector(".copy-public-url-btn");
  if (publicRow)    publicRow.style.display    = publicUrl ? "" : "none";
  if (publicEl)     publicEl.textContent        = publicUrl ?? "";
  if (copyPublicBtn) copyPublicBtn.style.display = publicUrl ? "" : "none";
}

function updateCard(card, conn) {
  card._conn = conn;

  const sc = statusClass(conn.status);

  STATUS_CLASSES.forEach(c => card.classList.remove(c));
  card.classList.add(sc);

  const dot = card.querySelector(".status-dot");
  if (dot) {
    STATUS_CLASSES.forEach(c => dot.classList.remove(c));
    dot.classList.add(sc);
  }

  const displayName = (conn.name && conn.name.trim()) ? conn.name : (conn.connection_type ?? "Connection");
  const nameEl = card.querySelector(".connection-name");
  if (nameEl) nameEl.textContent = displayName;

  const badgeEl = card.querySelector(".type-badge");
  if (badgeEl) badgeEl.textContent = TYPE_LABELS[conn.connection_type] ?? (conn.connection_type ?? "");

  const url   = buildUrl(conn);
  const urlEl = card.querySelector(".url-display");
  if (urlEl) urlEl.textContent = sc === "running" ? url : "";

  // Root path / base URL
  const rootRow = card.querySelector(".root-path-row");
  const rootEl  = card.querySelector(".root-path-display");
  const rootVal = (conn.root_paths && conn.root_paths.length)
    ? conn.root_paths[0]
    : (conn.auth_config?.base_url ?? "");
  if (rootEl)  rootEl.textContent = rootVal;
  if (rootRow) rootRow.classList.toggle("has-value", !!rootVal);

  const startBtn = card.querySelector(".start-btn");
  const stopBtn  = card.querySelector(".stop-btn");
  if (startBtn) startBtn.style.display = sc === "running" ? "none" : "";
  if (stopBtn)  stopBtn.style.display  = sc === "running" ? "" : "none";

  const oauthRow = card.querySelector(".oauth-authorize-row");
  if (oauthRow) {
    const needsAuth = conn.auth_config?.auth_method === "oauth" && !conn.auth_config?.is_authorized;
    oauthRow.style.display = needsAuth ? "" : "none";
  }

  updatePublicUrlOnCard(card, conn);

  refreshCount();
}

/** Refresh URL display on all cards when global port changes */
function updateAllUrls() {
  document.querySelectorAll(".connection-card").forEach(card => {
    if (!card._conn) return;
    const sc    = statusClass(card._conn.status);
    const urlEl = card.querySelector(".url-display");
    if (urlEl) urlEl.textContent = sc === "running" ? buildUrl(card._conn) : "";
  });
}

export function renderCard(conn) {
  const existing = document.querySelector(`[data-connection-id="${conn.id}"]`);
  if (existing) { updateCard(existing, conn); return; }

  const card = tmpl.content.cloneNode(true).querySelector(".connection-card");
  card.dataset.connectionId = conn.id;
  updateCard(card, conn);

  card.querySelector(".start-btn").addEventListener("click", async () => {
    try {
      await startServer(conn.id);
    } catch (e) {
      alert(`Failed to start: ${e}`);
    }
  });

  card.querySelector(".stop-btn").addEventListener("click", async () => {
    try {
      await stopServer(conn.id);
    } catch (e) {
      alert(`Failed to stop: ${e}`);
    }
  });

  card.querySelector(".delete-btn").addEventListener("click", async () => {
    if (!confirm(`Delete "${conn.name}"?`)) return;
    try {
      await deleteConnection(conn.id);
      card.remove();
      if (!document.querySelector(".connection-card")) emptyState.style.display = "";
      refreshCount();
    } catch (e) {
      alert(`Failed to delete: ${e}`);
    }
  });

  card.querySelector(".edit-btn").addEventListener("click", async () => {
    const { openEditModal } = await import("./modal.js");
    openEditModal(card._conn ?? conn);
  });

  card.querySelector(".copy-url-btn").addEventListener("click", () => {
    const c   = card._conn ?? conn;
    const url = buildUrl(c);
    navigator.clipboard.writeText(url).catch(() => {});
  });

  card.querySelector(".copy-public-url-btn")?.addEventListener("click", () => {
    const c   = card._conn ?? conn;
    const url = buildPublicUrl(c);
    if (url) navigator.clipboard.writeText(url).catch(() => {});
  });

  card.querySelector(".oauth-authorize-btn")?.addEventListener("click", async () => {
    const c = card._conn ?? conn;
    try {
      await startOAuthFlow(c.id);
      const all     = await listConnections();
      const updated = all.find(x => x.id === c.id);
      if (updated) renderCard(updated);
    } catch (e) {
      alert(`OAuth authorization failed: ${e}`);
    }
  });

  emptyState.style.display = "none";
  dashboard.appendChild(card);
  refreshCount();
}

export function removeCard(id) {
  document.querySelector(`[data-connection-id="${id}"]`)?.remove();
  if (!document.querySelector(".connection-card")) emptyState.style.display = "";
  refreshCount();
}

export function updateCardStatus(id, status) {
  const card = document.querySelector(`[data-connection-id="${id}"]`);
  if (!card || !card._conn) return;
  updateCard(card, { ...card._conn, status });
}

export async function loadDashboard() {
  try {
    // Load port first so cards show correct URLs
    globalPort = await getGlobalPort();
  } catch { /* use default */ }

  try {
    const conns = await listConnections();
    conns.forEach(renderCard);
  } catch (e) {
    console.error("loadDashboard failed:", e);
  }
}

// ── Settings modal ─────────────────────────────────────────────

async function openSettings() {
  settingsPort.value = globalPort;
  settingsStatus.style.display = "none";
  settingsModal.classList.add("is-active");

  // Load OAuth credentials
  try {
    const creds = await getOAuthCredentials();
    const epEl  = document.getElementById("oauth-token-endpoint");
    const idEl  = document.getElementById("oauth-client-id");
    const secEl = document.getElementById("oauth-client-secret");
    if (epEl)  epEl.value  = creds.token_endpoint;
    if (idEl)  idEl.value  = creds.client_id;
    if (secEl) secEl.value = creds.client_secret;
  } catch { /* ignore */ }
}

function closeSettings() {
  settingsModal.classList.remove("is-active");
}

btnSettings.addEventListener("click", openSettings);
settingsBg.addEventListener("click",   closeSettings);
settingsClose.addEventListener("click",  closeSettings);
settingsClose2.addEventListener("click", closeSettings);

btnSavePort.addEventListener("click", async () => {
  const port = parseInt(settingsPort.value);
  if (!port || port < 1024 || port > 65535) {
    settingsStatus.textContent = "Port must be between 1024 and 65535.";
    settingsStatus.className   = "help is-danger";
    settingsStatus.style.display = "";
    return;
  }
  try {
    btnSavePort.classList.add("is-loading");
    await setGlobalPort(port);
    globalPort = port;
    updateAllUrls();
    settingsStatus.textContent = `Port updated to ${port}.`;
    settingsStatus.className   = "help is-success";
    settingsStatus.style.display = "";
  } catch (e) {
    settingsStatus.textContent = `Failed: ${e}`;
    settingsStatus.className   = "help is-danger";
    settingsStatus.style.display = "";
  } finally {
    btnSavePort.classList.remove("is-loading");
  }
});

export function handlePortChanged(port) {
  globalPort = port;
  updateAllUrls();
}

export function handleTunnelChanged({ url, status }) {
  tunnelBaseUrl = url ?? null;
  updateAllPublicUrls();
  updateTunnelBadge(status, url);
}

function updateTunnelBadge(status, url) {
  const item  = document.getElementById("tunnel-status-item");
  const badge = document.getElementById("tunnel-badge");
  const label = document.getElementById("tunnel-label");
  if (!item || !badge || !label) return;

  badge.className = "tag is-small mr-2";
  badge.onclick = null;

  if (status === "Active" && url) {
    badge.classList.add("is-success");
    label.textContent = "tunnel active";
    badge.title = `Cloudflare Tunnel — click to copy: ${url}`;
    badge.style.cursor = "pointer";
    badge.onclick = () => navigator.clipboard.writeText(url).catch(() => {});
  } else if (status === "Unavailable") {
    badge.classList.add("is-light");
    badge.classList.add("has-text-grey");
    label.textContent = "tunnel unavailable";
    badge.title = "Cloudflare Tunnel — binary not found or spawn failed";
    badge.style.cursor = "default";
  } else {
    // Connecting
    badge.classList.add("is-warning");
    label.textContent = "connecting…";
    badge.title = "Cloudflare Tunnel — establishing connection…";
    badge.style.cursor = "default";
  }
}

// ── OAuth settings wiring ──────────────────────────────────────

function wireOAuthSettings() {
  function makeCopy(btnId, inputId) {
    document.getElementById(btnId)?.addEventListener("click", () => {
      const val = document.getElementById(inputId)?.value;
      if (val) navigator.clipboard.writeText(val).catch(() => {});
    });
  }
  makeCopy("btn-copy-token-endpoint", "oauth-token-endpoint");
  makeCopy("btn-copy-client-id",      "oauth-client-id");
  makeCopy("btn-copy-client-secret",  "oauth-client-secret");

  document.getElementById("btn-rotate-secret")?.addEventListener("click", async () => {
    const statusEl = document.getElementById("oauth-rotate-status");
    try {
      const creds = await rotateOAuthSecret();
      document.getElementById("oauth-client-id").value    = creds.client_id;
      document.getElementById("oauth-client-secret").value = creds.client_secret;
      if (statusEl) {
        statusEl.textContent   = "Secret rotated. All existing tokens invalidated.";
        statusEl.className     = "help is-success";
        statusEl.style.display = "";
      }
    } catch (e) {
      if (statusEl) {
        statusEl.textContent   = `Failed: ${e}`;
        statusEl.className     = "help is-danger";
        statusEl.style.display = "";
      }
    }
  });
}

wireOAuthSettings();
