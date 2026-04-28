//! Advance the `rc` branch to a green dev commit and cut an RC release.
//!
//! This is the nightly automation: after tier 2 passes on dev HEAD, merge
//! dev into rc, run knope to bump versions and generate changelogs, then
//! create GitHub pre-releases for each crate.
//!
//! Assumes the `rc` branch already exists (manual bootstrapping).

use std::path::Path;

use crate::{ci, log};
use super::release::name::ReleaseName;
use super::sync_workspace_deps;

pub(crate) fn run(green_commit: &str, root: &Path, dry_run: bool) -> Result<(), String> {
    log::info(&format!("Advancing rc to green commit {green_commit}"));

    // Check if rc already contains this commit.
    if ci::git_is_ancestor(green_commit, "origin/rc")? {
        log::info("rc already contains this commit. Nothing to do.");
        return Ok(());
    }

    // Merge dev into rc. rc's knope-set versions survive because
    // developers don't touch version lines in Cargo.toml.
    ci::git_checkout("rc", None, dry_run)?;
    let short = &green_commit[..green_commit.len().min(8)];
    let merge_result = ci::git_merge(
        green_commit,
        &format!("chore: advance rc to dev {short}"),
        dry_run,
    );

    if let Err(e) = merge_result {
        log::error(&format!("Merge conflict advancing rc: {e}"));
        log::info("Manual resolution needed. Aborting rc advancement.");
        return Err("rc advancement failed due to merge conflict".into());
    }

    ci::git_push("rc", dry_run)?;

    // Check for changesets -- if none, no version bump needed.
    if !ci::has_changesets(root) {
        log::info("No changesets found. rc advanced but no version bump.");
        return Ok(());
    }

    // Compute the release-event tag before knope runs.
    let release = ReleaseName::next();
    log::info(&format!("Cutting RC: {}", release.rc_tag()));

    // Cut the RC: bump versions, update changelogs, create releases.
    ci::knope_prepare_release(Some("rc"), dry_run)?;

    // Knope bumps crate versions but not workspace dependency versions.
    // Sync them so cargo can resolve pre-release versions.
    log::info("Syncing workspace dependency versions.");
    sync_workspace_deps::run(root, dry_run)?;

    ci::git_commit_all("chore: prepare rc release", dry_run)?;

    // Tag with the release-event name (e.g. 2026-05-01-rc.0) alongside
    // knope's per-crate tags.
    ci::git_tag_and_push(&release.rc_tag(), dry_run)?;

    ci::git_push("rc", dry_run)?;
    ci::knope_release(dry_run)?;

    log::ok(&format!("RC release complete: {}", release.rc_tag()));
    Ok(())
}
