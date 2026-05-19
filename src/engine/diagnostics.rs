use crate::kb::KnowledgeBase;

#[derive(Debug, Clone)]
pub struct DiagnosticSuggestion {
    pub cause: String,
    pub fix_options: Vec<String>,
}

pub fn diagnose_error(kb: &KnowledgeBase, stderr: &str) -> Option<DiagnosticSuggestion> {
    kb.error_patterns().iter().find_map(|pattern| {
        regex::Regex::new(&pattern.regex)
            .ok()
            .filter(|regex| regex.is_match(stderr))
            .map(|_| DiagnosticSuggestion {
                cause: pattern.cause.clone(),
                fix_options: pattern.fix_options.clone(),
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnose_error_returns_first_matching_pattern_with_full_context() {
        // Sanity: the bundled KB ships a pattern for the vfio
        // permission-denied stderr. Confirm `diagnose_error` returns
        // the matching cause + fix options as a structured
        // `DiagnosticSuggestion`.
        let kb = KnowledgeBase::default();
        let stderr = "vfio: error opening /dev/vfio/15: Permission denied\n";
        let suggestion = diagnose_error(&kb, stderr).expect("matching pattern in bundled KB");
        assert!(suggestion.cause.contains("VFIO group device"));
        assert!(!suggestion.fix_options.is_empty());
        assert!(suggestion.fix_options.iter().any(|fix| fix.contains("kvm")));
    }

    #[test]
    fn diagnose_error_returns_none_when_no_pattern_matches() {
        let kb = KnowledgeBase::default();
        let suggestion = diagnose_error(&kb, "totally unrelated stderr line\n");
        assert!(suggestion.is_none());
    }

    #[test]
    fn diagnose_error_picks_first_match_when_multiple_could_apply() {
        // The bundled list places `vfio-set-iommu-failed` above
        // `vfio-setup-container-failed`. A combined stderr that hits
        // both must report the higher-priority entry first.
        let kb = KnowledgeBase::default();
        let stderr = "vfio: failed to set iommu for container: Operation not permitted\n\
                      vfio 0000:01:00.0: failed to setup container for group 17";
        let suggestion = diagnose_error(&kb, stderr).expect("at least one match");
        assert!(
            suggestion.cause.contains("IOMMU is not enabled"),
            "expected the higher-priority IOMMU-not-enabled cause, got {:?}",
            suggestion.cause
        );
    }
}
