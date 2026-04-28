//! Versioning and release management.
//!
//! All versioning concerns live here as subcommands of `cargo xtask versioning`.
//! Knope handles the heavy lifting (version bumping, changelog generation,
//! GitHub releases). This module fills the gaps and orchestrates CI flows.

mod advance_rc;
mod changeset;
mod release;
pub(crate) mod sync_workspace_deps;
mod validate_rc;
mod version_table;

use std::path::Path;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Changeset file management.
    Changeset {
        #[command(subcommand)]
        action: changeset::Action,
    },
    /// Advance the rc branch to a green dev commit and cut an RC release.
    AdvanceRc {
        /// The commit SHA that passed medium tests (typically dev HEAD).
        #[arg(long)]
        green_commit: String,
    },
    /// Deploy an RC for heavy testing (creates GitHub deployment).
    DeployHeavyTest {
        /// The RC commit SHA.
        #[arg(long)]
        rc_sha: String,
        /// The RC tag (e.g. 2026-05-01-rc.0).
        #[arg(long)]
        rc_tag: String,
    },
    /// Promote an RC that passed heavy tests (advances validated-rc).
    PromoteRc {
        /// The RC commit SHA that passed.
        #[arg(long)]
        rc_sha: String,
    },
    /// Print the release dashboard PR body (markdown) to stdout.
    ReleasePrBody,
    /// Print the release dashboard PR title to stdout.
    ReleasePrTitle,
    /// Print the RC comment line to stdout.
    ReleaseRcComment {
        /// Short commit SHA for the RC comment.
        #[arg(long)]
        short_sha: String,
    },
    /// Run the stable release: finalize versions, create releases, sync to dev.
    ReleaseStable,
}

pub(crate) fn run(command: Command, root: &Path, dry_run: bool) -> Result<(), String> {
    match command {
        Command::Changeset { action } => changeset::run(action, root),
        Command::AdvanceRc { green_commit } => advance_rc::run(&green_commit, root, dry_run),
        Command::DeployHeavyTest { rc_sha, rc_tag } => {
            validate_rc::deploy(&rc_sha, &rc_tag, dry_run)
        }
        Command::PromoteRc { rc_sha } => validate_rc::promote(&rc_sha, dry_run),
        Command::ReleasePrBody => release::pr_body::run(root),
        Command::ReleasePrTitle => {
            print!("{}", release::pr_body::title());
            Ok(())
        }
        Command::ReleaseRcComment { short_sha } => {
            print!("{}", release::pr_body::rc_comment(&short_sha));
            Ok(())
        }
        Command::ReleaseStable => release::stable::run(root, dry_run),
    }
}
