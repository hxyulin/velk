use std::sync::atomic::{AtomicBool, Ordering};
use velk_files_contracts as core;
use velk_panels::{Content, Effect, Node, Panel, Reply, Request, Slot, Snapshot};
use velk_plugin_sdk::{Host, Plugin, Registrar, backend::Tasks, hooks};
#[derive(Default)]
pub struct Files {
    tasks: Tasks<core::Request, Snapshot>,
    next: u64,
    host: Option<Host>,
}
fn call(host: &Host, request: &core::Request) -> Result<core::Reply, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    serde_json::from_slice(&host.call_interface(core::INTERFACE, bytes)?).map_err(|e| e.to_string())
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
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::required(
            "plugin.call",
            "Read the shared lazy file tree backend.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            velk_plugin_sdk::host::PROJECT_WRITE,
            "Create, move, and explicitly delete project files through the host.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Project file tree panel".into()
    }
    fn attach_host(&mut self, host: Host) {
        self.host = Some(host.clone());
        self.tasks.start(host, run, |host| {
            matches!(
                call(host, &core::Request::Changes),
                Ok(core::Reply::Invalidated)
            )
        });
    }
    fn commands() -> Vec<velk_plugin_sdk::commands::Declaration> {
        use velk_plugin_sdk::commands::{Declaration, Handler};
        [
            (
                "create-file",
                "Files: Create file",
                "New project-relative file path",
            ),
            (
                "create-directory",
                "Files: Create directory",
                "New project-relative directory path",
            ),
            (
                "rename",
                "Files: Rename or move current file",
                "New project-relative file path",
            ),
            (
                "delete",
                "Files: Delete file or empty directory",
                "Confirm with: delete <project-relative path> (close affected buffers first)",
            ),
        ]
        .into_iter()
        .map(|(action, title, prompt)| Declaration {
            id: format!("velk.files.{action}"),
            title: title.into(),
            language: None,
            input: Some(prompt.into()),
            shortcut: None,
            handler: Handler::Request {
                hook: format!("velk.files.{action}"),
            },
        })
        .collect()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        for (action, hook) in [
            ("create-file", "velk.files.create-file"),
            ("create-directory", "velk.files.create-directory"),
            ("rename", "velk.files.rename"),
            ("delete", "velk.files.delete"),
        ] {
            reg.provide(
                hooks::Request(hook),
                move |this: &mut Self, bytes: &[u8]| this.operate(action, bytes),
            )?;
        }
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
    fn operate(&self, action: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let context = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let operation = operation(action, context)?;
        self.host
            .as_ref()
            .ok_or("file host unavailable")?
            .request(velk_plugin_sdk::host::Request::FileOperation { operation })?;
        serde_json::to_vec(&velk_plugin_sdk::commands::Outcome::Message {
            text: "File operation queued".into(),
        })
        .map_err(|e| e.to_string())
    }
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

fn operation(
    action: &str,
    context: velk_plugin_sdk::commands::Context,
) -> Result<velk_plugin_sdk::host::FileOperation, String> {
    use velk_plugin_sdk::host::FileOperation;
    let input = context.input.ok_or("file operation needs a path")?;
    let path = input.trim();
    if path.is_empty() {
        return Err("file operation needs a path".into());
    }
    if action == "rename" && std::path::Path::new(path).is_absolute() {
        return Err("enter a project-relative destination".into());
    }
    let operation = match action {
        "create-file" => FileOperation::CreateFile { path: path.into() },
        "create-directory" => FileOperation::CreateDirectory { path: path.into() },
        "rename" => FileOperation::Rename {
            source: context.path.ok_or("open a file before renaming it")?,
            destination: path.into(),
        },
        "delete" => FileOperation::Delete {
            path: path
                .strip_prefix("delete ")
                .filter(|p| !p.trim().is_empty())
                .ok_or("confirm deletion by typing: delete <project-relative path>")?
                .trim()
                .into(),
        },
        _ => return Err("unknown file action".into()),
    };
    if operation.paths().iter().any(|p| p.is_absolute()) && action != "rename" {
        return Err("enter a project-relative path".into());
    }
    velk_plugin_sdk::host::Request::FileOperation {
        operation: operation.clone(),
    }
    .validate()?;
    Ok(operation)
}

#[cfg(test)]
mod command_tests {
    use super::*;
    fn context(input: &str) -> velk_plugin_sdk::commands::Context {
        velk_plugin_sdk::commands::Context {
            input: Some(input.into()),
            path: Some("/project/file.rs".into()),
            language: None,
            revision: Some(1),
            position: Some(0),
        }
    }
    #[test]
    fn commands_use_the_host_and_do_not_dispatch_unconfirmed_deletion() {
        use velk_plugin_sdk::{
            host::{FileOperation, Request, Response},
            test_support::MockHost,
        };
        let mock = MockHost::new(|_| Ok(Response::Accepted));
        let files = Files {
            host: Some(mock.host()),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&context("file.rs")).unwrap();
        assert!(files.operate("delete", &bytes).is_err());
        assert!(mock.take_requests().is_empty());
        files.operate("create-file", &bytes).unwrap();
        assert_eq!(
            mock.take_requests(),
            vec![Request::FileOperation {
                operation: FileOperation::CreateFile {
                    path: "file.rs".into()
                }
            }]
        );
        let bytes = serde_json::to_vec(&context("delete file.rs")).unwrap();
        files.operate("delete", &bytes).unwrap();
        assert_eq!(
            mock.take_requests(),
            vec![Request::FileOperation {
                operation: FileOperation::Delete {
                    path: "file.rs".into()
                }
            }]
        );
    }
    #[test]
    fn mutation_commands_validate_confirmation_and_preserve_source_identity() {
        assert!(operation("delete", context("file.rs")).is_err());
        assert!(operation("delete", context("delete ../outside")).is_err());
        assert!(operation("create-file", context("/outside")).is_err());
        assert!(
            matches!(operation("delete", context("delete folder/file.rs")).unwrap(), velk_plugin_sdk::host::FileOperation::Delete { path } if path == std::path::Path::new("folder/file.rs"))
        );
        assert!(
            matches!(operation("rename", context("renamed.rs")).unwrap(), velk_plugin_sdk::host::FileOperation::Rename { source, destination } if source == std::path::Path::new("/project/file.rs") && destination == std::path::Path::new("renamed.rs"))
        );
        for command in Files::commands() {
            command.validate().unwrap();
        }
    }
}
