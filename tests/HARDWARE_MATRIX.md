# Virtu Hardware Test Matrix

This document defines the real-hardware validation matrix Virtu must
exercise before tagging v1.0. The hermetic test suite under
`cargo test` covers every parser, writer, and executor against
`MemoryFileSystem` and fixture roots; this matrix covers what the
hermetic suite cannot — the actual `apply → reboot → resume` cycle
on a physical machine that boots into the new kernel and binds
`vfio-pci` to a real GPU.

The matrix is consumed by the reproduction harness under
`tests/scripts/`:

- [`capture_fixture.sh`](scripts/capture_fixture.sh) snapshots a host
  into a sanitized fixture tree that can be added to
  `tests/fixtures/` for regression coverage.
- [`run_hardware_test.sh`](scripts/run_hardware_test.sh) drives the
  full `scan → plan → apply --phase a --confirm → reboot → resume`
  cycle on a single host and writes a timestamped log directory.
- [`RESULTS_TEMPLATE.md`](RESULTS_TEMPLATE.md) is the markdown
  template each run must fill out and attach to the GitHub issue
  tracking that hardware combination.

Throughout this document, **PASS** means Virtu produced a usable
host + VM end-state with rollback intact, **FAIL** means a regression
that blocks v1.0, and **PARTIAL** means a non-blocking limitation
that ships as a documented known-issue.

## Matrix Dimensions

The seven dimensions Virtu must reason about. Combinations are
counted multiplicatively only where they interact; many cells in the
full Cartesian product are redundant.

| Dimension | Values | Notes |
|---|---|---|
| GPU vendor | NVIDIA, AMD, Intel | Drives the unbind sequence in single-GPU hooks and the `<features><kvm><hidden state='on'/>` toggle. |
| Passthrough mode | Dual GPU, iGPU host, Single GPU | Selects between `GpuPassthroughMode::DualGpu`, `IgpuHost`, and `SingleGpu`. |
| Secure Boot | On, Off | When on, the host kernel enforces module signing. Virtu still emits `<features><smm state='on'/>` so OVMF Secure Boot inside the guest is independent of host Secure Boot. |
| Bootloader | GRUB2, systemd-boot | rEFInd, Syslinux/Extlinux, and EFISTUB are stretch goals; the planner refuses unknown bootloaders cleanly. |
| Distro family | Arch, Fedora, Debian/Ubuntu, openSUSE | Drives initramfs writer choice and OVMF path lookup via the bundled KB. |
| Display server | Wayland, X11 | Affects single-GPU hook recovery instructions only. The dispatcher script is identical. |
| Display manager | SDDM, GDM, LightDM, greetd, ly, lxdm | Drives `systemctl stop/start <dm>` lines in the begin/end hooks. |
| Monitors | One, Two, Two with second on passthrough GPU | Drives `MonitorPlan::OneMonitor` vs `TwoMonitors`. |

## Priority Tiers

Real-hardware testing is expensive (every cycle requires a reboot
and a manual checklist). The tiers below define the minimum
coverage v1.0 must achieve before the tag, plus the stretch
combinations that ship as known-issues if untested.

### Tier 1 — Required for v1.0 (must PASS)

| # | Vendor | Mode | Secure Boot | Bootloader | Distro | DM | DS | Monitors | Rationale |
|---|---|---|---|---|---|---|---|---|---|
| 1 | NVIDIA | Dual GPU | Off | GRUB2 | Arch | SDDM | X11 | Two | Most common gaming-on-Linux passthrough setup. NVIDIA Code-43 mitigation, `<vendor_id>` spoof, and the `<kvm><hidden>` block all exercised. |
| 2 | AMD | iGPU host | Off | systemd-boot | Fedora | GDM | Wayland | Two | Modern Linux desktop default. Tests the dracut writer, the Fedora OVMF paths (`/usr/share/edk2/ovmf/`), and the AMD reset-bug `<qemu:commandline>` block. |
| 3 | NVIDIA | Single GPU | Off | GRUB2 | Arch | SDDM | X11 | One | The single-GPU hook installer's marquee path. Exercises NVIDIA module unbind order, dispatcher routing, and `verify_hook_install` end-to-end. |

Pass criteria for every Tier-1 cell:

1. `virtu scan` reports `ready` or `ready with warnings` (no `Blocked` status).
2. `virtu plan` produces a 7-step plan with no validation errors.
3. `virtu apply --phase a --confirm` writes the snapshot, edits the
   bootloader, writes `/etc/modprobe.d/virtu-vfio.conf`, rebuilds
   the initramfs, and persists `~/.virtu/state/pending.toml`.
4. The host reboots cleanly into the new kernel.
5. After reboot, `/proc/cmdline` carries every parameter the
   pending plan declared, and `lsmod | grep vfio_pci` returns a
   match.
6. `virtu resume` reports `Verifier: Phase A landed cleanly.`
7. `virsh dominfo <vm_name>` returns the registered domain.
8. For single-GPU plans, `verify_hook_install` returns an empty
   divergence list.
9. `virsh start <vm_name>` boots the VM and the passed-through GPU
   is visible to the guest with the vendor's driver loaded.
10. `virtu rollback --to <snapshot_id>` restores every edited file
    to its pre-edit hash and runs the recorded post-restore actions
    cleanly.

### Tier 2 — Recommended (PASS or document as known-issue)

| # | Vendor | Mode | Secure Boot | Bootloader | Distro | DM | DS | Monitors | Rationale |
|---|---|---|---|---|---|---|---|---|---|
| 4 | Intel | iGPU host | Off | GRUB2 | Ubuntu | LightDM | X11 | One | Laptop-style host. Tests update-initramfs writer, Debian OVMF paths, and the iGPU/dGPU type classifier. |
| 5 | AMD | Dual GPU | On | GRUB2 | Arch | SDDM | Wayland | Two | Confirms Secure Boot on the host doesn't block VFIO module loading. The guest's Secure Boot status is independent. |
| 6 | NVIDIA | Single GPU | Off | systemd-boot | Arch | greetd | X11 | One | Confirms the dispatcher's `systemctl stop greetd` line works against a non-systemd-default DM. |
| 7 | AMD | Single GPU | Off | GRUB2 | Fedora | GDM | Wayland | One | Tests the AMD `amdgpu` → `radeon` unbind order and the Fedora dracut + GDM combination. |

### Tier 3 — Stretch (untested combinations ship as known-limitations)

| # | Vendor | Mode | Secure Boot | Bootloader | Distro | DM | DS | Monitors |
|---|---|---|---|---|---|---|---|---|
| 8 | Intel | Single GPU | Off | systemd-boot | Arch | ly | TTY | One |
| 9 | NVIDIA | Dual GPU | On | GRUB2 | openSUSE | LightDM | X11 | Two |
| 10 | NVIDIA | Dual GPU | Off | rEFInd | Arch | SDDM | X11 | Two |
| 11 | AMD | Dual GPU | Off | EFISTUB | Arch | (none) | TTY | Two |

Tiers 3 cells are explicit out-of-scope items for v1.0. The planner
already refuses to plan against unsupported bootloaders (rEFInd,
Syslinux/Extlinux, EFISTUB) with a clear "writer not implemented
yet" error. Test cases 10 and 11 confirm the refusal is graceful.

## Per-Dimension Validation Procedure

For each Tier-1 and Tier-2 cell, the runner script in
`tests/scripts/run_hardware_test.sh` enforces the checks below.
The list duplicates what the script automates so a maintainer
without the script handy can still validate by hand.

### GPU vendor

- **NVIDIA**: `lspci -k | grep -A 3 -i nvidia` should show `Kernel
  driver in use: vfio-pci` for the passthrough card after reboot.
  The companion HDMI audio function (`<bus>:<dev>.1`) must be on
  vfio-pci as well. The NVIDIA driver version in the guest must
  match what the user installed; Code 43 is the canonical failure
  mode if the `<features>` spoof regresses.
- **AMD**: `lspci -k | grep -A 3 -i amd` for the passthrough card.
  After the VM stops, `dmesg | tail` should not show the AMD reset
  bug (`amdgpu 0000:01:00.0: ring sdma0 timeout`); the
  `<qemu:commandline>` block in the generated XML mitigates it.
- **Intel**: `lspci -k | grep -A 3 -i intel` for the iGPU. iGPU
  passthrough is not supported on most consumer Intel chips
  (GVT-d / GVT-g require specific kernel + hardware combinations);
  the planner's `iGPU host` mode is what most Intel hosts will hit.

### Passthrough mode

- **Dual GPU**: After reboot, `lspci -k` shows the passthrough card
  bound to `vfio-pci` and the host card bound to its native driver
  (`nvidia`, `amdgpu`, etc.). The Linux desktop comes up on the
  host card.
- **iGPU host**: The dGPU is on `vfio-pci`; the iGPU is on `i915`
  or `amdgpu`. The Linux desktop comes up on the iGPU.
- **Single GPU**: Before VM start, `lspci -k` shows the GPU on its
  native driver. After `virsh start <vm_name>`, the host display
  manager is stopped and the GPU is on `vfio-pci`. After
  `virsh shutdown <vm_name>` (or guest shutdown), the GPU returns
  to its native driver and the display manager restarts. The TTY
  must remain reachable via Ctrl+Alt+F2 throughout.

### Secure Boot

- **On**: `mokutil --sb-state` reports `SecureBoot enabled`. Phase
  A completes and the new boot lands. `vfio_pci` loads (the kernel
  module is signed by the distro). The guest's Secure Boot is
  configured independently via OVMF.
- **Off**: `mokutil --sb-state` reports `SecureBoot disabled`. No
  module signing concerns.

### Bootloader

- **GRUB2**: After Phase A, `/etc/default/grub` carries the new
  parameters in `GRUB_CMDLINE_LINUX_DEFAULT`. After reboot,
  `/proc/cmdline` reflects them. `grub-mkconfig -o
  /boot/grub/grub.cfg` ran successfully (the regenerate wrapper
  reports no error).
- **systemd-boot**: The active loader entry under
  `/boot/loader/entries/` carries the new `options` line.
  `bootctl update` is best-effort (failures become a tracing
  warning, not a Phase A abort).

### Distro family

- **Arch / Manjaro / CachyOS**: `/etc/mkinitcpio.conf` carries
  `MODULES=(... vfio_pci vfio vfio_iommu_type1)` after Phase A.
  `mkinitcpio -P` ran successfully.
- **Fedora / openSUSE**: `/etc/dracut.conf.d/virtu-vfio.conf`
  exists with the managed banner and the `add_drivers+=` line.
  `dracut --force --regenerate-all` ran successfully.
- **Debian / Ubuntu**: `/etc/initramfs-tools/modules` carries the
  Virtu-managed section with vfio modules. `update-initramfs -u
  -k all` ran successfully.

### Display server / display manager

- The single-GPU `release` hook stops the configured DM with
  `systemctl stop <dm>`. After the VM starts, the display goes
  black (the GPU is now bound to `vfio-pci`), and the user
  switches input to the guest's monitor or the same monitor's
  VM input.
- The single-GPU `reattach` hook starts the same DM with
  `systemctl start <dm>`. The Linux login screen reappears.
- For X11 + ly + TTY-only environments, the recovery instruction
  in the ERR trap must mention the right service name.

### Monitor configuration

- **One monitor**: `MonitorPlan::OneMonitor`. The user is told to
  use input-switching at the monitor or `HookHandoff` (single GPU).
- **Two monitors with one per GPU**: `MonitorPlan::TwoMonitors`
  with the host and VM connector names from `/sys/class/drm/`.
  Validation rejects the plan if the connector names are not
  present in the host's DRM data.

## Pre-flight Checklist

Before running `tests/scripts/run_hardware_test.sh`, confirm:

1. **Backups exist** for `/etc/default/grub` (or
   `/boot/loader/entries/`), `/etc/mkinitcpio.conf` (or
   `/etc/dracut.conf.d/`), and `/etc/initramfs-tools/modules`.
   Virtu's snapshot manifest covers the same paths, but a manual
   backup gives the user a recovery path if `~/.virtu/snapshots/`
   itself is corrupted.
2. **A second working bootloader entry** exists in case the
   primary entry's cmdline edit goes wrong. On GRUB2, this is
   typically the "Advanced options" submenu's previous-kernel
   entry. On systemd-boot, this is any other `loader/entries/*.conf`
   file Virtu does not edit.
3. **A working TTY** is reachable. Press Ctrl+Alt+F2 from the
   active session and confirm a login prompt appears, then
   Ctrl+Alt+F1 to return. If the host display manager is stopped
   by a single-GPU hook and the GPU rebind fails, the TTY is the
   only recovery path.
4. **`/etc/sudoers` allows the test user to run targeted
   commands** without a TTY-bound `tty_tickets` requirement.
   Virtu invokes `sudo` only for specific commands (mkinitcpio,
   grub-mkconfig, virsh define against `qemu:///system`).
5. **`libvirtd` is running and the user is in `libvirt` and
   `kvm` groups**. `virsh list --all` should succeed.
6. **At least 100 GiB free** in the VM image directory
   (default `/var/lib/libvirt/images/`).
7. **No pending Virtu plan exists**. If `~/.virtu/state/pending.toml`
   is present, run `virtu resume` or
   `virtu rollback --to <id>` first; the runner script refuses to
   start with an unfinished apply outstanding.

## Out-of-Scope for v1.0

These combinations are explicitly not supported in v1.0 and the
test matrix does not exercise them:

- **rEFInd, Syslinux/Extlinux, and EFISTUB bootloaders.** The
  planner refuses with a "writer not implemented yet" error. The
  refusal itself is in scope (Tier-3 cases 10 and 11), but
  successful applies are not.
- **Multi-GPU passthrough** (more than one GPU passed to the
  same VM). The planner emits a `MultiGpuNotImplemented`
  validation error. Single-VM-per-GPU is the supported model.
- **Looking Glass.** The data model and validation are kept for
  forward compatibility (see `LookingGlassChoice`), but no
  installer, IVSHMEM XML, or tmpfiles writer ships in v1.0. The
  planner records the step as `deferred_steps` in Phase B.
- **Hot-plug GPU rebinding** (single-GPU mode without a reboot
  cycle). The `apply → reboot → resume` cycle is the supported
  flow. Single-GPU is "release on VM start, reattach on VM stop"
  and lives entirely inside the libvirt hook.

## Reporting Test Results

After each run:

1. Copy `tests/RESULTS_TEMPLATE.md` to a new file. Suggested name:
   `tests/results/<timestamp>/RESULT.md` (the timestamp directory
   is created by `run_hardware_test.sh`; the directory itself is
   gitignored).
2. Fill in every section. The runner script populates
   `scan.txt`, `plan.txt`, `phase_a.txt`, and (after reboot)
   `resume.txt` and `status.txt` automatically.
3. Decide PASS / FAIL / PARTIAL per the criteria above and record
   the verdict at the top of the results file.
4. If you found a regression, file a GitHub issue and link the
   results file from `~/.virtu/logs/<timestamp>.log` (the runner
   tarballs both into a single attachment).
5. If you found a known-limitation that doesn't block v1.0, add
   the case to `tests/HARDWARE_MATRIX.md` under "Out-of-Scope for
   v1.0" with a one-line description and a link to the GitHub
   issue.

## Maintenance

This matrix is part of the public source tree. Keep it accurate:

- A new GPU vendor → new row in the vendor table and a new
  column in the validation procedure.
- A new bootloader writer landing → move that bootloader from
  Tier 3 / Out-of-Scope into Tier 1 or Tier 2.
- A new known-limitation discovered on real hardware → append it
  under "Out-of-Scope for v1.0" with the reproducer.

The fixture-capture script (`tests/scripts/capture_fixture.sh`)
turns any host into a sanitized `tests/fixtures/` tree. When a
real-hardware run finds a regression, capture the host's fixture
*before* the regression is fixed so a hermetic regression test
can be written.
