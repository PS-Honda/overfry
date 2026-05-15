# Milestone 8 — Semantic Search (v0.3)

## Objective

Add optional semantic search to ObsidianFilesystem connections via an opt-in feature gate.
Ships as a separate binary variant (`--features semantic-search`).
Default lightweight build is unaffected.

## Success Criteria

- [ ] `[features] semantic-search` gate compiles clean with no warnings
- [ ] `fastembed` + `usearch` added as optional workspace deps
- [ ] New Tauri commands: `get_semantic_search_status`, `download_embedding_model`, `rebuild_search_index`
- [ ] New MCP tool: `semantic_search(query, limit?, threshold?)` on ObsidianFilesystem servers
- [ ] Model download streams progress via Tauri event `model-download-progress {percent}`
- [ ] Index stored at `{vault_root}/.overfry/search.index` (gitignore-able)
- [ ] Hybrid search: BM25 (existing search_files) + semantic score (60/40 weight)
- [ ] `cargo tauri build --features semantic-search` succeeds on Windows + macOS
- [ ] Unit tests for: embedding dim check, HNSW insert/query, hybrid score merge
- [ ] UI: Settings panel shows "Enable Semantic Search" toggle + Download Model button + index progress

## Out of Scope

- Python or external process dependency (pure Rust via fastembed-rs + ONNX Runtime)
- Automatic model download on first run (must be user-initiated)
- Semantic search on Filesystem or RemoteProxy connections (ObsidianFilesystem only)
- GPU inference (CPU only for cross-platform reliability)

## Notes

- Model: `paraphrase-multilingual-MiniLM-L12-v2` (~440 MB F32, ~120 MB Q2_K)
- fastembed-rs handles ONNX Runtime bundling — no system dep needed
- usearch: HNSW file-backed index, pure Rust bindings
- Thai language support is primary motivation (BM25 breaks on no-space CJK/Thai text)
- Build time for semantic variant: ~20-30 min (ONNX Runtime compile)
- Index location inside vault makes it portable and easy to gitignore
