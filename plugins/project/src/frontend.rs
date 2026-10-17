use std::sync::atomic::{AtomicBool, Ordering};
use velk_plugin_sdk::{
    CapabilityDeclaration, Host, Plugin, Registrar,
    backend::Tasks,
    commands::{self, PickerCancel, PickerMode, PickerReply, PickerRequest},
    hooks,
};
use velk_project_contracts as project;
#[derive(Default)]
pub struct QuickOpen(Tasks<PickerRequest, PickerReply>);
#[derive(Default)]
pub struct WorkspaceFind(Tasks<PickerRequest, PickerReply>);
fn query(
    host: &Host,
    provider: &str,
    request: &project::Request,
) -> Result<project::Reply, String> {
    let input = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    serde_json::from_slice(&host.call_interface(provider, input)?).map_err(|e| e.to_string())
}
fn run(
    host: &Host,
    provider: &str,
    request: PickerRequest,
    cancelled: &AtomicBool,
) -> Result<PickerReply, String> {
    let PickerRequest::Begin {
        session,
        id,
        query: search,
        ..
    } = request
    else {
        return Err("expected query".into());
    };
    let mut reply = query(
        host,
        provider,
        &project::Request::Begin {
            session,
            id,
            query: search,
        },
    )?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = query(host, provider, &project::Request::Cancel { id });
            return Err("query cancelled".into());
        }
        match reply {
            project::Reply::Ready { matches, notice } => {
                return Ok(PickerReply::Ready {
                    items: matches
                        .into_iter()
                        .map(|result| commands::Choice {
                            title: if result.preview.is_some() {
                                format!(
                                    "{}:{}:{}",
                                    result.relative,
                                    result.location.position.line + 1,
                                    result.location.position.column + 1
                                )
                            } else {
                                result.relative
                            },
                            detail: result.preview,
                            action: commands::ChoiceAction::Location {
                                location: result.location,
                            },
                        })
                        .collect(),
                    notice,
                });
            }
            project::Reply::Cancelled => return Err("backend query cancelled".into()),
            project::Reply::Pending => {
                std::thread::sleep(std::time::Duration::from_millis(8));
                reply = query(host, provider, &project::Request::Poll { id })?;
            }
        }
    }
}
macro_rules! frontend {
    ($ty:ty, $id:literal, $name:literal, $provider:expr, $hook:literal, $key:literal, $shift:literal, $mode:expr) => {
        impl Plugin for $ty {
            const ID: &'static str = $id;
            const NAME: &'static str = $name;
            const README: &'static str = concat!("# ", $name, "\n\nDefault picker frontend over a reusable project backend. Queries run off the UI thread. Type to filter, use Up/Down and Enter to open a result, or Escape to cancel. Backend indexing and matching can be reused by other frontend plugins.");
            const VERSION: &'static str = env!("CARGO_PKG_VERSION");
            const CAPABILITIES: &'static [CapabilityDeclaration] =
                &[CapabilityDeclaration::required(
                    "plugin.call",
                    "Query the shared project backend.",
                )];
            fn describe(&mut self) -> String {
                $name.into()
            }
            fn attach_host(&mut self, host: Host) {
                self.0.start(
                    host,
                    |host, input, cancelled| run(host, $provider, input, cancelled),
                    |_| false,
                );
            }
            fn commands() -> Vec<commands::Declaration> {
                vec![commands::Declaration {
                    id: $id.into(),
                    title: $name.into(),
                    language: None,
                    input: None,
                    shortcut: Some(commands::Shortcut {
                        key: $key.into(),
                        primary: true,
                        shift: $shift,
                        ..Default::default()
                    }),
                    handler: commands::Handler::Picker {
                        hook: $hook.into(),
                        mode: $mode,
                    },
                }]
            }
            fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
                reg.provide(hooks::Request($hook), |this: &mut Self, bytes: &[u8]| {
                    let reply = match serde_json::from_slice(bytes).map_err(|e| e.to_string())? {
                        request @ PickerRequest::Begin { id, .. } => {
                            this.0.begin(id, request)?;
                            PickerReply::Pending
                        }
                        PickerRequest::Poll { id } => {
                            this.0.poll(id)?.unwrap_or(PickerReply::Pending)
                        }
                    };
                    serde_json::to_vec(&reply).map_err(|e| e.to_string())
                })?;
                reg.on(
                    hooks::Event(commands::PICKER_CANCEL_HOOK),
                    |this: &mut Self, bytes: &[u8]| {
                        let cancel: PickerCancel =
                            serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                        if cancel.plugin == Self::ID {
                            this.0.cancel(cancel.id);
                        }
                        Ok(())
                    },
                )
            }
        }
    };
}
frontend!(
    QuickOpen,
    "velk.quick-open",
    "Quick Open",
    project::FILES_INTERFACE,
    "velk.quick-open.query",
    "p",
    false,
    PickerMode::Files
);
frontend!(
    WorkspaceFind,
    "velk.workspace-find",
    "Find in Workspace",
    project::SEARCH_INTERFACE,
    "velk.workspace-find.query",
    "f",
    true,
    PickerMode::Content
);
