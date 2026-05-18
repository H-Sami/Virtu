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

    /// Apply a plan to the host. Phase A (snapshot, bootloader edit, VFIO
    /// modprobe, initramfs rebuild) is the only phase wired up so far. After
    /// Phase A finishes, reboot the host and run `virtu resume`.
    Apply {
        /// Which apply phase to run. Currently only `a` is supported.
        #[arg(long, value_name = "PHASE", default_value = "a")]
        phase: ApplyPhase,

        /// Required to actually mutate the host. Without `--confirm`, the
        /// command prints what *would* happen but does not write anything.
        #[arg(long)]
        confirm: bool,
    },

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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ApplyPhase {
    /// Phase A: snapshot, bootloader edit, VFIO modprobe, initramfs rebuild.
    /// User must reboot afterwards.
    A,
}
