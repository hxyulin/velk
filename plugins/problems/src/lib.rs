mod filter;
use filter::Filter;
use velk_panels::{Content, Effect, Node, Panel, Reply, Request, Slot, Snapshot};
use velk_plugin_sdk::{
    CapabilityDeclaration, Host, Plugin, Registrar,
    commands::{self, DiagnosticLocation, DiagnosticTarget, Outcome},
    hooks,
    host::{self, DiagnosticDocument, Severity},
};

#[derive(Default)]
pub struct Problems {
    host: Option<Host>,
    filter: Filter,
    described_filter: Filter,
    documents: Vec<DiagnosticDocument>,
    truncated: bool,
    revision: u64,
}
fn display(text: &str, limit: usize) -> String {
    let mut result = String::new();
    for c in text.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if result.len() + c.len_utf8() > limit {
            break;
        }
        result.push(c);
    }
    if result.trim().is_empty() {
        "Diagnostic".into()
    } else {
        result
    }
}
fn document_key(document: &DiagnosticDocument) -> String {
    match &document.target {
        DiagnosticTarget::Buffer { id, .. } => format!("buffer-{id}"),
        DiagnosticTarget::File { path, .. } => format!(
            "file-{}",
            commands::text_hash(path.as_os_str().as_encoded_bytes().iter().copied())
        ),
    }
}
impl Problems {
    fn read(&self) -> Result<(Vec<DiagnosticDocument>, bool), String> {
        self.read_filtered(false)
    }
    fn read_filtered(&self, errors_only: bool) -> Result<(Vec<DiagnosticDocument>, bool), String> {
        match self
            .host
            .as_ref()
            .ok_or("host services unavailable")?
            .request(host::Request::Diagnostics)?
        {
            host::Response::Diagnostics {
                mut documents,
                truncated,
            } => {
                let mut merged: Vec<DiagnosticDocument> = Vec::new();
                for document in documents.drain(..) {
                    if let Some(existing) = merged.iter_mut().find(|d| d.target == document.target)
                    {
                        existing.diagnostics.extend(document.diagnostics);
                    } else {
                        merged.push(document);
                    }
                }
                documents = merged;
                documents.sort_by(|a, b| a.title.cmp(&b.title));
                for document in &mut documents {
                    let mut seen = std::collections::HashSet::new();
                    document
                        .diagnostics
                        .retain(|item| seen.insert(serde_json::to_vec(item).unwrap_or_default()));
                    document.diagnostics.sort_by_key(|d| (d.start, d.end));
                }
                self.filter.apply(&mut documents, errors_only);
                Ok((documents, truncated))
            }
            _ => Err("invalid diagnostics response".into()),
        }
    }
    fn snapshot(&self, effects: Vec<Effect>) -> Snapshot {
        let mut nodes = vec![Node {
            id: "filter-severity".into(),
            label: format!("Filter: {}", self.filter.severity_label()),
            depth: 0,
            branch: false,
            expanded: false,
            lazy: false,
            action: Some("cycle-severity".into()),
        }];
        if !self.filter.source.is_empty() || !self.filter.text.is_empty() {
            nodes.push(Node {
                id: "filter-reset".into(),
                label: display(
                    &format!(
                        "Clear filters: source={} text={}",
                        self.filter.source, self.filter.text
                    ),
                    256,
                ),
                depth: 0,
                branch: false,
                expanded: false,
                lazy: false,
                action: Some("reset-filters".into()),
            });
        }
        let count: usize = self.documents.iter().map(|d| d.diagnostics.len()).sum();
        for document in &self.documents {
            let key = document_key(document);
            nodes.push(Node {
                id: key.clone(),
                label: display(
                    &format!("{} ({})", document.title, document.diagnostics.len()),
                    256,
                ),
                depth: 0,
                branch: true,
                expanded: true,
                lazy: false,
                action: None,
            });
            for (index, diagnostic) in document.diagnostics.iter().enumerate() {
                let severity = match diagnostic.severity {
                    Severity::Error => "Error",
                    Severity::Warning => "Warning",
                    Severity::Information => "Info",
                    Severity::Hint => "Hint",
                };
                let source = [diagnostic.source.as_deref(), diagnostic.code.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(":");
                nodes.push(Node {
                    id: format!("{key}-{index}"),
                    label: display(
                        &format!(
                            "{severity} {}:{} {}{}",
                            diagnostic.line + 1,
                            diagnostic.column + 1,
                            if source.is_empty() {
                                String::new()
                            } else {
                                format!("[{source}] ")
                            },
                            diagnostic.message
                        ),
                        256,
                    ),
                    depth: 1,
                    branch: false,
                    expanded: false,
                    lazy: false,
                    action: Some("reveal".into()),
                });
            }
        }
        Snapshot {
            watch: true,
            stream: false,
            effects,
            panels: vec![Panel {
                id: "problems".into(),
                title: format!(
                    "Problems ({count}{})",
                    if self.truncated { "+" } else { "" }
                ),
                slot: Slot::Bottom,
                revision: self.revision,
                content: Content::Tree({
                    if count == 0 {
                        nodes.push(Node {
                            id: "no-problems".into(),
                            label: "No current diagnostics match these filters.".into(),
                            depth: 0,
                            branch: false,
                            expanded: false,
                            lazy: false,
                            action: None,
                        });
                    }
                    nodes
                }),
            }],
        }
    }
    fn panel(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Changes => {
                let (documents, truncated) = self.read()?;
                Ok(
                    if documents == self.documents
                        && truncated == self.truncated
                        && self.filter == self.described_filter
                    {
                        Reply::Unchanged
                    } else {
                        Reply::Invalidated
                    },
                )
            }
            Request::Describe { .. } => {
                let (documents, truncated) = self.read()?;
                if self.revision == 0
                    || documents != self.documents
                    || truncated != self.truncated
                    || self.filter != self.described_filter
                {
                    self.revision += 1;
                    self.described_filter = self.filter.clone();
                    self.documents = documents;
                    self.truncated = truncated;
                }
                Ok(Reply::Ready {
                    snapshot: self.snapshot(vec![]),
                })
            }
            Request::Invoke {
                panel,
                revision,
                node,
                action,
            } => {
                if panel != "problems"
                    || revision != self.revision
                    || self.filter != self.described_filter
                {
                    return Err("stale or invalid diagnostic action".into());
                }
                if node == "filter-severity" && action == "cycle-severity"
                    || node == "filter-reset" && action == "reset-filters"
                {
                    if action == "cycle-severity" {
                        self.filter.cycle();
                    } else {
                        self.filter = Filter::default();
                    }
                    return self.panel(Request::Describe {
                        context: Default::default(),
                    });
                }
                if action != "reveal" {
                    return Err("stale or invalid diagnostic action".into());
                }
                if self.host.is_some() {
                    let (documents, truncated) = self.read()?;
                    if documents != self.documents || truncated != self.truncated {
                        return Err("Diagnostic results changed; refresh and try again".into());
                    }
                }
                let location = self
                    .documents
                    .iter()
                    .find_map(|document| {
                        document
                            .diagnostics
                            .iter()
                            .enumerate()
                            .find(|(i, _)| node == format!("{}-{i}", document_key(document)))
                            .map(|(_, d)| DiagnosticLocation {
                                target: document.target.clone(),
                                position: d.start,
                            })
                    })
                    .ok_or("unknown diagnostic row")?;
                Ok(Reply::Ready {
                    snapshot: self.snapshot(vec![Effect::RevealDiagnostic { location }]),
                })
            }
            Request::Cancel { .. } => Ok(Reply::Cancelled),
            _ => Err("unsupported diagnostics panel request".into()),
        }
    }
    fn navigate(
        &self,
        context: commands::Context,
        backwards: bool,
        errors_only: bool,
    ) -> Result<Outcome, String> {
        let (documents, _) = self.read_filtered(errors_only)?;
        let buffers = match self
            .host
            .as_ref()
            .ok_or("host services unavailable")?
            .request(host::Request::Buffers)?
        {
            host::Response::Buffers { buffers } => buffers,
            _ => return Err("invalid buffer response".into()),
        };
        let active = buffers.iter().find(|b| b.active);
        Ok(navigate(
            &documents,
            active.map(|b| b.id),
            context.position.unwrap_or(0),
            backwards,
        ))
    }
}
fn navigate(
    documents: &[DiagnosticDocument],
    active: Option<u64>,
    position: usize,
    backwards: bool,
) -> Outcome {
    let locations: Vec<_> = documents
        .iter()
        .enumerate()
        .flat_map(|(index, document)| {
            document.diagnostics.iter().map(move |d| {
                (
                    index,
                    DiagnosticLocation {
                        target: document.target.clone(),
                        position: d.start,
                    },
                )
            })
        })
        .collect();
    let current = documents.iter().position(
        |d| matches!(d.target, DiagnosticTarget::Buffer { id, .. } if Some(id) == active),
    );
    let location = if backwards {
        locations
            .iter()
            .rev()
            .find(|(index, l)| {
                current.is_some_and(|c| *index < c || *index == c && l.position < position)
            })
            .or_else(|| locations.last())
    } else {
        locations
            .iter()
            .find(|(index, l)| {
                current.is_some_and(|c| *index > c || *index == c && l.position > position)
            })
            .or_else(|| locations.first())
    };
    match location {
        Some((_, location)) => Outcome::RevealDiagnostic {
            location: location.clone(),
        },
        None => Outcome::Message {
            text: "No current workspace diagnostics".into(),
        },
    }
}
impl Plugin for Problems {
    const ID: &'static str = "velk.problems";
    const NAME: &'static str = "Diagnostics";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.diagnostics.read",
            "Read current workspace diagnostics.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Locate the active buffer for diagnostic navigation.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.manage",
            "Reveal diagnostic locations through normal buffer navigation.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Problems panel and diagnostic navigation across the workspace.".into()
    }
    fn attach_host(&mut self, host: Host) {
        self.host = Some(host);
    }
    fn settings() -> Vec<velk_plugin_sdk::settings::Definition> {
        Filter::definitions()
    }
    fn configure(&mut self, values: &velk_plugin_sdk::settings::Values) -> Result<(), String> {
        self.filter = Filter {
            severity: values
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("all")
                .into(),
            source: values
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            text: values
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        };
        Ok(())
    }
    fn commands() -> Vec<commands::Declaration> {
        [
            ("show", "Show Problems", None),
            ("next", "Next diagnostic", Some(false)),
            ("previous", "Previous diagnostic", Some(true)),
            ("next-error", "Next error", None),
            ("previous-error", "Previous error", None),
            ("filter", "Filter problems by text", None),
            ("filter-source", "Filter problems by source", None),
            ("cycle-severity", "Cycle problem severity", None),
            ("reset-filters", "Reset problem filters", None),
        ]
        .into_iter()
        .map(|(id, title, shift)| commands::Declaration {
            id: format!("velk.problems.{id}"),
            title: format!("Diagnostics: {title}"),
            language: None,
            input: match id {
                "filter" => Some("Match file path, diagnostic message, or code".into()),
                "filter-source" => Some("Match diagnostic source".into()),
                _ => None,
            },
            shortcut: shift.map(|shift| commands::Shortcut {
                key: "F8".into(),
                shift,
                ..Default::default()
            }),
            handler: commands::Handler::Request {
                hook: format!("velk.problems.{id}"),
            },
        })
        .collect()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        for id in [
            "next-error",
            "previous-error",
            "filter",
            "filter-source",
            "cycle-severity",
            "reset-filters",
        ] {
            let hook: &'static str = match id {
                "next-error" => "velk.problems.next-error",
                "previous-error" => "velk.problems.previous-error",
                "filter" => "velk.problems.filter",
                "filter-source" => "velk.problems.filter-source",
                "cycle-severity" => "velk.problems.cycle-severity",
                _ => "velk.problems.reset-filters",
            };
            reg.provide(
                hooks::Request(hook),
                move |this: &mut Self, bytes: &[u8]| {
                    let context: commands::Context =
                        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                    let outcome = match id {
                        "next-error" | "previous-error" => {
                            this.navigate(context, id == "previous-error", true)?
                        }
                        _ => {
                            match id {
                                "filter" => this.filter.text = context.input.unwrap_or_default(),
                                "filter-source" => {
                                    this.filter.source = context.input.unwrap_or_default()
                                }
                                "cycle-severity" => this.filter.cycle(),
                                _ => this.filter = Filter::default(),
                            }
                            Outcome::ShowPanel {
                                panel: "problems".into(),
                            }
                        }
                    };
                    serde_json::to_vec(&outcome).map_err(|e| e.to_string())
                },
            )?;
        }
        reg.provide(
            hooks::Request(velk_panels::HOOK),
            |this: &mut Self, bytes: &[u8]| {
                velk_panels::encode(&this.panel(velk_panels::decode(bytes)?)?)
            },
        )?;
        for (hook, direction) in [
            ("velk.problems.show", None),
            ("velk.problems.next", Some(false)),
            ("velk.problems.previous", Some(true)),
        ] {
            reg.provide(
                hooks::Request(hook),
                move |this: &mut Self, bytes: &[u8]| {
                    let context = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                    let outcome = match direction {
                        Some(backwards) => this.navigate(context, backwards, false)?,
                        None => Outcome::ShowPanel {
                            panel: "problems".into(),
                        },
                    };
                    serde_json::to_vec(&outcome).map_err(|e| e.to_string())
                },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document(id: u64, positions: &[usize]) -> DiagnosticDocument {
        DiagnosticDocument {
            target: DiagnosticTarget::Buffer { id, revision: 7 },
            title: format!("file-{id}"),
            diagnostics: positions
                .iter()
                .map(|&start| host::Diagnostic {
                    start,
                    end: start + 1,
                    line: 0,
                    column: start,
                    severity: Severity::Warning,
                    message: "warning".into(),
                    source: Some("test".into()),
                    code: Some("W1".into()),
                })
                .collect(),
        }
    }
    fn target(outcome: Outcome) -> (u64, usize) {
        let Outcome::RevealDiagnostic { location } = outcome else {
            panic!("expected navigation")
        };
        let DiagnosticTarget::Buffer { id, revision } = location.target else {
            panic!()
        };
        assert_eq!(revision, 7);
        (id, location.position)
    }
    #[test]
    fn severity_source_and_text_filters_compose_for_panel_and_navigation() {
        let mut first = document(1, &[2, 6]);
        first.title = "src/世界.rs".into();
        first.diagnostics[0].severity = Severity::Error;
        first.diagnostics[0].message = "missing Symbol".into();
        first.diagnostics[0].source = Some("RustC".into());
        first.diagnostics[0].code = Some("E0425".into());
        let mut second = document(2, &[4]);
        second.diagnostics[0].severity = Severity::Hint;
        let all = vec![first, second];
        for text in ["symbol", "世界", "e0425"] {
            let filter = Filter {
                severity: "errors".into(),
                source: "rustc".into(),
                text: text.into(),
            };
            let mut documents = all.clone();
            filter.apply(&mut documents, false);
            assert_eq!(documents.len(), 1);
            assert_eq!(documents[0].diagnostics.len(), 1);
            assert_eq!(target(navigate(&documents, Some(1), 0, false)), (1, 2));
        }
        let mut documents = all.clone();
        Filter::default().apply(&mut documents, true);
        assert_eq!(documents[0].diagnostics.len(), 1);
        let mut documents = all;
        Filter {
            source: "absent".into(),
            ..Default::default()
        }
        .apply(&mut documents, false);
        assert!(documents.is_empty());
        let mut filter = Filter::default();
        filter.cycle();
        assert_eq!(filter.severity, "warnings_and_errors");
        filter.cycle();
        assert_eq!(filter.severity, "errors");
        filter.cycle();
        assert_eq!(filter.severity, "all");
        velk_plugin_sdk::settings::validate_schema(&Problems::settings()).unwrap();
    }
    #[test]
    fn navigation_skips_duplicate_positions_crosses_buffers_and_wraps() {
        let documents = vec![document(1, &[2, 2, 6]), document(2, &[0, 4])];
        assert_eq!(target(navigate(&documents, Some(1), 2, false)), (1, 6));
        assert_eq!(target(navigate(&documents, Some(1), 6, false)), (2, 0));
        assert_eq!(target(navigate(&documents, Some(2), 4, false)), (1, 2));
        assert_eq!(target(navigate(&documents, Some(2), 0, true)), (1, 6));
        assert_eq!(target(navigate(&documents, Some(1), 2, true)), (2, 4));
        assert_eq!(target(navigate(&documents, None, 50, false)), (1, 2));
        assert_eq!(target(navigate(&documents, None, 50, true)), (2, 4));
        assert!(matches!(
            navigate(&[], None, 0, false),
            Outcome::Message { .. }
        ));
    }
    #[test]
    fn rows_are_bounded_and_actions_validate_identity_and_revision() {
        let mut document = document(8, &[1]);
        document.title = "界".repeat(300);
        document.diagnostics[0].message = "\n界".repeat(1000);
        let mut plugin = Problems {
            documents: vec![document],
            revision: 3,
            truncated: true,
            ..Default::default()
        };
        let snapshot = plugin.snapshot(vec![]);
        snapshot.validate().unwrap();
        let bytes = velk_panels::encode(&Reply::Ready { snapshot }).unwrap();
        let _: Reply = velk_panels::decode(&bytes).unwrap();
        let request = |revision, node: &str| Request::Invoke {
            panel: "problems".into(),
            revision,
            node: node.into(),
            action: "reveal".into(),
        };
        assert!(plugin.panel(request(2, "buffer-8-0")).is_err());
        assert!(plugin.panel(request(3, "unknown")).is_err());
        let Reply::Ready { snapshot } = plugin.panel(request(3, "buffer-8-0")).unwrap() else {
            panic!()
        };
        assert_eq!(
            snapshot.effects,
            vec![Effect::RevealDiagnostic {
                location: DiagnosticLocation {
                    target: DiagnosticTarget::Buffer { id: 8, revision: 7 },
                    position: 1
                }
            }]
        );
        let empty = Problems::default().snapshot(vec![]);
        assert!(
            matches!(&empty.panels[0].content, Content::Tree(nodes) if nodes.iter().any(|node| node.id == "no-problems"))
        );
    }
}
