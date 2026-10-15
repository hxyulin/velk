//! UI only schedules control operations; file creation/flushing stays off-thread.
use super::Document;
use rxui::prelude::*;
pub(super) struct Start;
impl Command for Start {}
pub(super) struct Stop;
impl Command for Stop {}
pub(super) struct Marker;
impl Command for Marker {}
impl Document {
    pub(super) fn trace_capture(&mut self, start: bool, cx: &mut Context<'_, Self>) {
        if self.diagnostics.is_some() || self.reloading() {
            return;
        }
        self.buffer.status = Some(
            if start {
                "Starting trace capture…"
            } else {
                "Finishing trace capture…"
            }
            .into(),
        );
        match cx.try_spawn_blocking(
            move || {
                if start {
                    velk_diagnostics::start()
                        .map(|path| format!("Trace capture started: {}", path.display()))
                } else {
                    velk_diagnostics::stop().map(|report| {
                        format!(
                            "Trace saved: {} · {} events · {} dropped · {}",
                            report.path.display(),
                            report.events,
                            report.dropped,
                            report.reason
                        )
                    })
                }
            },
            |this, result, _| {
                this.diagnostics = None;
                this.buffer.status = Some(match result {
                    Ok(Ok(message)) => message,
                    Ok(Err(error)) => format!("Trace failed: {error}"),
                    Err(error) => format!("Trace failed: {error}"),
                });
            },
        ) {
            Ok(task) => self.diagnostics = Some(task),
            Err(error) => self.buffer.status = Some(format!("Trace failed: {error}")),
        }
    }
}
