//! dracut (Fedora / RHEL / openSUSE) writer.
//!
//! Generates `/etc/dracut.conf.d/virtu-vfio.conf`. Dracut config files
//! are tiny: a single `add_drivers+=" … "` line is enough. We always
//! generate this as a Virtu-managed file rather than editing
//! `/etc/dracut.conf`, so cleanup is `rm` rather than a parse-and-revert.
//!
//! Output is fully reproducible and idempotent.

use super::super::WriterError;
use super::VFIO_MODULES;

pub const FILE_BANNER: &str = "# Managed by Virtu (https://github.com/H-Sami/Virtu).\n# Edit through `virtu` so the snapshot manifest stays consistent.\n";

pub fn generate_dracut_conf() -> Result<String, WriterError> {
    let mut out = String::new();
    out.push_str(FILE_BANNER);
    out.push('\n');
    // dracut is sensitive to leading/trailing whitespace inside the quoted
    // list. The canonical form is `add_drivers+=" mod1 mod2 "`.
    out.push_str(&format!("add_drivers+=\" {} \"\n", VFIO_MODULES.join(" ")));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_add_drivers_line_with_all_modules() {
        let output = generate_dracut_conf().unwrap();
        assert!(output.contains("add_drivers+=\" vfio_pci vfio vfio_iommu_type1 \""));
    }

    #[test]
    fn includes_managed_banner() {
        let output = generate_dracut_conf().unwrap();
        assert!(output.starts_with("# Managed by Virtu"));
    }

    #[test]
    fn output_is_byte_identical_across_calls() {
        let a = generate_dracut_conf().unwrap();
        let b = generate_dracut_conf().unwrap();
        assert_eq!(a, b);
    }
}
