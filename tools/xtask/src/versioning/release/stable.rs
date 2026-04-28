//! Stable release: finalize versions, create GitHub releases, sync to dev.
//!
//! Runs on the stable branch after the validated-rc→stable PR is merged.
//! Creates per-crate releases (via knope), publishes to crates.io, creates
//! a unified release entry linking everything, and syncs final versions
//! back to dev.

use std::path::Path;

use super::name::ReleaseName;
use crate::versioning::sync_workspace_deps;
use crate::versioning::version_table::VersionTable;
use crate::{ci, log, workspace};

/// Crates in dependency order (leaves first). A crate's dependencies
/// must be published before it.
const PUBLISH_ORDER: &[&str] = &[
    "zaino-common",
    "zaino-proto",
    "zaino-fetch",
    "zaino-state",
    "zaino-serve",
    "zainod",
];

pub(crate) fn run(root: &Path, dry_run: bool) -> Result<(), String> {
    log::info("Preparing stable release.");

    ci::knope_prepare_release(None, dry_run)?;
    sync_workspace_deps::run(root, dry_run)?;
    ci::git_commit_all("chore: prepare stable release", dry_run)?;
    ci::git_push("stable", dry_run)?;

    log::info("Creating per-crate GitHub releases.");
    ci::knope_release(dry_run)?;

    // Publish to crates.io in dependency order.
    log::info("Publishing to crates.io.");
    for crate_name in PUBLISH_ORDER {
        log::info(&format!("  Publishing {crate_name}..."));
        if let Err(e) = ci::cargo_publish(crate_name, dry_run) {
            log::error(&format!("Failed to publish {crate_name}: {e}"));
            log::info("Continuing with remaining crates.");
        }
    }

    // Create the unified release entry.
    create_unified_release(root, dry_run)?;

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

    log::ok("Stable release complete.");
    Ok(())
}

fn create_unified_release(root: &Path, dry_run: bool) -> Result<(), String> {
    let packages = workspace::knope_packages(root)?;
    let table = VersionTable::from_workspace(root, &packages)?;
    let release_name = ReleaseName::latest().unwrap_or_else(ReleaseName::next);

    let tag = release_name.target();
    let title = release_name.pr_title();

    let mut body = String::new();

    // Crate version table with links.
    body.push_str("## Crate Releases\n\n");
    body.push_str("| Crate | Version | Links |\n");
    body.push_str("| ----- | ------- | ----- |\n");
    for entry in &table.entries {
        let gh_tag = format!("{}/v{}", entry.name, entry.version);
        body.push_str(&format!(
            "| {} | {} | [release](../../releases/tag/{}) · [crates.io](https://crates.io/crates/{}/{}) |\n",
            entry.name, entry.version, gh_tag, entry.name, entry.version,
        ));
    }

    // Aggregated changelog.
    let changelog = table.to_markdown_changelog();
    if !changelog.is_empty() {
        body.push_str("\n---\n\n");
        body.push_str("## Changelog\n\n");
        body.push_str(&changelog);
    }

    log::info(&format!("Creating unified release: {tag}"));
    ci::gh_release_create(&tag, &title, &body, dry_run)?;

    Ok(())
}
