use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tracing::info;
use virtu::cli::{Cli, Commands};
use virtu::{detect, snapshot, tui};

fn setup_logging() -> Result<()> {
    let log_dir = dirs_home().join(".virtu").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let log_file = log_dir.join(format!("{timestamp}.log"));

    let file_appender = tracing_appender::rolling::never(log_dir, format!("{timestamp}.log"));
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .init();

    // Store the guard so the logger isn't dropped
    // In practice, store this in a OnceCell or pass it up
    std::mem::forget(_guard);

    info!(
        "Virtu {} starting up. Log: {}",
        env!("CARGO_PKG_VERSION"),
        log_file.display()
    );
    Ok(())
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn check_not_root() -> Result<()> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            anyhow::bail!(
                "Virtu must not be run as root.\n\
                 It will request elevated privileges only for specific operations via sudo.\n\
                 Please run as your normal user account."
            );
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Never run as root
    check_not_root()?;

    setup_logging().context("Failed to initialize logging")?;

    match cli.command {
        None | Some(Commands::Wizard) => {
            tui::run_wizard().await?;
        }
        Some(Commands::Scan) => {
            let profile = detect::scan_system().await?;
            detect::print_report(&profile);
        }
        Some(Commands::Rollback { list, snapshot_id }) => {
            if list {
                snapshot::list_snapshots()?;
            } else {
                let id = snapshot_id.context(
                    "Provide a snapshot ID with --to <ID>, or use --list to see available snapshots",
                )?;
                snapshot::rollback_to(&id).await?;
            }
        }
        Some(Commands::Status) => {
            detect::print_vfio_status().await?;
        }
    }

    Ok(())
}
