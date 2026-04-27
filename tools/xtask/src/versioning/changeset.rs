//! Changeset file validation.
//!
//! Changeset files live in `.changeset/*.md` and follow the knope/changesets
//! convention: YAML frontmatter mapping crate names to bump types, followed
//! by a markdown changelog description.
//!
//! Knope handles parsing, version bumping, and changelog generation.
//! This module only validates that changeset files reference crate names
//! declared in `knope.toml` and use valid bump types, catching typos before
//! they reach knope.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::{log, workspace};

const VALID_BUMPS: &[&str] = &["major", "minor", "patch"];

#[derive(Subcommand)]
pub(crate) enum Action {
    /// Validate all .changeset/*.md files.
    Validate,
}

pub(crate) fn run(action: Action, root: &Path) -> Result<(), String> {
    match action {
        Action::Validate => {
            let packages = workspace::knope_packages(root)?;
            let errors = validate(root, &packages);
            if errors.is_empty() {
                let count = changeset_files(root)?.len();
                if count == 0 {
                    log::info("No changeset files to validate.");
                } else {
                    log::ok(&format!("Validated {count} changeset file(s)."));
                }
                Ok(())
            } else {
                for e in &errors {
                    log::file_error(&e.file, e.line, &e.message);
                }
                Err(format!(
                    "Changeset validation failed with {} error(s).",
                    errors.len()
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core validation logic (no CLI concerns below this line)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ValidationError {
    file: String,
    line: Option<usize>,
    message: String,
}

fn validate(root: &Path, valid_crates: &BTreeSet<String>) -> Vec<ValidationError> {
    let files = match changeset_files(root) {
        Ok(f) => f,
        Err(msg) => {
            return vec![ValidationError {
                file: ".changeset/".into(),
                line: None,
                message: msg,
            }];
        }
    };

    files
        .iter()
        .flat_map(|path| validate_file(path, valid_crates))
        .collect()
}

fn validate_file(path: &Path, valid_crates: &BTreeSet<String>) -> Vec<ValidationError> {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return vec![ValidationError {
                file: name,
                line: None,
                message: format!("cannot read file: {e}"),
            }];
        }
    };

    let Some((frontmatter, body)) = split_frontmatter(&content) else {
        return vec![ValidationError {
            file: name,
            line: None,
            message: "missing or malformed YAML frontmatter (must be delimited by ---)".into(),
        }];
    };

    let mut errors = Vec::new();
    let mut has_entries = false;

    for (idx, line) in frontmatter.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line_num = idx + 2;

        let Some((crate_name, bump)) = line.split_once(':') else {
            errors.push(ValidationError {
                file: name.clone(),
                line: Some(line_num),
                message: format!(
                    "invalid frontmatter: '{line}' (expected 'crate-name: bump-type')"
                ),
            });
            continue;
        };

        let crate_name = crate_name.trim();
        let bump = bump.trim();

        if !valid_crates.contains(crate_name) {
            let crate_list: Vec<_> = valid_crates.iter().map(String::as_str).collect();
            errors.push(ValidationError {
                file: name.clone(),
                line: Some(line_num),
                message: format!(
                    "unknown crate '{crate_name}' (valid: {})",
                    crate_list.join(", ")
                ),
            });
        }

        if !VALID_BUMPS.contains(&bump) {
            errors.push(ValidationError {
                file: name.clone(),
                line: Some(line_num),
                message: format!("unknown bump '{bump}' (valid: {})", VALID_BUMPS.join(", ")),
            });
        }

        has_entries = true;
    }

    if !has_entries {
        errors.push(ValidationError {
            file: name.clone(),
            line: None,
            message: "no crate entries in frontmatter".into(),
        });
    }

    if body.trim().is_empty() {
        errors.push(ValidationError {
            file: name,
            line: None,
            message: "empty changelog description after frontmatter".into(),
        });
    }

    errors
}

fn changeset_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join(".changeset");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("cannot read .changeset/: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let content = content.trim_start();

    let rest = content.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);

    // Handle empty frontmatter (closing --- at start of rest).
    if let Some(after) = rest.strip_prefix("---") {
        let body = after.strip_prefix('\n').unwrap_or(after);
        return Some(("", body));
    }

    let closing = rest.find("\n---")?;
    let frontmatter = &rest[..closing];
    let after_closing = &rest[closing + 4..];
    let body = after_closing.strip_prefix('\n').unwrap_or(after_closing);

    Some((frontmatter, body))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_crates() -> BTreeSet<String> {
        ["zaino-state", "zainod", "zaino-common"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn split_frontmatter_valid() {
        let input = "---\nzaino-state: minor\n---\n\nSome description.\n";
        let (fm, body) = split_frontmatter(input).expect("should parse");
        assert_eq!(fm, "zaino-state: minor");
        assert_eq!(body.trim(), "Some description.");
    }

    #[test]
    fn split_frontmatter_multi_crate() {
        let input = "---\nzaino-state: minor\nzainod: patch\n---\n\nMulti-crate change.\n";
        let (fm, _body) = split_frontmatter(input).expect("should parse");
        assert!(fm.contains("zaino-state: minor"));
        assert!(fm.contains("zainod: patch"));
    }

    #[test]
    fn split_frontmatter_missing_closing() {
        assert!(split_frontmatter("---\nno closing\n").is_none());
    }

    #[test]
    fn split_frontmatter_no_opening() {
        assert!(split_frontmatter("no frontmatter").is_none());
    }

    #[test]
    fn catches_bad_crate() {
        let path = write_temp("---\nfake-crate: minor\n---\n\nSome change.\n");
        let errors = validate_file(&path, &test_crates());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("unknown crate"));
    }

    #[test]
    fn catches_bad_bump() {
        let path = write_temp("---\nzainod: huge\n---\n\nSome change.\n");
        let errors = validate_file(&path, &test_crates());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("unknown bump"));
    }

    #[test]
    fn catches_empty_body() {
        let path = write_temp("---\nzainod: patch\n---\n\n");
        let errors = validate_file(&path, &test_crates());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("empty changelog"));
    }

    #[test]
    fn accepts_valid() {
        let path = write_temp("---\nzaino-state: minor\nzainod: patch\n---\n\nGood change.\n");
        let errors = validate_file(&path, &test_crates());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    fn write_temp(content: &str) -> PathBuf {
        use std::time::SystemTime;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("time")
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("xtask-test-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("create tempdir");
        let path = dir.join("test.md");
        fs::write(&path, content).expect("write");
        path
    }
}
