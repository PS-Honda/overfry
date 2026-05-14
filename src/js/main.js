import { loadDashboard, updateCardStatus } from "./dashboard.js";
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

  listen("port-reassigned", ({ payload }) => {
    showPortNotification(payload.old_port, payload.new_port);
  });
}

function showPortNotification(oldPort, newPort) {
  document.getElementById("port-notification")?.remove();

  const notif = document.createElement("div");
  notif.id = "port-notification";
  notif.className = "notification is-warning is-light";
  notif.style.cssText = "position:fixed;bottom:1rem;right:1rem;max-width:360px;z-index:100;";

  const delBtn = document.createElement("button");
  delBtn.className = "delete";
  delBtn.addEventListener("click", () => notif.remove());

  const msg = document.createElement("span");
  msg.textContent = `Port conflict: connection reassigned from ${oldPort} to ${newPort}. Restart Claude MCP connection to use new URL.`;

  notif.append(delBtn, msg);
  document.body.appendChild(notif);
  setTimeout(() => notif.remove(), 8000);
}
