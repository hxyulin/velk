use super::Document;
use rxui::prelude::*;
use velk_plugin_host::{Client, Event};
pub(super) struct State {
    client: Client,
    task: Option<Task>,
    pub status: String,
}
impl Document {
    pub fn enable_plugins(&mut self, client: Client, cx: &mut Context<'_, Self>) {
        self.plugins = Some(State {
            client,
            task: None,
            status: String::new(),
        });
        self.change_plugins(true, cx);
    }
    pub(super) fn markup_renderer(&self) -> std::sync::Arc<dyn velk_markup::Renderer> {
        self.plugins.as_ref().map_or_else(
            || {
                std::sync::Arc::new(velk_markup::PlainText)
                    as std::sync::Arc<dyn velk_markup::Renderer>
            },
            |state| {
                std::sync::Arc::new(state.client.clone())
                    as std::sync::Arc<dyn velk_markup::Renderer>
            },
        )
    }
    pub(super) fn plugins_busy(&self) -> bool {
        self.plugins.as_ref().is_none_or(|s| s.task.is_some())
    }
    pub(super) fn change_plugins(&mut self, load: bool, cx: &mut Context<'_, Self>) {
        let Some(state) = &mut self.plugins else {
            return;
        };
        if state.task.is_some() {
            cx.unchanged();
            return;
        }
        let receiver = if load {
            state.client.load()
        } else {
            state.client.unload_all()
        };
        let receiver = match receiver {
            Ok(r) => r,
            Err(e) => {
                state.status = format!("Plugins: {e}");
                return;
            }
        };
        state.status = if load {
            "Loading plugins…"
        } else {
            "Unloading plugins…"
        }
        .into();
        match cx.try_spawn(async move { receiver.recv().await }, |this, result, _| {
            let Some(state) = &mut this.plugins else {
                return;
            };
            state.task = None;
            state.status = match result {
                Ok(Ok(Ok(report))) => {
                    let errors: Vec<_> = report
                        .events
                        .iter()
                        .filter_map(|e| match e {
                            Event::Rejected { id, message } => Some(format!("{id}: {message}")),
                            _ => None,
                        })
                        .collect();
                    if errors.is_empty() {
                        format!("Plugins: {} loaded", report.loaded)
                    } else {
                        format!("Plugins: {} loaded · {}", report.loaded, errors.join("; "))
                    }
                }
                Ok(Ok(Err(error))) => format!("Plugins: {error}"),
                _ => "Plugin worker unavailable".into(),
            };
        }) {
            Ok(task) => state.task = Some(task),
            Err(error) => state.status = format!("Plugins: {error}"),
        }
    }
}
