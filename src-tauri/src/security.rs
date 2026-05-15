use std::path::{Path, PathBuf};
use crate::error::AppError;

const MAX_SYMLINK_DEPTH: u32 = 40;

/// Validate that `requested` is within `root`. Returns the canonicalized absolute path.
/// Treats `requested` as relative to `root` (leading separators stripped).
pub fn validate_path(root: &Path, requested: &str) -> Result<PathBuf, AppError> {
    if requested.contains('\0') {
        return Err(AppError::PathTraversal("null byte in path".into()));
    }

    let stripped = requested.trim_start_matches(['/', '\\']);
    let candidate = if stripped.is_empty() { root.to_path_buf() } else { root.join(stripped) };

    check_symlink_depth(&candidate)?;

    let canonical_root = std::fs::canonicalize(root)?;
    let canonical_path = std::fs::canonicalize(&candidate)?;

    if !canonical_path.starts_with(&canonical_root) {
        return Err(AppError::PathTraversal(format!("path escapes root: {requested}")));
    }
    Ok(canonical_path)
}

pub fn check_symlink_depth(path: &Path) -> Result<(), AppError> {
    let mut current = path.to_path_buf();
    let mut depth = 0u32;
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                depth += 1;
                if depth > MAX_SYMLINK_DEPTH {
                    return Err(AppError::SymlinkLoop(
                        format!("depth > {MAX_SYMLINK_DEPTH} at {}", current.display()),
                    ));
                }
                current = std::fs::read_link(&current)?;
            }
            _ => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_byte_rejected() {
        let root = std::env::temp_dir();
        assert!(matches!(
            validate_path(&root, "some\0path"),
            Err(AppError::PathTraversal(_))
        ));
    }

    #[test]
    fn traversal_rejected() {
        let root = std::env::temp_dir();
        // Requesting root's parent must fail
        let result = validate_path(&root, "../../etc/passwd");
        assert!(matches!(result, Err(AppError::PathTraversal(_)) | Err(AppError::Io(_))));
    }

    #[test]
    fn valid_subpath_accepted() {
        let root = std::env::temp_dir();
        // Root itself (empty stripped path) must succeed
        let result = validate_path(&root, ".");
        assert!(result.is_ok());
    }
}
