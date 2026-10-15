//! Sessions retain callbacks and native handles until the provider has joined
//! its producers. Every input/lifecycle call stays on the host plugin worker.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use velk_plugin_sdk::ffi::semantic as wire;
use velk_semantic::{DocumentSnapshot, Event as SemanticEvent, Input, Session, Status};

struct Callback {
    active: AtomicBool,
    events: velk_semantic::EventSink,
}
unsafe extern "C" fn emit(
    context: *mut c_void,
    event: *const velk_plugin_api::SemanticEvent,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() || event.is_null() {
            return INVALID;
        }
        // SAFETY: boxed callback is retained until all producer calls have joined.
        let context = unsafe { &*context.cast::<Callback>() };
        if !context.active.load(Ordering::Acquire) {
            return OK;
        }
        match unsafe { wire::decode_event(&*event) } {
            Ok(event) => {
                context.events.publish(event);
                OK
            }
            Err(error) => {
                context
                    .events
                    .publish(SemanticEvent::Status(Status::Failed(error)));
                INVALID
            }
        }
    }))
    .unwrap_or(PANICKED)
}
struct NativeSession {
    provider: String,
    instance: NonNull<c_void>,
    input: SendSemantic,
    destroy: DestroySemantic,
    callback: Box<Callback>,
}
impl NativeSession {
    fn send(&self, input: &Input) -> Result<(), String> {
        let status = wire::with_input(input, |raw| {
            // SAFETY: borrowed views last this serialized call; provider remains
            // loaded until session retirement finishes.
            unsafe { (self.input)(self.instance.as_ptr(), raw) }
        })?;
        if status == OK {
            Ok(())
        } else {
            Err(format!("semantic input failed with status {status}"))
        }
    }
    fn retire(self) -> bool {
        self.callback.active.store(false, Ordering::Release);
        // SAFETY: exclusive lifecycle worker, one destroy per successful create.
        let status = unsafe { (self.destroy)(self.instance.as_ptr()) };
        if status != OK {
            // Producer quiescence is unproven. Preserve callback storage; caller
            // also pins provider code and blocks reactivation of this plugin ID.
            std::mem::forget(self);
            return false;
        }
        true
    }
}
pub(super) struct DocumentSession {
    session: Session,
    latest: Option<DocumentSnapshot>,
    native: Option<NativeSession>,
    attempted: Option<u64>,
}
impl Manager {
    pub(crate) fn open_semantics(&mut self, id: u64, session: Session) {
        session
            .events()
            .publish(SemanticEvent::Status(Status::Stopped));
        self.sessions.insert(
            id,
            DocumentSession {
                session,
                latest: None,
                native: None,
                attempted: None,
            },
        );
    }
    pub(crate) fn drive_semantics(&mut self) {
        let providers: Vec<_> = self
            .plugins
            .values()
            .filter(|p| !p.destroyed && p.api.semantic_create.is_some())
            .map(|p| (p.info.id.clone(), p.info.generation))
            .collect();
        // Temporarily remove entries to keep their callback/storage ownership
        // explicit while invoking a provider's serialized instance methods.
        let ids: Vec<_> = self.sessions.keys().copied().collect();
        for id in ids {
            let mut doc = self.sessions.remove(&id).unwrap();
            if doc.session.is_cancelled() {
                if let Some(native) = doc.native.take() {
                    let provider = native.provider.clone();
                    if !native.retire() {
                        self.quarantine_semantic_provider(&provider);
                    }
                }
                doc.session.events().clear();
                doc.session
                    .events()
                    .publish(SemanticEvent::Status(Status::Stopped));
                doc.session.events().close();
                continue;
            }
            // Bound one pass even if another thread continuously submits edits.
            // Every submission also queues a wake (or finds a full request queue).
            for _ in 0..3 {
                let Some(input) = doc.session.try_next() else {
                    break;
                };
                if let Input::Document(snapshot) = &input {
                    doc.latest = Some(snapshot.clone());
                }
                if let Some(native) = &doc.native
                    && let Err(error) = native.send(&input)
                {
                    doc.session
                        .events()
                        .publish(SemanticEvent::Status(Status::Failed(error)));
                }
            }
            if doc.native.is_none() {
                if let [(provider, generation)] = providers.as_slice() {
                    if doc.attempted != Some(*generation)
                        && self.plugins.get(provider).is_some_and(|p| !p.destroyed)
                    {
                        doc.attempted = Some(*generation);
                        doc.session.events().clear();
                        let plugin = self.plugins.get_mut(provider).unwrap();
                        let mut callback = Box::new(Callback {
                            active: AtomicBool::new(true),
                            events: doc.session.events(),
                        });
                        let mut instance = std::ptr::null_mut();
                        doc.session
                            .events()
                            .publish(SemanticEvent::Status(Status::Starting));
                        // SAFETY: live instance, stable boxed thread-safe sink,
                        // retained through joining semantic_destroy.
                        let status = unsafe {
                            plugin.api.semantic_create.unwrap()(
                                plugin.instance.as_ptr(),
                                emit,
                                (&mut *callback as *mut Callback).cast(),
                                &mut instance,
                            )
                        };
                        if status == OK
                            && let Some(instance) = NonNull::new(instance)
                        {
                            let native = NativeSession {
                                provider: provider.clone(),
                                instance,
                                input: plugin.api.semantic_input.unwrap(),
                                destroy: plugin.api.semantic_destroy.unwrap(),
                                callback,
                            };
                            if let Some(snapshot) = &doc.latest
                                && let Err(error) = native.send(&Input::Document(snapshot.clone()))
                            {
                                doc.session
                                    .events()
                                    .publish(SemanticEvent::Status(Status::Failed(error)));
                            }
                            doc.native = Some(native);
                        } else {
                            // A failed create must leave no live producer or retained
                            // sink. This is part of the trusted ABI contract.
                            doc.session
                                .events()
                                .publish(SemanticEvent::Status(Status::Failed(format!(
                                    "semantic session creation failed ({status})"
                                ))));
                        }
                    }
                } else if providers.len() > 1 && doc.attempted != Some(0) {
                    doc.attempted = Some(0);
                    doc.session
                        .events()
                        .publish(SemanticEvent::Status(Status::Failed(
                            "multiple semantic providers loaded".into(),
                        )));
                }
            }
            self.sessions.insert(id, doc);
        }
    }
    fn quarantine_semantic_provider(&mut self, id: &str) {
        if let Some(plugin) = self.plugins.get_mut(id) {
            plugin.destroyed = true;
            if let Some(library) = plugin.library.take() {
                std::mem::forget(library);
            }
        }
        self.blocked.insert(id.into());
    }
    pub(super) fn retire_semantics(&mut self, id: &str) -> bool {
        let mut success = true;
        for doc in self.sessions.values_mut() {
            if doc
                .native
                .as_ref()
                .is_some_and(|native| native.provider == id)
            {
                success &= doc.native.take().unwrap().retire();
                doc.attempted = None;
                doc.session.events().clear();
                doc.session
                    .events()
                    .publish(SemanticEvent::Status(Status::Stopped));
            }
        }
        if !success {
            self.quarantine_semantic_provider(id);
        }
        success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    static INSTANCE_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn fail_session(_: *mut c_void) -> i32 {
        PANICKED
    }
    unsafe extern "C" fn input(_: *mut c_void, _: *const SemanticInput) -> i32 {
        OK
    }
    unsafe extern "C" fn destroy_instance(_: *mut c_void) -> i32 {
        INSTANCE_DESTROYS.fetch_add(1, Ordering::SeqCst);
        OK
    }
    #[test]
    fn failed_session_join_preserves_callback_and_never_destroys_the_plugin_instance() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(dir.path().into());
        let (_service, session) = velk_semantic::Service::external(|| {});
        let callback = Box::new(Callback {
            active: AtomicBool::new(true),
            events: session.events(),
        });
        let retained = &*callback as *const Callback;
        manager.sessions.insert(
            1,
            DocumentSession {
                session,
                latest: None,
                attempted: Some(1),
                native: Some(NativeSession {
                    provider: "test.stateful".into(),
                    instance: NonNull::dangling(),
                    input,
                    destroy: fail_session,
                    callback,
                }),
            },
        );
        manager.plugins.insert(
            "test.stateful".into(),
            Generation {
                info: PluginInfo {
                    id: "test.stateful".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: dir.path().into(),
                    description: String::new(),
                    resident_code: false,
                },
                api: PluginApi {
                    abi_version: ABI_VERSION,
                    struct_size: std::mem::size_of::<PluginApi>() as u32,
                    flags: 0,
                    id: [0; 64],
                    version: [0; 32],
                    create: None,
                    destroy: Some(destroy_instance),
                    describe: None,
                    render_markdown: None,
                    semantic_create: None,
                    semantic_input: None,
                    semantic_destroy: None,
                },
                instance: NonNull::dangling(),
                destroyed: false,
                library: None,
            },
        );
        assert!(matches!(
            manager.unload("test.stateful"),
            Some(Event::Rejected { .. })
        ));
        assert_eq!(INSTANCE_DESTROYS.load(Ordering::SeqCst), 0);
        assert!(manager.blocked.contains("test.stateful"));
        assert_eq!(manager.plugins().count(), 0);
        // SAFETY: failed retirement intentionally retained this callback. Late
        // producer calls stay valid but are silenced after deactivation.
        assert!(!unsafe { &*retained }.active.load(Ordering::Acquire));
        assert!(manager.unload("test.stateful").is_none());
        assert_eq!(INSTANCE_DESTROYS.load(Ordering::SeqCst), 0);
    }
}
