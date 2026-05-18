//! update-initramfs (Debian / Ubuntu) writer.
//!
//! Edits `/etc/initramfs-tools/modules` to add Virtu's VFIO modules. The
//! file is a simple newline-separated list; lines starting with `#` are
//! comments. We append missing entries at the end of the file rather than
//! shuffling existing order, because some hosts have hand-edited
//! load-order constraints (e.g. `dm_mod` before LUKS modules) that we
//! must not touch.
//!
//! Idempotent.

use super::super::WriterError;
use super::VFIO_MODULES;

pub fn rewrite_initramfs_modules(input: &str) -> Result<String, WriterError> {
    // Build a set of existing non-comment, non-blank tokens so we know which
    // modules to skip.
    let existing: std::collections::HashSet<String> = input
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                None
            } else {
                // The Debian docs allow `module arg1=val arg2=val` syntax.
                // We only care about the module name (first token).
                trimmed.split_whitespace().next().map(String::from)
            }
        })
        .collect();

    let mut output = String::from(input);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }

    let mut missing: Vec<&'static str> = Vec::new();
    for module in VFIO_MODULES {
        if !existing.contains(*module) {
            missing.push(*module);
        }
    }

    if missing.is_empty() {
        return Ok(output);
    }

    // Add a small managed-section header the first time we touch the file
    // so cleanup is unambiguous.
    if !output.contains("# Virtu: VFIO modules") {
        output.push_str("# Virtu: VFIO modules\n");
    }
    for module in missing {
        output.push_str(module);
        output.push('\n');
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_missing_modules_after_existing_lines() {
        let input = "# host modules\ndm_mod\nraid1\n";
        let output = rewrite_initramfs_modules(input).unwrap();
        assert!(output.starts_with("# host modules\ndm_mod\nraid1\n"));
        for module in VFIO_MODULES {
            assert!(output.contains(&format!("\n{module}\n")));
        }
        assert!(output.contains("# Virtu: VFIO modules"));
    }

    #[test]
    fn idempotent_when_modules_already_listed() {
        let input = "# Virtu: VFIO modules\nvfio_pci\nvfio\nvfio_iommu_type1\n";
        let output = rewrite_initramfs_modules(input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn handles_module_args_when_checking_for_presence() {
        let input = "vfio_pci ids=10de:1c03\nvfio\nvfio_iommu_type1\n";
        let output = rewrite_initramfs_modules(input).unwrap();
        // No new lines added, since the first token of each line covers
        // every required module.
        assert_eq!(output, input);
    }

    #[test]
    fn ensures_trailing_newline_before_appending() {
        let input = "dm_mod"; // no trailing newline
        let output = rewrite_initramfs_modules(input).unwrap();
        assert!(output.starts_with("dm_mod\n"));
        assert!(output.contains("# Virtu: VFIO modules"));
    }

    #[test]
    fn empty_input_produces_only_managed_section() {
        let input = "";
        let output = rewrite_initramfs_modules(input).unwrap();
        assert!(output.contains("# Virtu: VFIO modules"));
        assert!(output.contains("vfio_pci\n"));
    }
}
