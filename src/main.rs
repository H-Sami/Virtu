use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tracing::info;
use virtu::cli::{ApplyPhase, Cli, Commands};
use virtu::{detect, engine, snapshot, tui};

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

async fn run_apply(phase: ApplyPhase, confirm: bool) -> Result<()> {
    match phase {
        ApplyPhase::A => {}
    }

    let virtu_home = dirs_home().join(".virtu");
    let snapshots_root = virtu_home.join("snapshots");
    let state_root = virtu_home.join("state");
    let pending_path = state_root.join(snapshot::pending::DEFAULT_FILENAME);

    if pending_path.exists() {
        anyhow::bail!(
            "A pending Virtu plan already exists at {}.\n\
             Run `virtu resume` after rebooting, or `virtu rollback --to <id>` to abort it.",
            pending_path.display()
        );
    }

    let profile = detect::scan_system().await?;
    let report = engine::build_compatibility_report(&profile);
    report.print_human();

    let config = virtu::vm::PassthroughConfig::recommended_defaults(&profile)
        .context("No GPUs detected; cannot build a plan.")?;

    let plan = match engine::plan(&profile, &report, &config) {
        Ok(plan) => plan,
        Err(err) => {
            println!("\nPlan refused: {err}");
            return Ok(());
        }
    };
    plan.print_human();

    if !confirm {
        println!(
            "\n--- DRY RUN ---\n\
             Re-run with `--confirm` to actually apply Phase A.\n\
             Phase A will: capture a snapshot, edit the bootloader, write the VFIO\n\
             modprobe snippet, rebuild the initramfs, and persist a pending-plan\n\
             record. The host will need a reboot before `virtu resume` can finish."
        );
        return Ok(());
    }

    let filesystem = snapshot::RealFileSystem::new();
    let outcome = engine::execute_phase_a(
        &plan,
        &profile,
        &config,
        &filesystem,
        &snapshots_root,
        &state_root,
        engine::RegenerateMode::Run,
    )
    .map_err(|err| anyhow::anyhow!("Phase A failed: {err}"))?;

    println!("\n=== PHASE A COMPLETE ===");
    println!("{}", outcome.next_step_message);
    println!(
        "Pending plan written to: {}",
        outcome.pending_plan_path.display()
    );
    Ok(())
}

async fn run_resume() -> Result<()> {
    let virtu_home = dirs_home().join(".virtu");
    let snapshots_root = virtu_home.join("snapshots");
    let state_root = virtu_home.join("state");
    let pending_path = state_root.join(snapshot::pending::DEFAULT_FILENAME);

    if !pending_path.exists() {
        anyhow::bail!(
            "No pending Virtu plan found at {}.\n\
             `virtu resume` only runs after a successful `virtu apply --phase a --confirm`.",
            pending_path.display()
        );
    }

    let raw = std::fs::read_to_string(&pending_path)
        .with_context(|| format!("reading pending-plan record at {}", pending_path.display()))?;
    let pending: snapshot::PendingPlan = toml::from_str(&raw)
        .with_context(|| format!("parsing pending-plan record at {}", pending_path.display()))?;

    println!("=== VIRTU RESUME ===");
    println!("Pending plan from: {}", pending.created_at.to_rfc3339());
    println!("Snapshot id:       {}", pending.snapshot_id);
    println!("Phase B steps:     {}", pending.remaining_steps.len());
    println!();

    let profile = detect::scan_system().await?;
    let readiness = engine::verify_phase_a_landed(&profile, &pending);
    match &readiness {
        engine::ResumeReadiness::Ready => {
            println!("Verifier: Phase A landed cleanly.\n");
        }
        engine::ResumeReadiness::NotReady { divergences } => {
            println!("Verifier: Phase A did NOT land cleanly. Resume refused.\n");
            for divergence in divergences {
                println!("  - {}", divergence.human_summary());
            }
            println!(
                "\nIf the bootloader edit failed, you can roll back with:\n  virtu rollback --to {}",
                pending.snapshot_id
            );
            return Ok(());
        }
        engine::ResumeReadiness::WrongHost { reasons } => {
            println!(
                "Verifier: this host does not match the one Phase A captured. Resume refused.\n"
            );
            for reason in reasons {
                println!("  - {}", reason.human_summary());
            }
            println!(
                "\nIf you intended to run Phase B on a different host, abort with:\n  virtu rollback --to {}",
                pending.snapshot_id
            );
            return Ok(());
        }
    }

    let filesystem = snapshot::RealFileSystem::new();
    let outcome = engine::execute_phase_b(
        &pending,
        &profile,
        &filesystem,
        &snapshots_root,
        &state_root,
        engine::HostCommandMode::Run,
    )
    .map_err(|err| anyhow::anyhow!("Phase B failed: {err}"))?;

    println!("\n=== PHASE B SUMMARY ===");
    if !outcome.completed_steps.is_empty() {
        println!("Completed:");
        for kind in &outcome.completed_steps {
            println!("  - {kind:?}");
        }
    }
    if !outcome.deferred_steps.is_empty() {
        println!("Deferred to a later milestone (not yet implemented):");
        for kind in &outcome.deferred_steps {
            println!("  - {kind:?}");
        }
    }

    // Defense-in-depth post-check: if Phase B installed single-GPU
    // hooks, re-read the freshly persisted manifest and run
    // `verify_hook_install` against the live filesystem. Catches:
    // - a partial chmod that left a script not executable,
    // - an OS update that overwrote the dispatcher between Phase B's
    //   write and resume's exit (rare, but possible if a package post-
    //   install hook fires inside the same transaction window),
    // - a manual edit a user might have done in another terminal.
    //
    // We do NOT fail the resume on divergences here — Phase B already
    // succeeded and the manifest is authoritative for rollback. We
    // surface them as warnings so the user can re-run `virtu apply` or
    // roll back before starting the VM.
    if outcome
        .completed_steps
        .contains(&engine::StepKind::HookInstall)
    {
        let manifest_path = snapshots_root
            .join(&outcome.snapshot_id)
            .join(snapshot::MANIFEST_FILENAME);
        match read_manifest_from_disk(&manifest_path) {
            Ok(manifest) => {
                let divergences =
                    engine::verify_hook_install(&manifest, &filesystem, &pending.config.vm_name);
                if !divergences.is_empty() {
                    println!("\n[WARN] Single-GPU hook scripts diverge from the Phase-B manifest:");
                    for divergence in &divergences {
                        println!("  - {}", divergence.human_summary());
                    }
                    println!(
                        "       Re-run `virtu apply` to regenerate, or `virtu rollback --to {}` to undo.",
                        outcome.snapshot_id
                    );
                }
            }
            Err(err) => {
                println!(
                    "\n[WARN] Could not re-read the snapshot manifest at {} to verify hook scripts: {err}.\n       Phase B succeeded; this is a verification-only failure.",
                    manifest_path.display()
                );
            }
        }
    }
    if outcome.pending_cleared {
        println!("\nPending-plan record cleared. You can run `virtu apply` again later.");
    } else {
        println!(
            "\nPending-plan record still on disk at {}. Inspect or remove it manually if you're done.",
            pending_path.display()
        );
    }
    println!("\nSnapshot id (kept for rollback): {}", outcome.snapshot_id);
    Ok(())
}

/// Read a SnapshotManifest from disk, propagating any I/O or parse
/// failures with context for the caller. Used by `run_resume` to
/// re-read the manifest Phase B just persisted so the post-Phase-B
/// hook verifier sees current bytes.
fn read_manifest_from_disk(path: &std::path::Path) -> Result<snapshot::SnapshotManifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing manifest at {}", path.display()))
}

/// Print a one-line warning if a pending Phase-A record exists. Called
/// from `virtu status` so the user is reminded of unfinished work.
fn check_pending_plan_warning() {
    let pending_path = dirs_home()
        .join(".virtu")
        .join("state")
        .join(snapshot::pending::DEFAULT_FILENAME);
    if pending_path.exists() {
        println!(
            "\n[NOTE] A Phase-A apply is pending. Reboot and run `virtu resume` to finish, or `virtu rollback --to <id>` to abort.\n       Record: {}",
            pending_path.display()
        );
    }
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
            let report = engine::build_compatibility_report(&profile);
            report.print_human();
        }
        Some(Commands::Plan) => {
            let profile = detect::scan_system().await?;
            let report = engine::build_compatibility_report(&profile);
            report.print_human();

            let Some(config) = virtu::vm::PassthroughConfig::recommended_defaults(&profile) else {
                println!("No GPUs detected; cannot build a plan.");
                return Ok(());
            };
            match engine::plan(&profile, &report, &config) {
                Ok(plan) => plan.print_human(),
                Err(err) => println!("\nPlan refused: {err}"),
            }
        }
        Some(Commands::Apply { phase, confirm }) => {
            run_apply(phase, confirm).await?;
        }
        Some(Commands::Resume) => {
            run_resume().await?;
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
            check_pending_plan_warning();
            detect::print_vfio_status().await?;
        }
    }

    Ok(())
}
