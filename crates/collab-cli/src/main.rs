//! Collab CLI binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "collab-cli")]
#[command(about = "CLI for collaborative document editing with E2E encryption")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new collaborative document as the owner.
    Init {
        /// Document identifier.
        doc_id: String,
        /// Your user identifier.
        #[arg(short, long, default_value = "user")]
        user: String,
        /// Save document state to file.
        #[arg(short, long)]
        state: Option<PathBuf>,
    },
    /// Generate a key package for joining a document.
    Keygen {
        /// Your user identifier.
        #[arg(short, long, default_value = "user")]
        user: String,
        /// Output file for the key package.
        #[arg(short, long, default_value = "keypackage.json")]
        output: PathBuf,
    },
    /// Create an invite for a new member (run by document owner).
    Invite {
        /// Document identifier.
        doc_id: String,
        /// Your user identifier (document owner).
        #[arg(short, long, default_value = "owner")]
        user: String,
        /// Path to the joiner's key package file.
        #[arg(short, long)]
        keypackage: PathBuf,
        /// Output file for the invite.
        #[arg(short, long, default_value = "invite.json")]
        output: PathBuf,
    },
    /// Join an existing collaborative document.
    Join {
        /// Path to the invite file.
        invite: PathBuf,
        /// Your user identifier.
        #[arg(short, long, default_value = "user")]
        user: String,
        /// Save document state to file.
        #[arg(short, long)]
        state: Option<PathBuf>,
    },
    /// Connect to a relay and collaborate (not yet implemented).
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
    /// Run a demo showing the full E2E encryption flow.
    Demo {
        /// Document identifier for the demo.
        #[arg(default_value = "demo-doc")]
        doc_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { doc_id, user, state } => {
            let result = collab_cli::commands::init(&doc_id, &user, state.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::Keygen { user, output } => {
            let result = collab_cli::commands::keygen(&user, &output)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::Invite { doc_id, user, keypackage, output } => {
            let result = collab_cli::commands::create_invite(&doc_id, &user, &keypackage, &output)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::Join { invite, user, state } => {
            let result = collab_cli::commands::join(&invite, &user, state.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::Connect { relay_url, user, doc } => {
            collab_cli::commands::connect(&relay_url, &user, &doc).await?;
        }
        Commands::Demo { doc_id } => {
            let result = collab_cli::commands::demo(&doc_id)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}
