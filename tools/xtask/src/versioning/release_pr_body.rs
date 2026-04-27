//! Generate the release dashboard PR body.
//!
//! Pure computation: builds a `VersionTable` and renders it as markdown
//! for the rc→stable PR. Outputs to stdout.

use std::path::Path;

use super::version_table::VersionTable;
use crate::workspace;

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let packages = workspace::knope_packages(root)?;
    let table = VersionTable::from_workspace(root, &packages)?;

    let mut body = String::new();
    body.push_str(
        "Merging this PR promotes the current RC to stable and triggers the release workflow.\n\n",
    );
    body.push_str("## Crate Versions\n\n");
    body.push_str(&table.to_markdown());

    print!("{body}");
    Ok(())
}
