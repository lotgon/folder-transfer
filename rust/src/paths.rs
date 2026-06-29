//! Path canonicalisation, the destination safety-prefix check, and helpers
//! shared by the client and server. See spec sections 6.1, 6.2, 6.5.

use std::path::{Path, PathBuf};

/// Already-compressed / encrypted extensions adaptive compression skips.
/// Exact list from `$script:IncompressibleExt` in ft-server.ps1 (section 6.5).
pub const INCOMPRESSIBLE_EXT: &[&str] = &[
    ".zip", ".7z", ".gz", ".tgz", ".rar", ".bz2", ".xz", ".zst", ".lz4", ".br", ".cab", ".msi",
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".tif", ".tiff", ".mp4", ".mkv", ".mov",
    ".avi", ".wmv", ".webm", ".mp3", ".aac", ".ogg", ".flac", ".m4a", ".pdf", ".docx", ".xlsx",
    ".pptx", ".odt", ".ods", ".jar", ".apk", ".iso",
];

/// Is this extension (with leading dot, any case) in the incompressible list?
pub fn is_incompressible(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    let lower = ext.to_ascii_lowercase();
    INCOMPRESSIBLE_EXT.contains(&lower.as_str())
}

/// Canonicalise a directory path the same way for the destination and every
/// target, so an 8.3 short path matches the expanded form (section 6.2).
/// Uses `dunce` on Windows to avoid the `\\?\` verbatim prefix.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    {
        dunce::canonicalize(path)
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path)
    }
}

/// The root prefix used by the safety check: canonical destination + separator.
pub fn root_prefix(canonical_to: &Path) -> String {
    let mut s = canonical_to.to_string_lossy().into_owned();
    let sep = std::path::MAIN_SEPARATOR;
    if !s.ends_with(sep) {
        s.push(sep);
    }
    s
}

/// Join a server-supplied `<rel>` onto the destination, lexically normalise it
/// (resolving `.`/`..` WITHOUT touching the filesystem, like .NET `GetFullPath`),
/// and reject anything that escapes the destination. Returns the safe target or
/// `None` if it is unsafe.
pub fn safe_join(to_folder: &Path, root_prefix: &str, rel: &str) -> Option<PathBuf> {
    let mut out = to_folder.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let target = out.to_string_lossy();
    if starts_with_prefix(&target, root_prefix) {
        Some(out)
    } else {
        None
    }
}

/// `<rel>`'s first non-empty segment (the shared folder's own name), used to
/// derive a mirror root.
pub fn top_segment(rel: &str) -> Option<&str> {
    rel.split(['/', '\\']).find(|s| !s.is_empty())
}

/// Key for the `seen` set and prefix comparisons: lowercased on Windows
/// (mirrors `ToLowerInvariant` / `OrdinalIgnoreCase`), as-is on case-sensitive
/// filesystems.
pub fn norm_key(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        s.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s
    }
}

/// Case-insensitive prefix match on Windows, exact on other platforms.
fn starts_with_prefix(target: &str, prefix: &str) -> bool {
    #[cfg(windows)]
    {
        target.to_lowercase().starts_with(&prefix.to_lowercase())
    }
    #[cfg(not(windows))]
    {
        target.starts_with(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompressible_is_case_insensitive() {
        assert!(is_incompressible(".ZIP"));
        assert!(is_incompressible(".mp4"));
        assert!(!is_incompressible(".txt"));
        assert!(!is_incompressible(""));
    }

    #[test]
    fn safe_join_accepts_inside() {
        // Build base + prefix consistently so separators match on every platform.
        let base = std::env::temp_dir().join("ft_test_dest");
        let prefix = root_prefix(&base);
        let t = safe_join(&base, &prefix, "Share/sub/file.txt").unwrap();
        assert!(t.to_string_lossy().contains("Share"));
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let base = std::env::temp_dir().join("ft_test_dest");
        let prefix = root_prefix(&base);
        assert!(safe_join(&base, &prefix, "../escape").is_none());
        assert!(safe_join(&base, &prefix, "Share/../../escape").is_none());
    }

    #[test]
    fn top_segment_handles_both_separators() {
        assert_eq!(top_segment("Share/a/b"), Some("Share"));
        assert_eq!(top_segment("Share\\a\\b"), Some("Share"));
        assert_eq!(top_segment("/Share/a"), Some("Share"));
    }
}
