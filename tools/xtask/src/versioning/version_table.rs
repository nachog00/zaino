//! Version table: a snapshot of per-crate versions with optional changelog
//! context.
//!
//! `to_markdown()` produces markdown for PR bodies and GitHub comments.
//! `to_terminal()` produces styled terminal output via `crate::log`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::log;

/// A snapshot of crate versions and their latest changelog entries.
pub(crate) struct VersionTable {
    pub(crate) entries: Vec<CrateVersion>,
}

pub(crate) struct CrateVersion {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) changelog: Option<String>,
}

impl VersionTable {
    /// Build a version table by reading Cargo.toml and CHANGELOG.md for each
    /// package declared in knope.toml.
    pub(crate) fn from_workspace(
        root: &Path,
        packages: &BTreeSet<String>,
    ) -> Result<Self, String> {
        let mut entries = Vec::with_capacity(packages.len());
        for name in packages {
            let version = read_version(root, name)?;
            let changelog_path = root.join("packages").join(name).join("CHANGELOG.md");
            let changelog = latest_changelog_section(&changelog_path);
            entries.push(CrateVersion {
                name: name.clone(),
                version,
                changelog,
            });
        }
        Ok(Self { entries })
    }

    /// Render as styled terminal output.
    pub(crate) fn to_terminal(&self) -> String {
        let name_width = self
            .entries
            .iter()
            .map(|e| e.name.len())
            .max()
            .unwrap_or(10);

        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                log::label(&entry.name),
                log::value(&entry.version),
                width = name_width,
            ));
        }
        out
    }

    /// Render the version table as markdown.
    pub(crate) fn to_markdown_table(&self) -> String {
        let mut out = String::new();
        out.push_str("| Crate | Version |\n");
        out.push_str("| ----- | ------- |\n");
        for entry in &self.entries {
            out.push_str(&format!("| {} | {} |\n", entry.name, entry.version));
        }
        out
    }

    /// Render changelog entries as markdown sections.
    pub(crate) fn to_markdown_changelog(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            if let Some(ref cl) = entry.changelog {
                out.push_str(&format!("### {}\n\n{cl}\n\n", entry.name));
            }
        }
        out
    }
}

fn read_version(root: &Path, crate_name: &str) -> Result<String, String> {
    let path = root.join("packages").join(crate_name).join("Cargo.toml");
    let content =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version") {
            if let Some(version) = rest.split('"').nth(1) {
                return Ok(version.to_string());
            }
        }
    }
    Err(format!("no version found in {}", path.display()))
}

fn latest_changelog_section(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut section = String::new();
    let mut in_section = false;

    for line in content.lines() {
        if line.starts_with("## ") {
            if in_section {
                break;
            }
            in_section = true;
            continue;
        }
        if in_section {
            section.push_str(line);
            section.push('\n');
        }
    }

    let trimmed = section.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
