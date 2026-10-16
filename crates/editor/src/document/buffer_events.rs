use super::Document;
use rxui::prelude::*;
use velk_plugin_contracts::buffers::Event;
#[derive(Default)]
pub(super) struct Delivery {
    pending: Vec<Event>,
    task: Option<Task>,
    resync: bool,
}
impl Document {
    pub(super) fn publish_buffer_snapshot(&mut self, cx: &mut Context<'_, Self>) {
        let buffers = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .map(|b| self.buffer_info(b, cx))
            .collect();
        self.buffer_events.pending.clear();
        self.publish_buffer_event(Event::Snapshot { buffers }, cx);
    }
    pub(super) fn publish_buffer_event(&mut self, event: Event, cx: &mut Context<'_, Self>) {
        if self.plugins.is_none() {
            return;
        }
        if matches!(event, Event::Changed { .. }) {
            self.buffer_events
                .pending
                .retain(|old| !matches!(old, Event::Changed { .. }) || old.id() != event.id());
        }
        self.buffer_events.pending.push(event);
        if self.buffer_events.pending.len() > 256 {
            self.buffer_events.pending.clear();
            self.buffer_events.resync = true;
        }
        self.deliver_buffer_events(cx);
    }
    pub(super) fn deliver_buffer_events(&mut self, cx: &mut Context<'_, Self>) {
        if self.buffer_events.task.is_some() {
            return;
        }
        if self.buffer_events.resync {
            if self.background_buffer {
                return;
            }
            self.buffer_events.resync = false;
            self.publish_buffer_snapshot(cx);
            return;
        }
        if self.buffer_events.pending.is_empty() {
            return;
        }
        let Some(plugins) = &self.plugins else {
            return;
        };
        let client = plugins.client.clone();
        let batch = std::mem::take(&mut self.buffer_events.pending);
        match cx.try_spawn_blocking(
            move || client.publish_buffer_events(batch),
            |this, result, cx| {
                this.buffer_events.task = None;
                if let Ok(Err(error)) = result {
                    tracing::warn!(%error, "buffer event delivery failed");
                }
                this.deliver_buffer_events(cx);
            },
        ) {
            Ok(task) => self.buffer_events.task = Some(task),
            Err(error) => tracing::warn!(%error, "cannot schedule buffer events"),
        }
    }
}
