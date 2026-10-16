use velk_plugin_sdk::{
    host::{DiagnosticDocument, Severity},
    settings,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub severity: String,
    pub source: String,
    pub text: String,
}
impl Filter {
    pub fn severity_label(&self) -> &str {
        match self.severity.as_str() {
            "errors" => "Errors",
            "warnings_and_errors" => "Errors and warnings",
            _ => "All severities",
        }
    }
    pub fn cycle(&mut self) {
        self.severity = match self.severity.as_str() {
            "errors" => "all",
            "warnings_and_errors" => "errors",
            _ => "warnings_and_errors",
        }
        .into();
    }
    pub fn apply(&self, documents: &mut Vec<DiagnosticDocument>, errors_only: bool) {
        let source = self.source.to_lowercase();
        let query = self.text.to_lowercase();
        for document in documents.iter_mut() {
            let path_matches = document.title.to_lowercase().contains(&query);
            document.diagnostics.retain(|d| {
                let severity = if errors_only {
                    d.severity == Severity::Error
                } else {
                    match self.severity.as_str() {
                        "errors" => d.severity == Severity::Error,
                        "warnings_and_errors" => {
                            matches!(d.severity, Severity::Error | Severity::Warning)
                        }
                        _ => true,
                    }
                };
                severity
                    && (source.is_empty()
                        || d.source
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&source))
                    && (query.is_empty()
                        || path_matches
                        || d.message.to_lowercase().contains(&query)
                        || d.code
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&query))
            });
        }
        documents.retain(|d| !d.diagnostics.is_empty());
    }
    pub fn definitions() -> Vec<settings::Definition> {
        vec![
            settings::Definition {
                key: "severity".into(),
                title: "Problem severity".into(),
                description: "Default severity filter for Problems and diagnostic navigation."
                    .into(),
                kind: settings::Kind::Choice {
                    options: vec!["all".into(), "warnings_and_errors".into(), "errors".into()],
                },
                default: "all".into(),
            },
            settings::Definition {
                key: "source".into(),
                title: "Problem source".into(),
                description: "Case-insensitive source substring; empty includes every source."
                    .into(),
                kind: settings::Kind::Text { max_length: 128 },
                default: "".into(),
            },
            settings::Definition {
                key: "text".into(),
                title: "Problem text".into(),
                description:
                    "Case-insensitive match in the file path, diagnostic message, or code.".into(),
                kind: settings::Kind::Text { max_length: 256 },
                default: "".into(),
            },
        ]
    }
}
