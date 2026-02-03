//! Collab CLI binary.

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "collab-cli")]
#[command(about = "CLI for collaborative document editing")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new collaborative document.
    Init {
        /// Document identifier.
        doc_id: String,
        /// Your user identifier.
        #[arg(short, long, default_value = "user")]
        user: String,
    },
    /// Join an existing collaborative document.
    Join {
        /// Path to the invite file.
        invite_path: String,
        /// Your user identifier.
        #[arg(short, long, default_value = "user")]
        user: String,
    },
    /// Connect to a relay and collaborate.
    Connect {
        /// Relay server URL.
        relay_url: String,
        /// Your user identifier.
        #[arg(short, long)]
        user: String,
        /// Document identifier.
        #[arg(short, long)]
        doc: String,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { doc_id, user } => {
            let invite = collab_cli::commands::init(&doc_id, &user)?;
            println!("{}", serde_json::to_string_pretty(&invite)?);
        }
        Commands::Join { invite_path, user } => {
            collab_cli::commands::join(&invite_path, &user)?;
            println!("Joined document successfully");
        }
        Commands::Connect { relay_url, user, doc } => {
            println!("Connecting to {relay_url} as {user} for doc {doc}");
            // TODO: Implement in T15
        }
    }

    Ok(())
}
