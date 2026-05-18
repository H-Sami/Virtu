# Virtu

Virtu is a Rust-based Linux GPU passthrough automation tool. Its goal is to guide a user from system detection to a working libvirt VM while making every risky system change inspectable, reversible, and verified.

This repository is in active development. The detection layer, compatibility report, user-choice model with read-only validation, dry-run planner, snapshot capture, manifest-backed atomic writes, and rollback are in place. Live bootloader/initramfs/VFIO writers, VM registration, Looking Glass setup, and single-GPU hooks are not implemented yet.

## Current Commands

```bash
cargo check
cargo test
cargo run -- scan                  # Detect host and print compatibility findings
cargo run -- plan                  # Build a dry-run plan from recommended choices
cargo run -- status                # Show current VFIO binding and IOMMU state
cargo run -- rollback --list       # List captured snapshots
cargo run -- rollback --to <id>    # Restore a captured snapshot
```

`scan`, `plan`, and `status` read `/proc`, `/sys`, `/etc/os-release`, systemd state, libvirt tools, and device nodes, so they are intended for Linux hosts. Tests run on any platform via fixture roots.

## Safety Model

Virtu must never silently change a system. The implementation follows this order for every mutating feature:

1. Detect the exact current state.
2. Build a plan that names every file and command involved.
3. Create a rollback snapshot.
4. Apply one atomic step.
5. Verify the expected state.
6. Diagnose or roll back on failure.

The `plan` command exposes step 2 in full: every step declares its risk, privilege need, touched files, commands, verification description, rollback description, reboot requirement, and explicit-confirmation flag. Step 3 is implemented via `Snapshot::capture` (manifest-backed under `~/.virtu/snapshots/<id>/`), and step 6's rollback path is `virtu rollback --to <id>`. Steps 4-5 are tracked under Phase 6.

GPU passthrough requires a host reboot to apply bootloader, initramfs, and module-load changes. Virtu handles this with a two-phase model: Phase A (snapshot, bootloader edit, VFIO modprobe, initramfs rebuild) runs before reboot; Phase B (`virtu resume`, planned next) verifies the new boot state and finishes VM creation.

## Target Scope

The first production-quality slice should support:

- Arch, Fedora, Debian/Ubuntu, and openSUSE families.
- GRUB2 and systemd-boot first, then rEFInd, Syslinux/Extlinux, and EFISTUB.
- Dual GPU and iGPU-host setups before single-GPU hooks.
- User-selected VM OS, ISO, RAM, CPU count, storage, monitor plan, Looking Glass preference, and keyboard/mouse passthrough.
