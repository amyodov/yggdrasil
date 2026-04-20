//! Directory walking + tree state.
//!
//! Two things live here.
//!
//! **`walk()`** — non-recursive enumeration of a single folder's
//! immediate entries, filtered (hidden files + common build/cache noise
//! skipped) and sorted (folders first, then files, each alphabetical).
//!
//! **`TreeState`** — the runtime state of the blind. Holds the root
//! listing, lazily-walked subfolders, per-folder expansion state, the
//! currently-selected file, and animation progress (bootstrap +
//! per-folder wind/unwind). `flatten(&tree)` derives the visible list
//! of slats for the renderer.
//!
//! The tree is NOT rendered on a substrate. It lives in Zone 1 (the
//! void) as a cascade of hanging slat objects connected by a left
//! wire, each slat pointing to a file or folder. Rendering is
//! handled by `blind.rs`.

use std::collections::{HashMap, HashSet};
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

// ---------------------------------------------------------------------------
// TreeState — runtime state of the file-tree blind.
// ---------------------------------------------------------------------------

/// Runtime state for the file-tree blind. Carries the root listing, any
/// lazily-walked subfolders, the user's expansion / selection choices,
/// and animation progress. Derived list of visible slats comes from
/// `flatten(&tree)`.
#[allow(dead_code)] // Consumers (blind renderer + input handler) wire up in
// the next commits of this milestone; fields / methods are the API surface.
#[derive(Debug, Clone)]
pub struct TreeState {
    /// Root directory's immediate entries.
    pub root: DirectoryListing,
    /// Walked subfolders, keyed by absolute path. Populated lazily the
    /// first time the user expands a folder.
    pub children: HashMap<PathBuf, DirectoryListing>,
    /// Folders currently expanded. An expanded folder not yet present
    /// in `children` gets walked on the next `ensure_expanded_walked`.
    pub expanded: HashSet<PathBuf>,
    /// File currently selected (what the code scroll shows).
    pub selected: Option<PathBuf>,
    /// Vertical scroll offset of the blind in physical pixels.
    pub scroll_y: f32,
    /// Bootstrap animation progress: 0.0 at startup / directory-switch,
    /// advances to 1.0 over ~500ms. Each slat's arrival is staggered by
    /// its depth (see `blind.rs` for the actual easing). Reset whenever
    /// the root directory changes.
    pub bootstrap_progress: f32,
    /// Per-folder expansion animation progress. 0.0 = freshly collapsed,
    /// 1.0 = fully expanded. Missing entry = at target (folder has
    /// reached its steady state). The expansion target is derived from
    /// `expanded` membership.
    pub anim_progress: HashMap<PathBuf, f32>,
}

#[allow(dead_code)] // Consumers land in next commits.
impl TreeState {
    /// Fresh state for a directory. All folders start collapsed.
    /// `selected` starts unset; the caller typically sets it to the
    /// representative file opened alongside the directory.
    pub fn new(root: DirectoryListing) -> Self {
        Self {
            root,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            scroll_y: 0.0,
            bootstrap_progress: 0.0,
            anim_progress: HashMap::new(),
        }
    }

    /// Walk any expanded folder that hasn't been walked yet. Call this
    /// whenever `expanded` changes or at the start of each frame.
    /// Errors during walk are silently swallowed — an unreadable
    /// subfolder just stays un-expanded in the UI.
    pub fn ensure_expanded_walked(&mut self) {
        let to_walk: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|p| !self.children.contains_key(p.as_path()))
            .cloned()
            .collect();
        for path in to_walk {
            if let Ok(listing) = walk(&path) {
                self.children.insert(path, listing);
            }
        }
    }

    /// Toggle a folder's expansion state. Kicks off animation progress
    /// from the opposite side (so a freshly-expanded folder animates
    /// from 0 toward 1; a freshly-collapsed one from 1 toward 0).
    pub fn toggle_folder(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
            self.anim_progress.insert(path.to_path_buf(), 1.0); // start at fully-open, animate to 0
        } else {
            self.expanded.insert(path.to_path_buf());
            self.anim_progress.insert(path.to_path_buf(), 0.0); // start at closed, animate to 1
        }
    }

    /// Expansion target for a folder: 1.0 if expanded, 0.0 if not.
    pub fn expansion_target(&self, path: &Path) -> f32 {
        if self.expanded.contains(path) {
            1.0
        } else {
            0.0
        }
    }
}

/// One visible slat in the flattened tree.
#[allow(dead_code)] // Consumed by the blind renderer landing in the next commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlatEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    /// Indent depth (0 = root-level entry, 1 = child of a root folder, etc.).
    pub depth: usize,
}

/// Flatten the tree into the ordered list of slats the renderer draws.
/// Respects `expanded`; folder children only appear when their folder
/// is expanded and walked.
#[allow(dead_code)] // Consumed by the blind renderer landing in the next commit.
pub fn flatten(tree: &TreeState) -> Vec<SlatEntry> {
    let mut out = Vec::new();
    flatten_rec(&tree.root, &tree.children, &tree.expanded, &mut out, 0);
    out
}

fn flatten_rec(
    listing: &DirectoryListing,
    children: &HashMap<PathBuf, DirectoryListing>,
    expanded: &HashSet<PathBuf>,
    out: &mut Vec<SlatEntry>,
    depth: usize,
) {
    for entry in &listing.entries {
        out.push(SlatEntry {
            path: entry.path.clone(),
            name: entry.name.clone(),
            kind: entry.kind,
            depth,
        });
        if entry.kind == EntryKind::Folder && expanded.contains(&entry.path) {
            if let Some(sub) = children.get(&entry.path) {
                flatten_rec(sub, children, expanded, out, depth + 1);
            }
        }
    }
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

    // ---- TreeState + flatten --------------------------------------

    /// Helper: build an in-memory listing without touching disk.
    fn mock_listing(root: &str, entries: &[(&str, EntryKind)]) -> DirectoryListing {
        DirectoryListing {
            root: PathBuf::from(root),
            entries: entries
                .iter()
                .map(|(name, kind)| DirectoryEntry {
                    name: name.to_string(),
                    path: PathBuf::from(format!("{root}/{name}")),
                    kind: *kind,
                })
                .collect(),
        }
    }

    #[test]
    fn flatten_root_only_when_nothing_expanded() {
        let root = mock_listing(
            "/r",
            &[("src", EntryKind::Folder), ("README.md", EntryKind::File)],
        );
        let tree = TreeState::new(root);
        let flat = flatten(&tree);
        let names: Vec<&str> = flat.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "README.md"]);
        // All at depth 0.
        assert!(flat.iter().all(|e| e.depth == 0));
    }

    #[test]
    fn flatten_reveals_children_when_folder_expanded() {
        let root = mock_listing(
            "/r",
            &[("src", EntryKind::Folder), ("README.md", EntryKind::File)],
        );
        let src_children =
            mock_listing("/r/src", &[("main.py", EntryKind::File), ("lib.py", EntryKind::File)]);
        let mut tree = TreeState::new(root);
        tree.children.insert(PathBuf::from("/r/src"), src_children);
        tree.expanded.insert(PathBuf::from("/r/src"));

        let flat = flatten(&tree);
        let shape: Vec<(&str, usize)> =
            flat.iter().map(|e| (e.name.as_str(), e.depth)).collect();
        assert_eq!(
            shape,
            vec![
                ("src", 0),
                ("main.py", 1),
                ("lib.py", 1),
                ("README.md", 0),
            ]
        );
    }

    #[test]
    fn flatten_skips_unwalked_expanded_folders_silently() {
        // A folder marked expanded but not yet walked shouldn't panic,
        // just shouldn't contribute children.
        let root = mock_listing("/r", &[("src", EntryKind::Folder)]);
        let mut tree = TreeState::new(root);
        tree.expanded.insert(PathBuf::from("/r/src"));
        // Deliberately don't populate `children`.
        let flat = flatten(&tree);
        let names: Vec<&str> = flat.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src"]);
    }

    #[test]
    fn flatten_handles_nested_expansion() {
        let root = mock_listing("/r", &[("a", EntryKind::Folder)]);
        let a_children = mock_listing("/r/a", &[("b", EntryKind::Folder)]);
        let b_children = mock_listing("/r/a/b", &[("leaf.py", EntryKind::File)]);
        let mut tree = TreeState::new(root);
        tree.children.insert(PathBuf::from("/r/a"), a_children);
        tree.children.insert(PathBuf::from("/r/a/b"), b_children);
        tree.expanded.insert(PathBuf::from("/r/a"));
        tree.expanded.insert(PathBuf::from("/r/a/b"));

        let flat = flatten(&tree);
        let shape: Vec<(&str, usize)> =
            flat.iter().map(|e| (e.name.as_str(), e.depth)).collect();
        assert_eq!(
            shape,
            vec![("a", 0), ("b", 1), ("leaf.py", 2)]
        );
    }

    #[test]
    fn toggle_folder_flips_expansion_and_seeds_animation() {
        let root = mock_listing("/r", &[("src", EntryKind::Folder)]);
        let mut tree = TreeState::new(root);
        let src = PathBuf::from("/r/src");

        // Initially collapsed.
        assert!(!tree.expanded.contains(&src));
        assert_eq!(tree.expansion_target(&src), 0.0);

        tree.toggle_folder(&src);
        assert!(tree.expanded.contains(&src));
        assert_eq!(tree.expansion_target(&src), 1.0);
        // Animation starts at 0 (just-opened, will progress toward 1).
        assert_eq!(tree.anim_progress.get(&src).copied(), Some(0.0));

        tree.toggle_folder(&src);
        assert!(!tree.expanded.contains(&src));
        assert_eq!(tree.expansion_target(&src), 0.0);
        // Animation starts at 1 (just-closed, will progress toward 0).
        assert_eq!(tree.anim_progress.get(&src).copied(), Some(1.0));
    }
}
