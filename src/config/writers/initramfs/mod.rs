//! Initramfs writers (slice 6.5).
//!
//! Three host-side initramfs systems are supported: mkinitcpio (Arch),
//! dracut (Fedora / RHEL / openSUSE), and update-initramfs (Debian /
//! Ubuntu). Each gets its own pure rewrite function. The writers ensure
//! the VFIO kernel modules are pulled into the initramfs so they load
//! before host GPU drivers.
//!
//! Modules every writer adds:
//! - `vfio_pci` (the device driver itself)
//! - `vfio` (umbrella module pulled in transitively by vfio_pci, but
//!   declared explicitly for older kernels)
//! - `vfio_iommu_type1` (Type-1 IOMMU backend)
//!
//! `vfio_virqfd` is intentionally omitted because it merged into vfio in
//! kernel 6.2 and listing it on a current kernel produces a "unknown
//! module" warning at boot. If the knowledge base ever needs to special-
//! case older kernels, that logic belongs here.

pub mod dracut;
pub mod mkinitcpio;
pub mod update_initramfs;

/// Modules every initramfs writer must pull in.
pub const VFIO_MODULES: &[&str] = &["vfio_pci", "vfio", "vfio_iommu_type1"];
