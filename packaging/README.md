# Virtu packaging

This directory holds distro-specific packaging recipes. Each subfolder
is a self-contained build target for one packaging format:

- `arch/` — Arch Linux `PKGBUILD` consumed by `makepkg`.
- `rpm/` — Fedora / openSUSE RPM `.spec` consumed by `rpmbuild`.
- `debian/` — Debian / Ubuntu source-package layout consumed by
  `dpkg-buildpackage`.

The shared man page lives in `share/man/virtu.1`. Every distro recipe
installs it to its conventional manpage directory.

## Building the Arch package

From a clean Arch host with `base-devel` and `rust` installed:

```bash
cd packaging/arch
makepkg --syncdeps --rmdeps --clean
sudo pacman -U virtu-*.pkg.tar.zst
```

The `PKGBUILD` builds Virtu with `cargo build --release --frozen`,
installs the binary to `/usr/bin/virtu`, the man page to
`/usr/share/man/man1/virtu.1.gz`, and the bundled `MIT` license to
`/usr/share/licenses/virtu/LICENSE`.

## Building the RPM package (Fedora / openSUSE)

From a clean Fedora or openSUSE host with `cargo`, `rust`, and
`rpm-build` installed:

```bash
cd packaging/rpm
rpmdev-setuptree
tar -czf ~/rpmbuild/SOURCES/virtu-0.1.0.tar.gz \
    --transform 's,^,virtu-0.1.0/,' \
    -C ../../ \
    Cargo.toml Cargo.lock src tests packaging/share README.md LICENSE
cp virtu.spec ~/rpmbuild/SPECS/
rpmbuild -ba ~/rpmbuild/SPECS/virtu.spec
sudo dnf install ~/rpmbuild/RPMS/x86_64/virtu-*.rpm   # Fedora
# or
sudo zypper install ~/rpmbuild/RPMS/x86_64/virtu-*.rpm  # openSUSE
```

The `.spec` builds Virtu with `cargo build --release --locked`,
runs the hermetic test suite under `%check`, installs the binary to
`/usr/bin/virtu`, and gzips the man page to `/usr/share/man/man1/`.
Distro-specific runtime dependencies (`qemu-system-x86` vs `qemu-x86`,
`edk2-ovmf` vs `qemu-ovmf-x86_64`, etc.) are gated by
`%if 0%{?suse_version}` blocks so a single recipe covers both
distros.

## Why no per-distro patches?

Virtu is a single Rust binary plus one bundled knowledge-base TOML
file (compiled in via `include_str!`), so there is nothing distro-
specific in the build path. The recipes differ only in:

- where they install the binary and man page,
- how they declare runtime dependencies (`qemu-base` on Arch,
  `qemu-system-x86` on Debian, etc.),
- which shell-completion / dependency conventions they expect.

If a distro ever needs a real patch — e.g. an OVMF path that does not
match what `src/kb/data/` ships — the right fix is to extend the
bundled knowledge base, not to fork the binary per distro.

## Building the Debian / Ubuntu package

From a clean Debian or Ubuntu host with `build-essential`, `devscripts`,
`debhelper-compat`, `cargo`, and `rustc` installed:

```bash
# From the repository root, copy the debian/ recipe in place and build:
cp -r packaging/debian ./debian
dpkg-buildpackage -us -uc -b
sudo dpkg -i ../virtu_0.1.0-1_amd64.deb
```

The recipe builds Virtu with `cargo build --release --locked`, runs
the hermetic test suite under `override_dh_auto_test`, installs the
binary to `/usr/bin/virtu`, and gzips the man page to
`/usr/share/man/man1/virtu.1.gz`. Runtime dependencies use Debian's
canonical names (`qemu-system-x86`, `libvirt-clients`,
`libvirt-daemon-system`, `ovmf`).
