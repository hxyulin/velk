use super::*;
use rxui::{BackgroundFuture, BlockingJob, Runtime, SpawnError, TaskExecutor};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct QueuedExecutor(Mutex<Vec<BlockingJob>>);
impl TaskExecutor for QueuedExecutor {
    fn spawn(&self, _: BackgroundFuture) -> Result<(), SpawnError> {
        Err(SpawnError::Unavailable)
    }
    fn spawn_blocking(&self, job: BlockingJob) -> Result<(), SpawnError> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}
impl QueuedExecutor {
    pub(super) fn pending(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    pub(super) fn complete_at(&self, index: usize, runtime: &mut Runtime) {
        let job = self.0.lock().unwrap().remove(index);
        std::thread::spawn(job).join().unwrap();
        runtime.poll_tasks();
        runtime.flush().unwrap();
    }
    pub(super) fn drain(&self, runtime: &mut Runtime) {
        for _ in 0..64 {
            if self.pending() == 0 {
                return;
            }
            self.complete(runtime);
        }
        panic!("headless test did not settle after 64 background batches");
    }
    pub(super) fn complete(&self, runtime: &mut Runtime) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        std::thread::spawn(move || {
            for job in jobs {
                job();
            }
        })
        .join()
        .unwrap();
        runtime.poll_tasks();
        runtime.flush().unwrap();
    }
}
/// Exercises host operations and background callbacks without opening a window.
pub(super) struct Harness {
    pub runtime: Runtime,
    pub document: Entity<Document>,
    pub executor: Arc<QueuedExecutor>,
    directory: tempfile::TempDir,
}
impl Harness {
    pub fn new() -> Self {
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        Self {
            runtime,
            document,
            executor,
            directory: tempfile::tempdir().unwrap(),
        }
    }
    pub fn write_file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.directory.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path.canonicalize().unwrap()
    }
    pub fn request(
        &mut self,
        request: velk_plugin_contracts::host::Request,
    ) -> Result<velk_plugin_contracts::host::Response, String> {
        let result = self.runtime.update(|cx| {
            self.document
                .update(cx, |d, cx| d.host_request(request, None, cx))
        });
        self.runtime.flush().unwrap();
        result
    }
    pub fn drain(&mut self) {
        self.executor.drain(&mut self.runtime);
    }
    pub fn snapshot(&mut self) -> serde_json::Value {
        use velk_plugin_contracts::host::{Request, Response};
        let Response::Buffers { buffers } = self.request(Request::Buffers).unwrap() else {
            panic!()
        };
        let buffers: Vec<_> = buffers
            .into_iter()
            .map(|buffer| {
                let Response::Buffer { buffer, text } =
                    self.request(Request::ReadBuffer { id: buffer.id }).unwrap()
                else {
                    panic!()
                };
                serde_json::json!({ "buffer": buffer, "text": text })
            })
            .collect();
        serde_json::json!({ "buffers": buffers, "pending_jobs": self.executor.pending() })
    }
}
#[test]
fn headless_harness_drains_chained_opens_and_controls_independent_save_completion_order() {
    use velk_plugin_contracts::host::{Request, Response};
    let mut harness = Harness::new();
    let first = harness.write_file("first.txt", "one");
    let second = harness.write_file("second.txt", "two");
    harness
        .request(Request::OpenBuffer {
            path: first.clone(),
        })
        .unwrap();
    assert!(
        harness
            .request(Request::OpenBuffer {
                path: second.clone()
            })
            .is_err()
    );
    harness.runtime.update(|cx| {
        harness
            .document
            .update(cx, |d, cx| d.open_path(second.clone(), cx))
    });
    harness.drain();
    let Response::Buffers { buffers } = harness.request(Request::Buffers).unwrap() else {
        panic!()
    };
    assert_eq!(buffers.len(), 2);
    for buffer in &buffers {
        let position = velk_commands::Position {
            line: 0,
            column: 0,
            encoding: velk_commands::Encoding::Utf32,
        };
        harness
            .request(Request::EditBuffer {
                id: buffer.id,
                revision: buffer.revision,
                edits: vec![velk_commands::TextEdit {
                    start: position,
                    end: position,
                    text: "edited ".into(),
                }],
            })
            .unwrap();
        harness
            .request(Request::SaveBuffer {
                id: buffer.id,
                expected_revision: None,
            })
            .unwrap();
    }
    assert_eq!(harness.executor.pending(), 2);
    harness.executor.complete_at(1, &mut harness.runtime);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "edited two");
    harness.drain();
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "edited one");
    let snapshot = harness.snapshot();
    assert_eq!(snapshot["pending_jobs"], 0);
    assert!(
        snapshot["buffers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["buffer"]["dirty"] == false)
    );
}
#[test]
fn async_save_persists_requested_snapshot_keeps_newer_edits_dirty_and_serializes_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let (file, editor) = FileState::open(&path).unwrap();
    let mut runtime = Runtime::new();
    let executor = Arc::new(QueuedExecutor::default());
    runtime.configure_tasks(executor.clone(), || {}).unwrap();
    let editor = runtime.update(|cx| cx.new(|_| editor));
    let document =
        runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path.clone(), cx)));
    runtime.update(|cx| editor.update(cx, |e, _| e.insert("one")));
    runtime.flush().unwrap();
    runtime.update(|cx| {
        assert!(document.read(cx).buffer.dirty);
        document.update(cx, |d, cx| d.save(None, cx));
        editor.update(cx, |e, _| e.insert("two"));
        // A second save must not race the first one's rename/baseline.
        document.update(cx, |d, cx| d.save(None, cx));
        assert!(document.read(cx).buffer.saving.is_some());
    });
    assert_eq!(executor.0.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    executor.complete(&mut runtime);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "oneoriginal");
    runtime.update(|cx| {
        assert!(editor.read(cx).buffer().is_dirty());
        assert!(document.read(cx).buffer.dirty);
        assert!(document.read(cx).buffer.saving.is_none());
        assert!(document.read(cx).buffer.status.is_none());
        assert_eq!(
            editor.read(cx).buffer().text().to_string(),
            "onetwooriginal"
        );
        document.update(cx, |d, cx| d.save(None, cx));
    });
    executor.complete(&mut runtime);
    runtime.update(|cx| {
        assert!(!editor.read(cx).buffer().is_dirty());
        assert!(!document.read(cx).buffer.dirty);
        assert!(editor.read(cx).buffer().can_undo());
        // A failed subsequent save retains the last successful checkpoint.
        editor.update(cx, |e, _| e.insert("three"));
        std::fs::write(&path, "external").unwrap();
        document.update(cx, |d, cx| d.save(None, cx));
    });
    executor.complete(&mut runtime);
    runtime.update(|cx| {
        assert!(editor.read(cx).buffer().is_dirty());
        assert!(
            document
                .read(cx)
                .buffer
                .status
                .as_ref()
                .unwrap()
                .contains("changed on disk")
        );
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
}
#[test]
fn document_layout_reserves_status_height_and_keeps_editor_visible_on_resize() {
    struct Measure;
    impl rxui::TextMeasure for Measure {
        fn measure(
            &mut self,
            _: rxui::ElementId,
            request: rxui::TextRequest<'_>,
        ) -> Result<[f32; 2], rxui::UiError> {
            Ok([request.text.len() as f32 * 7., 18.])
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let (file, editor) = FileState::open(&path).unwrap();
    let mut runtime = Runtime::new();
    let document = runtime.update(|cx| {
        let editor = cx.new(|_| editor);
        cx.new(|cx| Document::new(editor, file, path, cx))
    });
    let mut ui = rxui::Ui::new(&mut runtime, document).unwrap();
    for size in [[640., 400.], [320., 160.]] {
        ui.prepare(&mut runtime, size, &mut Measure).unwrap();
        let bounds = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .bounds;
        assert_eq!(bounds.width, size[0]);
        assert!(
            bounds.height >= size[1] - 74.,
            "editor unexpectedly collapsed: {bounds:?}"
        );
        assert!(
            bounds.height <= size[1] - 52.,
            "editor obscures status: {bounds:?}"
        );
    }
}

#[test]
fn failed_task_submission_keeps_edits_and_reports_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let (file, mut editor) = FileState::open(&path).unwrap();
    editor.insert("x");
    let mut runtime = Runtime::new();
    runtime.update(|cx| {
        let editor = cx.new(|_| editor);
        let document = cx.new(|cx| Document::new(editor, file, path.clone(), cx));
        document.update(cx, |d, cx| d.save(None, cx));
        assert!(
            document
                .read(cx)
                .buffer
                .status
                .as_ref()
                .unwrap()
                .contains("not configured")
        );
        assert!(document.read(cx).buffer.saving.is_none());
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
}

mod feature_walkthrough;
