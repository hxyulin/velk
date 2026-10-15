//! Native notifications are reduced to one bounded, debounced invalidation bit.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use velk_panels::{Content, Node, Snapshot};
#[derive(Default)]
pub(super) struct Signal {
    directories: BTreeSet<PathBuf>,
    root: Option<PathBuf>,
    retired: BTreeSet<PathBuf>,
    first: Option<Instant>,
    last: Option<Instant>,
    error: Option<String>,
}
impl Signal {
    fn event(&mut self, result: notify::Result<Event>) {
        let relevant = match result {
            Ok(event) => {
                if matches!(
                    event.kind,
                    EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
                ) {
                    self.retired.extend(
                        event
                            .paths
                            .iter()
                            .filter(|p| self.directories.contains(*p))
                            .cloned(),
                    );
                }
                event.need_rescan()
                    || (structural(event.kind)
                        && event.paths.iter().any(|path| {
                            self.root.as_ref() == Some(path)
                                || self.directories.contains(path)
                                || path.parent().is_some_and(|p| self.directories.contains(p))
                        }))
            }
            Err(error) => {
                self.error = Some(error.to_string());
                true
            }
        };
        if relevant {
            let now = Instant::now();
            self.first.get_or_insert(now);
            self.last = Some(now);
        }
    }
    pub fn changed(&mut self) -> bool {
        let ready = self
            .last
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(100))
            || self
                .first
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(500));
        if ready {
            self.first = None;
            self.last = None;
        }
        ready
    }
}
fn structural(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other)
    )
}
pub(super) struct Watches {
    watcher: Option<RecommendedWatcher>,
    paths: BTreeSet<PathBuf>,
    pub signal: Arc<Mutex<Signal>>,
    setup_error: Option<String>,
}
impl Watches {
    pub fn new(signal: Arc<Mutex<Signal>>) -> Self {
        let events = signal.clone();
        let result = notify::recommended_watcher(move |event| events.lock().unwrap().event(event));
        let (watcher, setup_error) = match result {
            Ok(watcher) => (Some(watcher), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            watcher,
            paths: BTreeSet::new(),
            signal,
            setup_error,
        }
    }
    fn watch(&mut self, path: &Path) {
        if self.paths.contains(path) {
            return;
        }
        if let Some(watcher) = &mut self.watcher {
            match watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.paths.insert(path.into());
                }
                Err(error) => self.setup_error = Some(error.to_string()),
            }
        }
    }
    fn retire(&mut self) {
        let retired = std::mem::take(&mut self.signal.lock().unwrap().retired);
        for path in retired {
            if let Some(watcher) = &mut self.watcher {
                let _ = watcher.unwatch(&path);
            }
            self.paths.remove(&path);
        }
    }
    /// Install before reading, so changes during the read are caught as well.
    pub fn directory(&mut self, path: &Path) {
        self.retire();
        self.signal.lock().unwrap().directories.insert(path.into());
        self.watch(path);
    }
    pub fn reconcile(&mut self, directories: Vec<PathBuf>, root: Option<&Path>) {
        self.retire();
        let mut desired: BTreeSet<_> = directories.into_iter().collect();
        {
            let mut signal = self.signal.lock().unwrap();
            signal.directories = desired.clone();
            signal.root = root.map(Path::to_path_buf);
        }
        // The parent catches root deletion/replacement. Sibling events are
        // filtered out by Signal; the parent is not listed or traversed.
        if let Some(parent) = root.and_then(Path::parent) {
            desired.insert(parent.into());
        }
        self.setup_error = self
            .watcher
            .is_none()
            .then(|| "native watcher unavailable".into());
        for path in self.paths.difference(&desired).cloned().collect::<Vec<_>>() {
            if let Some(watcher) = &mut self.watcher {
                let _ = watcher.unwatch(&path);
            }
            self.paths.remove(&path);
        }
        for path in desired {
            self.watch(&path);
        }
    }
    pub fn decorate(&self, snapshot: &mut Snapshot) {
        let signal = self.signal.lock().unwrap();
        if let Some(error) = signal.error.as_ref().or(self.setup_error.as_ref()) {
            let message = crate::model::caption(format!(
                "[File watching unavailable: {error}; use Panels: Refresh]"
            ));
            if let Some(panel) = snapshot.panels.first_mut() {
                match &mut panel.content {
                    Content::Tree(rows) => rows.push(Node {
                        id: "watch-error".into(),
                        label: message,
                        depth: 0,
                        branch: false,
                        expanded: false,
                        lazy: false,
                        action: None,
                    }),
                    Content::Text(text) => {
                        text.push('\n');
                        text.push_str(&message);
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_writes_and_unwatched_descendants_do_not_invalidate() {
        let base = std::env::temp_dir().join("velk-watch-filter");
        let mut signal = Signal {
            directories: [base.clone()].into(),
            ..Default::default()
        };
        signal.event(Ok(Event::new(EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Any,
        )))
        .add_path(base.join("a"))));
        signal.event(Ok(Event::new(EventKind::Create(
            notify::event::CreateKind::File,
        ))
        .add_path(base.join("closed/deep"))));
        assert!(signal.first.is_none());
        signal.event(Ok(Event::new(EventKind::Create(
            notify::event::CreateKind::File,
        ))
        .add_path(base.join("a"))));
        assert!(!signal.changed());
        signal.last = Some(Instant::now() - Duration::from_millis(101));
        assert!(signal.changed());
        assert!(!signal.changed());
    }
}
