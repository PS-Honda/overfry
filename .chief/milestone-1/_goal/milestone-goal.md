# Milestone 1 Goal — Scaffold + UI Shell

## Objective

Set up the Tauri v2 + Rust workspace from scratch and produce a working UI shell with Bulma CSS card dashboard. No real backend logic yet — just the skeleton that proves the build system works and the UI layout is correct.

## Success Criteria

- [ ] `cargo tauri dev` runs without error on Windows
- [ ] App window opens showing a card grid with hardcoded dummy connection cards
- [ ] Bulma CSS loaded from local file (no CDN)
- [ ] Add Server modal opens and closes (no form logic yet)
- [ ] Audit log panel renders (static, no data)
- [ ] All Tauri v2 plugins registered in `main.rs`: store, dialog, shell

## Out of Scope

- No real connections, no Rust backend logic
- No port manager, no MCP servers
- No JS → Rust IPC calls (all dummy data)

## Notes

- Use `cargo create-tauri-app` or manual scaffold — either fine
- Frontend: plain HTML + ES module JS — no bundler (Vite optional but not required for MVP)
- Local Bulma CSS: download `bulma-1.0.min.css` into `src/assets/`
