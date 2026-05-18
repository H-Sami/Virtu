# Virtu

Virtu is a Rust-based Linux GPU passthrough automation tool. Its goal is to guide a user from system detection to a working libvirt VM while making every risky system change inspectable, reversible, and verified.

This repository is in active development. Detection, compatibility reporting, user-choice modeling with read-only validation, dry-run planning, snapshot capture, manifest-backed atomic writes, rollback, Phase-A safe writers (GRUB / systemd-boot / VFIO / initramfs) with host-command regenerate, and the post-reboot `virtu resume` path are all in place. VM XML generation and libvirt registration, Looking Glass, and single-GPU hooks are scoped for the next milestones and are not implemented yet.

## Current Commands

```bash
cargo check
cargo test
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

The `plan` command exposes step 2 in full: every step declares its risk, privilege need, touched files, commands, verification description, rollback description, reboot requirement, and explicit-confirmation flag. Step 3 is implemented via `Snapshot::capture` (manifest-backed under `~/.virtu/snapshots/<id>/`). Step 4 is implemented via `apply --phase a --confirm` (snapshot, bootloader edit, VFIO modprobe, initramfs rebuild, with host-command regenerate). Step 5 is implemented via `virtu resume`, which re-detects the post-reboot host, verifies that IOMMU is active, vfio-pci is bound to the requested PCI ids, and the kernel cmdline carries the expected parameters; on `Ready` it finishes any remaining steps, on `NotReady` or `WrongHost` it lists the divergences and points at rollback. Step 6 is implemented via `rollback --to <id>`.

GPU passthrough requires a host reboot to apply bootloader, initramfs, and module-load changes. Virtu handles this with a resumable two-phase model: Phase A (snapshot, bootloader edit, VFIO modprobe, initramfs rebuild) runs before the reboot and persists a `PendingPlan` record; the user reboots manually; Phase B (`virtu resume`) verifies the post-reboot state and finishes the workflow.

## Target Scope

The first production-quality slice should support:

- Arch, Fedora, Debian/Ubuntu, and openSUSE families.
- GRUB2 and systemd-boot first, then rEFInd, Syslinux/Extlinux, and EFISTUB.
- Dual GPU and iGPU-host setups before single-GPU hooks.
- User-selected VM OS, ISO, RAM, CPU count, storage, monitor plan, Looking Glass preference, and keyboard/mouse passthrough.
