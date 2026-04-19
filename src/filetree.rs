//! Directory walking — produces a `DirectoryListing` describing the
//! entries of a folder on disk. First pass (M4.1 commit 1) is
//! non-recursive and data-only: no rendering, no visible tree. The
//! visible tree-plate (YGG-14 commit 2) consumes this data.
//!
//! Filtering is deliberately conservative for the first pass: hidden
//! files (leading dot) and standard noise (`__pycache__`, `.git`,
//! `target`, `node_modules`) are skipped. The filter list will grow
//! once the visible tree shows what's actually useful.

use std::path::{Path, PathBuf};

/// One entry in a directory listing. Files and folders are
/// distinguished by `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Entry name (filename only, no path).
    pub name: String,
    /// Absolute path to the entry on disk.
    pub path: PathBuf,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
}

/// The result of walking a directory: its path plus its immediate
/// entries. Non-recursive for now — folder entries point to folders
/// on disk but aren't expanded into their own listings yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    pub root: PathBuf,
    pub entries: Vec<DirectoryEntry>,
}

/// Walk `path` (a directory) and return its immediate entries,
/// filtered and sorted: folders first, then files, each group
/// alphabetical (case-insensitive).
///
/// Returns an error if `path` isn't a directory or can't be read.
pub fn walk(path: &Path) -> std::io::Result<DirectoryListing> {
    let read = std::fs::read_dir(path)?;
    let mut entries: Vec<DirectoryEntry> = Vec::new();

    for entry in read {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => continue, // non-UTF8 filename; skip
        };
        if is_filtered(&name_str) {
            continue;
        }

        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let kind = if ft.is_dir() {
            EntryKind::Folder
        } else if ft.is_file() {
            EntryKind::File
        } else {
            // Symlinks, sockets, etc. — skip for now. When symlink
            // handling matters we'll resolve via metadata() and
            // decide per type.
            continue;
        };

        entries.push(DirectoryEntry { name: name_str, path: entry.path(), kind });
    }

    entries.sort_by(|a, b| {
        match (a.kind, b.kind) {
            (EntryKind::Folder, EntryKind::File) => std::cmp::Ordering::Less,
            (EntryKind::File, EntryKind::Folder) => std::cmp::Ordering::Greater,
            _ => a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()),
        }
    });

    Ok(DirectoryListing { root: path.to_path_buf(), entries })
}

/// True for names the first pass always skips: hidden files (leading
/// dot) and common build / cache / vendor directories that clutter
/// browsing without teaching anything useful.
fn is_filtered(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name,
        "__pycache__" | "node_modules" | "target" | "build" | "dist" | ".venv" | "venv"
    )
}

/// Pick a representative file from a listing to open on `ygg <dir>`:
/// prefer README.md, then any top-level source file of a supported
/// language. Returns `None` if no suitable file is found (empty
/// directory, or only folders).
pub fn pick_representative_file(
    listing: &DirectoryListing,
    supported_extensions: &[&str],
) -> Option<PathBuf> {
    // First pass: exact match on README.md, case-insensitive.
    for e in &listing.entries {
        if e.kind == EntryKind::File && e.name.eq_ignore_ascii_case("README.md") {
            return Some(e.path.clone());
        }
    }
    // Second pass: first file whose extension matches a supported language.
    for e in &listing.entries {
        if e.kind != EntryKind::File {
            continue;
        }
        let Some(ext) = std::path::Path::new(&e.name).extension().and_then(|e| e.to_str())
        else {
            continue;
        };
        let lower = ext.to_ascii_lowercase();
        if supported_extensions.iter().any(|&s| s == lower.as_str()) {
            return Some(e.path.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn walk_lists_entries_folders_first_then_files_alphabetical() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("tests")).unwrap();
        fs::write(root.join("README.md"), b"# test").unwrap();
        fs::write(root.join("Cargo.toml"), b"[package]").unwrap();

        let listing = walk(root).unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "tests", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn walk_filters_hidden_and_standard_noise() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join(".hidden"), b"").unwrap();
        fs::write(root.join("visible.py"), b"").unwrap();

        let listing = walk(root).unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "visible.py"]);
    }

    #[test]
    fn walk_errors_on_nonexistent_path() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        assert!(walk(&missing).is_err());
    }

    #[test]
    fn pick_representative_file_prefers_readme() {
        let listing = DirectoryListing {
            root: PathBuf::from("/x"),
            entries: vec![
                DirectoryEntry {
                    name: "main.py".to_string(),
                    path: PathBuf::from("/x/main.py"),
                    kind: EntryKind::File,
                },
                DirectoryEntry {
                    name: "README.md".to_string(),
                    path: PathBuf::from("/x/README.md"),
                    kind: EntryKind::File,
                },
            ],
        };
        assert_eq!(
            pick_representative_file(&listing, &["py", "md"]),
            Some(PathBuf::from("/x/README.md"))
        );
    }

    #[test]
    fn pick_representative_file_falls_back_to_first_supported_source() {
        let listing = DirectoryListing {
            root: PathBuf::from("/x"),
            entries: vec![
                DirectoryEntry {
                    name: "data.json".to_string(),
                    path: PathBuf::from("/x/data.json"),
                    kind: EntryKind::File,
                },
                DirectoryEntry {
                    name: "main.py".to_string(),
                    path: PathBuf::from("/x/main.py"),
                    kind: EntryKind::File,
                },
            ],
        };
        assert_eq!(
            pick_representative_file(&listing, &["py", "rs"]),
            Some(PathBuf::from("/x/main.py"))
        );
    }

    #[test]
    fn pick_representative_file_returns_none_when_nothing_matches() {
        let listing = DirectoryListing {
            root: PathBuf::from("/x"),
            entries: vec![
                DirectoryEntry {
                    name: "unknown.xyz".to_string(),
                    path: PathBuf::from("/x/unknown.xyz"),
                    kind: EntryKind::File,
                },
            ],
        };
        assert_eq!(pick_representative_file(&listing, &["py", "rs"]), None);
    }

    #[test]
    fn pick_representative_file_is_case_insensitive_for_readme() {
        let listing = DirectoryListing {
            root: PathBuf::from("/x"),
            entries: vec![DirectoryEntry {
                name: "readme.md".to_string(),
                path: PathBuf::from("/x/readme.md"),
                kind: EntryKind::File,
            }],
        };
        assert_eq!(
            pick_representative_file(&listing, &["md"]),
            Some(PathBuf::from("/x/readme.md"))
        );
    }
}
