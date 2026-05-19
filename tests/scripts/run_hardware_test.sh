#!/usr/bin/env bash
#
# run_hardware_test.sh — orchestrate a real-hardware Virtu test
# cycle (Milestone 10, slice 10.8).
#
# Drives the full apply → reboot → resume cycle on a single
# host, with explicit confirmations at every mutating step and
# extensive output capture into a timestamped results directory.
# The script never decides on its own to mutate the host: every
# privileged action requires the user to (a) pass an explicit
# flag and (b) type a confirmation phrase. Read-only operations
# (scan, plan, status) are unguarded.
#
# This script is the runner for the matrix in
# tests/HARDWARE_MATRIX.md. The matrix says what to test; this
# script says how to test it.
#
# Usage:
#   tests/scripts/run_hardware_test.sh [SUBCOMMAND] [FLAGS...]
#
# Subcommands (default: scan):
#   scan      Run virtu scan + plan, capture the output. Read-only.
#   apply     Run Phase A. Requires --i-have-backups and --confirm.
#             Refuses if a pending plan already exists.
#   resume    Run Phase B (after the host has rebooted).
#             Captures status, divergence list, and rollback id.
#   verify    Re-read the live host without applying anything.
#             Useful between apply and reboot to inspect what's
#             about to happen.
#   rollback  Restore a snapshot. Requires --confirm.
#
# Output: tests/results/<timestamp>/ — gitignored. The runner
# creates a fresh subdirectory for each new run except when
# resuming an existing run (which appends to the same dir).
#
# This script never:
#   - calls `reboot` (manual: the user must reboot themselves).
#   - mutates the host without explicit per-run confirmation.
#   - touches /etc/, /boot/, or /var/lib/libvirt/ directly. Every
#     mutation goes through `virtu apply --confirm` so the
#     snapshot manifest stays the source of truth.

set -euo pipefail

# ---------------------------------------------------------------------------
# Resolve repo paths
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESULTS_ROOT="$REPO_ROOT/tests/results"
PENDING_PATH="${HOME:-/tmp}/.virtu/state/pending.toml"

# Build virtu fresh so the scripts always exercise the current
# tree. Use the release profile so behavior matches what users
# install via the packaging recipes.
VIRTU_BIN="$REPO_ROOT/target/release/virtu"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

usage() {
    sed -n '2,/^# Output:/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 64  # EX_USAGE
}

die() {
    echo "error: $*" >&2
    exit 1
}

confirm_phrase() {
    local phrase="$1"
    local prompt="$2"
    echo "$prompt"
    echo ""
    echo "Type the phrase '$phrase' (without quotes) to continue,"
    echo "or anything else to abort:"
    read -r typed
    if [ "$typed" != "$phrase" ]; then
        echo "Aborting (you typed '$typed')."
        exit 130  # ECANCELED-ish
    fi
}

ensure_virtu_built() {
    if [ ! -x "$VIRTU_BIN" ]; then
        echo "Building virtu (release)..."
        ( cd "$REPO_ROOT" && cargo build --release --locked --bin virtu )
    fi
}

ensure_no_pending_plan() {
    if [ -e "$PENDING_PATH" ]; then
        cat <<EOF >&2
error: a pending Virtu plan already exists at:
   $PENDING_PATH

A previous Phase A apply has not been finished or rolled back.
Refusing to start a new test run; the old plan must be resolved
first.

To finish the existing apply (after a reboot):
   virtu resume

To abort it:
   virtu rollback --to <snapshot-id>

Use 'virtu rollback --list' to see snapshots.
EOF
        exit 1
    fi
}

# Pick the freshest results directory or create one. Subcommands
# that follow apply (resume, rollback) want the same directory the
# apply wrote into; "scan" and "apply" each create a new one.
results_dir_for() {
    local subcommand="$1"
    case "$subcommand" in
        resume|rollback)
            # Reuse the most recent existing dir if one exists.
            local latest
            latest="$(ls -1d "$RESULTS_ROOT"/*/ 2>/dev/null | tail -n 1 || true)"
            if [ -n "$latest" ]; then
                # Strip trailing slash.
                printf '%s\n' "${latest%/}"
                return
            fi
            ;;
    esac
    # Default: brand-new timestamped directory.
    local stamp
    stamp="$(date -u +'%Y-%m-%dT%H-%M-%SZ')"
    local dir="$RESULTS_ROOT/$stamp"
    mkdir -p "$dir"
    printf '%s\n' "$dir"
}

# Run a virtu subcommand and tee the output into the results
# directory. Returns the subcommand's exit status.
run_virtu() {
    local out_path="$1"
    shift
    set +e
    "$VIRTU_BIN" "$@" 2>&1 | tee "$out_path"
    local rc="${PIPESTATUS[0]}"
    set -e
    return "$rc"
}

# Dump live host facts that virtu doesn't print itself.
dump_environment() {
    local out_dir="$1"
    mkdir -p "$out_dir"
    {
        echo "Captured at: $(date -u +'%Y-%m-%dT%H:%M:%SZ')"
        echo "Kernel: $(uname -r)"
        echo "Hostname: $(hostname || echo unknown)"
        echo "Distro: $(grep -E '^PRETTY_NAME=' /etc/os-release \
            | cut -d= -f2- | tr -d '"' || echo unknown)"
    } > "$out_dir/env.txt"
    cp /proc/cmdline "$out_dir/cmdline.txt"
    lsmod 2>/dev/null | grep -E '^vfio' > "$out_dir/lsmod-vfio.txt" || true
    if command -v lspci > /dev/null 2>&1; then
        lspci -k 2>/dev/null | grep -E -A 3 -i \
            'vga|display|3d controller|audio device' > "$out_dir/lspci-k.txt" || true
    fi
    if [ -d /sys/kernel/iommu_groups ]; then
        ls -1 /sys/kernel/iommu_groups > "$out_dir/iommu-groups.txt"
    fi
}

# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

cmd_scan() {
    ensure_virtu_built
    local results
    results="$(results_dir_for scan)"
    echo "Results directory: $results"
    echo ""
    dump_environment "$results/pre"
    echo "-- virtu scan --"
    run_virtu "$results/scan.txt" scan || true
    echo ""
    echo "-- virtu plan --"
    run_virtu "$results/plan.txt" plan || true
    echo ""
    echo "-- virtu status --"
    run_virtu "$results/status.txt" status || true
    echo ""
    echo "Read-only run complete. Inspect the files under:"
    echo "  $results"
    echo ""
    echo "Next: review the plan, then re-run this script with"
    echo "  $0 apply --i-have-backups --confirm"
    echo "to execute Phase A on this host."
}

cmd_verify() {
    ensure_virtu_built
    echo "-- virtu scan (read-only) --"
    "$VIRTU_BIN" scan
    echo ""
    echo "-- virtu plan (read-only) --"
    "$VIRTU_BIN" plan
    echo ""
    echo "-- virtu status (read-only) --"
    "$VIRTU_BIN" status
}

cmd_apply() {
    local have_backups=0
    local confirmed=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --i-have-backups) have_backups=1 ;;
            --confirm)        confirmed=1 ;;
            -h|--help)        usage ;;
            *) die "unknown apply flag: $1 (expected --i-have-backups, --confirm)" ;;
        esac
        shift
    done

    if [ "$have_backups" -ne 1 ]; then
        die "apply refused: pass --i-have-backups to acknowledge that you have a working bootloader entry, a TTY, and external backups of /etc and /boot."
    fi
    if [ "$confirmed" -ne 1 ]; then
        die "apply refused: pass --confirm to authorize Phase A mutations on this host."
    fi

    ensure_virtu_built
    ensure_no_pending_plan

    local results
    results="$(results_dir_for apply)"
    echo "Results directory: $results"
    dump_environment "$results/pre"

    echo ""
    echo "-- virtu scan --"
    run_virtu "$results/scan.txt" scan
    echo ""
    echo "-- virtu plan (dry run preview) --"
    run_virtu "$results/plan.txt" plan
    echo ""

    cat <<EOF

================================================================
WARNING: virtu apply --phase a --confirm will mutate this host.

It will:
  - capture a snapshot under ~/.virtu/snapshots/<id>/
  - edit your bootloader config (/etc/default/grub or
    /boot/loader/entries/<entry>.conf)
  - write /etc/modprobe.d/virtu-vfio.conf
  - rebuild the initramfs (mkinitcpio / dracut /
    update-initramfs)
  - persist a pending-plan record at
    ~/.virtu/state/pending.toml

You will need to reboot this host afterwards before the new
boot configuration takes effect.

If anything goes wrong, the snapshot under
~/.virtu/snapshots/<id>/ can be replayed with:
   virtu rollback --to <id>

The TTY (Ctrl+Alt+F2) remains the recovery path of last resort.
================================================================

EOF

    confirm_phrase "PROCEED" "Type 'PROCEED' to run virtu apply --phase a --confirm:"

    echo ""
    echo "-- virtu apply --phase a --confirm --"
    run_virtu "$results/phase_a.txt" apply --phase a --confirm

    echo ""
    cat <<EOF

================================================================
Phase A complete. Output captured at:
   $results/phase_a.txt

To finish setup:
   1. Reboot this host: systemctl reboot
   2. After the host comes back up, re-run this script with:
        $0 resume

The pending-plan record at ~/.virtu/state/pending.toml is your
checkpoint. It carries the snapshot id so 'virtu rollback' can
undo this apply at any time before resume.
================================================================

EOF
}

cmd_resume() {
    ensure_virtu_built
    if [ ! -e "$PENDING_PATH" ]; then
        die "no pending plan found at $PENDING_PATH; nothing to resume."
    fi

    local results
    results="$(results_dir_for resume)"
    echo "Results directory (reusing): $results"

    dump_environment "$results/post-reboot"
    echo ""
    echo "-- virtu resume --"
    run_virtu "$results/resume.txt" resume
    echo ""
    echo "-- virtu status --"
    run_virtu "$results/status.txt" status
    echo ""

    cat <<EOF

================================================================
Phase B complete. Output captured at:
   $results/resume.txt

Now run the manual VM-boot test described in
tests/HARDWARE_MATRIX.md and fill out
tests/RESULTS_TEMPLATE.md (write your filled copy as
$results/RESULT.md).

Optional: re-run rollback to confirm the snapshot replays
cleanly. Do this BEFORE you start the VM, or AFTER you confirm
the VM works:
   $0 rollback --confirm
================================================================

EOF
}

cmd_rollback() {
    local confirmed=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --confirm) confirmed=1 ;;
            -h|--help) usage ;;
            *) die "unknown rollback flag: $1 (expected --confirm)" ;;
        esac
        shift
    done
    if [ "$confirmed" -ne 1 ]; then
        die "rollback refused: pass --confirm to authorize the snapshot restore."
    fi

    ensure_virtu_built

    echo "-- virtu rollback --list --"
    "$VIRTU_BIN" rollback --list

    echo ""
    echo "Pick a snapshot id from the list above. The harness will"
    echo "ask you to confirm the id it replays."
    echo ""
    echo "Snapshot id to restore (paste exactly, then Enter):"
    read -r snap_id
    if [ -z "$snap_id" ]; then
        die "empty snapshot id; aborting."
    fi

    confirm_phrase "ROLLBACK" \
        "About to run: virtu rollback --to '$snap_id'"

    local results
    results="$(results_dir_for rollback)"
    run_virtu "$results/rollback.txt" rollback --to "$snap_id"

    echo ""
    cat <<EOF

================================================================
Rollback finished. Output captured at:
   $results/rollback.txt

Read the post-restore-action list at the bottom of that file. If
the manifest recorded an UndefineLibvirtDomain action, you must
run 'virsh undefine <name>' yourself; if it recorded
RemoveHookScripts, you must remove the hook directory yourself.
Future polish slices may automate these.

A reboot is recommended after rollback so the kernel cmdline
returns to its pre-edit state.
================================================================

EOF
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------

if [ $# -eq 0 ]; then
    cmd_scan
    exit 0
fi

subcommand="$1"
shift
case "$subcommand" in
    scan)     cmd_scan "$@" ;;
    verify)   cmd_verify "$@" ;;
    apply)    cmd_apply "$@" ;;
    resume)   cmd_resume "$@" ;;
    rollback) cmd_rollback "$@" ;;
    -h|--help) usage ;;
    *) die "unknown subcommand: $subcommand (expected: scan, verify, apply, resume, rollback)" ;;
esac
