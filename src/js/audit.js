const entriesEl = document.getElementById("audit-entries");
const toggle    = document.getElementById("audit-toggle");
const chevron   = document.getElementById("audit-chevron");
const MAX_ROWS  = 200;

let collapsed = false;

toggle.addEventListener("click", () => {
  collapsed = !collapsed;
  document.getElementById("audit-entries").style.display = collapsed ? "none" : "";
  chevron.textContent = collapsed ? "▸" : "▾";
});

export function appendAuditEntry(entry) {
  const placeholder = entriesEl.querySelector("p.has-text-grey");
  if (placeholder) placeholder.remove();

  const toolLower = (entry.tool_name ?? "").toLowerCase();
  const isWrite  = toolLower.includes("write") || toolLower.includes("append") || toolLower.includes("move") || toolLower.includes("patch");
  const isDenied = entry.result && typeof entry.result === "object" && "Denied" in entry.result;

  const row = document.createElement("div");
  row.className = `audit-row${isDenied ? " denied" : isWrite ? " write" : ""}`;

  const t = new Date(entry.timestamp);
  const hh = String(t.getHours()).padStart(2, "0");
  const mm = String(t.getMinutes()).padStart(2, "0");
  const ss = String(t.getSeconds()).padStart(2, "0");

  const timeEl = document.createElement("span");
  timeEl.className = "audit-time";
  timeEl.textContent = `${hh}:${mm}:${ss}`;

  const toolEl = document.createElement("span");
  toolEl.className = "audit-tool";
  toolEl.textContent = entry.tool_name ?? "?";

  const pathEl = document.createElement("span");
  pathEl.className = "audit-path";
  pathEl.textContent = entry.path ?? "";

  row.append(timeEl, toolEl, pathEl);
  entriesEl.prepend(row);

  const rows = entriesEl.querySelectorAll(".audit-row");
  if (rows.length > MAX_ROWS) rows[rows.length - 1].remove();
}
