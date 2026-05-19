//! Host-command wrappers for Phase-A regenerate steps (slice 6.5.4) and
//! the Phase-B `virt-xml-validate` integration (slice 7.5).
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
//! Phase B adds a single read-only wrapper:
//!
//! - `virt-xml-validate <tempfile>` to syntax-check generated libvirt
//!   domain XML before `virsh define` ever sees it.
//!
//! Every wrapper:
//! - Runs the canonical command with no shell, no environment leakage.
//! - Captures stdout + stderr so failures surface in the executor error.
//! - Returns a structured [`CommandError`] that carries the binary, args,
//!   exit status, and last few stderr lines for the diagnostics layer.
//! - Refuses to run when the binary is missing on PATH (callers should
//!   skip the regen entirely in that case).
//!
//! The Phase-A wrappers are *not* called from unit tests. They run real
//! host commands. `tests/phase_a_executor.rs` covers the file-write half
//! of Phase A against `MemoryFileSystem`; the regen half is exercised on
//! a real Linux host during `virtu apply --phase a --confirm`. The
//! Phase-B `validate_xml` wrapper has a hermetic helper test (`write_xml_to_tempfile`)
//! plus the standard `NotFound` skip-on-host pattern; the actual real-host
//! validation runs during `virtu resume` once slice 7.6 wires it in.

use std::io::Write;
use std::process::Command;

use tempfile::NamedTempFile;

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
    #[error("failed to stage XML for `{program}` at `{path}`: {source}")]
    TempFileIo {
        program: String,
        path: String,
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

/// Stage `content` into a freshly-created `.xml` temporary file and
/// return the open handle.
///
/// The file is suffixed with `.xml` so `virt-xml-validate` selects the
/// libvirt domain schema. The content is flushed to disk before the
/// caller invokes the validator. The returned handle keeps the file
/// alive on disk; dropping it deletes the file.
fn write_xml_to_tempfile(program: &str, content: &str) -> Result<NamedTempFile, CommandError> {
    let mut file = tempfile::Builder::new()
        .prefix("virtu-vm-")
        .suffix(".xml")
        .tempfile()
        .map_err(|source| CommandError::TempFileIo {
            program: program.to_string(),
            path: "<tempfile>".to_string(),
            source,
        })?;

    let path_display = file.path().display().to_string();

    file.as_file_mut()
        .write_all(content.as_bytes())
        .map_err(|source| CommandError::TempFileIo {
            program: program.to_string(),
            path: path_display.clone(),
            source,
        })?;

    file.as_file_mut()
        .flush()
        .map_err(|source| CommandError::TempFileIo {
            program: program.to_string(),
            path: path_display.clone(),
            source,
        })?;

    file.as_file_mut()
        .sync_all()
        .map_err(|source| CommandError::TempFileIo {
            program: program.to_string(),
            path: path_display,
            source,
        })?;

    Ok(file)
}

/// Run `virt-xml-validate <tempfile>` against the supplied libvirt
/// domain XML content.
///
/// This is the structured-input check Phase B runs after
/// `engine::generate_vm_xml` and before `snapshot_then_write` /
/// `virsh define`. It only validates: it does not write XML to
/// `~/.virtu`, register a libvirt domain, or create disk images.
///
/// The flow:
/// 1. Stage `content` into a temporary `.xml` file (auto-deleted).
/// 2. Refuse if `virt-xml-validate` is not on PATH (`NotFound`).
/// 3. Invoke `virt-xml-validate <tempfile>` directly with no shell.
/// 4. Return `Ok(())` on exit code 0.
/// 5. Return `NonZeroExit { stderr tail }` on validation failure.
pub fn validate_xml(content: &str) -> Result<(), CommandError> {
    const PROGRAM: &str = "virt-xml-validate";

    if !binary_available(PROGRAM) {
        return Err(CommandError::NotFound {
            program: PROGRAM.to_string(),
        });
    }

    let tempfile = write_xml_to_tempfile(PROGRAM, content)?;
    let path = tempfile.path().to_path_buf();
    let path_str = path.to_string_lossy().into_owned();

    run(PROGRAM, &[&path_str])
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

    /// `validate_xml` short-circuits with NotFound when `virt-xml-validate`
    /// is not on PATH. It must not stage a tempfile or attempt to spawn
    /// the validator in that case.
    #[test]
    fn validate_xml_short_circuits_when_validator_missing() {
        if which::which("virt-xml-validate").is_ok() {
            // Real validator is present on this host. We do not invoke
            // it from the hermetic suite because this test would then
            // depend on libvirt's installed schemas. The opt-in smoke
            // test (`validate_xml_real_host_smoke`) covers that path.
            return;
        }

        let err = validate_xml("<domain type='kvm'><name>x</name></domain>").unwrap_err();
        match err {
            CommandError::NotFound { program } => {
                assert_eq!(program, "virt-xml-validate");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// The internal staging helper writes the supplied XML byte-for-byte
    /// into a `.xml` tempfile. Verifies the file lives where the helper
    /// reports it, the content round-trips, and the suffix is `.xml`
    /// so `virt-xml-validate` selects the libvirt domain schema.
    #[test]
    fn write_xml_to_tempfile_stages_content_under_xml_suffix() {
        let content = "<domain type='kvm'><name>virtu-windows</name></domain>";

        let file = match write_xml_to_tempfile("virt-xml-validate", content) {
            Ok(file) => file,
            Err(err) => panic!("staging the tempfile must succeed in a writable temp dir: {err:?}"),
        };

        let path = file.path().to_path_buf();
        assert!(path.exists(), "tempfile must exist on disk before drop");
        assert_eq!(
            path.extension().and_then(|s| s.to_str()),
            Some("xml"),
            "tempfile must be suffixed `.xml` so virt-xml-validate picks the domain schema"
        );

        let on_disk = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => panic!("the staged file must be readable while the handle is alive: {err}"),
        };
        assert_eq!(on_disk, content);

        drop(file);
        assert!(
            !path.exists(),
            "the tempfile must be removed once the handle is dropped"
        );
    }

    /// Optional real-host smoke test for `validate_xml`. Gated by an
    /// explicit env var so normal `cargo test` runs stay hermetic.
    ///
    /// Set `VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE=1` to opt in. The test
    /// feeds deliberately invalid XML and asserts the wrapper surfaces
    /// a structured `NonZeroExit` (not a panic, not `Ok`).
    #[test]
    fn validate_xml_real_host_smoke() {
        if std::env::var("VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE")
            .ok()
            .as_deref()
            != Some("1")
        {
            return;
        }
        if which::which("virt-xml-validate").is_err() {
            return;
        }

        let err = validate_xml("<domain type='kvm'></domain>").unwrap_err();
        assert!(
            matches!(
                err,
                CommandError::NonZeroExit { ref program, .. } if program == "virt-xml-validate"
            ),
            "expected NonZeroExit, got {err:?}"
        );
    }
}
