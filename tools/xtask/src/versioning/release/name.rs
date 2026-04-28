//! Release naming: derives the release target date and RC label.
//!
//! Releases follow a periodic schedule. The release name is based on the
//! next scheduled release date. RCs within a period are numbered
//! sequentially: `2026-05-02-rc.0`, `2026-05-02-rc.1`, etc.

use std::process::Command;

use chrono::{Datelike, Local, NaiveDate, Weekday};

/// Release schedule configuration.
///
/// `RELEASE_EPOCH` is the Friday that anchors the first release period.
/// `RELEASE_CADENCE_WEEKS` controls how many weeks between releases.
const RELEASE_EPOCH: &str = "2026-05-01";
const RELEASE_CADENCE_WEEKS: u32 = 1;

/// A release name with structured access to its components.
pub(crate) struct ReleaseName {
    target_date: NaiveDate,
    rc_number: u32,
}

impl ReleaseName {
    /// Compute the release name for the current date and git state.
    pub(crate) fn current() -> Self {
        let target_date = next_release_date();
        let target_str = target_date.format("%Y-%m-%d").to_string();
        let rc_number = next_rc_number(&target_str);
        Self {
            target_date,
            rc_number,
        }
    }

    /// The release target date (e.g. `2026-05-02`).
    pub(crate) fn target(&self) -> String {
        self.target_date.format("%Y-%m-%d").to_string()
    }

    /// The RC number within this release period.
    pub(crate) fn rc_number(&self) -> u32 {
        self.rc_number
    }

    /// The full RC tag (e.g. `2026-05-02-rc.0`).
    pub(crate) fn rc_tag(&self) -> String {
        format!("{}-rc.{}", self.target(), self.rc_number)
    }

    /// The PR title for this release (e.g. `Release 2026-05-02`).
    pub(crate) fn pr_title(&self) -> String {
        format!("Release {}", self.target())
    }
}

/// Compute the next release target date (a Friday) based on the
/// configured epoch and cadence.
fn next_release_date() -> NaiveDate {
    let epoch = NaiveDate::parse_from_str(RELEASE_EPOCH, "%Y-%m-%d")
        .expect("RELEASE_EPOCH must be a valid date");
    debug_assert_eq!(
        epoch.weekday(),
        Weekday::Fri,
        "RELEASE_EPOCH must be a Friday"
    );

    let today = Local::now().date_naive();
    let cadence_days = (RELEASE_CADENCE_WEEKS * 7) as i64;

    if today <= epoch {
        return epoch;
    }

    let days_since_epoch = (today - epoch).num_days();
    let periods_elapsed = days_since_epoch / cadence_days;
    let next_period = periods_elapsed + 1;
    epoch + chrono::Duration::days(next_period * cadence_days)
}

/// Count existing RC tags for a release target to derive the next RC number.
fn next_rc_number(target: &str) -> u32 {
    let prefix = format!("{target}-rc.");
    let output = Command::new("git")
        .args(["tag", "-l", &format!("{prefix}*")])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    output
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .map(|n| n + 1)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_a_friday() {
        let epoch = NaiveDate::parse_from_str(RELEASE_EPOCH, "%Y-%m-%d").unwrap();
        assert_eq!(epoch.weekday(), Weekday::Fri);
    }

    #[test]
    fn next_release_is_a_friday() {
        let date = next_release_date();
        assert_eq!(date.weekday(), Weekday::Fri);
    }

    #[test]
    fn next_release_is_in_the_future() {
        let today = Local::now().date_naive();
        let date = next_release_date();
        assert!(date >= today);
    }

    #[test]
    fn release_name_format() {
        let name = ReleaseName {
            target_date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            rc_number: 3,
        };
        assert_eq!(name.target(), "2026-05-01");
        assert_eq!(name.rc_tag(), "2026-05-01-rc.3");
        assert_eq!(name.pr_title(), "Release 2026-05-01");
        assert_eq!(name.rc_number(), 3);
    }
}
