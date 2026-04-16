//! Analyzer — reads source files from disk into structures the renderer can
//! consume.
//!
//! In M1 the analyzer's job is trivial: read a file into a `SourceFile`. The
//! struct pre-splits by line because every future milestone (virtualized
//! scroll in M2, AST-indexed card layout in M3, diff-overlay in M6+) needs
//! line-indexed access. Splitting once here avoids repeated scans downstream.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A source file loaded from disk, ready for rendering.
///
/// `lines` is the canonical form the renderer consumes. `contents` is kept for
/// later passes (tree-sitter will want the whole string in M2) and so we
/// don't need to re-join on every reparse.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub contents: String,
    // Consumed in M2 (virtualized scroll) — kept here so the analyzer's
    // output shape doesn't change between milestones.
    #[allow(dead_code)]
    pub lines: Vec<String>,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("file does not exist: {0}")]
    NotFound(PathBuf),
    #[error("path is a directory, not a file: {0}")]
    IsDirectory(PathBuf),
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SourceFile {
    /// Read `path` into a `SourceFile`. Classifies the common failure cases
    /// (missing, directory-where-file-expected) into typed errors so the caller
    /// can print useful messages; everything else bubbles up as `Io`.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        let path = path.as_ref();

        // Classify the two common failures up-front so we don't have to
        // interpret an opaque `io::Error` kind downstream.
        match fs::metadata(path) {
            Ok(m) if m.is_dir() => return Err(SourceError::IsDirectory(path.to_path_buf())),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SourceError::NotFound(path.to_path_buf()));
            }
            Err(source) => {
                return Err(SourceError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }

        let contents = fs::read_to_string(path).map_err(|source| SourceError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        // split('\n') keeps empty trailing lines visible (important for files
        // that end with '\n' — the reader shouldn't drop that last empty line).
        // `\r\n` is handled by stripping a trailing `\r` from each line.
        let lines: Vec<String> = contents
            .split('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s).to_string())
            .collect();

        Ok(SourceFile {
            path: path.to_path_buf(),
            contents,
            lines,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tempfile::TempDir;

    #[rstest]
    #[case::lf_terminated("hello\nworld\n", 3, "hello")]
    #[case::crlf("a\r\nb\r\n",              3, "a")]
    #[case::no_final_newline("single",      1, "single")]
    #[case::empty("",                       1, "")]
    fn reads_files_correctly(
        #[case] body: &str,
        #[case] line_count: usize,
        #[case] first_line: &str,
    ) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sample.txt");
        std::fs::write(&path, body).unwrap();

        let sf = SourceFile::read(&path).expect("should read");
        assert_eq!(sf.lines.len(), line_count, "line count for body {:?}", body);
        assert_eq!(sf.lines[0], first_line);
        assert_eq!(sf.contents, body);
    }

    #[rstest]
    #[case::missing(
        |tmp: &TempDir| tmp.path().join("nope.txt"),
        |e: &SourceError| matches!(e, SourceError::NotFound(_)),
    )]
    #[case::directory(
        |tmp: &TempDir| tmp.path().to_path_buf(),
        |e: &SourceError| matches!(e, SourceError::IsDirectory(_)),
    )]
    fn reports_typed_errors(
        #[case] path_fn: fn(&TempDir) -> PathBuf,
        #[case] check: fn(&SourceError) -> bool,
    ) {
        let tmp = TempDir::new().unwrap();
        let err = SourceFile::read(path_fn(&tmp)).unwrap_err();
        assert!(check(&err), "unexpected error: {:?}", err);
    }
}
