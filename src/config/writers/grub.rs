//! Pure GRUB2 config writer (slice 6.2).
//!
//! Edits `/etc/default/grub` to ensure each requested kernel parameter
//! appears in `GRUB_CMDLINE_LINUX_DEFAULT`. The function is pure: it takes
//! the existing file contents and returns the new contents. The companion
//! `run_grub_mkconfig` wrapper is provided for callers that want to
//! regenerate `grub.cfg` after writing.
//!
//! Idempotence is a hard requirement. Re-running with the same params on a
//! file that already has them must be a no-op (byte-for-byte equality with
//! the input).
//!
//! Tests live in this file and run on any platform.

use super::WriterError;

/// Variable that holds the kernel cmdline used for normal boots. We do not
/// touch `GRUB_CMDLINE_LINUX` (which is appended to *every* entry,
/// including recovery/single-user); editing only `_DEFAULT` keeps the
/// rescue/recovery path free of VFIO parameters.
const TARGET_VAR: &str = "GRUB_CMDLINE_LINUX_DEFAULT";

/// Rewrite `/etc/default/grub` so `GRUB_CMDLINE_LINUX_DEFAULT` contains
/// every parameter in `params`. Existing parameters are preserved; missing
/// ones are appended after the existing values, separated by a single
/// space. The variable's quoting style (`"…"` or `'…'`) is preserved if
/// present.
///
/// If `GRUB_CMDLINE_LINUX_DEFAULT` is missing entirely, the writer appends
/// a new line `GRUB_CMDLINE_LINUX_DEFAULT="<params>"` at the end of the
/// file, after a single trailing newline if needed. This case is rare in
/// real-world Arch / Debian / Fedora templates but defensible.
///
/// The writer is conservative: it does not touch `GRUB_CMDLINE_LINUX`,
/// `GRUB_TIMEOUT`, or anything else.
pub fn rewrite_grub_default(input: &str, params: &[String]) -> Result<String, WriterError> {
    if params.is_empty() {
        return Err(WriterError::EmptyParams);
    }

    let mut found = false;
    let mut output_lines: Vec<String> = Vec::with_capacity(input.lines().count() + 1);

    for (idx, line) in input.lines().enumerate() {
        if let Some((key, _)) = split_assignment(line) {
            if key == TARGET_VAR {
                if found {
                    return Err(WriterError::MalformedInput {
                        line: idx + 1,
                        detail: format!("duplicate {TARGET_VAR} assignment"),
                    });
                }
                found = true;
                let updated = ensure_params_in_line(line, params).map_err(|detail| {
                    WriterError::MalformedInput {
                        line: idx + 1,
                        detail,
                    }
                })?;
                output_lines.push(updated);
                continue;
            }
        }
        output_lines.push(line.to_string());
    }

    if !found {
        let appended = format!("{}=\"{}\"", TARGET_VAR, params.join(" "));
        output_lines.push(appended);
    }

    let mut output = output_lines.join("\n");
    if input.ends_with('\n') && !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// Splits a `KEY=VALUE` line into `(key, value)`, returning `None` for
/// blank lines, comments, and lines without `=`.
fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    let value = &line[eq + 1..];
    Some((key, value))
}

/// Given a `KEY="value"` (or `KEY='value'`, or unquoted) line, return the
/// same line with each missing parameter appended to the value. Preserves
/// the original quoting style and any in-line comment after the value.
fn ensure_params_in_line(line: &str, params: &[String]) -> Result<String, String> {
    // Pull the assignment itself out of the line (e.g. strip trailing
    // comments). GRUB doesn't officially support inline comments on the
    // same line, but we preserve anything after the closing quote in case
    // a host has them.
    let eq = line
        .find('=')
        .ok_or_else(|| "no '=' in target line".to_string())?;
    let key = &line[..eq];
    let value_with_tail = &line[eq + 1..];

    let (existing_quote, existing_value, tail) = parse_quoted_value(value_with_tail)?;

    let mut tokens: Vec<String> = if existing_value.is_empty() {
        Vec::new()
    } else {
        existing_value
            .split_whitespace()
            .map(String::from)
            .collect()
    };
    let mut changed = false;
    for param in params {
        if !tokens.iter().any(|t| t == param) {
            tokens.push(param.clone());
            changed = true;
        }
    }

    let _ = changed; // We always return a (possibly identical) line.
    let new_value = tokens.join(" ");
    let formatted = match existing_quote {
        Some('"') => format!("\"{new_value}\""),
        Some('\'') => format!("'{new_value}'"),
        _ => new_value,
    };
    Ok(format!("{key}={formatted}{tail}"))
}

/// Returns `(quote_char, inner_value, trailing_text)` from a value
/// expression. `quote_char` is `Some('"')`, `Some('\'')`, or `None`.
fn parse_quoted_value(value_with_tail: &str) -> Result<(Option<char>, &str, &str), String> {
    let bytes = value_with_tail.as_bytes();
    if bytes.is_empty() {
        return Ok((None, "", ""));
    }

    let first = bytes[0] as char;
    if first == '"' || first == '\'' {
        // Find the matching closing quote.
        let close_rel = value_with_tail[1..]
            .find(first)
            .ok_or_else(|| format!("unterminated {first} quote"))?;
        let inner = &value_with_tail[1..1 + close_rel];
        let tail = &value_with_tail[1 + close_rel + 1..];
        Ok((Some(first), inner, tail))
    } else {
        // Unquoted value: consume everything to end of line.
        Ok((None, value_with_tail, ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(input: &str) -> String {
        input.to_string()
    }

    #[test]
    fn appends_params_to_existing_quoted_value() {
        let input = "GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\nGRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash\"\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(
            output.contains("GRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash intel_iommu=on iommu=pt\"")
        );
        // Other lines untouched.
        assert!(output.contains("GRUB_DEFAULT=0"));
        assert!(output.contains("GRUB_TIMEOUT=5"));
        // Trailing newline preserved.
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn idempotent_when_all_params_already_present() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash intel_iommu=on iommu=pt\"\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert_eq!(output, input, "idempotent on already-applied input");
    }

    #[test]
    fn appends_only_missing_params() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet intel_iommu=on\"\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("GRUB_CMDLINE_LINUX_DEFAULT=\"quiet intel_iommu=on iommu=pt\""));
    }

    #[test]
    fn preserves_single_quotes() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT='quiet splash'\n";
        let params = vec![s("amd_iommu=on")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("GRUB_CMDLINE_LINUX_DEFAULT='quiet splash amd_iommu=on'"));
    }

    #[test]
    fn handles_unquoted_value() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=quiet\n";
        let params = vec![s("intel_iommu=on")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("GRUB_CMDLINE_LINUX_DEFAULT=quiet intel_iommu=on"));
    }

    #[test]
    fn appends_target_var_when_missing() {
        let input = "GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\n";
        let params = vec![s("intel_iommu=on"), s("iommu=pt")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("GRUB_CMDLINE_LINUX_DEFAULT=\"intel_iommu=on iommu=pt\""));
        assert!(output.starts_with("GRUB_DEFAULT=0"));
    }

    #[test]
    fn rejects_empty_param_list() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet\"\n";
        let err = rewrite_grub_default(input, &[]).unwrap_err();
        matches!(err, WriterError::EmptyParams);
    }

    #[test]
    fn rejects_unterminated_quote() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash\n";
        let params = vec![s("intel_iommu=on")];
        let err = rewrite_grub_default(input, &params).unwrap_err();
        match err {
            WriterError::MalformedInput { detail, .. } => {
                assert!(detail.contains("unterminated"));
            }
            other => panic!("expected MalformedInput, got {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_target_var_assignments() {
        let input = "GRUB_CMDLINE_LINUX_DEFAULT=\"a\"\nGRUB_CMDLINE_LINUX_DEFAULT=\"b\"\n";
        let params = vec![s("x")];
        let err = rewrite_grub_default(input, &params).unwrap_err();
        match err {
            WriterError::MalformedInput { line, .. } => assert_eq!(line, 2),
            other => panic!("expected MalformedInput, got {other:?}"),
        }
    }

    #[test]
    fn does_not_touch_grub_cmdline_linux_general() {
        let input = "GRUB_CMDLINE_LINUX=\"audit=1\"\nGRUB_CMDLINE_LINUX_DEFAULT=\"quiet\"\n";
        let params = vec![s("intel_iommu=on")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("GRUB_CMDLINE_LINUX=\"audit=1\""));
        assert!(output.contains("GRUB_CMDLINE_LINUX_DEFAULT=\"quiet intel_iommu=on\""));
    }

    #[test]
    fn preserves_blank_lines_and_comments() {
        let input =
            "# top comment\nGRUB_DEFAULT=0\n\nGRUB_CMDLINE_LINUX_DEFAULT=\"quiet\"\n# trailing\n";
        let params = vec![s("intel_iommu=on")];
        let output = rewrite_grub_default(input, &params).unwrap();
        assert!(output.contains("# top comment"));
        assert!(output.contains("# trailing"));
        // Empty line preserved.
        assert!(output.contains("GRUB_DEFAULT=0\n\nGRUB_CMDLINE_LINUX_DEFAULT="));
    }
}
