//! Shared workspace infrastructure.
//!
//! Provides the workspace root path and reads shared configuration from
//! `knope.toml` (the single source of truth for versioned package names).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Find the workspace root by walking up from CWD.
pub(crate) fn root() -> Result<PathBuf, String> {
    let mut dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            let content = fs::read_to_string(&candidate)
                .map_err(|e| format!("cannot read {}: {e}", candidate.display()))?;
            if content.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("cannot find workspace root (no Cargo.toml with [workspace])".into());
        }
    }
}

/// Extract the set of versioned package names from `knope.toml`.
pub(crate) fn knope_packages(root: &Path) -> Result<BTreeSet<String>, String> {
    let path = root.join("knope.toml");
    let content =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    let config: KnopeConfig =
        toml::from_str(&content).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

    if config.packages.is_empty() {
        return Err(format!("{} has no [packages.*] sections", path.display()));
    }

    Ok(config.packages.into_keys().collect())
}

#[derive(Deserialize)]
struct KnopeConfig {
    #[serde(default)]
    packages: BTreeMap<String, toml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_knope_packages() {
        let toml_str = r#"
[packages.zaino-state]
versioned_files = ["packages/zaino-state/Cargo.toml"]

[packages.zainod]
versioned_files = ["packages/zainod/Cargo.toml"]

[github]
owner = "zingolabs"
repo = "zaino"
"#;
        let config: KnopeConfig = toml::from_str(toml_str).expect("parse");
        let names: BTreeSet<_> = config.packages.into_keys().collect();
        assert!(names.contains("zaino-state"));
        assert!(names.contains("zainod"));
        assert_eq!(names.len(), 2);
    }
}
