# Fedora / openSUSE RPM spec for Virtu (Milestone 10, slice 10.6).
#
# Both Fedora and openSUSE ship Rust + Cargo through their primary
# repos and consume the same .spec layout, so a single recipe covers
# both distros. Distro-specific quirks (e.g. openSUSE's `qemu-x86`
# subpackage vs Fedora's `qemu-system-x86`) are handled with
# %if 0%{?suse_version} blocks below.
#
# Build with:
#   cd packaging/rpm
#   rpmdev-setuptree                       # one-time
#   tar -czf ~/rpmbuild/SOURCES/virtu-0.1.0.tar.gz \
#       --transform 's,^,virtu-0.1.0/,' \
#       -C ../../ \
#       Cargo.toml Cargo.lock src tests packaging/share README.md
#   cp virtu.spec ~/rpmbuild/SPECS/
#   rpmbuild -ba ~/rpmbuild/SPECS/virtu.spec

Name:           virtu
Version:        0.1.0
Release:        1%{?dist}
Summary:        Local-first Linux GPU passthrough automation tool

License:        MIT
URL:            https://github.com/H-Sami/Virtu
Source0:        %{name}-%{version}.tar.gz

# Build dependencies. cargo + rust ship under the same package name on
# Fedora and openSUSE, so no %if guard is needed here.
BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gzip

# Architecture: Virtu calls libc::getuid() and shells out to host-only
# tooling, so it's effectively Linux-x86_64-only. Keep the spec
# explicit so a stray ARM build doesn't quietly produce a broken
# package.
ExclusiveArch:  x86_64

# Runtime dependencies. The packages below are what Virtu's CLI shells
# out to at apply / resume time.
%if 0%{?suse_version}
Requires:       qemu-x86
Requires:       qemu-tools
Requires:       libvirt-client
Requires:       libvirt-daemon-qemu
Requires:       qemu-ovmf-x86_64
%else
Requires:       qemu-system-x86
Requires:       qemu-img
Requires:       libvirt-client
Requires:       libvirt-daemon-driver-qemu
Requires:       edk2-ovmf
%endif
Requires:       bash

# Optional dependencies: nice-to-have, never blocking. The
# compatibility report surfaces missing optional tools as PASS-with-
# note instead of FAIL.
Recommends:     virt-manager
Recommends:     dnsmasq

%description
Virtu is a local-first Rust tool that detects a Linux host's hardware
and software configuration, explains the viable GPU passthrough paths,
generates a verified plan, snapshots every file it may touch, applies
changes atomically with rollback support, and registers a libvirt
domain.

The tool is designed around the principle of honest refusal: rather
than claim to "fix any PC", it explains exactly what is and isn't
possible on the detected host, and only mutates state through a
snapshot manifest that can be replayed in reverse.

Virtu never sends data to remote services, requires no account, and
runs entirely from local data. The bundled knowledge base of GPU
quirks and distro paths is compiled into the binary at build time.

%prep
%autosetup -n %{name}-%{version}

%build
# Pin against the lockfile in the source tree. `--frozen` would also
# work, but `--locked` produces a clearer error if a contributor
# forgot to commit a Cargo.lock update. CARGO_TARGET_DIR keeps the
# build artefacts inside %{_builddir} so a second `rpmbuild` from the
# same tarball doesn't reuse stale artefacts.
export CARGO_TARGET_DIR=%{_builddir}/%{name}-%{version}/target
cargo build --release --locked --bin virtu

%check
# The full suite is hermetic (MemoryFileSystem + fixture roots). No
# real-host commands run, so this is safe inside rpmbuild's mock
# sandbox.
export CARGO_TARGET_DIR=%{_builddir}/%{name}-%{version}/target
cargo test --release --locked

%install
# Binary.
install -Dm755 \
    %{_builddir}/%{name}-%{version}/target/release/virtu \
    %{buildroot}%{_bindir}/virtu

# Man page (gzipped per RPM packaging guidelines).
install -Dm644 \
    %{_builddir}/%{name}-%{version}/packaging/share/man/virtu.1 \
    %{buildroot}%{_mandir}/man1/virtu.1
gzip -9 %{buildroot}%{_mandir}/man1/virtu.1

%files
%license LICENSE
%doc README.md
%{_bindir}/virtu
%{_mandir}/man1/virtu.1.gz

%changelog
* Tue May 19 2026 Virtu Contributors <https://github.com/H-Sami/Virtu> - 0.1.0-1
- Initial RPM packaging for Milestone 10 (slice 10.6).
- Single binary plus shared man page; bundled knowledge base is
  compiled in via include_str! so no runtime data files are needed.
- Hermetic test suite runs under %check.
