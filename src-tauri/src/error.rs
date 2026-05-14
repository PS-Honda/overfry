use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("path traversal attempt: {0}")]
    PathTraversal(String),
    #[error("symlink loop detected: {0}")]
    SymlinkLoop(String),
    #[error("operation not permitted: {0}")]
    OperationNotPermitted(String),
    #[error("port conflict: {0}")]
    PortConflict(String),
    #[error("server already running")]
    ServerAlreadyRunning,
    #[error("server not found")]
    ServerNotFound,
    #[error("connection not found")]
    ConnectionNotFound,
    #[error("file too large: {0} bytes")]
    FileTooLarge(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("{0}")]
    Other(String),
}

// tauri::command functions must return String errors
impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// Convenience: Into<String> so tauri commands can use ? directly
impl From<AppError> for String {
    fn from(e: AppError) -> Self { e.to_string() }
}
