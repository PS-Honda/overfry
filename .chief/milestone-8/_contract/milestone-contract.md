# Milestone 8 Contract

## Cargo Feature Gate

```toml
# workspace Cargo.toml — add optional deps:
fastembed = { version = "0.8", optional = true }
usearch   = { version = "0.8", optional = true }

# src-tauri/Cargo.toml — add feature:
[features]
semantic-search = ["fastembed", "usearch"]
```

All semantic code gated behind `#[cfg(feature = "semantic-search")]`.

## New Tauri Commands (commands.rs)

```rust
get_semantic_search_status(connection_id: String)
  -> Result<SemanticSearchStatus, AppError>

download_embedding_model(connection_id: String)
  -> Result<(), AppError>  // emits "model-download-progress" {percent: f32}

rebuild_search_index(connection_id: String)
  -> Result<(), AppError>  // emits "index-progress" {done: u32, total: u32}
```

## New MCP Tool (obsidian_fs_server.rs)

```
semantic_search(query: String, limit?: u32, threshold?: f32)
  -> [{file, score, title, excerpt}]
```

Returns error with message `"semantic search not enabled"` in lightweight build.

## New Models (models.rs)

```rust
pub struct SemanticSearchStatus {
    pub enabled: bool,
    pub model_downloaded: bool,
    pub indexed_notes: u32,
    pub index_size_bytes: u64,
}
```

## New File: src-tauri/src/semantic_search.rs

Owns:
- `SemanticSearchEngine` struct (fastembed embedder + usearch Index)
- `build_index(root: &Path, connection_id: &str) -> Result<(), AppError>`
- `query(text: &str, limit: usize, threshold: f32) -> Vec<SemanticResult>`
- `hybrid_score(bm25: f32, semantic: f32) -> f32`  // 0.4*bm25 + 0.6*semantic

## Index Location

`{vault_root}/.overfry/search.index` — binary usearch file

## Tauri Events

| Event | Payload |
|---|---|
| `model-download-progress` | `{connection_id, percent: f32}` |
| `index-progress` | `{connection_id, done: u32, total: u32}` |
| `index-complete` | `{connection_id, indexed: u32}` |
