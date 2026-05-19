# Real-hardware reproduction harness

This directory holds the scripts that drive Virtu's real-hardware
test matrix (Milestone 10, slice 10.8). The matrix itself is at
`tests/HARDWARE_MATRIX.md`; the results template a tester fills
out is at `tests/RESULTS_TEMPLATE.md`.

The hermetic suite under `cargo test` cannot reboot a host or
bind a real GPU to `vfio-pci`. These scripts cover what the
hermetic suite cannot.

## Scripts

### `run_hardware_test.sh` — orchestrate one test cycle

Drives the full `scan → plan → apply → reboot → resume` cycle
on the live host. Every mutating step requires explicit
confirmation flags (`--i-have-backups`, `--confirm`) plus a
typed-phrase prompt.

```bash
# Step 1 — read-only preflight (always safe).
tests/scripts/run_hardware_test.sh scan

# Step 2 — apply Phase A. Refuses if a pending plan exists.
tests/scripts/run_hardware_test.sh apply --i-have-backups --confirm

# Step 3 — reboot the host manually.
sudo systemctl reboot

# Step 4 — finish Phase B after the host comes back.
tests/scripts/run_hardware_test.sh resume

# Step 5 — optional rollback test.
tests/scripts/run_hardware_test.sh rollback --confirm
```

Output lands under `tests/results/<UTC-timestamp>/` (gitignored).
Each subdirectory carries `scan.txt`, `plan.txt`, `phase_a.txt`,
`resume.txt`, and `status.txt`, plus an `env.txt` capturing the
host facts the runner saw.

### `capture_fixture.sh` — snapshot a host into a fixture

Turns a live host into a sanitized `tests/fixtures/<name>/`
tree the existing `*_from_root` parser entry points already
consume. Use this when you find a regression on real hardware
and want a hermetic test to lock the fix in place.

```bash
tests/scripts/capture_fixture.sh nvidia-amd-cachyos-grub
```

The script is read-only on the host and never asks for
privileges. It sanitizes hostnames, usernames, home paths, and
MAC addresses in place.

## Safety contract

Both scripts follow the same rules:

- **Never call `reboot`.** The user reboots manually.
- **Never modify `/etc/`, `/boot/`, or `/var/lib/libvirt/`
  directly.** Every mutation goes through `virtu apply
  --confirm` so the snapshot manifest stays authoritative.
- **Refuse on a pending plan.** The runner refuses to start a
  new apply if `~/.virtu/state/pending.toml` already exists.
- **Refuse without explicit confirmation.** Even `apply` with
  `--i-have-backups --confirm` still prompts the user to type
  `PROCEED` before the actual mutation runs.
- **Only public CLI commands.** The scripts call `virtu scan`,
  `virtu plan`, `virtu apply`, `virtu resume`, `virtu rollback`,
  `virtu status` — nothing else. No direct invocation of
  `virsh`, `qemu-img`, `mkinitcpio`, etc. that would bypass the
  manifest.

## Validation

Both scripts pass `bash -n`. A maintainer running `shellcheck`
should also see them clean; this is not yet wired into CI
because the hermetic suite never invokes shell. If you change
either script, please run:

```bash
bash -n tests/scripts/capture_fixture.sh
bash -n tests/scripts/run_hardware_test.sh
shellcheck tests/scripts/*.sh   # optional but recommended
```

## Maintenance

- A new GPU vendor or distro entering the matrix → update
  `tests/HARDWARE_MATRIX.md` and re-run `capture_fixture.sh`
  on a representative host.
- A regression discovered on real hardware → capture the
  pre-fix fixture, write a hermetic test that loads it, then
  fix the bug. The pre-fix fixture is the regression test's
  ground truth.
- A new CLI subcommand on `virtu` → extend
  `run_hardware_test.sh` if it's relevant to a real-hardware
  cycle, otherwise leave it alone.
