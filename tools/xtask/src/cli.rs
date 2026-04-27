use clap::{Parser, Subcommand};

use crate::{versioning, workspace};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    /// Preview what would happen without executing side effects.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Versioning and release management.
    Versioning {
        #[command(subcommand)]
        action: versioning::Command,
    },
}

pub fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let root = workspace::root()?;

    match cli.command {
        Command::Versioning { action } => versioning::run(action, &root, cli.dry_run),
    }
}
