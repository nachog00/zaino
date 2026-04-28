//! Generate the release dashboard PR title and body.
//!
//! The body is regenerated each time rc advances, showing "what would
//! happen if merged now". History is tracked via PR comments (one per
//! RC update).

use std::path::Path;

use super::name::ReleaseName;
use crate::versioning::version_table::VersionTable;
use crate::workspace;

/// Derive the release PR title from the current release schedule.
pub(crate) fn title() -> String {
    ReleaseName::current().pr_title()
}

/// The RC comment for the PR trail (e.g. "RC: 2026-05-02-rc.1 (abc1234)").
pub(crate) fn rc_comment(short_sha: &str) -> String {
    let name = ReleaseName::current();
    format!("**{}** ({})", name.rc_tag(), short_sha)
}

/// Print the release PR body (markdown) to stdout.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let packages = workspace::knope_packages(root)?;
    let table = VersionTable::from_workspace(root, &packages)?;
    let name = ReleaseName::current();

    let mut body = String::new();

    body.push_str(&format!(
        "Merging this PR promotes **{}** to stable and triggers the release workflow.\n\n",
        name.rc_tag()
    ));

    body.push_str("---\n\n");
    body.push_str("## Crate Versions\n\n");
    body.push_str(&table.to_markdown_table());

    let changelog = table.to_markdown_changelog();
    if !changelog.is_empty() {
        body.push_str("\n---\n\n");
        body.push_str("## Changelog\n\n");
        body.push_str(&changelog);
    }

    print!("{body}");
    Ok(())
}
