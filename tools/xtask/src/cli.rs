use clap::{Parser, Subcommand};

use crate::{versioning, workspace};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
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
        Command::Versioning { action } => versioning::run(action, &root),
    }
}
