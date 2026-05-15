import { suggestPort, pickFolder } from "./api.js";
import { renderCard } from "./dashboard.js";

const modal    = document.getElementById("modal-add");
const modalBg  = document.getElementById("modal-bg");
const modalClose = document.getElementById("modal-close");
const modalTitle = document.getElementById("modal-title");

const step1  = document.getElementById("step-1");
const step2a = document.getElementById("step-2a");
const step2b = document.getElementById("step-2b");

const btnNext   = document.getElementById("btn-next");
const btnBack   = document.getElementById("btn-back");
const btnCreate = document.getElementById("btn-create");

const fsName = document.getElementById("fs-name");
const fsPath = document.getElementById("fs-path");
const fsPort = document.getElementById("fs-port");
const fsPortWarn = document.getElementById("fs-port-warning");
const btnBrowse  = document.getElementById("btn-browse");

const rpName  = document.getElementById("rp-name");
const rpPreset = document.getElementById("rp-preset");
const rpUrl   = document.getElementById("rp-url");
const rpToken = document.getElementById("rp-token");
const rpPort  = document.getElementById("rp-port");
const rpPortWarn = document.getElementById("rp-port-warning");
const rpUrlLabel   = document.getElementById("rp-url-label");
const rpTokenLabel = document.getElementById("rp-token-label");

let selectedType = null;

const PRESETS = {
  obsidian: { url: "https://127.0.0.1:27123", urlLabel: "Plugin URL", tokenLabel: "Plugin API Key" },
  outline:  { url: "",                         urlLabel: "Base URL",   tokenLabel: "API Token" },
  custom:   { url: "",                         urlLabel: "Base URL",   tokenLabel: "API Token" },
};

export function openModal() {
  resetModal();
  modal.classList.add("is-active");
}

function closeModal() {
  modal.classList.remove("is-active");
}

function resetModal() {
  selectedType = null;
  document.querySelectorAll(".type-choice-card").forEach(c => c.classList.remove("selected"));
  showStep(1);
  fsName.value = fsPath.value = "";
  rpName.value = rpUrl.value = rpToken.value = "";
  fsPortWarn.classList.remove("visible");
  rpPortWarn.classList.remove("visible");
  btnNext.disabled = true;
}

function showStep(n) {
  const isFs = selectedType === "Filesystem" || selectedType === "ObsidianFilesystem";
  step1.style.display  = n === 1 ? "" : "none";
  step2a.style.display = n === 2 && isFs ? "" : "none";
  step2b.style.display = n === 2 && selectedType === "RemoteProxy" ? "" : "none";
  btnBack.style.display   = n === 2 ? "" : "none";
  btnNext.style.display   = n === 1 ? "" : "none";
  btnCreate.style.display = n === 2 ? "" : "none";
  if (n === 1) btnNext.disabled = !selectedType;
  if (n === 2) {
    if (selectedType === "Filesystem") modalTitle.textContent = "Local Folder";
    else if (selectedType === "ObsidianFilesystem") modalTitle.textContent = "Obsidian Vault";
    else modalTitle.textContent = "Remote API";
  }
}

// Type card selection
document.querySelectorAll(".type-choice-card").forEach(card => {
  card.addEventListener("click", () => {
    document.querySelectorAll(".type-choice-card").forEach(c => c.classList.remove("selected"));
    card.classList.add("selected");
    selectedType = card.dataset.type;
    btnNext.disabled = false;
  });
});

// Preset change
rpPreset.addEventListener("change", () => {
  const p = PRESETS[rpPreset.value] ?? PRESETS.custom;
  rpUrl.value = p.url;
  rpUrlLabel.textContent   = p.urlLabel;
  rpTokenLabel.textContent = p.tokenLabel;
  if (rpPreset.value === "obsidian") rpUrl.readOnly = true;
  else rpUrl.readOnly = false;
});

// Port validation
async function validatePort(input, warnEl) {
  const val = parseInt(input.value);
  if (!val || val < 50000 || val > 59999) return;
  try {
    const suggested = await suggestPort(val);
    if (suggested !== val) {
      warnEl.textContent = `Port ${val} in use — next free: ${suggested}`;
      warnEl.classList.add("visible");
      input.value = suggested;
    } else {
      warnEl.classList.remove("visible");
    }
  } catch { /* pre-Milestone 2, ignore */ }
}

fsPort.addEventListener("change", () => validatePort(fsPort, fsPortWarn));
rpPort.addEventListener("change", () => validatePort(rpPort, rpPortWarn));

// Pre-fill port on step 2 open
async function prefillPort(input) {
  try {
    const port = await suggestPort(null);
    input.value = port;
  } catch { input.value = 50000; }
}

// Browse folder
btnBrowse.addEventListener("click", async () => {
  try {
    const path = await pickFolder();
    if (path) { fsPath.value = path; if (!fsName.value) fsName.value = path.split(/[\\/]/).pop(); }
  } catch { /* pre-Milestone 2 */ }
});

// Nav buttons
btnNext.addEventListener("click", () => { showStep(2); prefillPort(selectedType === "Filesystem" ? fsPort : rpPort); });
btnBack.addEventListener("click", () => { showStep(1); modalTitle.textContent = "Add Connection"; });
modalClose.addEventListener("click", closeModal);
modalBg.addEventListener("click",    closeModal);

// Create
btnCreate.addEventListener("click", async () => {
  try {
    const { createConnection } = await import("./api.js");
    let req;
    if (selectedType === "Filesystem" || selectedType === "ObsidianFilesystem") {
      req = { name: fsName.value.trim(), connection_type: selectedType, port: parseInt(fsPort.value), root_paths: [fsPath.value], auth_config: null };
    } else {
      req = { name: rpName.value.trim(), connection_type: "RemoteProxy", port: parseInt(rpPort.value), root_paths: [], auth_config: { base_url: rpUrl.value.trim(), token: rpToken.value.trim(), extra_headers: {}, preset: rpPreset.value } };
    }
    const conn = await createConnection(req);
    renderCard(conn);
    closeModal();
  } catch (e) {
    alert(`Could not create connection: ${e}`);
  }
});
