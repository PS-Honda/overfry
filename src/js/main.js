import { loadDashboard, updateCardStatus, handlePortChanged, handleTunnelChanged, handleAuthStatusChanged } from "./dashboard.js";
import { appendAuditEntry } from "./audit.js";
import { openModal } from "./modal.js";
import { getTunnelStatus } from "./api.js";

document.addEventListener("DOMContentLoaded", async () => {
  await loadDashboard();
  wireTauriEvents();
  // Query current tunnel state — handles the case where Active event fired
  // before JS was ready (timing race on startup or after webview reload).
  try {
    const status = await getTunnelStatus();
    handleTunnelChanged(status);
  } catch { /* not in Tauri context */ }
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
    handleTunnelChanged(payload ?? { url: null, status: "Unavailable" });
  });

  listen("auth-status-changed", ({ payload }) => {
    handleAuthStatusChanged(payload ?? {});
  });
}
