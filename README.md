# Virtu

Virtu is a Rust-based Linux GPU passthrough automation tool. Its goal is to guide a user from system detection to a working libvirt VM while making every risky system change inspectable, reversible, and verified.

Status: feature-complete for v1.0 across all milestones (detection, compatibility reporting, user-choice modeling and validation, dry-run planning, snapshot capture and rollback, Phase-A safe writers for GRUB and systemd-boot plus the VFIO modprobe and initramfs writers for mkinitcpio/dracut/update-initramfs, the post-reboot `virtu resume` path, libvirt domain XML generation + `virt-xml-validate` + `virsh define`, single-GPU passthrough hooks with display-manager-aware release/reattach scripts, the diagnostics knowledge base for GPU quirks and host-command error patterns, the TUI wizard for detection → choices → plan preview → confirm, native packages for Arch / Fedora / RHEL / openSUSE / Debian / Ubuntu, and the real-hardware test matrix + reproduction harness under `tests/HARDWARE_MATRIX.md` and `tests/scripts/`). Two follow-up audit fixes shipped after the matrix landed: PCI-id sort consistency in plan output, and a plan-time refusal of single-GPU hook hand-off when no managed display manager is detected. **264 tests pass; clippy is clean with `-D warnings`; fmt clean.**

The remaining work before tagging v1.0 is walking the test matrix on real hardware, plus a small set of polish items tracked under "Known Limitations" below. The hermetic test suite covers every parser, writer, and executor against an in-memory filesystem and fixture roots; `apply → reboot → resume` against a physical GPU is what the real-hardware matrix exists to validate.

Looking Glass is explicitly out of scope for v1.0. The data model and validation still understand `LookingGlassChoice` for forward compatibility, but no installer or auto-build path will ship. Users who want Looking Glass can install the client manually after Virtu finishes.

## Current Commands

```bash
cargo check
cargo test
cargo run -- wizard                        # Interactive TUI wizard (detection → choices → plan → confirm)
cargo run -- scan                          # Detect host and print compatibility findings
cargo run -- plan                          # Build a dry-run plan from recommended choices
cargo run -- status                        # Show current VFIO binding and IOMMU state
cargo run -- apply --phase a               # Dry-run Phase A
cargo run -- apply --phase a --confirm     # Execute Phase A, then reboot manually
cargo run -- resume                        # After reboot, verify and finish post-reboot steps
cargo run -- rollback --list               # List captured snapshots
cargo run -- rollback --to <id>            # Restore a captured snapshot
```

`scan`, `plan`, `status`, `apply`, and `resume` read `/proc`, `/sys`, `/etc/os-release`, systemd state, libvirt tools, and device nodes, so they are intended for Linux hosts. Tests run on any platform via fixture roots and an in-memory filesystem for snapshot work.

## Safety Model

Virtu must never silently change a system. The implementation follows this order for every mutating feature:

1. Detect the exact current state.
2. Build a plan that names every file and command involved.
3. Create a rollback snapshot.
4. Apply one atomic step.
5. Verify the expected state.
6. Diagnose or roll back on failure.

The `plan` command exposes step 2 in full: every step declares its risk, privilege need, touched files, commands, verification description, rollback description, reboot requirement, and explicit-confirmation flag. Step 3 is implemented via `Snapshot::capture` (manifest-backed under `~/.virtu/snapshots/<id>/`). Step 4 is implemented via `apply --phase a --confirm` (snapshot, bootloader edit, VFIO modprobe, initramfs rebuild, with host-command regenerate). Step 5 is implemented via `virtu resume`, which re-detects the post-reboot host, verifies that IOMMU is active, vfio-pci is bound to the requested PCI ids, and the kernel cmdline carries the expected parameters; on `Ready` it generates the libvirt domain XML, runs `virt-xml-validate`, writes the XML through the snapshot manifest, creates the disk image with `qemu-img create` if needed, registers the domain with `virsh define`, and (for single-GPU plans) installs syntax-checked libvirt hook scripts under `/etc/libvirt/hooks/qemu.d/<vm>/`. On `NotReady` or `WrongHost` it lists the divergences and points at rollback. Step 6 is implemented via `rollback --to <id>`.

GPU passthrough requires a host reboot to apply bootloader, initramfs, and module-load changes. Virtu handles this with a resumable two-phase model: Phase A (snapshot, bootloader edit, VFIO modprobe, initramfs rebuild) runs before the reboot and persists a `PendingPlan` record; the user reboots manually; Phase B (`virtu resume`) verifies the post-reboot state and finishes the workflow.

## Target Scope

The first production-quality slice should support:

- Arch, Fedora, Debian/Ubuntu, and openSUSE families (native packages available for all).
- GRUB2 and systemd-boot first, then rEFInd, Syslinux/Extlinux, and EFISTUB.
- Dual GPU, iGPU-host, and single-GPU passthrough (with libvirt hook scripts for display manager release/reattach).
- User-selected VM OS, ISO, RAM, CPU count, storage, monitor plan, and keyboard/mouse passthrough.
- Interactive TUI wizard for guided setup, or direct CLI commands for automation.

Looking Glass is excluded from v1.0; users who want it integrate it manually after Virtu defines the VM.
## Real-Hardware Testing

Virtu's hermetic test suite covers every parser, writer, and executor against an in-memory filesystem. Real GPUs and reboots cannot be exercised that way, so the [`tests/HARDWARE_MATRIX.md`](tests/HARDWARE_MATRIX.md) document defines the priority-tiered test matrix v1.0 must walk before tagging:

- Tier 1 (required for v1.0): NVIDIA dual-GPU on Arch + GRUB2 + SDDM + X11; AMD iGPU-host on Fedora + systemd-boot + GDM + Wayland; NVIDIA single-GPU on Arch + GRUB2 + SDDM + X11.
- Tier 2 (recommended): Intel iGPU-host, AMD with Secure Boot, NVIDIA single-GPU on systemd-boot, AMD single-GPU on Fedora.
- Tier 3 (stretch): exotic combinations including untested bootloaders, ship as known-limitations.

The reproduction harness in [`tests/scripts/`](tests/scripts/) drives one cell at a time:

```bash
tests/scripts/run_hardware_test.sh scan                              # read-only preflight
tests/scripts/run_hardware_test.sh apply --i-have-backups --confirm  # Phase A
sudo systemctl reboot                                                # manual reboot
tests/scripts/run_hardware_test.sh resume                            # Phase B
tests/scripts/run_hardware_test.sh rollback --confirm                # optional rollback test
```

Output lands under `tests/results/<UTC-timestamp>/` (gitignored) with `scan.txt`, `plan.txt`, `phase_a.txt`, `resume.txt`, `status.txt`, plus pre/post host-fact dumps. Fill in [`tests/RESULTS_TEMPLATE.md`](tests/RESULTS_TEMPLATE.md) to record verdicts.

When you find a regression, [`tests/scripts/capture_fixture.sh`](tests/scripts/capture_fixture.sh) snapshots the live host into a sanitized `tests/fixtures/<name>/` tree so the bug can be locked down with a hermetic regression test before it is fixed.

## Known Limitations

These are the items the v1.0 closeout audit surfaced as known-state. They are not blockers; each is a polish item or a deliberate v1.0 scope cut.

- **Real-hardware validation pending.** The hermetic test suite confirms every parser, writer, and executor works in isolation. `cargo test` does not exercise the actual `apply → reboot → resume` cycle against a physical GPU. Walk `tests/HARDWARE_MATRIX.md` Tier-1 cells before relying on Virtu in production.
- **Live `virt-xml-validate` smoke covers one config.** The env-gated test (`VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE=1`) renders the recommended-default Windows-11 plan and feeds it to libvirt's real schema validator. NVIDIA passthrough, ISO-attached, bridge networking, hugepages-enabled, and Linux-guest variants will likely pass too (the per-device golden tests pin their byte sequences) but have not been validated against the live binary.
- **Display managers other than GDM / SDDM / LightDM / greetd / ly / lxdm are reported as `Unknown`.** This is detection-honest, not a bug: a host running e.g. KDE Plasma's `plasmalogin` will not get single-GPU hook hand-off because the planner refuses unknown DMs at plan time. Switching to `SwitchInputs` is the supported workaround.
- **Bootloader writers ship for GRUB2 and systemd-boot only.** rEFInd, Syslinux/Extlinux, and EFISTUB are detected but the planner refuses to emit a Phase-A plan against them with a clear "writer not implemented yet" error.
- **Multi-GPU passthrough** (more than one GPU passed to a single VM) is rejected by validation. Single-VM-per-GPU is the supported model.
- **Looking Glass** is intentionally cut from v1.0. The data model and validation rules stay for forward compatibility; no installer, IVSHMEM tmpfile writer, or VM-XML `<shmem>` block ships. Users who want Looking Glass install it manually after Virtu defines the VM.
- **`virtu resume` does not yet auto-call `virt-xml-validate` against the rendered XML in `HostCommandMode::Skip`.** Production runs always use `HostCommandMode::Run`, which does invoke the validator; the env-gated smoke pins the live behavior. The hermetic full-cycle test runs in Skip mode for fixture-fs safety.
- **`shellcheck` is recommended but not yet wired into CI** for the harness scripts under `tests/scripts/`. Both scripts pass `bash -n`; the maintenance section in `tests/scripts/README.md` documents the recommended invocation.
- **`Cargo.toml` lists `tempfile` in both `[dependencies]` and `[dev-dependencies]`.** Allowed by cargo. Slightly redundant; kept so dev-only features can be added later without touching the runtime declaration.
- **`recommended_defaults` always picks Windows 11.** The TUI wizard's choice screen exposes guest-OS selection visually, but no CLI flag overrides the default for headless installs. CLI-driven overrides are scoped for a post-v1.0 wizard polish slice.

## Contributing

The repository is local-first by design. When adding new hardware combinations, follow the discipline `tests/HARDWARE_MATRIX.md` lays out: capture a sanitized fixture with `tests/scripts/capture_fixture.sh`, write a hermetic regression test that loads it through every `*_from_root` parser, and only then change the production code that should pass it.

Standard verification chain before any commit:

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --no-deps -- -D warnings
cargo test
```

Optional env-gated smokes that exercise real host commands when available:

```bash
VIRTU_RUN_VIRT_XML_VALIDATE_SMOKE=1 cargo test --lib   # libvirt schema validator
VIRTU_RUN_BASH_SYNTAX_SMOKE=1 cargo test               # bash -n on every hook script
VIRTU_RUN_CAPTURE_FIXTURE_SMOKE=<name> \
    cargo test --test capture_fixture_smoke            # validates a captured fixture
```

License: MIT. See [`LICENSE`](LICENSE).
