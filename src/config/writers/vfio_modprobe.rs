//! Pure VFIO modprobe writer (slice 6.4).
//!
//! Generates the contents of `/etc/modprobe.d/virtu-vfio.conf`. This file
//! tells the kernel which PCI devices vfio-pci should claim at boot and
//! ensures vfio-pci loads before nvidia / amdgpu / i915 / nouveau, so the
//! host GPU drivers do not grab the passthrough GPU first.
//!
//! The writer is pure: given a sorted, deduplicated list of `<vendor>:
//! <device>` PCI ids, it returns the file contents as a UTF-8 string. The
//! file does not exist on a stock host, so this writer is paired with
//! `declare_created_entry` in `config::atomic_write` to register a snapshot
//! entry before the first write.
//!
//! The generated file is fully reproducible: same inputs always produce
//! byte-identical output. That keeps idempotency trivial.

use super::WriterError;

/// Banner identifying Virtu-managed files. Helps human operators tell which
/// modprobe configs are safe to remove during cleanup.
pub const FILE_BANNER: &str = "# Managed by Virtu (https://github.com/H-Sami/Virtu).\n# Edit through `virtu` so the snapshot manifest stays consistent.\n";

/// Drivers we explicitly want to keep behind vfio-pci. The list is small
/// on purpose; we add to it as the knowledge base grows.
const SOFTDEPS: &[&str] = &[
    "nvidia",
    "nvidia_drm",
    "nouveau",
    "amdgpu",
    "radeon",
    "i915",
];

/// Generate `/etc/modprobe.d/virtu-vfio.conf`.
///
/// `pci_ids` must be a non-empty list of `<vendor>:<device>` strings. The
/// caller is responsible for sorting and deduplicating; this writer
/// validates the format and refuses anything that does not parse as
/// `[0-9a-fA-F]{4}:[0-9a-fA-F]{4}`.
pub fn generate_vfio_modprobe_conf(pci_ids: &[String]) -> Result<String, WriterError> {
    if pci_ids.is_empty() {
        return Err(WriterError::EmptyParams);
    }

    for (idx, id) in pci_ids.iter().enumerate() {
        if !is_vendor_device_id(id) {
            return Err(WriterError::MalformedInput {
                line: idx + 1,
                detail: format!("`{id}` is not a `vendor:device` PCI id"),
            });
        }
    }

    let mut output = String::new();
    output.push_str(FILE_BANNER);
    output.push('\n');

    output.push_str(&format!("options vfio-pci ids={}\n", pci_ids.join(",")));
    output.push_str("options vfio-pci disable_vga=1\n\n");

    for driver in SOFTDEPS {
        output.push_str(&format!("softdep {driver} pre: vfio-pci\n"));
    }

    Ok(output)
}

fn is_vendor_device_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 9 {
        return false;
    }
    if bytes[4] != b':' {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(i, &b)| if i == 4 { true } else { b.is_ascii_hexdigit() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_options_line_with_sorted_ids() {
        let ids = vec!["10de:1c03".to_string(), "10de:0fb9".to_string()];
        let output = generate_vfio_modprobe_conf(&ids).unwrap();
        assert!(output.contains("options vfio-pci ids=10de:1c03,10de:0fb9"));
    }

    #[test]
    fn includes_softdeps_for_common_gpu_drivers() {
        let ids = vec!["10de:1c03".to_string()];
        let output = generate_vfio_modprobe_conf(&ids).unwrap();
        for driver in ["nvidia", "nouveau", "amdgpu", "radeon", "i915"] {
            assert!(
                output.contains(&format!("softdep {driver} pre: vfio-pci")),
                "missing softdep for {driver} in:\n{output}"
            );
        }
    }

    #[test]
    fn includes_managed_banner() {
        let ids = vec!["10de:1c03".to_string()];
        let output = generate_vfio_modprobe_conf(&ids).unwrap();
        assert!(output.starts_with("# Managed by Virtu"));
    }

    #[test]
    fn rejects_empty_id_list() {
        let err = generate_vfio_modprobe_conf(&[]).unwrap_err();
        matches!(err, WriterError::EmptyParams);
    }

    #[test]
    fn rejects_malformed_pci_id() {
        for bad in ["1234", "10de1c03", "xxxx:1c03", "10de:zzzz", "10de :1c03"] {
            let err = generate_vfio_modprobe_conf(&[bad.to_string()]).unwrap_err();
            match err {
                WriterError::MalformedInput { detail, .. } => {
                    assert!(
                        detail.contains("vendor:device"),
                        "unexpected detail: {detail}"
                    );
                }
                other => panic!("expected MalformedInput for `{bad}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn output_is_byte_identical_across_calls() {
        let ids = vec!["10de:1c03".to_string(), "10de:0fb9".to_string()];
        let a = generate_vfio_modprobe_conf(&ids).unwrap();
        let b = generate_vfio_modprobe_conf(&ids).unwrap();
        assert_eq!(a, b);
    }
}
