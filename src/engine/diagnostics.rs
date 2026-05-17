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
