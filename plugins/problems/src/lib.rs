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
                    document.diagnostics.sort_by_key(|d| (d.start, d.end));
                }
                Ok((documents, truncated))
            }
            _ => Err("invalid diagnostics response".into()),
        }
    }
    fn snapshot(&self, effects: Vec<Effect>) -> Snapshot {
        let mut nodes = Vec::new();
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
                content: if nodes.is_empty() {
                    Content::Text("No current workspace diagnostics.".into())
                } else {
                    Content::Tree(nodes)
                },
            }],
        }
    }
    fn panel(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Changes => {
                let (documents, truncated) = self.read()?;
                Ok(
                    if documents == self.documents && truncated == self.truncated {
                        Reply::Unchanged
                    } else {
                        Reply::Invalidated
                    },
                )
            }
            Request::Describe { .. } => {
                let (documents, truncated) = self.read()?;
                if self.revision == 0 || documents != self.documents || truncated != self.truncated
                {
                    self.revision += 1;
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
                if panel != "problems" || revision != self.revision || action != "reveal" {
                    return Err("stale or invalid diagnostic action".into());
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
    fn navigate(&self, context: commands::Context, backwards: bool) -> Result<Outcome, String> {
        let (documents, _) = self.read()?;
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
            "Read current diagnostics from open documents.",
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
    fn commands() -> Vec<commands::Declaration> {
        [
            ("show", "Show Problems", None),
            ("next", "Next diagnostic", Some(false)),
            ("previous", "Previous diagnostic", Some(true)),
        ]
        .into_iter()
        .map(|(id, title, shift)| commands::Declaration {
            id: format!("velk.problems.{id}"),
            title: format!("Diagnostics: {title}"),
            language: None,
            input: None,
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
                        Some(backwards) => this.navigate(context, backwards)?,
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
        assert!(matches!(&empty.panels[0].content, Content::Text(_)));
    }
}
