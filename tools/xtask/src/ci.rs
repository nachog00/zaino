//! Dry-run-aware helpers for CI operations.
//!
//! These functions shell out to git, knope, and gh. When `dry_run` is
//! true they log what they *would* do without executing side effects.

use std::process::Command;

use crate::log;

/// Run a command, streaming output. Returns the exit status.
fn exec(cmd: &mut Command, dry_run: bool) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    let display = format!("{program} {}", args.join(" "));

    if dry_run {
        log::info(&format!("[dry-run] would run: {display}"));
        return Ok(());
    }

    log::info(&format!("running: {display}"));
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("`{display}` exited with {status}"))
    }
}

/// Run a command and capture stdout. Not affected by dry_run -- used for
/// queries that don't mutate state (e.g. git rev-parse, git merge-base).
fn query(cmd: &mut Command) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`{program}` failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

/// Configure git user for CI commits.
pub(crate) fn git_configure(dry_run: bool) -> Result<(), String> {
    exec(
        Command::new("git").args(["config", "user.name", "github-actions[bot]"]),
        dry_run,
    )?;
    exec(
        Command::new("git").args(["config", "user.email", "github-actions[bot]@users.noreply.github.com"]),
        dry_run,
    )
}

/// Check if `ancestor` is an ancestor of `branch`.
pub(crate) fn git_is_ancestor(ancestor: &str, branch: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, branch])
        .status()
        .map_err(|e| format!("git merge-base failed: {e}"))?;
    Ok(status.success())
}

/// Get the SHA of a ref.
pub(crate) fn git_rev_parse(refspec: &str) -> Result<String, String> {
    query(Command::new("git").args(["rev-parse", refspec]))
}

/// Check if a remote branch exists.
pub(crate) fn git_remote_branch_exists(branch: &str) -> Result<bool, String> {
    let result = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &format!("refs/remotes/origin/{branch}")])
        .status()
        .map_err(|e| format!("git show-ref failed: {e}"))?;
    Ok(result.success())
}

/// Checkout a branch (create if needed with `-B`).
pub(crate) fn git_checkout(branch: &str, create_at: Option<&str>, dry_run: bool) -> Result<(), String> {
    let mut cmd = Command::new("git");
    match create_at {
        Some(start) => { cmd.args(["-c", "advice.detachedHead=false", "checkout", "-B", branch, start]); }
        None => { cmd.args(["checkout", branch]); }
    };
    exec(&mut cmd, dry_run)
}

/// Merge a ref into the current branch.
pub(crate) fn git_merge(ref_: &str, message: &str, dry_run: bool) -> Result<(), String> {
    exec(
        Command::new("git").args(["merge", ref_, "-m", message]),
        dry_run,
    )
}

/// Stage all changes and commit.
pub(crate) fn git_commit_all(message: &str, dry_run: bool) -> Result<(), String> {
    exec(Command::new("git").args(["add", "-A"]), dry_run)?;
    exec(
        Command::new("git").args(["commit", "-m", message]),
        dry_run,
    )
}

/// Push a branch to origin.
pub(crate) fn git_push(branch: &str, dry_run: bool) -> Result<(), String> {
    exec(
        Command::new("git").args(["push", "origin", branch]),
        dry_run,
    )
}

/// Create a lightweight tag and push it.
pub(crate) fn git_tag_and_push(tag: &str, dry_run: bool) -> Result<(), String> {
    exec(Command::new("git").args(["tag", tag]), dry_run)?;
    exec(
        Command::new("git").args(["push", "origin", tag]),
        dry_run,
    )
}

// ---------------------------------------------------------------------------
// knope
// ---------------------------------------------------------------------------

/// Run `knope prepare-release`, optionally with a prerelease label.
pub(crate) fn knope_prepare_release(prerelease_label: Option<&str>, dry_run: bool) -> Result<(), String> {
    let mut cmd = Command::new("knope");
    cmd.arg("prepare-release");
    if let Some(label) = prerelease_label {
        cmd.args(["--prerelease-label", label]);
    }
    exec(&mut cmd, dry_run)
}

/// Run `knope release` to create GitHub releases.
pub(crate) fn knope_release(dry_run: bool) -> Result<(), String> {
    exec(Command::new("knope").arg("release"), dry_run)
}

// ---------------------------------------------------------------------------
// gh cli
// ---------------------------------------------------------------------------

/// Run a `gh api` call and capture stdout. Respects dry_run for mutating
/// calls (POST/PUT/DELETE). GET calls always execute (they're read-only).
fn gh_api(
    endpoint: &str,
    method: &str,
    fields: &[(&str, &str)],
    raw_fields: &[(&str, &str)],
    dry_run: bool,
) -> Result<String, String> {
    let is_mutating = method != "GET";

    let mut cmd = Command::new("gh");
    cmd.args(["api", endpoint, "--method", method]);
    for (key, value) in fields {
        cmd.args(["-f", &format!("{key}={value}")]);
    }
    for (key, value) in raw_fields {
        cmd.args(["-F", &format!("{key}={value}")]);
    }

    if dry_run && is_mutating {
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        log::info(&format!("[dry-run] would run: gh {}", args.join(" ")));
        return Ok("{}".into());
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run gh api: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api {endpoint} failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a `gh api` call with a JSON body piped via stdin.
fn gh_api_with_body(
    endpoint: &str,
    method: &str,
    json_body: &str,
    dry_run: bool,
) -> Result<String, String> {
    if dry_run {
        log::info(&format!(
            "[dry-run] would run: gh api {endpoint} --method {method} (body: {json_body})"
        ));
        return Ok("{}".into());
    }

    log::info(&format!("running: gh api {endpoint} --method {method}"));

    let mut child = Command::new("gh")
        .args(["api", endpoint, "--method", method, "--input", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run gh api: {e}"))?;

    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(json_body.as_bytes())
        .map_err(|e| format!("failed to write to gh stdin: {e}"))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api {endpoint} failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create a GitHub deployment. Returns the deployment ID.
pub(crate) fn gh_create_deployment(
    environment: &str,
    ref_: &str,
    description: &str,
    dry_run: bool,
) -> Result<String, String> {
    // Use --input - to pass JSON body directly, since required_contexts
    // is an array that -F can't handle properly.
    let body = format!(
        r#"{{"ref":"{}","environment":"{}","description":"{}","auto_merge":false,"required_contexts":[]}}"#,
        ref_, environment, description
    );

    let response = gh_api_with_body(
        "repos/{owner}/{repo}/deployments",
        "POST",
        &body,
        dry_run,
    )?;

    if dry_run {
        return Ok("dry-run-id".into());
    }

    // Extract "id" from JSON response.
    response
        .split("\"id\":")
        .nth(1)
        .and_then(|s| s.trim().split(|c: char| !c.is_ascii_digit()).next())
        .map(String::from)
        .ok_or_else(|| "cannot parse deployment id from response".into())
}

/// Update a GitHub deployment status.
pub(crate) fn gh_update_deployment_status(
    deployment_id: &str,
    state: &str,
    description: &str,
    dry_run: bool,
) -> Result<(), String> {
    let endpoint = format!(
        "repos/{{owner}}/{{repo}}/deployments/{deployment_id}/statuses"
    );
    let body = format!(r#"{{"state":"{state}","description":"{description}"}}"#);

    gh_api_with_body(&endpoint, "POST", &body, dry_run)?;
    Ok(())
}

/// Publish a crate to crates.io.
pub(crate) fn cargo_publish(crate_name: &str, dry_run: bool) -> Result<(), String> {
    let mut cmd = Command::new("cargo");
    cmd.args(["publish", "--package", crate_name]);
    if dry_run {
        cmd.arg("--dry-run");
    }
    exec(&mut cmd, false) // always execute (cargo --dry-run handles it)
}

/// Create a GitHub release via `gh release create`.
pub(crate) fn gh_release_create(
    tag: &str,
    title: &str,
    body: &str,
    dry_run: bool,
) -> Result<(), String> {
    if dry_run {
        log::info(&format!("[dry-run] would create release: tag={tag} title={title}"));
        return Ok(());
    }

    // Write body to a temp file to avoid shell quoting issues.
    let body_path = std::env::temp_dir().join("xtask-release-body.md");
    std::fs::write(&body_path, body)
        .map_err(|e| format!("cannot write release body: {e}"))?;

    exec(
        Command::new("gh").args([
            "release", "create", tag,
            "--title", title,
            "--notes-file", &body_path.to_string_lossy(),
        ]),
        false,
    )
}

// ---------------------------------------------------------------------------
// github actions outputs
// ---------------------------------------------------------------------------

/// Write a key=value pair to `$GITHUB_OUTPUT` if running in CI.
/// No-op locally.
pub(crate) fn set_output(key: &str, value: &str) {
    if let Ok(path) = std::env::var("GITHUB_OUTPUT") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
            let _ = writeln!(f, "{key}={value}");
        }
    }
}

// ---------------------------------------------------------------------------
// changesets
// ---------------------------------------------------------------------------

/// Check if any .changeset/*.md files exist.
pub(crate) fn has_changesets(root: &std::path::Path) -> bool {
    let dir = root.join(".changeset");
    if !dir.is_dir() {
        return false;
    }
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        })
        .unwrap_or(false)
}
