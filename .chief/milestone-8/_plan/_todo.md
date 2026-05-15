# TODO — Milestone 8: Semantic Search (v0.3)

- [ ] task-1: Add feature gate + optional deps to Cargo.toml (workspace + src-tauri)
        fastembed + usearch as optional; [features] semantic-search = ["fastembed","usearch"]
        Verify: `cargo check` passes both with and without feature

- [ ] task-2: Create src-tauri/src/semantic_search.rs
        SemanticSearchEngine struct, build_index(), query(), hybrid_score()
        All gated behind #[cfg(feature = "semantic-search")]
        Unit tests: embedding dim=384, HNSW insert+query roundtrip, hybrid score formula

- [ ] task-3: Add SemanticSearchStatus model + 3 new Tauri commands
        get_semantic_search_status, download_embedding_model, rebuild_search_index
        Commands emit Tauri events for progress streaming
        Stub returns SemanticSearchStatus{enabled:false,...} in lightweight build

- [ ] task-4: Add semantic_search MCP tool to obsidian_fs_server.rs
        Returns error "semantic search not enabled" in lightweight build (#[cfg] stub)
        In semantic build: call SemanticSearchEngine::query() then hybrid merge with BM25

- [ ] task-5: UI — Settings panel in index.html + JS
        "Enable Semantic Search" section per ObsidianFilesystem connection card
        Download Model button → calls download_embedding_model command → listen to progress event
        Re-index button → calls rebuild_search_index → progress bar
        Status display: model downloaded Y/N, indexed N notes, index size

- [ ] task-6: Verify clean build both variants + all tests pass
        cargo check (no features) — must pass
        cargo check --features semantic-search — must pass
        cargo test (both)
        cargo clippy -- -D warnings (both)
        cargo tauri build (lightweight)
        cargo tauri build --features semantic-search (semantic — expect 20-30 min)
