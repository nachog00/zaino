//! Versioning and release management.
//!
//! All versioning concerns live here as subcommands of `cargo xtask versioning`.
//! Knope handles the heavy lifting (version bumping, changelog generation,
//! GitHub releases). This module fills the gaps knope doesn't cover.

mod changeset;

use std::path::Path;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Changeset file management.
    Changeset {
        #[command(subcommand)]
        action: changeset::Action,
    },
}

pub(crate) fn run(command: Command, root: &Path) -> Result<(), String> {
    match command {
        Command::Changeset { action } => changeset::run(action, root),
    }
}
