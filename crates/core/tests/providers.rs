use std::sync::Arc;
use velk_core::history::*;
use velk_core::{Buffer, Selection, TextEdit, Transaction};

#[test]
fn core_buffers_can_edit_without_history_and_attach_an_explicit_provider_later() {
    let mut buffer = Buffer::new("abc");
    buffer.insert("x");
    assert_eq!(buffer.text().to_string(), "xabc");
    assert!(buffer.history_provider().is_none());
    assert!(!buffer.can_undo());
    let mut restored = Buffer::restore(&buffer.snapshot().encode().unwrap()).unwrap();
    restored
        .attach_history(Arc::new(velk_undo_tree::Tree))
        .unwrap();
    restored.insert("y");
    restored.undo();
    assert_eq!(restored.text().to_string(), "xabc");
    assert!(
        restored
            .attach_history(Arc::new(velk_undo_tree::Tree))
            .is_err()
    );
}
#[derive(Debug)]
struct Broken {
    reject_record: bool,
}
struct BrokenSession {
    reject_record: bool,
}
struct Saved;
impl Snapshot for Saved {
    fn encode(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }
}
impl Provider for Broken {
    fn id(&self) -> &str {
        "test.broken"
    }
    fn open(&self, _: u64) -> Box<dyn Session> {
        Box::new(BrokenSession {
            reject_record: self.reject_record,
        })
    }
    fn restore(&self, _: Restore) -> Result<Box<dyn Session>, String> {
        Err("unsupported test archive".into())
    }
}
impl Session for BrokenSession {
    fn status(&self) -> Status {
        Status {
            can_undo: true,
            ..Default::default()
        }
    }
    fn snapshot(&self) -> Result<Arc<dyn Snapshot>, String> {
        Ok(Arc::new(Saved))
    }
    fn request(&mut self, request: Request) -> Result<Response, String> {
        let step = match request {
            Request::Record(_) if self.reject_record => return Err("record failed".into()),
            Request::Undo(current) => Some(Step {
                source: current,
                target: 0,
                selection: Selection::default(),
                patches: vec![Patch {
                    change: velk_core::Change::between(
                        0,
                        &ropey::Rope::from_str("a"),
                        &ropey::Rope::new(),
                        0..1,
                        0,
                    ),
                    removed: "a".into(),
                    inserted: "".into(),
                }],
            }),
            _ => None,
        };
        Ok(Response {
            status: self.status(),
            step,
        })
    }
}
#[test]
fn provider_errors_and_invalid_travel_leave_document_revision_and_peer_views_unchanged() {
    let mut buffer = Buffer::with_history(
        "abc",
        Arc::new(Broken {
            reject_record: true,
        }),
    );
    let checkpoint = buffer.checkpoint();
    assert!(
        buffer
            .apply_transaction(Transaction {
                revision: 0,
                edits: vec![TextEdit {
                    range: 0..0,
                    text: "x".into()
                }],
                selection: None,
            })
            .is_err()
    );
    assert_eq!(buffer.revision(), 0);
    assert_eq!(buffer.checkpoint(), checkpoint);
    assert_eq!(buffer.text().to_string(), "abc");
    let mut buffer = Buffer::with_history(
        "abc",
        Arc::new(Broken {
            reject_record: false,
        }),
    );
    buffer.insert("x");
    let peer = buffer.new_view();
    let revision = buffer.revision();
    let selection = peer.selection();
    buffer.undo();
    assert_eq!(buffer.revision(), revision);
    assert_eq!(buffer.text().to_string(), "xabc");
    assert_eq!(peer.selection(), selection);
}
#[test]
fn archives_require_their_declared_provider() {
    let mut buffer = Buffer::with_history("abc", Arc::new(velk_undo_tree::Tree));
    buffer.insert("x");
    let archive = buffer.snapshot().encode().unwrap();
    assert_eq!(
        Buffer::archive_history_provider(&archive)
            .unwrap()
            .as_deref(),
        Some(DEFAULT_PROVIDER)
    );
    assert!(Buffer::restore(&archive).is_err());
    assert!(
        Buffer::restore_with_history(
            &archive,
            Arc::new(Broken {
                reject_record: false
            })
        )
        .is_err()
    );
}
