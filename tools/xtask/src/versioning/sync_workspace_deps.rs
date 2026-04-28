//! Sync workspace dependency versions after knope bumps crate versions.
//!
//! When knope bumps `packages/zaino-common/Cargo.toml` from `0.1.0` to
//! `0.1.1-rc.0`, the root `Cargo.toml`'s `[workspace.dependencies]`
//! still says `version = "0.1.0"`. Cargo rejects the mismatch for
//! pre-releases. This module patches the root to match.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::log;

/// Read each crate's version from its Cargo.toml and update the
/// corresponding workspace dependency version in the root Cargo.toml.
pub(crate) fn run(root: &Path, dry_run: bool) -> Result<(), String> {
    let root_toml_path = root.join("Cargo.toml");
    let root_content = fs::read_to_string(&root_toml_path)
        .map_err(|e| format!("cannot read root Cargo.toml: {e}"))?;

    let crate_versions = read_crate_versions(root)?;

    let mut updated = root_content.clone();
    let mut changes = 0;

    for (name, version) in &crate_versions {
        // Match lines like: zaino-common = { path = "...", version = "0.1.0" }
        // We need to replace the version value while keeping everything else.
        let old_pattern = find_dep_version_in_workspace(&updated, name);
        if let Some((old_line, old_version)) = old_pattern {
            if *version != old_version {
                let new_line = old_line.replace(
                    &format!("version = \"{old_version}\""),
                    &format!("version = \"{version}\""),
                );
                updated = updated.replace(&old_line, &new_line);
                log::info(&format!("  {name}: {old_version} -> {version}"));
                changes += 1;
            }
        }
    }

    if changes == 0 {
        log::info("Workspace dependency versions already in sync.");
        return Ok(());
    }

    if dry_run {
        log::info(&format!(
            "[dry-run] would update {changes} workspace dependency version(s)"
        ));
        return Ok(());
    }

    fs::write(&root_toml_path, updated)
        .map_err(|e| format!("cannot write root Cargo.toml: {e}"))?;

    log::ok(&format!(
        "Updated {changes} workspace dependency version(s)."
    ));
    Ok(())
}

/// Read the `version` field from each crate's own Cargo.toml.
fn read_crate_versions(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut versions = BTreeMap::new();
    let packages_dir = root.join("packages");

    let entries = fs::read_dir(&packages_dir)
        .map_err(|e| format!("cannot read packages/: {e}"))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let cargo_toml = entry.path().join("Cargo.toml");
        if !cargo_toml.exists() {
            continue;
        }

        let name = entry
            .file_name()
            .to_string_lossy()
            .to_string();

        let content = fs::read_to_string(&cargo_toml)
            .map_err(|e| format!("cannot read {}: {e}", cargo_toml.display()))?;

        if let Some(version) = extract_package_version(&content) {
            versions.insert(name, version);
        }
    }

    Ok(versions)
}

fn extract_package_version(cargo_toml_content: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo_toml_content.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_package = false;
            continue;
        }
        if in_package {
            if let Some(rest) = trimmed.strip_prefix("version") {
                if let Some(version) = rest.split('"').nth(1) {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

/// Find the workspace dependency line for a crate and extract its version.
/// Returns (full_line, version_string) if found.
fn find_dep_version_in_workspace(root_toml: &str, crate_name: &str) -> Option<(String, String)> {
    for line in root_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(crate_name)
            && trimmed.contains("path")
            && trimmed.contains("version")
        {
            // Extract the version value from version = "X.Y.Z"
            if let Some(version_start) = trimmed.find("version = \"") {
                let after = &trimmed[version_start + 11..];
                if let Some(end) = after.find('"') {
                    let version = &after[..end];
                    return Some((line.to_string(), version.to_string()));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_dep_version() {
        let toml = r#"
[workspace.dependencies]
zaino-common = { path = "packages/zaino-common", version = "0.1.0" }
zaino-fetch = { path = "packages/zaino-fetch", version = "0.1.0" }
"#;
        let (line, version) = find_dep_version_in_workspace(toml, "zaino-common").unwrap();
        assert_eq!(version, "0.1.0");
        assert!(line.contains("zaino-common"));
    }

    #[test]
    fn extracts_package_version() {
        let toml = r#"
[package]
name = "zaino-common"
version = "0.1.1-rc.0"
edition = "2021"
"#;
        assert_eq!(
            extract_package_version(toml),
            Some("0.1.1-rc.0".to_string())
        );
    }

    #[test]
    fn skips_non_package_version() {
        let toml = r#"
[package]
name = "zaino-common"
version = "0.1.0"

[dependencies]
serde = { version = "1.0" }
"#;
        assert_eq!(
            extract_package_version(toml),
            Some("0.1.0".to_string())
        );
    }
}
