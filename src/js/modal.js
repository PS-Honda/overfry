import { pickFolder } from "./api.js";
import { renderCard } from "./dashboard.js";

const modal      = document.getElementById("modal-add");
const modalBg    = document.getElementById("modal-bg");
const modalClose = document.getElementById("modal-close");
const modalTitle = document.getElementById("modal-title");

const step1  = document.getElementById("step-1");
const step2a = document.getElementById("step-2a");
const step2b = document.getElementById("step-2b");

const btnNext   = document.getElementById("btn-next");
const btnBack   = document.getElementById("btn-back");
const btnCreate = document.getElementById("btn-create");

const fsName    = document.getElementById("fs-name");
const fsPath    = document.getElementById("fs-path");
const btnBrowse = document.getElementById("btn-browse");
const fsMcpPath = document.getElementById("fs-mcp-path");

const rpName    = document.getElementById("rp-name");
const rpPreset  = document.getElementById("rp-preset");
const rpUrl     = document.getElementById("rp-url");
const rpToken   = document.getElementById("rp-token");
const rpUrlLabel   = document.getElementById("rp-url-label");
const rpTokenLabel = document.getElementById("rp-token-label");
const rpMcpPath    = document.getElementById("rp-mcp-path");
const rpAuthToken  = document.getElementById("rp-auth-token");
const rpAuthOAuth  = document.getElementById("rp-auth-oauth");
const rpTokenSection = document.getElementById("rp-token-section");
const rpOAuthSection = document.getElementById("rp-oauth-section");
const rpClientId     = document.getElementById("rp-client-id");
const rpClientSecret = document.getElementById("rp-client-secret");
const rpOAuthAuthUrl  = document.getElementById("rp-oauth-auth-url");
const rpOAuthTokenUrl = document.getElementById("rp-oauth-token-url");
const rpOAuthScopes   = document.getElementById("rp-oauth-scopes");

let selectedType = null;
let editingId    = null; // non-null → edit mode

const PRESETS = {
  obsidian: { url: "https://127.0.0.1:27123/",          urlLabel: "Plugin MCP URL", tokenLabel: "Plugin API Key", authMethod: "token" },
  outline:  { url: "",                                   urlLabel: "Upstream MCP URL", tokenLabel: "API Token",      authMethod: "token" },
  notion:   { url: "https://api.notion.com/v1/mcp",     urlLabel: "Upstream MCP URL", tokenLabel: "",               authMethod: "oauth",
              oauthAuthUrl: "https://api.notion.com/v1/oauth/authorize",
              oauthTokenUrl: "https://api.notion.com/v1/oauth/token" },
  custom:   { url: "",                                   urlLabel: "Upstream MCP URL", tokenLabel: "API Token",      authMethod: "token" },
};

export function openModal() {
  editingId = null;
  resetModal();
  modal.classList.add("is-active");
}

export function openEditModal(conn) {
  editingId = conn.id;
  resetModal();

  selectedType = conn.connection_type;
  document.querySelectorAll(".type-choice-card").forEach(c => {
    c.classList.toggle("selected", c.dataset.type === selectedType);
  });

  if (selectedType === "Filesystem" || selectedType === "ObsidianFilesystem") {
    fsName.value    = conn.name ?? "";
    fsPath.value    = (conn.root_paths && conn.root_paths[0]) ? conn.root_paths[0] : "";
    fsMcpPath.value = conn.mcp_path ?? "";
    showStep(2);
  } else {
    rpName.value    = conn.name ?? "";
    rpMcpPath.value = conn.mcp_path ?? "";

    const auth = conn.auth_config;
    if (auth) {
      rpUrl.value    = auth.base_url ?? "";
      const method   = auth.auth_method ?? "token";
      rpAuthToken.checked = method === "token";
      rpAuthOAuth.checked = method === "oauth";
      setAuthMethod(method);

      // Preset — try to match
      const preset = auth.preset ?? "custom";
      rpPreset.value = preset;
      const p = PRESETS[preset] ?? PRESETS.custom;
      rpUrlLabel.textContent   = p.urlLabel;
      rpTokenLabel.textContent = p.tokenLabel ?? "API Token";
      rpUrl.readOnly = (preset === "obsidian");
    }
    showStep(2);
  }

  modalTitle.textContent = "Edit Connection";
  btnCreate.textContent  = "Save Changes";
  modal.classList.add("is-active");
}

function closeModal() {
  modal.classList.remove("is-active");
}

function resetModal() {
  selectedType = null;
  document.querySelectorAll(".type-choice-card").forEach(c => c.classList.remove("selected"));
  showStep(1);
  fsName.value = fsPath.value = fsMcpPath.value = "";
  fsName.classList.remove("is-danger");
  rpName.value = rpUrl.value = rpToken.value = rpMcpPath.value = "";
  rpName.classList.remove("is-danger");
  rpPreset.value = "custom";
  rpUrlLabel.textContent   = PRESETS.custom.urlLabel;
  rpTokenLabel.textContent = PRESETS.custom.tokenLabel ?? "API Token";
  rpUrl.readOnly = false;
  btnNext.disabled = true;
  // Reset OAuth fields
  rpAuthToken.checked = true;
  rpClientId.value = rpClientSecret.value = rpOAuthAuthUrl.value = rpOAuthTokenUrl.value = rpOAuthScopes.value = "";
  rpTokenSection.style.display = "";
  rpOAuthSection.style.display = "none";
  // Reset button text
  btnCreate.textContent = "Create Connection";
  modalTitle.textContent = "Add Connection";
}

function showStep(n) {
  const isFs = selectedType === "Filesystem" || selectedType === "ObsidianFilesystem";
  step1.style.display  = n === 1 ? "" : "none";
  step2a.style.display = n === 2 && isFs ? "" : "none";
  step2b.style.display = n === 2 && selectedType === "RemoteProxy" ? "" : "none";
  btnBack.style.display   = n === 2 && !editingId ? "" : "none";
  btnNext.style.display   = n === 1 ? "" : "none";
  btnCreate.style.display = n === 2 ? "" : "none";
  if (n === 1) btnNext.disabled = !selectedType;
  if (n === 2 && !editingId) {
    if (selectedType === "Filesystem") modalTitle.textContent = "Local Folder";
    else if (selectedType === "ObsidianFilesystem") modalTitle.textContent = "Obsidian Vault";
    else modalTitle.textContent = "Remote API";
  }
}

// Auth method toggle
function setAuthMethod(method) {
  rpTokenSection.style.display = method === "token" ? "" : "none";
  rpOAuthSection.style.display = method === "oauth" ? "" : "none";
}
rpAuthToken.addEventListener("change", () => setAuthMethod("token"));
rpAuthOAuth.addEventListener("change", () => setAuthMethod("oauth"));

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
  rpUrl.value              = p.url;
  rpUrlLabel.textContent   = p.urlLabel;
  rpTokenLabel.textContent = p.tokenLabel ?? "API Token";
  rpUrl.readOnly = (rpPreset.value === "obsidian");

  const method = p.authMethod ?? "token";
  rpAuthToken.checked = method === "token";
  rpAuthOAuth.checked = method === "oauth";
  setAuthMethod(method);

  if (p.oauthAuthUrl)  rpOAuthAuthUrl.value  = p.oauthAuthUrl;
  if (p.oauthTokenUrl) rpOAuthTokenUrl.value = p.oauthTokenUrl;
});

// Clear is-danger on input for name fields
fsName.addEventListener("input", () => fsName.classList.remove("is-danger"));
rpName.addEventListener("input", () => rpName.classList.remove("is-danger"));

// Browse folder
btnBrowse.addEventListener("click", async () => {
  try {
    const path = await pickFolder();
    if (path) {
      fsPath.value = path;
      if (!fsName.value) fsName.value = path.split(/[\\/]/).pop();
    }
  } catch { /* pre-Milestone 2 */ }
});

// Nav buttons
btnNext.addEventListener("click", () => showStep(2));
btnBack.addEventListener("click", () => { showStep(1); modalTitle.textContent = "Add Connection"; });
modalClose.addEventListener("click", closeModal);
modalBg.addEventListener("click",    closeModal);

// Slug helper: turn name into /connector/<slug>
function toSlug(name) {
  return "/connector/" + name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

function buildRequest() {
  if (selectedType === "Filesystem" || selectedType === "ObsidianFilesystem") {
    if (!fsName.value.trim()) { fsName.classList.add("is-danger"); fsName.focus(); return null; }
    return {
      name:            fsName.value.trim(),
      connection_type: selectedType,
      root_paths:      [fsPath.value],
      auth_config:     null,
      mcp_path:        fsMcpPath.value.trim() || toSlug(fsName.value.trim()),
    };
  } else {
    if (!rpName.value.trim()) { rpName.classList.add("is-danger"); rpName.focus(); return null; }
    const isOAuth = rpAuthOAuth.checked;
    return {
      name:            rpName.value.trim(),
      connection_type: "RemoteProxy",
      root_paths:      [],
      auth_config: {
        base_url:        rpUrl.value.trim(),
        token:           isOAuth ? "" : rpToken.value.trim(),
        extra_headers:   {},
        preset:          rpPreset.value,
        auth_method:     isOAuth ? "oauth" : "token",
        client_id:       isOAuth ? rpClientId.value.trim() : "",
        client_secret:   isOAuth ? rpClientSecret.value.trim() : "",
        oauth_auth_url:  isOAuth ? rpOAuthAuthUrl.value.trim() : "",
        oauth_token_url: isOAuth ? rpOAuthTokenUrl.value.trim() : "",
        oauth_scopes:    isOAuth ? rpOAuthScopes.value.trim() : "",
        access_token:    "",
        refresh_token:   "",
      },
      mcp_path: rpMcpPath.value.trim() || toSlug(rpName.value.trim()),
    };
  }
}

// Create / Save
btnCreate.addEventListener("click", async () => {
  const req = buildRequest();
  if (!req) return;

  try {
    const { createConnection, updateConnection } = await import("./api.js");
    let conn;
    if (editingId) {
      conn = await updateConnection(editingId, req);
    } else {
      conn = await createConnection(req);
    }
    renderCard(conn);
    closeModal();
  } catch (e) {
    alert(`Could not ${editingId ? "update" : "create"} connection: ${e}`);
  }
});
