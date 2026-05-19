//! Local diagnostics knowledge base (Milestone 10, slice 10.1).
//!
//! The knowledge base bundles three pieces of editorial content the
//! rest of the codebase consults at runtime:
//!
//! - **Distro paths.** The OVMF firmware and qemu binary locations
//!   differ across Arch / Debian / Fedora / openSUSE. The default
//!   tables here are conservative; users who need to override one can
//!   load a personal TOML through [`KnowledgeBase::from_file`].
//! - **GPU quirks.** Vendor / device patterns plus remediation steps.
//!   `src/vm/xml/mod.rs` already consults `quirks_for_gpu` to enable
//!   the AMD reset workaround when the bundled `reset-bug` issue
//!   matches.
//! - **Error patterns.** Regexes that match common host-command
//!   failures (vfio permission denied, KVM module missing, virsh
//!   domain already defined, etc.) plus a human cause and ordered
//!   fix options. `src/engine/diagnostics.rs::diagnose_error` walks
//!   them top-to-bottom.
//!
//! The bundled TOML files live under `src/kb/data/` and are baked into
//! the binary with `include_str!` so a fresh `cargo install` ships
//! everything users need without any external file dependency. Users
//! who want to override a quirk (e.g. they have a tested fix for a new
//! AMD card the bundled list does not cover) can load a personal TOML
//! at startup. `KnowledgeBase::from_file` parses the same schema as
//! the bundled files, so the user override fully replaces the bundled
//! list when the file is present.

pub mod schema;

use std::path::Path;

use crate::detect::distro::{DistroFamily, DistroInfo};
use schema::{DistroPaths, ErrorPattern, ErrorPatternsFile, GpuQuirk, GpuQuirksFile};

/// Bundled `gpu_quirks.toml`. Compiled into the binary so the runtime
/// never depends on the shipped install layout.
const BUNDLED_GPU_QUIRKS_TOML: &str = include_str!("data/gpu_quirks.toml");

/// Bundled `error_patterns.toml`. See [`BUNDLED_GPU_QUIRKS_TOML`].
const BUNDLED_ERROR_PATTERNS_TOML: &str = include_str!("data/error_patterns.toml");

#[derive(Debug, Clone)]
pub struct KnowledgeBase {
    generic_paths: DistroPaths,
    arch_paths: DistroPaths,
    debian_paths: DistroPaths,
    fedora_paths: DistroPaths,
    opensuse_paths: DistroPaths,
    gpu_quirks: Vec<GpuQuirk>,
    error_patterns: Vec<ErrorPattern>,
}

/// Parse and load failure modes for [`KnowledgeBase::from_file`] and
/// the bundled-data validators.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeBaseError {
    #[error("read knowledge-base file at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse knowledge-base TOML at {path}: {source}")]
    ParseFile {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("parse bundled knowledge-base TOML `{name}`: {source}")]
    ParseBundled {
        name: &'static str,
        #[source]
        source: toml::de::Error,
    },
    #[error(
        "knowledge-base entry has invalid regex `{regex}` (id `{id}`): {source}. \
         Either the bundled data is corrupt or the user override is malformed."
    )]
    InvalidRegex {
        id: String,
        regex: String,
        #[source]
        source: regex::Error,
    },
    #[error(
        "knowledge-base entry has invalid PCI vendor id `{vendor_id}` for issue `{issue_id}`: \
         expected 4-hex-digit lowercase value (e.g. `10de`)"
    )]
    InvalidVendorId { issue_id: String, vendor_id: String },
    #[error(
        "knowledge-base entry has invalid device-id pattern `{pattern}` for issue `{issue_id}`: \
         expected `*` or 4-hex-digit lowercase value"
    )]
    InvalidDeviceIdPattern { issue_id: String, pattern: String },
    #[error("knowledge-base file at {path} does not exist")]
    Missing { path: std::path::PathBuf },
    #[error(
        "user knowledge-base override at {path} declared {section} entry without an `{field}` field"
    )]
    MissingField {
        path: std::path::PathBuf,
        section: &'static str,
        field: &'static str,
    },
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        // The default impl never fails: it ships with the bundled data
        // pre-validated by the `bundled_data_is_well_formed` test, so
        // any panic here is a build-time correctness bug we want to
        // surface immediately. The bundled() constructor returns the
        // same value, but is fallible at the type level so callers
        // can pre-load custom data without unwrapping. Internal callers
        // who already know the data is valid (e.g. `XmlBuilder::new`)
        // use `default`.
        Self::load_bundled().unwrap_or_else(|err| {
            // Fallback: empty quirks/patterns plus default paths. We
            // log the parse failure so the test suite catches it
            // before shipping. tracing::error! is a no-op when no
            // subscriber is installed.
            tracing::error!(?err, "bundled knowledge-base data failed to parse; falling back to empty tables. This is a build-time bug.");
            Self::empty()
        })
    }
}

impl KnowledgeBase {
    /// Load the bundled knowledge base. Infallible because the
    /// bundled TOML is validated by the test suite at build time;
    /// runtime callers want a `KnowledgeBase`, not a `Result`. If
    /// validation ever does fail (a build-time correctness bug),
    /// the returned KB falls back to empty quirks/patterns plus
    /// the default distro paths so the rest of the binary can still
    /// run; the failure is logged through `tracing::error!`.
    ///
    /// Use [`KnowledgeBase::try_bundled`] in tests that want the
    /// stricter `Result`-returning variant for parse-failure
    /// regressions.
    pub fn bundled() -> Self {
        Self::default()
    }

    /// Strict variant of [`KnowledgeBase::bundled`] that propagates
    /// parse / validation failures instead of falling back. Used by
    /// the test suite to pin the bundled data.
    pub fn try_bundled() -> Result<Self, KnowledgeBaseError> {
        Self::load_bundled()
    }

    /// Build a knowledge base by overlaying user-supplied quirks and
    /// error-pattern files on top of the bundled data. Either or both
    /// paths may be `None`; missing files are an error (so a typo'd
    /// path surfaces clearly).
    ///
    /// User-supplied entries are appended after the bundled list, so
    /// matches go to the bundled entry first when both lists carry the
    /// same id. This keeps the bundled data authoritative and lets a
    /// user add new quirks without overriding the official set.
    pub fn from_files(
        gpu_quirks_path: Option<&Path>,
        error_patterns_path: Option<&Path>,
    ) -> Result<Self, KnowledgeBaseError> {
        let mut kb = Self::load_bundled()?;
        if let Some(path) = gpu_quirks_path {
            let extra = load_gpu_quirks_from_path(path)?;
            kb.gpu_quirks.extend(extra);
        }
        if let Some(path) = error_patterns_path {
            let extra = load_error_patterns_from_path(path)?;
            kb.error_patterns.extend(extra);
        }
        kb.validate()?;
        Ok(kb)
    }

    pub fn paths_for_distro(&self, distro: &DistroInfo) -> &DistroPaths {
        match distro.family {
            DistroFamily::Arch => &self.arch_paths,
            DistroFamily::Debian | DistroFamily::Ubuntu => &self.debian_paths,
            DistroFamily::Fedora | DistroFamily::Rhel => &self.fedora_paths,
            DistroFamily::OpenSuse => &self.opensuse_paths,
            _ => &self.generic_paths,
        }
    }

    pub fn quirks_for_gpu(&self, vendor_id: &str, device_id: &str) -> Vec<&GpuQuirk> {
        self.gpu_quirks
            .iter()
            .filter(|quirk| {
                quirk.vendor_id.eq_ignore_ascii_case(vendor_id)
                    && (quirk.device_id_pattern == "*"
                        || quirk.device_id_pattern.eq_ignore_ascii_case(device_id))
            })
            .collect()
    }

    pub fn error_patterns(&self) -> &[ErrorPattern] {
        &self.error_patterns
    }

    /// Number of GPU quirks currently loaded. Useful for tests and
    /// the `virtu scan --verbose` summary line.
    pub fn gpu_quirk_count(&self) -> usize {
        self.gpu_quirks.len()
    }

    fn empty() -> Self {
        Self {
            generic_paths: DistroPaths::generic(),
            arch_paths: arch_paths(),
            debian_paths: debian_paths(),
            fedora_paths: fedora_paths(),
            opensuse_paths: opensuse_paths(),
            gpu_quirks: Vec::new(),
            error_patterns: Vec::new(),
        }
    }

    fn load_bundled() -> Result<Self, KnowledgeBaseError> {
        let quirks: GpuQuirksFile = toml::from_str(BUNDLED_GPU_QUIRKS_TOML).map_err(|source| {
            KnowledgeBaseError::ParseBundled {
                name: "gpu_quirks.toml",
                source,
            }
        })?;
        let patterns: ErrorPatternsFile =
            toml::from_str(BUNDLED_ERROR_PATTERNS_TOML).map_err(|source| {
                KnowledgeBaseError::ParseBundled {
                    name: "error_patterns.toml",
                    source,
                }
            })?;
        let kb = Self {
            generic_paths: DistroPaths::generic(),
            arch_paths: arch_paths(),
            debian_paths: debian_paths(),
            fedora_paths: fedora_paths(),
            opensuse_paths: opensuse_paths(),
            gpu_quirks: quirks.quirks,
            error_patterns: patterns.patterns,
        };
        kb.validate()?;
        Ok(kb)
    }

    /// Validate every loaded entry. Cheap: regex compile + simple hex
    /// checks. Run after every load so the runtime can rely on the
    /// invariants further downstream.
    fn validate(&self) -> Result<(), KnowledgeBaseError> {
        for quirk in &self.gpu_quirks {
            if !is_valid_pci_id_4hex(&quirk.vendor_id) {
                return Err(KnowledgeBaseError::InvalidVendorId {
                    issue_id: quirk.issue_id.clone(),
                    vendor_id: quirk.vendor_id.clone(),
                });
            }
            if quirk.device_id_pattern != "*" && !is_valid_pci_id_4hex(&quirk.device_id_pattern) {
                return Err(KnowledgeBaseError::InvalidDeviceIdPattern {
                    issue_id: quirk.issue_id.clone(),
                    pattern: quirk.device_id_pattern.clone(),
                });
            }
        }
        for pattern in &self.error_patterns {
            regex::Regex::new(&pattern.regex).map_err(|source| {
                KnowledgeBaseError::InvalidRegex {
                    id: pattern.id.clone(),
                    regex: pattern.regex.clone(),
                    source,
                }
            })?;
        }
        Ok(())
    }
}

fn arch_paths() -> DistroPaths {
    DistroPaths {
        ovmf_code: Some("/usr/share/edk2/x64/OVMF_CODE.fd".to_string()),
        ovmf_vars: Some("/usr/share/edk2/x64/OVMF_VARS.fd".to_string()),
        qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
        libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
    }
}

fn debian_paths() -> DistroPaths {
    DistroPaths {
        ovmf_code: Some("/usr/share/OVMF/OVMF_CODE.fd".to_string()),
        ovmf_vars: Some("/usr/share/OVMF/OVMF_VARS.fd".to_string()),
        qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
        libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
    }
}

fn fedora_paths() -> DistroPaths {
    DistroPaths {
        ovmf_code: Some("/usr/share/edk2/ovmf/OVMF_CODE.fd".to_string()),
        ovmf_vars: Some("/usr/share/edk2/ovmf/OVMF_VARS.fd".to_string()),
        qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
        libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
    }
}

fn opensuse_paths() -> DistroPaths {
    DistroPaths {
        ovmf_code: Some("/usr/share/qemu/ovmf-x86_64-code.bin".to_string()),
        ovmf_vars: Some("/usr/share/qemu/ovmf-x86_64-vars.bin".to_string()),
        qemu_binary: "/usr/bin/qemu-system-x86_64".to_string(),
        libvirt_images_dir: "/var/lib/libvirt/images".to_string(),
    }
}

fn is_valid_pci_id_4hex(id: &str) -> bool {
    id.len() == 4
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn load_gpu_quirks_from_path(path: &Path) -> Result<Vec<GpuQuirk>, KnowledgeBaseError> {
    if !path.exists() {
        return Err(KnowledgeBaseError::Missing {
            path: path.to_path_buf(),
        });
    }
    let content = std::fs::read_to_string(path).map_err(|source| KnowledgeBaseError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: GpuQuirksFile =
        toml::from_str(&content).map_err(|source| KnowledgeBaseError::ParseFile {
            path: path.to_path_buf(),
            source,
        })?;
    for quirk in &parsed.quirks {
        if quirk.issue_id.is_empty() {
            return Err(KnowledgeBaseError::MissingField {
                path: path.to_path_buf(),
                section: "[[quirks]]",
                field: "issue_id",
            });
        }
    }
    Ok(parsed.quirks)
}

fn load_error_patterns_from_path(path: &Path) -> Result<Vec<ErrorPattern>, KnowledgeBaseError> {
    if !path.exists() {
        return Err(KnowledgeBaseError::Missing {
            path: path.to_path_buf(),
        });
    }
    let content = std::fs::read_to_string(path).map_err(|source| KnowledgeBaseError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: ErrorPatternsFile =
        toml::from_str(&content).map_err(|source| KnowledgeBaseError::ParseFile {
            path: path.to_path_buf(),
            source,
        })?;
    for pattern in &parsed.patterns {
        if pattern.id.is_empty() {
            return Err(KnowledgeBaseError::MissingField {
                path: path.to_path_buf(),
                section: "[[patterns]]",
                field: "id",
            });
        }
    }
    Ok(parsed.patterns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_data_is_well_formed() {
        // Hard guarantee: the bundled TOML files parse and validate.
        // If this fails the binary cannot ship. Any maintainer adding
        // a new quirk or pattern must keep this green.
        let kb = KnowledgeBase::try_bundled().expect("bundled KB must parse and validate");
        assert!(
            kb.gpu_quirk_count() >= 1,
            "bundled KB must include at least one quirk"
        );
        assert!(
            !kb.error_patterns().is_empty(),
            "bundled KB must include at least one error pattern"
        );
    }

    #[test]
    fn bundled_quirks_include_amd_reset_bug() {
        // src/vm/xml/mod.rs::XmlBuilder consults quirks_for_gpu to
        // decide whether to emit the AMD reset-bug firmware block.
        // Pin the bundled entry so a future refactor doesn't silently
        // drop it.
        let kb = KnowledgeBase::try_bundled().unwrap();
        let amd = kb.quirks_for_gpu("1002", "7590");
        assert!(
            amd.iter().any(|q| q.issue_id == "reset-bug"),
            "bundled quirks must keep `reset-bug` for AMD GPUs"
        );
    }

    #[test]
    fn bundled_error_patterns_match_real_failure_strings() {
        // Regression pin: each pattern must actually match the kind of
        // stderr line we shipped it for. If a maintainer rewrites a
        // regex these checks fail loudly instead of silently dropping
        // a diagnostic.
        let kb = KnowledgeBase::try_bundled().unwrap();
        let cases: &[(&str, &str)] = &[
            (
                "vfio-permission-denied",
                "vfio: error opening /dev/vfio/15: Permission denied",
            ),
            (
                "vfio-set-iommu-failed",
                "vfio: failed to set iommu for container: Operation not permitted",
            ),
            (
                "kvm-permission-denied",
                "Could not access KVM kernel module: Permission denied",
            ),
            (
                "kvm-module-missing",
                "Could not access KVM kernel module: No such file or directory",
            ),
            (
                "qemu-img-exists",
                "qemu-img: /var/lib/libvirt/images/win.qcow2: File exists",
            ),
            (
                "virsh-domain-already-defined",
                "operation failed: domain 'virtu-windows' already exists",
            ),
            (
                "vfio-setup-container-failed",
                "vfio 0000:01:00.0: failed to setup container for group 17",
            ),
        ];
        for (id, sample) in cases {
            let pattern = kb
                .error_patterns()
                .iter()
                .find(|p| p.id == *id)
                .unwrap_or_else(|| panic!("bundled pattern `{id}` missing"));
            let regex = regex::Regex::new(&pattern.regex)
                .unwrap_or_else(|err| panic!("regex for `{id}` does not compile: {err}"));
            assert!(
                regex.is_match(sample),
                "regex `{}` for `{id}` did not match expected sample `{sample}`",
                pattern.regex
            );
        }
    }

    #[test]
    fn invalid_regex_is_rejected_at_load() {
        // Hand-build a KB with a busted regex and confirm validate()
        // returns InvalidRegex. Guards against a future patch that
        // skips validation on a custom load path.
        let mut kb = KnowledgeBase::empty();
        kb.error_patterns.push(ErrorPattern {
            id: "bad".to_string(),
            regex: "(unclosed".to_string(),
            cause: "x".to_string(),
            fix_options: Vec::new(),
        });
        let err = kb.validate().expect_err("bad regex must surface");
        assert!(
            matches!(err, KnowledgeBaseError::InvalidRegex { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn invalid_vendor_id_is_rejected_at_load() {
        let mut kb = KnowledgeBase::empty();
        kb.gpu_quirks.push(GpuQuirk {
            issue_id: "test".to_string(),
            vendor_id: "10DE".to_string(), // upper-case is rejected
            device_id_pattern: "*".to_string(),
            description: String::new(),
            fixes: Vec::new(),
        });
        let err = kb
            .validate()
            .expect_err("upper-case vendor id must be rejected");
        assert!(
            matches!(err, KnowledgeBaseError::InvalidVendorId { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn invalid_device_id_pattern_is_rejected_at_load() {
        let mut kb = KnowledgeBase::empty();
        kb.gpu_quirks.push(GpuQuirk {
            issue_id: "test".to_string(),
            vendor_id: "10de".to_string(),
            device_id_pattern: "abcde".to_string(), // 5 chars
            description: String::new(),
            fixes: Vec::new(),
        });
        let err = kb
            .validate()
            .expect_err("non-`*` non-4hex pattern must be rejected");
        assert!(
            matches!(err, KnowledgeBaseError::InvalidDeviceIdPattern { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn from_files_with_user_quirks_appends_to_bundled() {
        // Write a tempfile with one extra quirk and confirm the loader
        // surfaces both the bundled entries and the user-supplied one.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[[quirks]]
issue_id = "user-extra"
vendor_id = "10de"
device_id_pattern = "1f08"
description = "Specific to a user RTX card"
fixes = ["Test"]
"#,
        )
        .unwrap();

        let kb = KnowledgeBase::from_files(Some(tmp.path()), None)
            .expect("user-supplied quirks must load on top of bundled");
        assert!(kb
            .quirks_for_gpu("10de", "1f08")
            .iter()
            .any(|q| q.issue_id == "user-extra"));
        // Bundled entries still present.
        assert!(kb
            .quirks_for_gpu("1002", "7590")
            .iter()
            .any(|q| q.issue_id == "reset-bug"));
    }

    #[test]
    fn from_files_missing_path_returns_missing_error() {
        let err = KnowledgeBase::from_files(
            Some(Path::new("/nonexistent/virtu/quirks-please-fail.toml")),
            None,
        )
        .expect_err("missing override file must error");
        assert!(
            matches!(err, KnowledgeBaseError::Missing { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn from_files_with_user_error_patterns_appends_and_validates() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[[patterns]]
id = "user-extra"
regex = "test-marker-[0-9]+"
cause = "user-supplied marker hit"
fix_options = ["restart"]
"#,
        )
        .unwrap();

        let kb = KnowledgeBase::from_files(None, Some(tmp.path()))
            .expect("user-supplied patterns must load on top of bundled");
        assert!(kb.error_patterns().iter().any(|p| p.id == "user-extra"));
        // Pattern compiles via the validator path.
        let regex = regex::Regex::new(
            &kb.error_patterns()
                .iter()
                .find(|p| p.id == "user-extra")
                .unwrap()
                .regex,
        )
        .unwrap();
        assert!(regex.is_match("test-marker-42"));
    }

    #[test]
    fn from_files_with_invalid_user_regex_is_rejected() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
[[patterns]]
id = "user-broken"
regex = "(unclosed"
cause = "x"
fix_options = []
"#,
        )
        .unwrap();
        let err = KnowledgeBase::from_files(None, Some(tmp.path()))
            .expect_err("user-supplied invalid regex must be rejected at load time");
        assert!(
            matches!(err, KnowledgeBaseError::InvalidRegex { .. }),
            "got {err:?}"
        );
    }
}
