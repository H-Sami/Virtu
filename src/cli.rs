use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "virtu",
    version,
    about = "GPU passthrough automation for Linux — from detection to running VM",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Launch the interactive setup wizard (default)
    Wizard,

    /// Scan and display system compatibility report without making any changes
    Scan,

    /// Build a dry-run plan from detected state and recommended user choices.
    /// No host changes are made.
    Plan,

    /// Roll back a previous Virtu configuration
    Rollback {
        /// List available snapshots
        #[arg(long)]
        list: bool,

        /// Restore a specific snapshot by ID
        #[arg(long = "to", value_name = "SNAPSHOT_ID")]
        snapshot_id: Option<String>,
    },

    /// Show current VFIO binding status and IOMMU state
    Status,
}
