# Virtu packaging

This directory holds distro-specific packaging recipes. Each subfolder
is a self-contained build target for one packaging format:

- `arch/` — Arch Linux `PKGBUILD` consumed by `makepkg`.
- `rpm/` — Fedora / openSUSE RPM `.spec` consumed by `rpmbuild` (added in slice 10.6).
- `debian/` — Debian / Ubuntu source-package layout consumed by
  `dpkg-buildpackage` (added in slice 10.7).

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
