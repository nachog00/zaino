//! Consistent CLI output for xtask.
//!
//! All user-facing output goes through these helpers so the CLI has a
//! uniform look. Domain modules return data; this module renders it.

use console::style;

/// Print a success line: `ok Some message`
pub(crate) fn ok(message: &str) {
    println!("{} {message}", style("ok").green().bold());
}

/// Print a dimmed informational line.
pub(crate) fn info(message: &str) {
    println!("{}", style(message).dim());
}

/// Print an error summary to stderr: `error Some message`
pub(crate) fn error(message: &str) {
    eprintln!("{} {message}", style("error").red().bold());
}

/// Print a file-located error to stderr.
pub(crate) fn file_error(file: &str, line: Option<usize>, message: &str) {
    let location = match line {
        Some(n) => format!("{file}:{n}"),
        None => file.to_string(),
    };
    eprintln!(
        "  {} {}: {message}",
        style("error").red().bold(),
        style(location).cyan(),
    );
}

// ---------------------------------------------------------------------------
// Formatting helpers (return styled strings, don't print)
// ---------------------------------------------------------------------------

/// Format text as a label (cyan).
pub(crate) fn label(text: &str) -> String {
    format!("{}", style(text).cyan())
}

/// Format text as a highlighted value (green bold).
pub(crate) fn value(text: &str) -> String {
    format!("{}", style(text).green().bold())
}
