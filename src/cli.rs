//! CLI argument parsing and mode dispatch.
//!
//! The CLI shape is locked in from Milestone 1 per CLAUDE.md:
//!
//! ```text
//! ygg <file>                       show one file        (M1)
//! ygg <dir>                        show a tree + code   (M4)
//! ygg <dir> --git                  add git timeline     (M5)
//! ygg diff <ref1> <ref2>           diff two refs        (M6)
//! ygg diff <ref1> <ref2> <path>    diff a specific path (M6)
//! ```
//!
//! Only `ygg <file>` is wired up in M1; other modes parse but return a
//! "not yet implemented" error so the surface is stable for later milestones
//! to fill in without CLI reshuffling.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(
    name = "ygg",
    version,
    about = "Ygg — a spatial code viewer for AI-generated code",
    long_about = None,
    // `ygg` with no args prints help rather than erroring silently.
    // "path required unless subcommand" is checked later in `resolve()`,
    // not at parse time, because clap's derive doesn't model it cleanly.
    arg_required_else_help = true,
)]
pub struct Cli {
    /// File (M1) or directory (M4+) to view. Required unless a subcommand is used.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Enable git-aware mode (history + timeline). Applies to directory view. (M5)
    #[arg(long)]
    pub git: bool,

    /// Debug: override the full day cycle length, in seconds. Default ≈ 120s
    /// (2 min). Drop to e.g. 30 to flip through night → noon → dusk quickly
    /// when tuning SkyLight consumers; raise toward 600 for release-cadence
    /// viewing. Invisible in the UI — purely a time-base override.
    #[arg(long, value_name = "SECONDS")]
    pub debug_day_loop_length: Option<f32>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show semantic diff between two git refs. (M6)
    Diff(DiffArgs),
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Base git ref.
    pub ref1: String,
    /// Target git ref.
    pub ref2: String,
    /// Optional path to restrict the diff to.
    pub path: Option<PathBuf>,
}

/// Resolved command: what the rest of the program should actually do.
///
/// Non-M1 variants exist so main-dispatch is stable from day one; they are
/// unreachable in M1 because `resolve()` errors out earlier for them. The
/// `#[allow(dead_code)]` sheds warnings until M4/M6 wire them up.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Mode {
    /// Show a single file (M1).
    File { path: PathBuf },
    /// Show a directory tree + code pane (M4). Optionally git-aware (M5).
    Directory { path: PathBuf, git: bool },
    /// Diff between two refs (M6).
    Diff {
        ref1: String,
        ref2: String,
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CliError {
    #[error("no path given — usage: ygg <path> or ygg diff <ref1> <ref2> [path]")]
    PathMissing,
    #[error("path does not exist: {0}")]
    PathNotFound(PathBuf),
    #[error("expected a file, got a directory: {0}")]
    ExpectedFileGotDir(PathBuf),
    #[error("`ygg diff` mode is not yet implemented (planned for M6)")]
    DiffModeNotImplemented,
}

impl Cli {
    /// Resolve the parsed CLI into a `Mode`, validating paths against the
    /// filesystem. Invariants clap can't enforce (file vs directory, existence)
    /// are checked here against the supplied filesystem view.
    pub fn resolve(self, fs: &dyn FsProbe) -> Result<Mode, CliError> {
        if let Some(Command::Diff(d)) = self.command {
            // M1: parse but don't dispatch — the shape is locked in, behavior arrives in M6.
            let _ = (d.ref1, d.ref2, d.path);
            return Err(CliError::DiffModeNotImplemented);
        }

        let path = self.path.ok_or(CliError::PathMissing)?;

        if !fs.exists(&path) {
            return Err(CliError::PathNotFound(path));
        }

        if fs.is_dir(&path) {
            // M4.1: directory mode is being wired up. First pass opens a
            // representative file from the directory and carries a flat
            // listing alongside; visible tree lands in a later commit.
            return Ok(Mode::Directory { path, git: self.git });
        }

        if !fs.is_file(&path) {
            // Exists but is neither dir nor regular file (e.g. broken symlink).
            return Err(CliError::ExpectedFileGotDir(path));
        }

        Ok(Mode::File { path })
    }
}

/// Minimal filesystem probe trait so `resolve` is testable without hitting disk.
pub trait FsProbe {
    fn exists(&self, p: &Path) -> bool;
    fn is_file(&self, p: &Path) -> bool;
    fn is_dir(&self, p: &Path) -> bool;
}

/// Real filesystem implementation used from main().
pub struct RealFs;

impl FsProbe for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn is_dir(&self, p: &Path) -> bool {
        p.is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rstest::rstest;
    use std::collections::HashSet;

    /// A scripted filesystem for tests: you declare the set of existing
    /// files and directories, and `FsProbe` queries answer from that.
    struct ScriptedFs {
        files: HashSet<PathBuf>,
        dirs: HashSet<PathBuf>,
    }
    impl ScriptedFs {
        fn new(files: &[&str], dirs: &[&str]) -> Self {
            Self {
                files: files.iter().map(PathBuf::from).collect(),
                dirs: dirs.iter().map(PathBuf::from).collect(),
            }
        }
    }
    impl FsProbe for ScriptedFs {
        fn exists(&self, p: &Path) -> bool {
            self.files.contains(p) || self.dirs.contains(p)
        }
        fn is_file(&self, p: &Path) -> bool {
            self.files.contains(p)
        }
        fn is_dir(&self, p: &Path) -> bool {
            self.dirs.contains(p)
        }
    }

    // One parameterized test covers:
    //  - valid file path            → Mode::File
    //  - missing path on disk       → PathNotFound
    //  - directory given            → Mode::Directory (M4.1)
    //  - directory with --git flag  → Mode::Directory { git: true }
    //  - `diff` subcommand          → DiffModeNotImplemented (M1)
    //
    // The expected outcome is encoded as a closure so each case asserts only on
    // what's meaningful for it.
    #[rstest]
    #[case::file_ok(
        &["ygg", "hello.py"],
        &["hello.py"], &[],
        |r: &Result<Mode, CliError>| matches!(r, Ok(Mode::File { path }) if path == Path::new("hello.py")),
    )]
    #[case::missing_path(
        &["ygg", "missing.py"],
        &[], &[],
        |r: &Result<Mode, CliError>| matches!(r, Err(CliError::PathNotFound(p)) if p == Path::new("missing.py")),
    )]
    #[case::directory_ok(
        &["ygg", "some_dir"],
        &[], &["some_dir"],
        |r: &Result<Mode, CliError>| matches!(r, Ok(Mode::Directory { path, git: false }) if path == Path::new("some_dir")),
    )]
    #[case::directory_with_git(
        &["ygg", "some_dir", "--git"],
        &[], &["some_dir"],
        |r: &Result<Mode, CliError>| matches!(r, Ok(Mode::Directory { path, git: true }) if path == Path::new("some_dir")),
    )]
    #[case::diff_not_yet_impl(
        &["ygg", "diff", "HEAD~1", "HEAD"],
        &[], &[],
        |r: &Result<Mode, CliError>| matches!(r, Err(CliError::DiffModeNotImplemented)),
    )]
    fn resolve_modes(
        #[case] argv: &[&str],
        #[case] files: &[&str],
        #[case] dirs: &[&str],
        #[case] check: impl Fn(&Result<Mode, CliError>) -> bool,
    ) {
        let cli = Cli::try_parse_from(argv).expect("valid argv");
        let fs = ScriptedFs::new(files, dirs);
        let result = cli.resolve(&fs);
        assert!(check(&result), "unexpected outcome: {:?}", result);
    }

    /// Missing the required positional (`ygg` with no args) must fail at parse
    /// time — not silently fall through to a default behavior.
    #[test]
    fn no_args_parses_as_error() {
        let err = Cli::try_parse_from(["ygg"]).unwrap_err();
        // clap's "required" error. We only assert it failed — the exact kind
        // can shift between clap versions.
        assert!(!err.to_string().is_empty());
    }
}
