//! mkinitcpio (Arch) writer.
//!
//! Edits `/etc/mkinitcpio.conf` so the `MODULES=(…)` array contains every
//! VFIO module Virtu needs. Other lines and the `HOOKS=` array are left
//! untouched. Idempotent.

use super::super::WriterError;
use super::VFIO_MODULES;

pub fn rewrite_mkinitcpio_conf(input: &str) -> Result<String, WriterError> {
    let mut output_lines: Vec<String> = Vec::with_capacity(input.lines().count() + 1);
    let mut found = false;

    for (idx, line) in input.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("MODULES=") {
            if found {
                return Err(WriterError::MalformedInput {
                    line: idx + 1,
                    detail: "duplicate MODULES= assignment".to_string(),
                });
            }
            found = true;
            let updated = ensure_modules_in_array_line(line).map_err(|detail| {
                WriterError::MalformedInput {
                    line: idx + 1,
                    detail,
                }
            })?;
            output_lines.push(updated);
        } else {
            output_lines.push(line.to_string());
        }
    }

    if !found {
        output_lines.push(format!("MODULES=({})", VFIO_MODULES.join(" ")));
    }

    let mut output = output_lines.join("\n");
    if input.ends_with('\n') && !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn ensure_modules_in_array_line(line: &str) -> Result<String, String> {
    let eq = line
        .find('=')
        .ok_or_else(|| "no '=' in MODULES line".to_string())?;
    let key = &line[..eq];
    let value = &line[eq + 1..];

    let trimmed = value.trim();
    if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
        return Err("MODULES value is not a `(...)` array".to_string());
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    let mut tokens: Vec<String> = inner.split_whitespace().map(String::from).collect();

    for module in VFIO_MODULES {
        if !tokens.iter().any(|t| t == module) {
            tokens.push((*module).to_string());
        }
    }

    Ok(format!("{key}=({})", tokens.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_vfio_modules_to_existing_modules_array() {
        let input = "MODULES=()\nHOOKS=(base udev autodetect)\n";
        let output = rewrite_mkinitcpio_conf(input).unwrap();
        assert!(output.contains("MODULES=(vfio_pci vfio vfio_iommu_type1)"));
        assert!(output.contains("HOOKS=(base udev autodetect)"));
    }

    #[test]
    fn idempotent_when_modules_already_present() {
        let input = "MODULES=(vfio_pci vfio vfio_iommu_type1)\nHOOKS=(base)\n";
        let output = rewrite_mkinitcpio_conf(input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn appends_only_missing_modules() {
        let input = "MODULES=(vfio_pci other_thing)\n";
        let output = rewrite_mkinitcpio_conf(input).unwrap();
        assert!(output.contains("MODULES=(vfio_pci other_thing vfio vfio_iommu_type1)"));
    }

    #[test]
    fn appends_modules_assignment_when_missing() {
        let input = "HOOKS=(base udev)\n";
        let output = rewrite_mkinitcpio_conf(input).unwrap();
        assert!(output.contains("MODULES=(vfio_pci vfio vfio_iommu_type1)"));
        assert!(output.contains("HOOKS=(base udev)"));
    }

    #[test]
    fn rejects_malformed_modules_value() {
        let input = "MODULES=not-an-array\n";
        let err = rewrite_mkinitcpio_conf(input).unwrap_err();
        match err {
            WriterError::MalformedInput { detail, .. } => assert!(detail.contains("array")),
            other => panic!("expected MalformedInput, got {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_modules_assignments() {
        let input = "MODULES=(a)\nMODULES=(b)\n";
        let err = rewrite_mkinitcpio_conf(input).unwrap_err();
        match err {
            WriterError::MalformedInput { line, .. } => assert_eq!(line, 2),
            other => panic!("expected MalformedInput, got {other:?}"),
        }
    }
}
