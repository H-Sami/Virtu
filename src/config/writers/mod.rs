//! Pure config writers (Milestone 6).
//!
//! Each writer is a pure function that takes the current contents of a
//! config file and returns the new contents. The shell wrappers that run
//! `grub-mkconfig`, `mkinitcpio`, `dracut`, etc. live in sibling modules
//! and are intentionally thin so the rewrite logic stays fully testable
//! without touching the host.

pub mod commands;
pub mod grub;
pub mod initramfs;
pub mod systemd_boot;
pub mod vfio_modprobe;

/// Errors returned by the pure rewrite functions.
#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    #[error("writer found malformed input near line {line}: {detail}")]
    MalformedInput { line: usize, detail: String },
    #[error("writer rejected an empty parameter list")]
    EmptyParams,
}
