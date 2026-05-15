import { listConnections, startServer, stopServer, deleteConnection } from "./api.js";

const dashboard   = document.getElementById("dashboard");
const emptyState  = document.getElementById("empty-state");
const countBadge  = document.getElementById("connection-count");
const tmpl        = document.getElementById("tmpl-connection-card");

const TYPE_LABELS = {
  Filesystem:         "Filesystem",
  ObsidianFilesystem: "Obsidian Vault",
  RemoteProxy:        "Remote API",
};

const STATUS_CLASSES = ["running", "starting", "error", "stopped"];

function statusClass(status) {
  if (!status) return "stopped";
  if (typeof status === "string") return status.toLowerCase();
  return Object.keys(status)[0].toLowerCase();
}

export function renderCard(conn) {
  const existing = document.querySelector(`[data-connection-id="${conn.id}"]`);
  if (existing) { updateCard(existing, conn); return; }

  const card = tmpl.content.cloneNode(true).querySelector(".connection-card");
  card.dataset.connectionId = conn.id;
  updateCard(card, conn);

  card.querySelector(".start-btn").addEventListener("click",  () => onStart(conn.id));
  card.querySelector(".stop-btn").addEventListener("click",   () => onStop(conn.id));
  card.querySelector(".delete-btn").addEventListener("click", () => onDelete(conn.id));
  card.querySelector(".copy-url-btn").addEventListener("click", () => onCopyUrl(conn));

  emptyState.style.display = "none";
  dashboard.appendChild(card);
  refreshCount();
}

function updateCard(card, conn) {
  const sc = statusClass(conn.status);
  STATUS_CLASSES.forEach(c => card.classList.remove(c));
  card.classList.add(sc);

  const dot = card.querySelector(".status-dot");
  STATUS_CLASSES.forEach(c => dot.classList.remove(c));
  dot.classList.add(sc);

  card.querySelector(".connection-name").textContent = conn.name;
  card.querySelector(".type-badge").textContent = TYPE_LABELS[conn.connection_type] ?? conn.connection_type;

  const url = `http://127.0.0.1:${conn.port}/mcp`;
  card.querySelector(".url-display").textContent = sc === "running" ? url : `Port ${conn.port} — stopped`;

  const paths = conn.root_paths ?? [];
  card.querySelector(".root-path-display").textContent = paths.length ? paths[0] : (conn.auth_config?.base_url ?? "");

  const startBtn = card.querySelector(".start-btn");
  const stopBtn  = card.querySelector(".stop-btn");
  startBtn.style.display = sc === "running" ? "none" : "";
  stopBtn.style.display  = sc === "running" ? "" : "none";
}

export function removeCard(id) {
  document.querySelector(`[data-connection-id="${id}"]`)?.remove();
  if (!document.querySelector(".connection-card")) emptyState.style.display = "";
  refreshCount();
}

export function updateCardStatus(id, status) {
  const card = document.querySelector(`[data-connection-id="${id}"]`);
  if (!card) return;
  updateCard(card, { id, status, port: card.querySelector(".url-display").textContent.match(/\d+/)?.[0] ?? 50000 });
}

function refreshCount() {
  const n = document.querySelectorAll(".connection-card").length;
  const running = document.querySelectorAll(".connection-card.running").length;
  countBadge.textContent = `${running} active`;
  countBadge.className = `tag ${running > 0 ? "is-success" : "is-light"}`;
}

async function onStart(id) {
  try { await startServer(id); } catch (e) { alert(`Failed to start: ${e}`); }
}

async function onStop(id) {
  try { await stopServer(id); } catch (e) { alert(`Failed to stop: ${e}`); }
}

async function onDelete(id) {
  if (!confirm("Remove this connection?")) return;
  try {
    await deleteConnection(id);
    removeCard(id);
  } catch (e) { alert(`Failed to delete: ${e}`); }
}

function onCopyUrl(conn) {
  const url = `http://127.0.0.1:${conn.port}/mcp`;
  navigator.clipboard.writeText(url).then(() => {
    const btn = document.querySelector(`[data-connection-id="${conn.id}"] .copy-url-btn`);
    if (btn) { btn.textContent = "Copied!"; setTimeout(() => { btn.textContent = "Copy URL"; }, 1500); }
  });
}

export async function loadDashboard() {
  const conns = await listConnections();
  conns.forEach(renderCard);
}
