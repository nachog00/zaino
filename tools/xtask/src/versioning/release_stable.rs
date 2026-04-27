//! Stable release: finalize versions, create GitHub releases, sync to dev.
//!
//! Runs on the stable branch after the rc→stable PR is merged. Calls knope
//! to strip pre-release suffixes, consume changesets, and create final
//! GitHub releases. Then merges stable back into dev.

use std::path::Path;

use crate::{ci, log};

pub(crate) fn run(root: &Path, dry_run: bool) -> Result<(), String> {
    log::info("Preparing stable release.");

    ci::knope_prepare_release(None, dry_run)?;
    ci::git_commit_all("chore: prepare stable release", dry_run)?;
    ci::git_push("stable", dry_run)?;

    log::info("Creating GitHub releases.");
    ci::knope_release(dry_run)?;

    // Sync stable back to dev so dev has the final versions.
    log::info("Syncing stable to dev.");
    ci::git_checkout("dev", None, dry_run)?;

    let merge_result = ci::git_merge("stable", "chore: sync stable to dev", dry_run);
    if let Err(e) = merge_result {
        log::error(&format!("Failed to sync stable to dev: {e}"));
        log::info("Manual resolution needed.");
        return Err("stable→dev sync failed".into());
    }

    ci::git_push("dev", dry_run)?;

    if !ci::has_changesets(root) {
        log::ok("Stable release complete. Changesets consumed.");
    } else {
        log::ok("Stable release complete.");
    }
    Ok(())
}
