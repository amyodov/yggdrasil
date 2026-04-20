//! `ygg` binary entry point. Parses CLI, reads the file, runs the viewer.

mod analyzer;
mod app;
mod background;
mod blind;
mod cards;
mod cli;
mod composite;
mod filetree;
mod header;
mod icon_pipeline;
mod icons;
mod language;
mod lens_pipeline;
mod substrate;
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
            let state = open_file(&path, None, day_cycle_override)?;
            App::new(state).run().context("event loop exited with error")?;
            Ok(())
        }
        Mode::Directory { path, git: _ } => {
            let listing = filetree::walk(&path)
                .with_context(|| format!("failed to read directory {}", path.display()))?;
            // Pick a representative file to open on the right. README.md
            // or the first matching-extension source file; visible tree
            // on the left lands in a later commit.
            let supported: Vec<&str> = supported_extensions();
            let file = filetree::pick_representative_file(&listing, &supported).ok_or_else(
                || {
                    anyhow::anyhow!(
                        "no readable files found in {} (only folders or unsupported types)",
                        path.display()
                    )
                },
            )?;
            let mut tree = filetree::TreeState::new(listing);
            tree.selected = Some(file.clone());
            let state = open_file(&file, Some(tree), day_cycle_override)?;
            App::new(state).run().context("event loop exited with error")?;
            Ok(())
        }
        Mode::Diff { .. } => anyhow::bail!("diff mode is not yet implemented (planned for M6)"),
    }
}

/// Load a source file and build an AppState ready to hand to the event
/// loop. Shared between single-file mode and directory mode (where the
/// directory walker has already picked which file to open first).
fn open_file(
    path: &std::path::Path,
    tree_state: Option<filetree::TreeState>,
    day_cycle_override: Option<f32>,
) -> Result<AppState> {
    let source = SourceFile::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let module = language::for_path(path).ok_or_else(|| {
        anyhow::anyhow!("no language module for file extension: {}", path.display())
    })?;
    let mut highlighter = Highlighter::new_for_language(module)
        .with_context(|| format!("load {} grammar", module.name()))?;
    let line_offsets = compute_line_offsets(&source.contents);
    let ast = highlighter
        .parse(&source.contents)
        .context("tree-sitter failed to parse source")?;
    let kinds = highlighter.highlight_tree(&ast, &source.contents);
    let cards = module.extract_cards(&ast, &source.contents, &line_offsets);
    drop(ast);

    let highlighted = HighlightedSource::from_parts(source, kinds, line_offsets);
    let mut state = AppState::new(highlighted, cards);
    state.tree = tree_state;
    if let Some(secs) = day_cycle_override {
        state.day_cycle_secs = secs.max(crate::sky::MIN_DAY_CYCLE_SECS);
    }
    Ok(state)
}

/// Flat list of extensions every registered LanguageModule accepts.
/// Used by the directory walker to pick a representative file.
fn supported_extensions() -> Vec<&'static str> {
    // Kept as a small duplicated list rather than pulling from the
    // registry so this module doesn't grow a dependency on iterating
    // the registry. Update alongside new LanguageModules.
    vec!["py", "pyi", "rs", "md", "markdown"]
}
