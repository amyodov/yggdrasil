//! `ygg` binary entry point. Parses CLI, reads the file, runs the viewer.

mod analyzer;
mod app;
mod cli;
mod renderer;
mod state;
mod syntax;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::analyzer::SourceFile;
use crate::app::App;
use crate::cli::{Cli, Mode, RealFs};
use crate::state::{AppState, HighlightedSource};
use crate::syntax::Highlighter;

fn main() -> ExitCode {
    // RUST_LOG=info ygg ... surfaces wgpu/winit/egui diagnostics.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ygg: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mode = cli.resolve(&RealFs).context("invalid command-line arguments")?;

    match mode {
        Mode::File { path } => {
            let source = SourceFile::read(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            // M2: Python-only. Later milestones dispatch on file extension.
            let mut highlighter = Highlighter::new_python().context("load Python grammar")?;
            let kinds = highlighter.highlight(&source.contents);
            let highlighted = HighlightedSource::new(source, kinds);
            let state = AppState::new(highlighted);
            App::new(state).run().context("event loop exited with error")?;
            Ok(())
        }
        // Defensive: cli::Cli::resolve currently rejects these. When later
        // milestones enable them, replace the arms with their dispatch.
        Mode::Directory { .. } => {
            anyhow::bail!("directory mode is not yet implemented (planned for M4)")
        }
        Mode::Diff { .. } => anyhow::bail!("diff mode is not yet implemented (planned for M6)"),
    }
}
