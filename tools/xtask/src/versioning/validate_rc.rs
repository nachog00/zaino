//! Heavy test validation for RC commits.
//!
//! Two operations:
//! - `deploy`: Create a GitHub deployment for an RC commit, triggering
//!   the external heavy test (k8s long sync). Sets status to `in_progress`.
//! - `promote`: Called when heavy tests pass. Advances `validated-rc`.
//!
//! The external system (k8s) or a manual operator reports the result by
//! updating the deployment status to `success` or `failure` via:
//!   gh api repos/{owner}/{repo}/deployments/{id}/statuses \
//!     --method POST -f state=success

use crate::{ci, log};

const HEAVY_TEST_ENVIRONMENT: &str = "heavy-test";

/// Deploy an RC commit for heavy testing.
///
/// Creates a GitHub deployment and sets it to `in_progress`. The external
/// system (or a manual operator) must report the result by updating the
/// deployment status.
pub(crate) fn deploy(rc_sha: &str, rc_tag: &str, dry_run: bool) -> Result<(), String> {
    log::info(&format!("Deploying {rc_tag} ({rc_sha}) for heavy testing."));

    let deployment_id = ci::gh_create_deployment(
        HEAVY_TEST_ENVIRONMENT,
        rc_sha,
        &format!("Heavy test for {rc_tag}"),
        dry_run,
    )?;

    log::info(&format!("Deployment created: {deployment_id}"));

    ci::gh_update_deployment_status(
        &deployment_id,
        "in_progress",
        &format!("{rc_tag} heavy test running"),
        dry_run,
    )?;

    log::ok(&format!("Heavy test deployment started for {rc_tag}."));
    log::info("Report result when ready:");
    log::info(&format!(
        "  gh api repos/{{owner}}/{{repo}}/deployments/{deployment_id}/statuses --method POST -f state=success"
    ));
    Ok(())
}

/// Promote an RC that passed heavy tests by advancing `validated-rc`.
pub(crate) fn promote(rc_sha: &str, dry_run: bool) -> Result<(), String> {
    log::info(&format!("Promoting {rc_sha} to validated-rc."));

    ci::git_checkout("validated-rc", Some(rc_sha), dry_run)?;
    ci::git_push("validated-rc", dry_run)?;

    log::ok(&format!("validated-rc advanced to {rc_sha}"));
    Ok(())
}
