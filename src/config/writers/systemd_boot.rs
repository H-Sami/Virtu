//! Pure systemd-boot entry writer (slice 6.3).
//!
//! systemd-boot stores one config file per boot entry under
//! `/boot/loader/entries/<name>.conf`. The kernel cmdline lives on a single
//! `options` line. Virtu rewrites only the entry the user is currently
//! booted from (planner already isolates this), and only the `options`
//! line; everything else (`title`, `linux`, `initrd`, `version`,
//! `machine-id`, etc.) is preserved verbatim.
//!
//! This writer is pure: it takes a `<entry>.conf` body and returns a new
//! body. Companion shell-out wrappers (`bootctl update` etc.) live in a
//! sibling module once the executor needs them.

use super::WriterError;

/// Rewrite a systemd-boot entry so its `options` line contains every
/// parameter in `params`. Existing parameters are preserved; missing ones
/// are appended.
///
/// If the entry has no `options` line, the writer appends one. This is a
/// supported case: a stripped-down entry that boots a minimal kernel still
/// gets the IOMMU parameters.
///
/// Idempotence: re-running with the same params produces byte-identical
/// output.
pub fn rewrite_systemd_boot_entry(input: &str, params: &[String]) -> Result<String, WriterError> {
    if params.is_empty() {
        return Err(WriterError::EmptyParams);
    }

    let mut found = false;
    let mut output_lines: Vec<String> = Vec::with_capacity(input.lines().count() + 1);

    for (idx, line) in input.lines().enumerate() {
        // systemd-boot uses `key value` separated by whitespace, not `=`.
        // Comments are `#`. Leading whitespace is allowed.
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            output_lines.push(line.to_string());
            continue;
        }

        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        if key == "options" {
            if found {
                return Err(WriterError::MalformedInput {
                    line: idx + 1,
                    detail: "duplicate `options` line".to_string(),
                });
            }
            found = true;
            let value = parts.next().unwrap_or("").trim();
            let mut tokens: Vec<String> = if value.is_empty() {
                Vec::new()
            } else {
                value.split_whitespace().map(String::from).collect()
            };
            for param in params {
                if !tokens.iter().any(|t| t == param) {
                    tokens.push(param.clone());
                }
            }
            // Preserve the original leading whitespace on the line.
            let leading: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            output_lines.push(format!("{leading}options {}", tokens.join(" ")));
        } else {
            output_lines.push(line.to_string());
        }
    }

    if !found {
        output_lines.push(format!("options {}", params.join(" ")));
    }

    let mut output = output_lines.join("\n");
    if input.ends_with('\n') && !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(input: &str) -> String {
        input.to_string()
    }

    const ARCH_ENTRY: &str = "title Arch Linux\nlinux /vmlinuz-linux\ninitrd /initramfs-linux.img\noptions root=UUID=test rw quiet\n";

    #[test]
    fn appends_missing_params_to_options_line() {
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_systemd_boot_entry(ARCH_ENTRY, &params).unwrap();
        assert!(output.contains("options root=UUID=test rw quiet intel_iommu=on iommu=pt"));
        assert!(output.contains("title Arch Linux"));
        assert!(output.contains("linux /vmlinuz-linux"));
        assert!(output.contains("initrd /initramfs-linux.img"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn idempotent_when_all_params_already_present() {
        let entry =
            "title Arch\nlinux /vmlinuz-linux\noptions root=UUID=x rw intel_iommu=on iommu=pt\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_systemd_boot_entry(entry, &params).unwrap();
        assert_eq!(output, entry);
    }

    #[test]
    fn appends_only_missing_params() {
        let entry = "options root=UUID=x rw intel_iommu=on\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_systemd_boot_entry(entry, &params).unwrap();
        assert!(output.contains("options root=UUID=x rw intel_iommu=on iommu=pt"));
    }

    #[test]
    fn appends_options_line_when_missing() {
        let entry = "title Stripped Entry\nlinux /vmlinuz\ninitrd /initramfs.img\n";
        let params = vec![s("amd_iommu=on"), s("iommu=pt")];
        let output = rewrite_systemd_boot_entry(entry, &params).unwrap();
        assert!(output.contains("options amd_iommu=on iommu=pt"));
        assert!(output.contains("title Stripped Entry"));
    }

    #[test]
    fn rejects_empty_params() {
        let err = rewrite_systemd_boot_entry(ARCH_ENTRY, &[]).unwrap_err();
        matches!(err, WriterError::EmptyParams);
    }

    #[test]
    fn rejects_duplicate_options_lines() {
        let entry = "options a=1\noptions b=2\n";
        let params = vec![s("c=3")];
        let err = rewrite_systemd_boot_entry(entry, &params).unwrap_err();
        match err {
            WriterError::MalformedInput { line, .. } => assert_eq!(line, 2),
            other => panic!("expected MalformedInput, got {other:?}"),
        }
    }

    #[test]
    fn preserves_comments_and_blank_lines() {
        let entry = "# header\ntitle Test\n\noptions quiet\n";
        let params = vec![s("intel_iommu=on")];
        let output = rewrite_systemd_boot_entry(entry, &params).unwrap();
        assert!(output.starts_with("# header\n"));
        assert!(output.contains("\n\noptions quiet intel_iommu=on"));
    }

    #[test]
    fn preserves_leading_whitespace_on_options_line() {
        // Some hand-edited entries indent option lines for readability. We
        // do not strip that.
        let entry = "title Test\n  options quiet\n";
        let params = vec![s("intel_iommu=on")];
        let output = rewrite_systemd_boot_entry(entry, &params).unwrap();
        assert!(output.contains("  options quiet intel_iommu=on"));
    }
}
