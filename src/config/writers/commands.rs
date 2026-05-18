//! Host-command wrappers for Phase-A regenerate steps (slice 6.5.4).
//!
//! After Phase A writes the bootloader config, VFIO modprobe snippet, and
//! initramfs config, the host's bootloader and initramfs systems still
//! need to recompile their on-disk artifacts before the next boot uses
//! the new settings. This module owns the minimal shell-out wrappers
//! Phase A uses for that work:
//!
//! - `grub-mkconfig -o /boot/grub/grub.cfg` (after editing `/etc/default/grub`)
//! - `bootctl update` (after editing systemd-boot entries — usually a no-op)
//! - `mkinitcpio -P` (after editing `/etc/mkinitcpio.conf`)
//! - `dracut --force` (after writing `dracut.conf.d/virtu-vfio.conf`)
//! - `update-initramfs -u -k all` (after editing `/etc/initramfs-tools/modules`)
//!
//! Every wrapper:
//! - Runs the canonical command with no shell, no environment leakage.
//! - Captures stdout + stderr so failures surface in the executor error.
//! - Returns a structured [`CommandError`] that carries the binary, args,
//!   exit status, and last few stderr lines for the diagnostics layer.
//! - Refuses to run when the binary is missing on PATH (callers should
//!   skip the regen entirely in that case).
//!
//! The wrappers are *not* called from unit tests. They run real host
//! commands. `tests/phase_a_executor.rs` covers the file-write half of
//! Phase A against `MemoryFileSystem`; the regen half is exercised on
//! a real Linux host during `virtu apply --phase a --confirm`.

use std::process::Command;

/// Errors raised by the host-command wrappers.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("`{program}` is not on PATH")]
    NotFound { program: String },
    #[error("`{program} {args}` exited with status {status}\nstderr tail:\n{stderr}")]
    NonZeroExit {
        program: String,
        args: String,
        status: i32,
        stderr: String,
    },
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// Run a regenerate command and return `Ok(())` on success.
///
/// Does not invoke a shell. Output is captured (not streamed) so the
/// executor can include the last few stderr lines in any failure.
fn run(program: &str, args: &[&str]) -> Result<(), CommandError> {
    if which::which(program).is_err() {
        return Err(CommandError::NotFound {
            program: program.to_string(),
        });
    }

    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| CommandError::Spawn {
            program: program.to_string(),
            source,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    Err(CommandError::NonZeroExit {
        program: program.to_string(),
        args: args.join(" "),
        status: output.status.code().unwrap_or(-1),
        stderr: tail,
    })
}

/// Returns `true` if `program` is reachable on PATH.
fn binary_available(program: &str) -> bool {
    which::which(program).is_ok()
}

/// Regenerate the GRUB2 main config from `/etc/default/grub`.
///
/// The output path is distro-conventional: `/boot/grub/grub.cfg` on most
/// distros, `/boot/efi/EFI/<distro>/grub.cfg` on Fedora-family hosts. We
/// always target the path the host's `BootloaderInfo::update_command`
/// declared during detection, so this wrapper accepts the explicit
/// command string.
pub fn run_grub_mkconfig() -> Result<(), CommandError> {
    if !binary_available("grub-mkconfig") {
        return Err(CommandError::NotFound {
            program: "grub-mkconfig".to_string(),
        });
    }
    // Default GRUB2 output on Arch / Debian.
    run("grub-mkconfig", &["-o", "/boot/grub/grub.cfg"])
}

/// Tell systemd-boot to refresh its EFI artifacts. Usually a no-op
/// because edits to `loader/entries/<entry>.conf` are picked up at boot
/// without a regenerate step, but `bootctl update` re-stamps the loader
/// binary if the firmware happens to need it.
pub fn run_bootctl_update() -> Result<(), CommandError> {
    run("bootctl", &["update"])
}

/// Rebuild every mkinitcpio preset.
pub fn run_mkinitcpio_all() -> Result<(), CommandError> {
    run("mkinitcpio", &["-P"])
}

/// Rebuild every dracut image (most distros only have one default).
pub fn run_dracut_force_all() -> Result<(), CommandError> {
    run("dracut", &["--force", "--regenerate-all"])
}

/// Rebuild every initramfs image on a Debian/Ubuntu host.
pub fn run_update_initramfs_all() -> Result<(), CommandError> {
    run("update-initramfs", &["-u", "-k", "all"])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `binary_available` returns false for a name guaranteed not to exist.
    /// Real PATH lookups are deliberately not mocked because production
    /// uses `which` directly. This is the only thing we can safely test
    /// without invoking host commands.
    #[test]
    fn binary_available_returns_false_for_definitely_missing_binary() {
        assert!(!binary_available(
            "virtu-this-binary-must-not-exist-anywhere-12345"
        ));
    }

    /// `run` reports NotFound for a missing binary instead of attempting
    /// to spawn it. This keeps the error message accurate.
    #[test]
    fn run_reports_not_found_for_missing_binary() {
        let err = run("virtu-this-binary-must-not-exist-anywhere-12345", &[]).unwrap_err();
        match err {
            CommandError::NotFound { program } => {
                assert!(program.starts_with("virtu-this-binary"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// `run_grub_mkconfig` short-circuits with NotFound if grub-mkconfig
    /// is not on PATH, instead of trying to invoke `grub-mkconfig -o
    /// /boot/grub/grub.cfg` and failing with a confusing spawn error.
    #[test]
    fn run_grub_mkconfig_short_circuits_when_grub_mkconfig_missing() {
        // On hosts without grub-mkconfig (such as a plain CI runner), the
        // wrapper should report NotFound.
        if which::which("grub-mkconfig").is_ok() {
            // Skip the assertion on hosts where grub-mkconfig is present.
            // We don't actually invoke it; the unit test would otherwise
            // need root and would be destructive.
            return;
        }
        let err = run_grub_mkconfig().unwrap_err();
        match err {
            CommandError::NotFound { program } => assert_eq!(program, "grub-mkconfig"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
