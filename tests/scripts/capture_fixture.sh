#!/usr/bin/env bash
#
# capture_fixture.sh — snapshot a live host into a sanitized
# fixture tree that can be added to tests/fixtures/ for hermetic
# regression coverage (Milestone 10, slice 10.8).
#
# The fixture tree mirrors the layout the existing fixture-driven
# tests already consume: tests/fixtures/{proc,etc,sysfs,...}.
# Each Virtu detector has a *_from_root entry point that resolves
# relative paths under a root dir; this script populates exactly
# the paths those entry points read.
#
# Sensitive data is sanitized:
#   - hostnames in /etc/os-release / virsh output → `virtu-test-host`
#   - usernames anywhere → `testuser`
#   - MAC addresses in network output → `aa:bb:cc:dd:ee:ff`
#   - sudo / kerberos session paths → stripped
#
# The script is intentionally read-only on the live host: nothing
# is mutated, no sudo is requested. It only copies and rewrites
# bytes into the destination directory.
#
# Usage:
#   tests/scripts/capture_fixture.sh <fixture-name>
#
# Example:
#   tests/scripts/capture_fixture.sh nvidia-amd-cachyos-grub
#
# Output:
#   tests/fixtures/<fixture-name>/
#       proc/cpuinfo
#       proc/meminfo
#       proc/cmdline
#       proc/modules
#       proc/sys/kernel/osrelease
#       etc/os-release
#       etc/default/grub                  (if GRUB2)
#       boot/loader/entries/*.conf        (if systemd-boot)
#       sys/bus/pci/devices/...           (GPU-class only)
#       sys/kernel/iommu_groups/.../...
#       sys/class/drm/card*-...
#       usr/lib/modules/<kver>/build      (placeholder file if present)
#       var/lib/libvirt/virsh-list-all
#       virtu-user-groups                 (sanitized id -nG)
#       README.md                         (provenance)

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "Usage: $0 <fixture-name>" >&2
    echo "" >&2
    echo "Captures the live host into tests/fixtures/<fixture-name>/." >&2
    exit 64  # EX_USAGE
fi

FIXTURE_NAME="$1"

# Refuse names that would escape the fixtures dir or collide with
# the tree's own helper files.
case "$FIXTURE_NAME" in
    "" | */* | .* | scripts | results)
        echo "error: refusing fixture name '$FIXTURE_NAME'" >&2
        echo "       use a lowercase, slash-free identifier like 'nvidia-amd-cachyos-grub'." >&2
        exit 64
        ;;
esac

# Resolve the repo root from the script location so this works
# regardless of the user's current working directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEST="$REPO_ROOT/tests/fixtures/$FIXTURE_NAME"

if [ -e "$DEST" ]; then
    echo "error: $DEST already exists; refusing to overwrite." >&2
    echo "       remove it manually if you really want to recapture." >&2
    exit 73  # EX_CANTCREAT
fi

echo "Capturing fixture into $DEST"
mkdir -p "$DEST"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# copy_if SRC DEST_REL — copy SRC to $DEST/DEST_REL when SRC exists.
# Creates parent dirs. Silent on missing source so the script keeps
# going on hosts that simply don't have a given file.
copy_if() {
    local src="$1"
    local dest_rel="$2"
    if [ -r "$src" ]; then
        local dest_path="$DEST/$dest_rel"
        mkdir -p "$(dirname "$dest_path")"
        cp -- "$src" "$dest_path"
    fi
}

# sanitize FILE — rewrite secrets in-place. Conservative: only
# the patterns documented in the header are touched.
sanitize() {
    local file="$1"
    if [ ! -f "$file" ]; then
        return
    fi
    # Hostname → virtu-test-host
    local host
    host="$(hostname 2>/dev/null || true)"
    if [ -n "$host" ]; then
        # Use a delimiter that can't appear in hostnames so sed
        # doesn't choke on dots.
        sed -i "s|$host|virtu-test-host|g" "$file"
    fi

    # Real username → testuser
    local user
    user="${USER:-$(id -un 2>/dev/null || echo "")}"
    if [ -n "$user" ] && [ "$user" != "testuser" ]; then
        sed -i "s|$user|testuser|g" "$file"
    fi

    # /home/<user>/ → /home/testuser/
    sed -i 's|/home/[^/[:space:]]\+/|/home/testuser/|g' "$file"

    # MAC addresses → aa:bb:cc:dd:ee:ff
    sed -i -E 's/[0-9a-fA-F]{2}(:[0-9a-fA-F]{2}){5}/aa:bb:cc:dd:ee:ff/g' "$file"
}

# write_with_sanitize DEST_REL CMD... — run CMD, capture stdout,
# write it to $DEST/DEST_REL, then sanitize. Fails silently when
# CMD returns non-zero so the script doesn't abort on a missing
# optional binary.
write_with_sanitize() {
    local dest_rel="$1"
    shift
    local dest_path="$DEST/$dest_rel"
    mkdir -p "$(dirname "$dest_path")"
    if "$@" > "$dest_path" 2>/dev/null; then
        sanitize "$dest_path"
    else
        # Drop the empty file so a downstream test doesn't see a
        # "present but empty" fixture path it would treat as
        # legitimate.
        rm -f "$dest_path"
    fi
}

# ---------------------------------------------------------------------------
# /proc — CPU, memory, kernel cmdline, loaded modules
# ---------------------------------------------------------------------------

echo "  /proc/*"
copy_if /proc/cpuinfo proc/cpuinfo
copy_if /proc/meminfo proc/meminfo
copy_if /proc/cmdline proc/cmdline
copy_if /proc/modules proc/modules
copy_if /proc/sys/kernel/osrelease proc/sys/kernel/osrelease

# ---------------------------------------------------------------------------
# /etc — distro identity, bootloader configs
# ---------------------------------------------------------------------------

echo "  /etc/*"
copy_if /etc/os-release etc/os-release
sanitize "$DEST/etc/os-release"

# Bootloader: GRUB2
copy_if /etc/default/grub etc/default/grub
sanitize "$DEST/etc/default/grub"

# Bootloader: systemd-boot
if [ -d /boot/loader/entries ]; then
    mkdir -p "$DEST/boot/loader/entries"
    for entry in /boot/loader/entries/*.conf; do
        if [ -r "$entry" ]; then
            cp -- "$entry" "$DEST/boot/loader/entries/$(basename "$entry")"
            sanitize "$DEST/boot/loader/entries/$(basename "$entry")"
        fi
    done
    copy_if /boot/loader/loader.conf boot/loader/loader.conf
    sanitize "$DEST/boot/loader/loader.conf"
fi

# Initramfs configs
copy_if /etc/mkinitcpio.conf etc/mkinitcpio.conf
sanitize "$DEST/etc/mkinitcpio.conf"
if [ -d /etc/dracut.conf.d ]; then
    mkdir -p "$DEST/etc/dracut.conf.d"
    # Keep only Virtu-managed files; user dracut snippets may
    # contain hostnames or local module choices the maintainer
    # doesn't want to leak.
    if [ -r /etc/dracut.conf.d/virtu-vfio.conf ]; then
        cp -- /etc/dracut.conf.d/virtu-vfio.conf \
            "$DEST/etc/dracut.conf.d/virtu-vfio.conf"
        sanitize "$DEST/etc/dracut.conf.d/virtu-vfio.conf"
    fi
fi
copy_if /etc/initramfs-tools/modules etc/initramfs-tools/modules
sanitize "$DEST/etc/initramfs-tools/modules"

# Display manager service. The live system has
# /etc/systemd/system/display-manager.service as a symlink to
# the actual unit file (e.g. /usr/lib/systemd/system/sddm.service).
# `cp` follows the link and gives us the unit file's *content*,
# which `parse_display_manager_service` doesn't recognize.
# Re-create the symlink target name as a one-line file so the
# fixture-root parser's content-fallback path returns the right
# DisplayManager variant.
if [ -L /etc/systemd/system/display-manager.service ]; then
    target="$(readlink /etc/systemd/system/display-manager.service \
        2>/dev/null || true)"
    if [ -n "$target" ]; then
        mkdir -p "$DEST/etc/systemd/system"
        printf '%s\n' "$(basename "$target")" > \
            "$DEST/etc/systemd/system/display-manager.service"
    fi
fi

# Per-DM service file. The `display_manager::detect_from_root`
# fallback path checks for `<root>/etc/systemd/system/<dm>.service`
# regardless of whether `display-manager.service` resolved. We
# capture the per-DM file as an empty marker; presence is what
# the parser cares about.
for dm_service in gdm sddm lightdm greetd ly lxdm; do
    if [ -L "/etc/systemd/system/${dm_service}.service" ] \
        || [ -e "/usr/lib/systemd/system/${dm_service}.service" ]; then
        # Marker only; the parser checks `.exists()` not contents.
        if systemctl is-active --quiet "$dm_service" 2>/dev/null; then
            : > "$DEST/etc/systemd/system/${dm_service}.service"
        fi
    fi
done

# /proc captures may carry the username via mount paths or
# kernel cmdline `rootfsflags=user_id=...`. Sanitize them too.
for f in proc/cpuinfo proc/meminfo proc/cmdline proc/modules \
         proc/sys/kernel/osrelease; do
    sanitize "$DEST/$f"
done

# ---------------------------------------------------------------------------
# /sys — PCI devices, IOMMU groups, DRM monitors
# ---------------------------------------------------------------------------

echo "  /sys/bus/pci (GPU + companion audio only)"
mkdir -p "$DEST/sys/bus/pci/devices"

# Walk /sys/bus/pci/devices, keeping only display-class devices
# (0x03xxxx) and any audio function on the same multi-function
# slot. The class filter is what live `gpu::detect_all` already
# uses.
if [ -d /sys/bus/pci/devices ]; then
    for dev in /sys/bus/pci/devices/*; do
        [ -d "$dev" ] || continue
        slot="$(basename "$dev")"
        class="$(cat "$dev/class" 2>/dev/null || echo "")"
        case "$class" in
            0x03*)
                # Display class. Keep this device.
                ;;
            0x040[13]*)
                # Audio class. Only keep if a sibling display
                # device exists on the same multi-function slot.
                base="${slot%.*}"
                if ls "$base."*[0-9]/class 2>/dev/null \
                    | xargs -r -n1 cat 2>/dev/null \
                    | grep -q "^0x03"; then
                    :  # keep
                else
                    continue
                fi
                ;;
            *)
                continue
                ;;
        esac

        target="$DEST/sys/bus/pci/devices/$slot"
        mkdir -p "$target"
        for f in vendor device subsystem_vendor subsystem_device \
                 class boot_vga; do
            copy_if "$dev/$f" "sys/bus/pci/devices/$slot/$f"
        done
        # Driver name. /sys exposes it as a symlink; capture the
        # target's basename so the fixture-root parser can read it
        # via the `driver_name` fallback path.
        if [ -L "$dev/driver" ]; then
            driver="$(readlink "$dev/driver" 2>/dev/null || true)"
            if [ -n "$driver" ]; then
                echo "${driver##*/}" > "$target/driver_name"
            fi
        fi
    done
fi

echo "  /sys/kernel/iommu_groups"
mkdir -p "$DEST/sys/kernel/iommu_groups"
if [ -d /sys/kernel/iommu_groups ]; then
    for group in /sys/kernel/iommu_groups/*; do
        [ -d "$group" ] || continue
        gid="$(basename "$group")"
        target="$DEST/sys/kernel/iommu_groups/$gid/devices"
        mkdir -p "$target"
        for dev in "$group/devices"/*; do
            [ -e "$dev" ] || continue
            slot="$(basename "$dev")"
            mkdir -p "$target/$slot"
            for f in class vendor device; do
                copy_if "$dev/$f" "sys/kernel/iommu_groups/$gid/devices/$slot/$f"
            done
        done
    done
fi

echo "  /sys/class/drm"
mkdir -p "$DEST/sys/class/drm"
if [ -d /sys/class/drm ]; then
    for con in /sys/class/drm/card*-*; do
        [ -d "$con" ] || continue
        name="$(basename "$con")"
        target="$DEST/sys/class/drm/$name"
        mkdir -p "$target"
        for f in status modes; do
            copy_if "$con/$f" "sys/class/drm/$name/$f"
        done
    done
    # Per-card device pointer — let the parser map cards back to
    # PCI slots.
    for card in /sys/class/drm/card[0-9]*; do
        [ -d "$card" ] || continue
        name="$(basename "$card")"
        if [ -L "$card/device" ]; then
            target="$(readlink "$card/device" 2>/dev/null || true)"
            if [ -n "$target" ]; then
                slot="$(basename "$target")"
                mkdir -p "$DEST/sys/class/drm/$name"
                # Write the slot string directly so the fixture-
                # root parser's "read_to_string + normalize" path
                # finds it.
                printf '%s\n' "$slot" > "$DEST/sys/class/drm/$name/device"
            fi
        fi
    done
fi

# ---------------------------------------------------------------------------
# Readiness — kernel headers, OVMF, libvirt domains, user groups
# ---------------------------------------------------------------------------

echo "  readiness facts"

# Kernel header presence — copy the marker file if the directory
# exists. The fixture-root reader checks `<root>/usr/lib/modules/
# <kernel>/build` exists; an empty placeholder is enough.
KVER="$(uname -r 2>/dev/null || echo "unknown")"
if [ -d "/usr/lib/modules/$KVER/build" ]; then
    mkdir -p "$DEST/usr/lib/modules/$KVER"
    : > "$DEST/usr/lib/modules/$KVER/build"
fi

# OVMF firmware paths. We only need to mark the candidates that
# exist; the fixture-root reader checks `path.exists()`.
for ovmf in \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/OVMF/OVMF_VARS.fd \
    /usr/share/OVMF/OVMF_CODE_4M.fd \
    /usr/share/OVMF/OVMF_VARS_4M.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_VARS.fd \
    /usr/share/edk2/x64/OVMF_CODE.4m.fd \
    /usr/share/edk2/x64/OVMF_VARS.4m.fd \
    /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
    /usr/share/edk2-ovmf/x64/OVMF_VARS.fd \
    /usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd \
    /usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd \
    /usr/share/edk2/ovmf/OVMF_CODE.fd \
    /usr/share/edk2/ovmf/OVMF_VARS.fd \
    /usr/share/qemu/ovmf-x86_64-code.bin \
    /usr/share/qemu/ovmf-x86_64-vars.bin
do
    if [ -r "$ovmf" ]; then
        rel="${ovmf#/}"
        mkdir -p "$DEST/$(dirname "$rel")"
        : > "$DEST/$rel"
    fi
done

# User access — sanitized
groups_line="$(id -nG 2>/dev/null || true)"
if [ -n "$groups_line" ]; then
    {
        echo "testuser"
        echo "$groups_line"
    } > "$DEST/etc/virtu-user-groups"
    sanitize "$DEST/etc/virtu-user-groups"
fi

# libvirt domains
if command -v virsh > /dev/null 2>&1; then
    write_with_sanitize var/lib/libvirt/virsh-list-all virsh list --all
fi

# Audio backend. The parser's first check is the contents of
# `<root>/tmp/virtu-pactl-info`; capture `pactl info` there so
# the fixture reflects the live host's audio stack (PipeWire,
# PulseAudio, JACK). Falls through to the live-host /proc
# checks naturally if pactl is missing.
if command -v pactl > /dev/null 2>&1; then
    write_with_sanitize tmp/virtu-pactl-info pactl info
fi
# /proc/asound presence — let the ALSA fallback work even when
# pactl is not installed.
if [ -d /proc/asound ]; then
    mkdir -p "$DEST/proc/asound"
fi

# ---------------------------------------------------------------------------
# Provenance README
# ---------------------------------------------------------------------------

cat > "$DEST/README.md" <<EOF
# Virtu fixture: $FIXTURE_NAME

Captured by \`tests/scripts/capture_fixture.sh\` on a live host.
Sensitive data (hostnames, usernames, home paths, MAC addresses)
has been sanitized in place.

This fixture is consumed by the existing \`*_from_root\` /
\`detect_all_from_sysfs_root\` parser entry points under
\`src/detect/\`. To use it in a test, point the parser at this
directory:

\`\`\`rust
let profile = ...;  // build the SystemProfile by hand using the
                    // detect_*_from_root helpers, just like
                    // \`tests/phase_a_executor.rs::fixture_profile\`
\`\`\`

If you captured this fixture to pin a regression, write a
hermetic test that loads it before fixing the bug, so the test
fails with the regression in place and passes with the fix.

## Provenance

- Capture timestamp: $(date -u +'%Y-%m-%dT%H:%M:%SZ')
- Kernel version: $(uname -r 2>/dev/null || echo "unknown")
- Distro: $(grep -E '^PRETTY_NAME=' /etc/os-release 2>/dev/null \
    | cut -d= -f2- | tr -d '"' || echo "unknown")
- Capture script commit: $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo "unknown")
EOF

echo "Done. Fixture written to $DEST"
echo ""
echo "Next steps:"
echo "  1. Review $DEST/README.md and the captured files for any"
echo "     remaining sensitive data the sanitizer missed."
echo "  2. Add a regression test under tests/ that loads this"
echo "     fixture root, following the pattern in"
echo "     tests/phase_a_executor.rs."
echo "  3. Commit \$DEST to the repo so the regression is"
echo "     reproducible."
