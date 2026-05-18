# Overfry — MCP Bridge

[![Release Builds](https://github.com/PS-Honda/overfry/actions/workflows/release.yml/badge.svg)](https://github.com/PS-Honda/overfry/actions/workflows/release.yml)
![Version](https://img.shields.io/badge/version-0.2.0-blue)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

Expose local resources as MCP servers — connect Claude Desktop and Claude.ai to your files, Obsidian vault, or remote APIs in minutes.

---

## Download

> **Latest release:** [github.com/PS-Honda/overfry/releases/latest](https://github.com/PS-Honda/overfry/releases/latest)

| Platform | Format | Type |
|---|---|---|
| **Windows** | `.msi` | Installer (recommended) |
| **Windows** | `*-setup.exe` | NSIS Installer |
| **Windows** | `Overfry-portable.exe` | Portable — no install needed¹ |
| **Linux** | `.AppImage` | Portable — no install needed² |
| **Linux** | `.deb` | Debian / Ubuntu package |
| **macOS (Apple Silicon)** | `.dmg` | Installer |
| **macOS (Intel)** | `.dmg` | Installer |
| **macOS** | `Overfry-portable.app.zip` | Portable — unzip and run³ |

> ¹ Requires WebView2 (pre-installed on Windows 10/11)  
> ² `chmod +x Overfry-*.AppImage && ./Overfry-*.AppImage`  
> ³ First launch: right-click → Open to bypass Gatekeeper

---

## What is Overfry?

Overfry runs a local MCP server on your machine that Claude can talk to. One app, one port, multiple connections — each exposed as a separate path.

```
Claude Desktop / Claude.ai
        ↓
http://127.0.0.1:51552/connector/<slug>   (local)
https://xxx.trycloudflare.com/connector/<slug>   (public, via tunnel)
        ↓
    Overfry MCP Bridge
        ↓
  📁 Local folder  |  🔮 Obsidian vault  |  ☁️ Remote API
```

---

## Connection Types

### 📁 Local Folder (`Filesystem`)
Expose any folder on your computer as MCP file tools — read, write, list, search. Works without any external service.

### 🔮 Obsidian Vault (`ObsidianFilesystem`)
Direct vault access — read notes, write, patch frontmatter, full-text search. **No Obsidian plugin required.**

### ☁️ Remote API (`RemoteProxy`)
Proxy any remote MCP-compatible API through Overfry. Built-in presets:
- **Outline** — team knowledge base
- **Obsidian Local REST API** — with plugin
- **Notion (OAuth)**
- Custom upstream MCP URL

---

## Quick Start

1. **Install** Overfry and launch it
2. Click **+ Add Connection**, choose a type, fill in the path/URL
3. Click **▶ Start** on the connection card
4. **Copy URL** → paste into Claude Desktop config or Claude.ai connector settings

### Claude Desktop config example
```json
{
  "mcpServers": {
    "my-vault": {
      "url": "http://127.0.0.1:51552/connector/my-vault"
    }
  }
}
```

### Claude.ai connector
Use the **Public URL** shown on the card (requires tunnel to be active). See [Authentication](#authentication) below.

---

## Public URL (Cloudflare Tunnel)

Overfry auto-starts a [Cloudflare Quick Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/) on launch — no account required.

- Navbar badge shows: `☁ connecting…` → `☁ tunnel active` (click to copy base URL)
- Each connection card shows a **Public** URL alongside the local URL
- Public URL format: `https://xxx.trycloudflare.com/connector/<slug>`

> **Note:** The public URL changes every time Overfry restarts (ephemeral tunnel).

---

## ⚠️ Security & Privacy Warning

**When the tunnel is active, all traffic passes through Cloudflare's servers as plaintext.**

Cloudflare terminates HTTPS at the edge and forwards unencrypted HTTP to Overfry locally. Cloudflare can inspect the full content of every request and response.

**Do NOT use the public tunnel URL with:**
- Company confidential documents
- Database query results containing sensitive data
- Proprietary knowledge bases (Outline, Notion with private content)
- Any information subject to NDA or data protection obligations

**For sensitive data:** use Claude Desktop with the **local URL only** (`http://127.0.0.1:51552/...`). The local URL never leaves your machine — no tunnel required.

---

## Authentication

Overfry uses OAuth2 `client_credentials` to protect the public tunnel endpoint.

| Scenario | Auth required? |
|---|---|
| Claude Desktop → local URL | Off by default (toggle in Settings) |
| Claude.ai → public tunnel URL | **Always on** (forced when tunnel active) |

### Get your credentials
Open **Settings (⚙)** → copy the **Token Endpoint**, **Client ID**, and **Client Secret**.

### Request a token
```bash
curl -X POST http://127.0.0.1:51552/oauth/token \
  -d "grant_type=client_credentials&client_id=<id>&client_secret=<secret>"
# → {"access_token":"...","token_type":"bearer","expires_in":86400}
```

### Use the token
```bash
curl https://xxx.trycloudflare.com/connector/my-vault \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/list","id":1}'
```

---

## Development

### Requirements
- Rust (stable) — [rustup.rs](https://rustup.rs)
- Tauri CLI v2 — `npm install -g @tauri-apps/cli@2`
- cloudflared binary in `src-tauri/binaries/` (gitignored — download separately)

### Commands
```bash
cargo tauri dev      # dev mode with hot reload
cargo tauri build    # release build
cargo test -p overfry       # run tests
cargo check                 # fast type check
cargo clippy -- -D warnings # lint
```

### cloudflared binary (dev)
Download for your platform from [github.com/cloudflare/cloudflared/releases](https://github.com/cloudflare/cloudflared/releases/latest) and place in `src-tauri/binaries/` with the correct Tauri triple name:

| Platform | Filename |
|---|---|
| Windows x64 | `cloudflared-x86_64-pc-windows-msvc.exe` |
| Linux x64 | `cloudflared-x86_64-unknown-linux-gnu` |
| macOS Apple Silicon | `cloudflared-aarch64-apple-darwin` |
| macOS Intel | `cloudflared-x86_64-apple-darwin` |

---

## Tech Stack

| Layer | Technology |
|---|---|
| App framework | Tauri v2 |
| Backend | Rust + axum 0.8 + tokio |
| Frontend | HTML + Bulma CSS 1.0 + Vanilla JS (ES modules) |
| MCP transport | Streamable HTTP (2025-03-26 spec) |
| Persistence | tauri-plugin-store (JSON) |
| Public tunnel | Cloudflare Quick Tunnel (cloudflared sidecar) |

---

## System Requirements

| Platform | Minimum |
|---|---|
| Windows | 10 / 11 (WebView2 built-in) |
| macOS | 11.0 (Big Sur) |
| Linux | WebKit2GTK 4.1 required (`libwebkit2gtk-4.1`) |
