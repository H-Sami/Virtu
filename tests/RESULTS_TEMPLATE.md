# Virtu Real-Hardware Test Result

> Copy this template to `tests/results/<timestamp>/RESULT.md`
> after running `tests/scripts/run_hardware_test.sh`. The
> `tests/results/` directory is gitignored; commit only the
> matrix entry that summarizes a result, not the result itself,
> unless the entry is small enough to inline.

## Verdict

- **Result:** PASS / FAIL / PARTIAL
- **Tier (per `HARDWARE_MATRIX.md`):** 1 / 2 / 3 / off-matrix
- **Blocker for v1.0:** yes / no
- **GitHub issue (if any):** #XXXX

## Test Metadata

| Field | Value |
|---|---|
| Date (UTC) | YYYY-MM-DD HH:MM |
| Tester | name / handle |
| Virtu version | output of `virtu --version` |
| Virtu commit | git rev-parse HEAD |
| Reproduction harness | tests/scripts/run_hardware_test.sh @ <commit> |
| Hours spent | ~N |

## Hardware Configuration

Capture from `cargo run --quiet -- scan`. Trim to the salient
fields below; the full output is at
`tests/results/<timestamp>/scan.txt`.

| Field | Value |
|---|---|
| CPU | e.g. `AMD Ryzen 7 5700X3D 8-Core Processor (AuthenticAMD)` |
| VT-d / AMD-Vi | supported / not detected |
| IOMMU groups | N |
| RAM | N GiB |
| Distro | e.g. `CachyOS` |
| Kernel | e.g. `7.0.8-1-cachyos` |
| Bootloader | GRUB2 / systemd-boot / rEFInd / Syslinux / EFISTUB |
| Initramfs | mkinitcpio / dracut / update-initramfs |
| Display server | Wayland / X11 / TTY |
| Display manager | SDDM / GDM / LightDM / greetd / ly / lxdm / none |
| Audio | PipeWire / PulseAudio / ALSA / JACK |
| Secure Boot (host) | enabled / disabled |
| OVMF | available / not found |
| User in `libvirt` group | yes / no |
| User in `kvm` group | yes / no |
| Existing libvirt domains | N (list names if relevant) |

### GPUs

For each detected GPU:

| Slot | Vendor | Type | Model | IOMMU group | Isolated | Driver | Boot VGA |
|---|---|---|---|---|---|---|---|
| `0000:01:00.0` | AMD | dGPU | Radeon RX 9060 XT | 1 | yes | none → vfio-pci | no |
| `0000:02:00.0` | NVIDIA | dGPU | RTX 2060 | 2 | yes | nvidia | yes |

### Monitors

| Connector | Connected | GPU | Mode | Internal |
|---|---|---|---|---|
| `DP-1` | yes | `0000:01:00.0` | `2560x1440` | no |
| `HDMI-A-1` | yes | `0000:02:00.0` | `1920x1080` | no |

## Pre-Test State

Captured before any apply. The runner script writes these to
`tests/results/<timestamp>/pre/`:

- `pre/cmdline.txt` — `/proc/cmdline`
- `pre/lsmod-vfio.txt` — `lsmod | grep -E '^vfio'`
- `pre/lspci-k.txt` — `lspci -k` filtered to display + audio
  classes
- `pre/dm-state.txt` — `systemctl is-active <dm>`
- `pre/grub-default.txt` — `/etc/default/grub` (GRUB2)
- `pre/loader-entry.txt` — active `/boot/loader/entries/<entry>.conf`
  (systemd-boot)
- `pre/initramfs-config.txt` — `/etc/mkinitcpio.conf` /
  `/etc/dracut.conf.d/` listing / `/etc/initramfs-tools/modules`

Paste anything unusual that wasn't already captured by the script.

## Phase A (`virtu apply --phase a --confirm`)

> Output captured at `tests/results/<timestamp>/phase_a.txt`.

- Phase A exit status: `0` / non-zero (paste error)
- Snapshot id: `<id>`
- Pending plan path: `~/.virtu/state/pending.toml`
- Steps completed: list `StepKind` values
- Regenerate command success:
  - bootloader: `grub-mkconfig` / `bootctl update` — exit code, stderr tail
  - initramfs: `mkinitcpio -P` / `dracut --force --regenerate-all` /
    `update-initramfs -u -k all` — exit code, stderr tail

### Observations

Anything notable. Examples:

- "Phase A asked for sudo three times; expected once." → file
  GitHub issue against the apply UX.
- "GRUB regenerate took 12 seconds." → reasonable, no concern.
- "`bootctl update` warned `Skipping current boot entry`." →
  expected on first install per the slice 6.5.4 audit.

## Reboot

- Reboot exit method: `systemctl reboot`, hard reboot, etc.
- Boot picked the right entry: yes / no
- Display manager came up: yes / no / no (intentional, single-GPU)
- TTY reachable: yes / no
- Time-to-login from POST: ~N seconds

## Post-Reboot State

Captured before `virtu resume` runs. The runner writes these to
`tests/results/<timestamp>/post-reboot/`:

- `post-reboot/cmdline.txt` — must contain every parameter the
  pending plan expected
- `post-reboot/lsmod-vfio.txt` — must show `vfio_pci`
- `post-reboot/lspci-k.txt` — must show `vfio-pci` against the
  passthrough GPU's PCI slot
- `post-reboot/iommu-groups.txt` — `ls /sys/kernel/iommu_groups/`
  must list at least one group

## Phase B (`virtu resume`)

> Output at `tests/results/<timestamp>/resume.txt`.

- Verifier verdict: `Ready` / `NotReady` / `WrongHost`
- If `NotReady`, list every divergence the verifier reported.
- Phase B exit status: `0` / non-zero
- Steps completed: `VmXmlGenerate`, `VmRegister`, `Verify`,
  `HookInstall` (single-GPU only), `LookingGlassInstall`
  (deferred always)
- Manifest entries added: list `original_path` per new entry
- Restore actions added: list (e.g. `UndefineLibvirtDomain`,
  `RemoveHookScripts`)

### XML Validation

- `virt-xml-validate ~/.virtu/<vm_name>.xml` exit code: `0`
- Generated XML byte size: N bytes
- Anything noteworthy in the XML (e.g. AMD reset-bug
  `<qemu:commandline>` block ships, NVIDIA `<vendor_id>` spoof
  ships)

### libvirt Registration

- `virsh dominfo <vm_name>` exit code: `0`
- Domain shows in `virsh list --all`: yes / no
- Disk image created at expected path: yes / no, size: N
- For DiskChoice::Existing, the existing image is referenced
  unchanged: yes / no

### Hook Verification (single-GPU only)

- Three scripts present at `/etc/libvirt/hooks/qemu.d/<vm>` and
  `/etc/libvirt/hooks/qemu.d/<vm>.d/{release,reattach}`: yes / no
- All three executable: yes / no
- `engine::verify_hook_install` returns empty divergence list:
  yes / no
- Each script passes `bash -n`: yes / no

## VM Boot Test

After Phase B completes, the matrix expects the user to actually
start the VM and confirm passthrough works:

- `virsh start <vm_name>` exit code: `0`
- Guest sees the GPU on its monitor: yes / no
- Guest device manager / lspci shows the passed-through GPU
  vendor and device id: yes / no
- Vendor driver loaded inside the guest: yes (paste version) /
  no (paste error)
- 3D acceleration smoke test: passed (e.g. ran a guest benchmark)
  / not tested

For single-GPU plans:

- Host DM stopped on VM start: yes / no
- GPU rebound to host driver on VM stop: yes / no
- Host DM restarted on VM stop: yes / no
- TTY remained reachable throughout: yes / no

## Rollback Test (optional but recommended)

- `virtu rollback --to <snapshot_id>` exit code: `0`
- Bootloader file restored to pre-edit hash: yes / no
- Initramfs config restored: yes / no
- VFIO modprobe snippet removed (deleted, not just blanked): yes / no
- Hook scripts removed (single-GPU only): yes / no
- `RestoreSummary::print_human` listed every restored path: yes / no
- After a reboot following rollback, `lsmod | grep vfio_pci` shows
  the host is no longer binding vfio-pci: yes / no
- `virsh undefine <vm_name>` ran successfully (or was clearly
  recommended in the post-restore actions): yes / no

## Issues Encountered

For each issue, fill in:

### Issue N: short title

- Severity: blocker / non-blocker
- Reproduction:
  1. Step
  2. Step
  3. Step
- Observed: what actually happened
- Expected: what should have happened
- Workaround: what you did to keep going
- GitHub issue: #XXXX
- Snapshot for regression test: `tests/fixtures/regressions/<name>/`
  (captured via `tests/scripts/capture_fixture.sh` before fixing)

## Known-Limitations Discovered

If the run uncovered a non-blocking limitation that should ship as
a v1.0 known-issue, add it here and propose an entry for
`HARDWARE_MATRIX.md` under "Out-of-Scope for v1.0".

## Final State

After everything, what does the host look like?

- Pending plan: present / cleared / rolled back
- Snapshot retained at: `~/.virtu/snapshots/<id>/`
- VM defined: yes / no, name: `<vm_name>`
- Bootloader still has Virtu-applied parameters: yes / no
  (rolled back)
- Time-to-recover-baseline (rollback + reboot): ~N minutes

## Conclusion

One paragraph summary suitable for the GitHub issue body or the
v1.0 release notes.
