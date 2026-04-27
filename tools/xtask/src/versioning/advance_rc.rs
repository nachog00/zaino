//! Advance the `rc` branch to a green dev commit and cut an RC release.
//!
//! This is the nightly automation: after tier 2 passes on dev HEAD, merge
//! dev into rc, run knope to bump versions and generate changelogs, then
//! create GitHub pre-releases for each crate.

use std::path::Path;

use crate::{ci, log};

pub(crate) fn run(green_commit: &str, root: &Path, dry_run: bool) -> Result<(), String> {
    log::info(&format!("Advancing rc to green commit {green_commit}"));

    // Check if rc already contains this commit.
    if ci::git_remote_branch_exists("rc")? {
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
    } else {
        log::info("Creating rc branch.");
        ci::git_checkout("rc", Some(green_commit), dry_run)?;
    }

    ci::git_push("rc", dry_run)?;

    // Check for changesets -- if none, no version bump needed.
    if !ci::has_changesets(root) {
        log::info("No changesets found. rc advanced but no version bump.");
        return Ok(());
    }

    // Cut the RC: bump versions, update changelogs, create releases.
    log::info("Changesets found. Preparing RC release.");
    ci::knope_prepare_release(Some("rc"), dry_run)?;
    ci::git_commit_all("chore: prepare rc release", dry_run)?;
    ci::git_push("rc", dry_run)?;
    ci::knope_release(dry_run)?;

    log::ok("RC release complete.");
    Ok(())
}
