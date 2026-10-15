//! A panel API demo. No disk access, watching, or file opening.
use velk_panels::{Content, Node, Panel, Request, Slot, Snapshot};
use velk_plugin_sdk::{Plugin, Registrar, hooks};
#[derive(Default)]
pub struct MockFiles;
impl Plugin for MockFiles {
    const ID: &'static str = "velk.demo.files";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn describe(&mut self) -> String {
        "Mock Files panel · sample data only".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(hooks::Request(velk_panels::HOOK), |_, bytes| {
            let _scope = velk_plugin_sdk::diagnostics::scope!("mock_files.snapshot");
            match velk_panels::decode::<Request>(bytes)? {
                Request::Describe => velk_panels::encode(&snapshot()),
                Request::Invoke { .. } => Err("The mock tree has no file actions".into()),
            }
        })
    }
}
pub fn snapshot() -> Snapshot {
    let nodes = [
        ("project", "velk-demo", 0, true, true),
        ("src", "src", 1, true, true),
        ("main", "main.rs", 2, false, false),
        ("editor", "editor", 2, true, true),
        ("buffer", "buffer.rs", 3, false, false),
        ("view", "view.rs", 3, false, false),
        ("plugins", "plugins", 1, true, false),
        ("mock", "mock-files.rs", 2, false, false),
        ("assets", "assets", 1, true, false),
        ("theme", "theme.json", 2, false, false),
        ("manifest", "Cargo.toml", 1, false, false),
        ("readme", "README.md", 1, false, false),
    ]
    .into_iter()
    .map(|(id, label, depth, branch, expanded)| Node {
        id: id.into(),
        label: label.into(),
        depth,
        branch,
        expanded,
        action: None,
    })
    .collect();
    Snapshot {
        panels: vec![Panel {
            id: "files".into(),
            title: "Files (mock)".into(),
            slot: Slot::Left,
            revision: 1,
            content: Content::Tree(nodes),
        }],
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn sample_is_valid_and_has_no_actions() {
        let s = super::snapshot();
        s.validate().unwrap();
        let velk_panels::Content::Tree(nodes) = &s.panels[0].content else {
            panic!()
        };
        assert!(nodes.iter().all(|n| n.action.is_none()));
    }
}
