import { loadDashboard, updateCardStatus, handlePortChanged, handleTunnelChanged } from "./dashboard.js";
import { appendAuditEntry } from "./audit.js";
import { openModal } from "./modal.js";

document.addEventListener("DOMContentLoaded", async () => {
  await loadDashboard();
  wireTauriEvents();
});

document.getElementById("btn-add-server").addEventListener("click", openModal);

function wireTauriEvents() {
  if (!window.__TAURI__?.event) return;
  const { listen } = window.__TAURI__.event;

  listen("connection-status-changed", ({ payload }) => {
    updateCardStatus(payload.id, payload.status);
  });

  listen("audit-entry-added", ({ payload }) => {
    appendAuditEntry(payload);
  });

  listen("global-port-changed", ({ payload }) => {
    if (payload?.port) handlePortChanged(payload.port);
  });

  listen("tunnel-status-changed", ({ payload }) => {
    handleTunnelChanged(payload?.url ?? null);
  });
}
