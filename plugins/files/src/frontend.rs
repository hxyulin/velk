use std::sync::atomic::{AtomicBool, Ordering};
use velk_files_contracts as core;
use velk_panels::{Content, Effect, Node, Panel, Reply, Request, Slot, Snapshot};
use velk_plugin_sdk::{Host, Plugin, Registrar, backend::Tasks, hooks};
#[derive(Default)]
pub struct Files {
    tasks: Tasks<core::Request, Snapshot>,
    next: u64,
}
fn call(host: &Host, request: &core::Request) -> Result<core::Reply, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    serde_json::from_slice(&host.call_plugin(core::PROVIDER, core::HOOK, bytes)?)
        .map_err(|e| e.to_string())
}
pub(super) fn snapshot(tree: core::Tree) -> Snapshot {
    let mut rows: Vec<_> = tree
        .entries
        .into_iter()
        .map(|entry| Node {
            id: entry.id,
            label: entry.label,
            depth: entry.depth,
            branch: entry.directory,
            expanded: entry.expanded,
            lazy: entry.lazy,
            action: (entry.path.is_some() && !entry.directory).then(|| "open".into()),
        })
        .collect();
    let content = if rows.is_empty() {
        Content::Text(tree.notice.unwrap_or_default())
    } else {
        if let Some(notice) = tree.notice {
            rows.push(Node {
                id: "backend-notice".into(),
                label: notice,
                depth: 0,
                branch: false,
                expanded: false,
                lazy: false,
                action: None,
            });
        }
        Content::Tree(rows)
    };
    Snapshot {
        watch: true,
        stream: false,
        panels: vec![Panel {
            id: "files".into(),
            title: "Files".into(),
            slot: Slot::Left,
            revision: tree.revision,
            content,
        }],
        effects: tree
            .open
            .into_iter()
            .map(|path| Effect::OpenFile { path })
            .collect(),
    }
}
fn run(host: &Host, request: core::Request, cancelled: &AtomicBool) -> Result<Snapshot, String> {
    let mut reply = call(host, &request)?;
    loop {
        match reply {
            core::Reply::Ready { tree } => {
                tree.validate()?;
                let snapshot = snapshot(tree);
                snapshot.validate()?;
                return Ok(snapshot);
            }
            core::Reply::Pending { id } => {
                if cancelled.load(Ordering::Acquire) {
                    let _ = call(host, &core::Request::Cancel { id });
                    return Err("file tree query cancelled".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
                reply = call(host, &core::Request::Poll { id })?;
            }
            _ => return Err("unexpected file tree reply".into()),
        }
    }
}
impl Plugin for Files {
    const ID: &'static str = "velk.files";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Files";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] =
        &[velk_plugin_sdk::CapabilityDeclaration::required(
            "plugin.call",
            "Read the shared lazy file tree backend.",
        )];
    fn describe(&mut self) -> String {
        "Project file tree panel".into()
    }
    fn attach_host(&mut self, host: Host) {
        self.tasks.start(host, run, |host| {
            matches!(
                call(host, &core::Request::Changes),
                Ok(core::Reply::Invalidated)
            )
        });
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(velk_panels::HOOK),
            |this: &mut Self, bytes| {
                let reply = this.request(velk_panels::decode(bytes)?)?;
                velk_panels::encode(&reply)
            },
        )
    }
}
impl Files {
    fn request(&mut self, request: Request) -> Result<Reply, String> {
        let input = match request {
            Request::Changes => {
                return Ok(if self.tasks.changed() {
                    Reply::Invalidated
                } else {
                    Reply::Unchanged
                });
            }
            Request::Poll { id } => {
                return Ok(self
                    .tasks
                    .poll(id)?
                    .map_or(Reply::Pending { id }, |snapshot| Reply::Ready { snapshot }));
            }
            Request::Cancel { id } => {
                self.tasks.cancel(id);
                return Ok(Reply::Cancelled);
            }
            Request::Describe { context } => core::Request::Describe {
                context: core::Context {
                    root: context.root,
                    document: context.document,
                    initialized: context.initialized,
                    expanded: context.expanded,
                },
            },
            Request::Expand {
                panel,
                revision,
                node,
                expanded,
            } if panel == "files" => core::Request::Expand {
                revision,
                node,
                expanded,
            },
            Request::Invoke {
                panel,
                revision,
                node,
                action,
            } if panel == "files" && action == "open" => core::Request::Resolve { revision, node },
            _ => return Err("unsupported file tree action".into()),
        };
        self.next = self
            .next
            .checked_add(1)
            .ok_or("file tree request counter exhausted")?;
        self.tasks.begin(self.next, input)?;
        Ok(Reply::Pending { id: self.next })
    }
}
