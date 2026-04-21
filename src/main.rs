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
mod slat3d;
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
    // `cosmic_text::font::system` is silenced at warn-level because it
    // chokes on macOS's GB18030 bitmap font (and any other non-TTF
    // system font) — harmless noise, every run would otherwise print
    // the same warnings.
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,cosmic_text::font::system=error"),
    )
    .init();

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
    let expand_all = cli.debug_expand_all;
    let slat_mode = cli.debug_slat_mode.unwrap_or_default();
    let wrap_mode = cli.debug_wrap.unwrap_or_default();
    let perspective_compass = cli.debug_perspective_compass;
    let slat_angle_rad = cli
        .debug_slat_angle
        .map(|deg| deg.to_radians())
        .unwrap_or(0.0);
    let slat_arc_depth = cli
        .debug_slat_arc
        .unwrap_or(crate::slat3d::DEFAULT_ARC_DEPTH);
    let mode = cli.resolve(&RealFs).context("invalid command-line arguments")?;

    match mode {
        Mode::File { path } => {
            let state = open_file(
                &path,
                None,
                day_cycle_override,
                slat_mode,
                wrap_mode,
                perspective_compass,
                slat_angle_rad,
                slat_arc_depth,
            )?;
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
            if expand_all {
                crate::blind::expand_all(&mut tree);
            }
            let state = open_file(
                &file,
                Some(tree),
                day_cycle_override,
                slat_mode,
                wrap_mode,
                perspective_compass,
                slat_angle_rad,
                slat_arc_depth,
            )?;
            App::new(state).run().context("event loop exited with error")?;
            Ok(())
        }
        Mode::Diff { .. } => anyhow::bail!("diff mode is not yet implemented (planned for M6)"),
    }
}

/// Load a source file and build an AppState ready to hand to the event
/// loop. Shared between single-file mode and directory mode (where the
/// directory walker has already picked which file to open first).
#[allow(clippy::too_many_arguments)] // Many debug flags; regrouping
// into a struct would front-load work for a signature that changes
// each time we add a debug knob.
fn open_file(
    path: &std::path::Path,
    tree_state: Option<filetree::TreeState>,
    day_cycle_override: Option<f32>,
    slat_mode: cli::SlatMode,
    wrap_mode: cli::WrapMode,
    perspective_compass: bool,
    slat_angle_rad: f32,
    slat_arc_depth: f32,
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
    state.slat_mode = slat_mode;
    state.wrap_mode = wrap_mode;
    state.debug_perspective_compass = perspective_compass;
    state.slat_angle_rad = slat_angle_rad;
    state.slat_arc_depth = slat_arc_depth;
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
