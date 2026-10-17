use velk_panels::{Content, Node, Panel, Reply, Request, Slot, Snapshot};
use velk_plugin_sdk::{CapabilityDeclaration, Host, Plugin, Registrar, commands, hooks, host};

fn display(text: &str, bytes: usize) -> String {
    let mut result = String::new();
    for c in text.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if result.len() + c.len_utf8() > bytes {
            break;
        }
        result.push(c);
    }
    result
}
#[derive(Default)]
pub struct UndoTree {
    host: Option<Host>,
    history: Option<(host::Buffer, velk_core_contracts::history::Inspection)>,
    revision: u64,
}
impl UndoTree {
    fn read(
        &self,
    ) -> Result<Option<(host::Buffer, velk_core_contracts::history::Inspection)>, String> {
        let host = self.host.as_ref().ok_or("host services unavailable")?;
        let host::Response::Buffers { buffers } = host.request(host::Request::Buffers)? else {
            return Err("invalid buffer response".into());
        };
        let Some(buffer) = buffers.into_iter().find(|b| b.active) else {
            return Ok(None);
        };
        let host::Response::History { buffer, history } =
            host.request(host::Request::InspectHistory { id: buffer.id })?
        else {
            return Err("invalid history response".into());
        };
        Ok(Some((buffer, history)))
    }
    fn snapshot(&self) -> Snapshot {
        let mut rows = Vec::new();
        let mut title = "Undo tree".to_string();
        if let Some((buffer, history)) = &self.history {
            title = format!("Undo tree: {}", display(&buffer.title, 110));
            if let Some(tree) = &history.tree {
                let mut children = std::collections::BTreeMap::<_, Vec<_>>::new();
                for node in &tree.nodes {
                    children.entry(node.parent).or_default().push(node);
                }
                let mut stack: Vec<_> = children
                    .get(&None)
                    .into_iter()
                    .flatten()
                    .rev()
                    .map(|n| (*n, 0u8))
                    .collect();
                while let Some((node, depth)) = stack.pop() {
                    let branch = children.contains_key(&Some(node.state));
                    let markers = format!(
                        "{}{}",
                        if node.state == history.current {
                            " [current]"
                        } else {
                            ""
                        },
                        if node.state == history.saved {
                            " [saved]"
                        } else {
                            ""
                        }
                    );
                    let label = format!(
                        "{}{} +{} -{} {}",
                        node.state, markers, node.inserted, node.removed, node.summary
                    );
                    rows.push(Node {
                        id: node.state.to_string(),
                        label: display(&label, 256),
                        depth,
                        branch,
                        expanded: true,
                        lazy: false,
                        action: (!branch && node.state != history.current)
                            .then(|| "checkout".into()),
                    });
                    if branch && node.state != history.current {
                        rows.push(Node {
                            id: format!("go-{}", node.state),
                            label: format!("Restore state {}", node.state),
                            depth: (depth + 1).min(32),
                            branch: false,
                            expanded: false,
                            lazy: false,
                            action: Some("checkout".into()),
                        });
                    }
                    if let Some(nodes) = children.get(&Some(node.state)) {
                        stack.extend(nodes.iter().rev().map(|n| (*n, (depth + 1).min(32))));
                    }
                }
            }
        }
        if rows.is_empty() {
            rows.push(Node {
                id: "empty".into(),
                label: if self.history.is_some() {
                    "This undo provider does not expose a history tree."
                } else {
                    "Open a buffer to inspect its undo history."
                }
                .into(),
                depth: 0,
                branch: false,
                expanded: false,
                lazy: false,
                action: None,
            });
        }
        Snapshot {
            watch: true,
            stream: false,
            effects: vec![],
            panels: vec![Panel {
                id: "undo-tree".into(),
                title,
                slot: Slot::Right,
                revision: self.revision,
                content: Content::Tree(rows),
            }],
        }
    }
    fn panel(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Changes => Ok(if self.read()? == self.history {
                Reply::Unchanged
            } else {
                Reply::Invalidated
            }),
            Request::Describe { .. } => {
                let history = self.read()?;
                if self.revision == 0 || history != self.history {
                    self.revision += 1;
                    self.history = history;
                }
                Ok(Reply::Ready {
                    snapshot: self.snapshot(),
                })
            }
            Request::Invoke {
                panel,
                revision,
                node,
                action,
            } => {
                if panel != "undo-tree"
                    || revision != self.revision
                    || action != "checkout"
                    || self.read()? != self.history
                {
                    return Err("History changed; refresh and try again".into());
                }
                let (buffer, history) = self.history.as_ref().ok_or("buffer closed")?;
                let state = node
                    .strip_prefix("go-")
                    .unwrap_or(&node)
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                if !history
                    .tree
                    .as_ref()
                    .is_some_and(|t| t.nodes.iter().any(|n| n.state == state))
                {
                    return Err("unknown history state".into());
                }
                self.host
                    .as_ref()
                    .ok_or("host services unavailable")?
                    .request(host::Request::CheckoutHistory {
                        id: buffer.id,
                        revision: buffer.revision,
                        state,
                    })?;
                self.panel(Request::Describe {
                    context: Default::default(),
                })
            }
            Request::Cancel { .. } => Ok(Reply::Cancelled),
            _ => Err("unsupported undo panel request".into()),
        }
    }
}
impl Plugin for UndoTree {
    const ID: &'static str = "velk.undo.browser";
    const NAME: &'static str = "Undo history browser";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            host::BUFFER_READ,
            "Inspect the active document's retained history.",
        ),
        CapabilityDeclaration::required(
            host::BUFFER_WRITE,
            "Navigate the document to a retained undo state.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Browse and navigate retained undo branches.".into()
    }
    fn attach_host(&mut self, host: Host) {
        self.host = Some(host);
    }
    fn commands() -> Vec<commands::Declaration> {
        vec![commands::Declaration {
            id: "velk.undo.browser.show".into(),
            title: "Undo: Show history tree".into(),
            language: None,
            input: None,
            shortcut: None,
            handler: commands::Handler::Request {
                hook: "velk.undo.browser.show".into(),
            },
        }]
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(velk_panels::HOOK),
            |this: &mut Self, bytes: &[u8]| {
                velk_panels::encode(&this.panel(velk_panels::decode(bytes)?)?)
            },
        )?;
        reg.provide(
            hooks::Request("velk.undo.browser.show"),
            |_: &mut Self, _: &[u8]| {
                serde_json::to_vec(&commands::Outcome::ShowPanel {
                    panel: "undo-tree".into(),
                })
                .map_err(|e| e.to_string())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_core_contracts::history::{Inspection, Node as State, Tree};
    #[test]
    fn renders_branched_history_markers_unicode_and_deep_histories_within_panel_limits() {
        let mut nodes = vec![State {
            state: 0,
            parent: None,
            inserted: 0,
            removed: 0,
            summary: "Root".into(),
        }];
        for state in 1..100 {
            nodes.push(State {
                state,
                parent: Some(state - 1),
                inserted: 2,
                removed: 1,
                summary: "界\n".repeat(48),
            });
        }
        nodes.push(State {
            state: 100,
            parent: Some(0),
            inserted: 1,
            removed: 0,
            summary: "Alternative".into(),
        });
        let plugin = UndoTree {
            history: Some((
                host::Buffer {
                    id: 1,
                    title: "界\n".repeat(80),
                    path: None,
                    revision: 3,
                    dirty: true,
                    active: true,
                    anchor: 0,
                    head: 0,
                },
                Inspection {
                    current: 100,
                    saved: 2,
                    tree: Some(Tree { nodes }),
                },
            )),
            revision: 4,
            ..Default::default()
        };
        let snapshot = plugin.snapshot();
        snapshot.validate().unwrap();
        let Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!()
        };
        assert_eq!(rows.len(), 200);
        assert!(
            rows.iter()
                .find(|n| n.id == "go-2")
                .unwrap()
                .action
                .is_some()
        );
        assert!(
            rows.iter()
                .find(|n| n.id == "2")
                .unwrap()
                .label
                .contains("[saved]")
        );
        assert!(rows.last().unwrap().label.contains("[current]"));
        assert!(rows.last().unwrap().action.is_none());
        assert_eq!(rows.last().unwrap().depth, 1);
        assert!(rows.iter().all(|n| n.depth <= 32));
        let empty = UndoTree::default().snapshot();
        empty.validate().unwrap();
    }
    #[test]
    fn rejects_stale_or_unknown_panel_actions_before_contacting_host() {
        let mut plugin = UndoTree {
            revision: 5,
            ..Default::default()
        };
        for (panel, revision, action) in [
            ("undo-tree", 4, "checkout"),
            ("other", 5, "checkout"),
            ("undo-tree", 5, "other"),
        ] {
            assert!(
                plugin
                    .panel(Request::Invoke {
                        panel: panel.into(),
                        revision,
                        node: "1".into(),
                        action: action.into()
                    })
                    .is_err()
            );
        }
    }
}
