//! Generate the release dashboard PR title and body.
//!
//! The body is regenerated each time rc advances, showing "what would
//! happen if merged now". History is tracked via PR comments (one per
//! RC update).

use std::path::Path;

use super::version_table::VersionTable;
use crate::workspace;

/// Derive the release PR title.
///
/// TODO: Final format TBD (likely date-based release identity).
pub(crate) fn title() -> String {
    "Release".to_string()
}

/// Print the release PR body (markdown) to stdout.
pub(crate) fn run(root: &Path) -> Result<(), String> {
    let packages = workspace::knope_packages(root)?;
    let table = VersionTable::from_workspace(root, &packages)?;

    let mut body = String::new();

    body.push_str(
        "Merging this PR promotes the current RC to stable and triggers the release workflow.\n\n",
    );

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
