use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use velk_plugin_sdk::{
    CapabilityDeclaration, Host, Plugin, Registrar,
    buffers::Event,
    host::{Buffer, Request},
    settings,
};
struct State {
    stop: bool,
    enabled: bool,
    delay: Duration,
    host: Option<Host>,
    pending: BTreeMap<u64, (u64, Instant)>,
    observed: BTreeMap<u64, u64>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            stop: false,
            enabled: false,
            delay: Duration::from_millis(1000),
            host: None,
            pending: BTreeMap::new(),
            observed: BTreeMap::new(),
        }
    }
}
impl State {
    fn record(&mut self, buffer: Buffer, now: Instant, reset: bool) {
        if !self.enabled || !buffer.dirty || buffer.path.is_none() {
            self.pending.remove(&buffer.id);
            self.observed.remove(&buffer.id);
        } else if reset
            || self
                .pending
                .get(&buffer.id)
                .is_none_or(|(revision, _)| *revision != buffer.revision)
        {
            self.observed.insert(buffer.id, buffer.revision);
            self.pending
                .insert(buffer.id, (buffer.revision, now + self.delay));
        }
    }
    fn event(&mut self, event: Event, now: Instant) {
        match event {
            Event::Snapshot { buffers } => {
                self.pending.clear();
                self.observed.clear();
                for buffer in buffers {
                    self.record(buffer, now, true);
                }
            }
            Event::Opened { buffer } | Event::Changed { buffer } => self.record(buffer, now, true),
            Event::Saved { buffer, .. } => self.record(buffer, now, false),
            Event::Closed { id } => {
                self.pending.remove(&id);
                self.observed.remove(&id);
            }
        }
    }
}
pub struct Autosave {
    shared: Arc<(Mutex<State>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Default for Autosave {
    fn default() -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("velk-autosave".into())
            .spawn(move || {
                loop {
                    let (id, revision, host) = {
                        let mut current = state.0.lock().unwrap();
                        loop {
                            if current.stop {
                                return;
                            }
                            if let Some((&id, &(revision, deadline))) = current
                                .pending
                                .iter()
                                .min_by_key(|(_, (_, deadline))| *deadline)
                                && let Some(host) = current.host.clone()
                            {
                                let remaining = deadline.saturating_duration_since(Instant::now());
                                if remaining.is_zero() {
                                    current.pending.remove(&id);
                                    break (id, revision, host);
                                }
                                current = state.1.wait_timeout(current, remaining).unwrap().0;
                            } else {
                                current = state.1.wait(current).unwrap();
                            }
                        }
                    };
                    if let Err(error) = host.request(Request::SaveBuffer {
                        id,
                        expected_revision: Some(revision),
                    }) && error.contains("busy")
                    {
                        let mut current = state.0.lock().unwrap();
                        if current.enabled
                            && !current.stop
                            && current.observed.get(&id) == Some(&revision)
                        {
                            let due = Instant::now() + current.delay;
                            current.pending.entry(id).or_insert((revision, due));
                        }
                    }
                }
            })
            .ok();
        Self { shared, worker }
    }
}
impl Drop for Autosave {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Plugin for Autosave {
    const ID: &'static str = "velk.autosave";
    const NAME: &'static str = "Autosave";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Observe committed buffer changes and save state.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.manage",
            "Request normal saves for file-backed dirty buffers.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Save file-backed buffers after an idle delay (opt-in).".into()
    }
    fn attach_host(&mut self, host: Host) {
        self.shared.0.lock().unwrap().host = Some(host);
        self.shared.1.notify_one();
    }
    fn settings() -> Vec<settings::Definition> {
        vec![settings::Definition { key: "enabled".into(), title: "Enable autosave".into(), description: "Save file-backed documents after editing pauses. Untitled documents use recovery.".into(), kind: settings::Kind::Boolean, default: false.into() },
        settings::Definition { key: "delay_ms".into(), title: "Idle delay (ms)".into(), description: "Wait this long after the latest committed change before saving.".into(), kind: settings::Kind::Integer { min: 250, max: 60000 }, default: 1000.into() }]
    }
    fn configure(&mut self, values: &settings::Values) -> Result<(), String> {
        let mut state = self.shared.0.lock().unwrap();
        state.enabled = values["enabled"]
            .as_bool()
            .ok_or("invalid enabled setting")?;
        state.delay =
            Duration::from_millis(values["delay_ms"].as_u64().ok_or("invalid idle delay")?);
        state.pending.clear();
        state.observed.clear();
        self.shared.1.notify_one();
        Ok(())
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.on_buffer_event(|this, event| {
            this.shared.0.lock().unwrap().event(event, Instant::now());
            this.shared.1.notify_one();
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn buffer(revision: u64) -> Buffer {
        Buffer {
            id: 1,
            title: "file".into(),
            path: Some("/project/file".into()),
            revision,
            dirty: true,
            active: false,
            anchor: 0,
            head: 0,
        }
    }
    #[test]
    fn changes_debounce_and_saved_older_revisions_preserve_new_work() {
        let now = Instant::now();
        let mut state = State {
            enabled: true,
            ..Default::default()
        };
        state.event(Event::Changed { buffer: buffer(1) }, now);
        state.event(
            Event::Changed { buffer: buffer(2) },
            now + Duration::from_millis(500),
        );
        assert_eq!(state.pending[&1], (2, now + Duration::from_millis(1500)));
        state.event(
            Event::Saved {
                buffer: buffer(2),
                saved_revision: 1,
            },
            now + Duration::from_millis(600),
        );
        assert_eq!(state.pending[&1].1, now + Duration::from_millis(1500));
        state.event(Event::Closed { id: 1 }, now);
        assert!(state.pending.is_empty());
    }
    #[test]
    fn defaults_untitled_and_clean_buffers_never_schedule_saves() {
        let now = Instant::now();
        let mut state = State::default();
        state.record(buffer(1), now, true);
        assert!(state.pending.is_empty());
        state.enabled = true;
        let mut b = buffer(1);
        b.path = None;
        state.record(b, now, true);
        assert!(state.pending.is_empty());
        let mut b = buffer(1);
        b.dirty = false;
        state.record(b, now, true);
        assert!(state.pending.is_empty());
        state.event(
            Event::Snapshot {
                buffers: vec![buffer(4)],
            },
            now,
        );
        assert_eq!(state.pending[&1].0, 4);
        state.event(Event::Snapshot { buffers: vec![] }, now);
        assert!(state.pending.is_empty());
    }
}
