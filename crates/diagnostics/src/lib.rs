//! Lightweight instrumentation facade plus an executable-owned optional collector.
use std::sync::OnceLock;
pub use tracing;
use velk_plugin_api::TraceSink;
#[cfg(feature = "collector")]
mod collector;
#[cfg(feature = "collector")]
pub use collector::*;

static SINK: OnceLock<TraceSink> = OnceLock::new();
/// Install the executable-owned, process-lifetime callback table for this image.
/// # Safety
/// Every callback must remain callable on any producer thread until process exit.
#[allow(unsafe_code)]
pub unsafe fn attach(sink: TraceSink) {
    let _ = SINK.set(sink);
}

/// SDK timings use the host clock and do not depend on shared Rust globals.
pub struct Scope {
    sink: Option<TraceSink>,
    name: &'static str,
    target: &'static str,
    start: u64,
    id: u64,
    asynchronous: bool,
}
impl Scope {
    #[allow(unsafe_code)]
    pub fn new(name: &'static str, target: &'static str, asynchronous: bool) -> Self {
        let sink = SINK
            .get()
            .copied()
            .filter(|sink| unsafe { (sink.enabled)() });
        let (start, id) = sink.map_or((0, 0), |sink| unsafe {
            (
                (sink.now_ns)(),
                if asynchronous { (sink.next_id)() } else { 0 },
            )
        });
        let scope = Self {
            sink,
            name,
            target,
            start,
            id,
            asynchronous,
        };
        if asynchronous {
            scope.record(2, 0);
        }
        scope
    }
    #[allow(unsafe_code)]
    fn record(&self, kind: u32, duration: u64) {
        if let Some(sink) = self.sink {
            unsafe {
                (sink.record)(
                    kind,
                    self.id,
                    bytes(self.name),
                    bytes(self.target),
                    self.start,
                    duration,
                );
            }
        }
    }
}
impl Drop for Scope {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if let Some(sink) = self.sink {
            self.record(
                if self.asynchronous { 3 } else { 0 },
                unsafe { (sink.now_ns)() }.saturating_sub(self.start),
            );
        }
    }
}
fn bytes(text: &str) -> velk_plugin_api::Bytes {
    velk_plugin_api::Bytes {
        data: text.as_ptr(),
        length: text.len(),
    }
}
#[macro_export]
macro_rules! scope {
    ($name:literal) => {
        $crate::Scope::new($name, module_path!(), false)
    };
}
#[macro_export]
macro_rules! async_scope {
    ($name:literal) => {
        $crate::Scope::new($name, module_path!(), true)
    };
}

/// Absent for standalone hosts that have not installed a collector.
pub fn host_sink() -> Option<TraceSink> {
    #[cfg(feature = "collector")]
    {
        available().then(sink)
    }
    #[cfg(not(feature = "collector"))]
    {
        None
    }
}
