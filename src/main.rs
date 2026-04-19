//! `ygg` binary entry point. Parses CLI, reads the file, runs the viewer.

mod analyzer;
mod app;
mod background;
mod cards;
mod cli;
mod composite;
mod header;
mod icon_pipeline;
mod icons;
mod lens_pipeline;
mod plate;
mod renderer;
mod shapes;
mod sky;
mod state;
mod syntax;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::analyzer::SourceFile;
use crate::app::App;
use crate::cards::extract_cards;
use crate::cli::{Cli, Mode, RealFs};
use crate::state::{compute_line_offsets, AppState, HighlightedSource};
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
    let day_cycle_override = cli.debug_day_loop_length;
    let mode = cli.resolve(&RealFs).context("invalid command-line arguments")?;

    match mode {
        Mode::File { path } => {
            let source = SourceFile::read(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            // Python-only in M3. Later milestones dispatch on file extension.
            let mut highlighter = Highlighter::new_python().context("load Python grammar")?;
            let line_offsets = compute_line_offsets(&source.contents);
            // Parse once, use the tree for both highlighting and card extraction.
            let tree = highlighter
                .parse(&source.contents)
                .context("tree-sitter failed to parse source")?;
            let kinds = highlighter.highlight_tree(&tree, &source.contents);
            let cards = extract_cards(&tree, &source.contents, &line_offsets);
            drop(tree);

            let highlighted = HighlightedSource::from_parts(source, kinds, line_offsets);
            let mut state = AppState::new(highlighted, cards);
            if let Some(secs) = day_cycle_override {
                state.day_cycle_secs = secs.max(crate::sky::MIN_DAY_CYCLE_SECS);
            }
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
