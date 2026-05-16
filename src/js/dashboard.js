import { listConnections, startServer, stopServer, deleteConnection, startOAuthFlow } from "./api.js";

const dashboard = document.getElementById("dashboard");
const emptyState = document.getElementById("empty-state");
const connectionCount = document.getElementById("connection-count");
const tmpl = document.getElementById("tmpl-connection-card");

const TYPE_LABELS = {
  Filesystem: "Folder",
  ObsidianFilesystem: "Obsidian Vault",
  RemoteProxy: "Remote API",
};

const STATUS_CLASSES = ["stopped", "running", "starting", "error"];

function statusClass(status) {
  if (!status) return "stopped";
  if (typeof status === "string") return status.toLowerCase();
  return Object.keys(status)[0].toLowerCase();
}

function refreshCount() {
  const running = document.querySelectorAll(".connection-card.running").length;
  connectionCount.textContent = `${running} active`;
  connectionCount.className = `tag ${running > 0 ? "is-success" : "is-light"}`;
}

function updateCard(card, conn) {
  card._conn = conn;

  const sc = statusClass(conn.status);

  // Card border class
  STATUS_CLASSES.forEach(c => card.classList.remove(c));
  card.classList.add(sc);

  // Status dot
  const dot = card.querySelector(".status-dot");
  if (dot) {
    STATUS_CLASSES.forEach(c => dot.classList.remove(c));
    dot.classList.add(sc);
  }

  // Name + badge
  const displayName = (conn.name && conn.name.trim()) ? conn.name : (conn.connection_type ?? "Connection");
  const nameEl = card.querySelector(".connection-name");
  if (nameEl) nameEl.textContent = displayName;

  const badgeEl = card.querySelector(".type-badge");
  if (badgeEl) badgeEl.textContent = TYPE_LABELS[conn.connection_type] ?? (conn.connection_type ?? "");

  // URL
  const scheme = conn.use_https ? "https" : "http";
  const mcpPath = conn.mcp_path || "/mcp";
  const url = `${scheme}://127.0.0.1:${conn.port}${mcpPath}`;
  const urlEl = card.querySelector(".url-display");
  if (urlEl) urlEl.textContent = sc === "running" ? url : "";

  // Port
  const portEl = card.querySelector(".port-display");
  if (portEl) portEl.textContent = conn.port ?? "";

  // Root path / base URL
  const rootRow = card.querySelector(".root-path-row");
  const rootEl  = card.querySelector(".root-path-display");
  const rootVal = (conn.root_paths && conn.root_paths.length)
    ? conn.root_paths[0]
    : (conn.auth_config?.base_url ?? "");
  if (rootEl) rootEl.textContent = rootVal;
  if (rootRow) rootRow.classList.toggle("has-value", !!rootVal);

  // Start / stop buttons
  const startBtn = card.querySelector(".start-btn");
  const stopBtn  = card.querySelector(".stop-btn");
  if (startBtn) startBtn.style.display = sc === "running" ? "none" : "";
  if (stopBtn)  stopBtn.style.display  = sc === "running" ? "" : "none";

  // Authorize button — only for OAuth connections that haven't been authorized yet
  const oauthRow = card.querySelector(".oauth-authorize-row");
  if (oauthRow) {
    const needsAuth = conn.auth_config?.auth_method === "oauth" && !conn.auth_config?.is_authorized;
    oauthRow.style.display = needsAuth ? "" : "none";
  }

  refreshCount();
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

  card.querySelector(".copy-url-btn").addEventListener("click", () => {
    const c = card._conn ?? conn;
    const scheme = c.use_https ? "https" : "http";
    const url = `${scheme}://127.0.0.1:${c.port}${c.mcp_path || "/mcp"}`;
    navigator.clipboard.writeText(url).catch(() => {});
  });

  card.querySelector(".oauth-authorize-btn")?.addEventListener("click", async () => {
    const c = card._conn ?? conn;
    try {
      await startOAuthFlow(c.id);
      // Re-fetch this connection so the Authorize button hides
      const all = await listConnections();
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
    const conns = await listConnections();
    conns.forEach(renderCard);
  } catch (e) {
    console.error("loadDashboard failed:", e);
  }
}
