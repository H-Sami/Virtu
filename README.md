# Virtu

Virtu is a Rust-based Linux GPU passthrough automation tool. Its goal is to guide a user from system detection to a working libvirt VM while making every risky system change inspectable, reversible, and verified.

This repository is currently an early scaffold. The detection layer and core project structure exist; bootloader writers, initramfs writers, VFIO application, VM registration, Looking Glass setup, and single-GPU hooks are not implemented yet.

## Current Commands

```powershell
cargo check
cargo test
cargo run -- scan
cargo run -- status
```

`scan` and `status` are intended for Linux hosts. They read `/proc`, `/sys`, `/etc/os-release`, systemd state, libvirt tools, and device nodes.

## Safety Model

Virtu must never silently change a system. The implementation should follow this order for every mutating feature:

1. Detect the exact current state.
2. Build a plan that names every file and command involved.
3. Create a rollback snapshot.
4. Apply one atomic step.
5. Verify the expected state.
6. Diagnose or roll back on failure.

## Target Scope

The first production-quality slice should support:

- Arch, Fedora, Debian/Ubuntu, and openSUSE families.
- GRUB2 and systemd-boot first, then rEFInd, Syslinux/Extlinux, and EFISTUB.
- Dual GPU and iGPU-host setups before single-GPU hooks.
- User-selected VM OS, ISO, RAM, CPU count, storage, monitor plan, Looking Glass preference, and keyboard/mouse passthrough.
