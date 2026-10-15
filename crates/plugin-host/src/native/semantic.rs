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
    profile: String,
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
#[derive(Clone, PartialEq, Eq)]
struct ResolutionKey {
    language: String,
    plugins: Vec<(String, u64)>,
}
pub(super) struct DocumentSession {
    session: Session,
    latest: Option<DocumentSnapshot>,
    native: Option<NativeSession>,
    attempted: Option<ResolutionKey>,
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
            if let Some(snapshot) = &doc.latest {
                let key = ResolutionKey {
                    language: snapshot.language.clone(),
                    plugins: self
                        .plugins
                        .values()
                        .filter(|p| !p.destroyed && (p.language.is_some() || p.semantic.is_some()))
                        .map(|p| (p.info.id.clone(), p.info.generation))
                        .collect(),
                };
                if doc.attempted.as_ref() != Some(&key) {
                    doc.attempted = Some(key.clone());
                    let retired = if let Some(native) = doc.native.take() {
                        let provider = native.provider.clone();
                        if native.retire() {
                            true
                        } else {
                            self.quarantine_semantic_provider(&provider);
                            false
                        }
                    } else {
                        true
                    };
                    doc.session.events().clear();
                    let result = if !retired {
                        Err("previous semantic session failed shutdown; restart required".into())
                    } else {
                        self.resolve_language_server(&key.language)
                            .and_then(|definition| {
                                let Some((profile, definition)) = definition else {
                                    return Ok(None);
                                };
                                let plugin = self.plugins.get_mut(&definition.transport).unwrap();
                                let api = plugin.semantic.unwrap();
                                let mut callback = Box::new(Callback {
                                    active: AtomicBool::new(true),
                                    events: doc.session.events(),
                                });
                                let mut instance = std::ptr::null_mut();
                                doc.session
                                    .events()
                                    .publish(SemanticEvent::Status(Status::Starting));
                                let status =
                                    velk_plugin_sdk::ffi::language_server::with_definition(
                                        &definition,
                                        |raw| {
                                            // SAFETY: live transport, copied definition borrowed
                                            // for this call, stable callback retained until join.
                                            unsafe {
                                                (api.create)(
                                                    api.context,
                                                    raw,
                                                    emit,
                                                    (&mut *callback as *mut Callback).cast(),
                                                    &mut instance,
                                                )
                                            }
                                        },
                                    )?;
                                if status != OK || instance.is_null() {
                                    return Err(format!(
                                        "{}: semantic session creation failed ({status})",
                                        definition.transport
                                    ));
                                }
                                Ok(Some(NativeSession {
                                    provider: definition.transport,
                                    profile,
                                    instance: NonNull::new(instance).unwrap(),
                                    input: api.input,
                                    destroy: api.destroy,
                                    callback,
                                }))
                            })
                    };
                    match result {
                        Ok(Some(native)) => {
                            if let Err(error) = native.send(&Input::Document(snapshot.clone())) {
                                doc.session
                                    .events()
                                    .publish(SemanticEvent::Status(Status::Failed(error)));
                            }
                            doc.native = Some(native);
                        }
                        Ok(None) => doc
                            .session
                            .events()
                            .publish(SemanticEvent::Status(Status::Stopped)),
                        Err(error) => doc
                            .session
                            .events()
                            .publish(SemanticEvent::Status(Status::Failed(error))),
                    }
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
        let mut failed_providers = Vec::new();
        for doc in self.sessions.values_mut() {
            if doc
                .native
                .as_ref()
                .is_some_and(|native| native.provider == id || native.profile == id)
            {
                let native = doc.native.take().unwrap();
                let provider = native.provider.clone();
                if !native.retire() {
                    success = false;
                    failed_providers.push(provider);
                }
                doc.attempted = None;
                doc.session.events().clear();
                doc.session
                    .events()
                    .publish(SemanticEvent::Status(Status::Stopped));
            }
        }
        if !success {
            for provider in failed_providers {
                self.quarantine_semantic_provider(&provider);
            }
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
                attempted: None,
                native: Some(NativeSession {
                    provider: "test.stateful".into(),
                    profile: "test.profile".into(),
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
                    metadata: Default::default(),
                    resident_code: false,
                    hooks: vec![],
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
                    hook_count: None,
                    hook_info: None,
                    get_hook: None,
                },
                instance: NonNull::dangling(),
                destroyed: false,
                hooks: vec![],
                markdown: None,
                language: None,
                semantic: None,
                library: None,
            },
        );
        // Removing a definition must quarantine the actual transport too if
        // joining one of its dependent sessions fails.
        let transport = manager.plugins.get("test.stateful").unwrap();
        let profile = Generation {
            info: PluginInfo {
                id: "test.profile".into(),
                ..transport.info.clone()
            },
            api: transport.api,
            instance: NonNull::dangling(),
            destroyed: false,
            hooks: vec![],
            markdown: None,
            language: None,
            semantic: None,
            library: None,
        };
        manager.plugins.insert("test.profile".into(), profile);
        assert!(matches!(
            manager.unload("test.profile"),
            Some(Event::Rejected { .. })
        ));
        assert_eq!(INSTANCE_DESTROYS.load(Ordering::SeqCst), 0);
        assert!(manager.blocked.contains("test.stateful"));
        assert!(manager.blocked.contains("test.profile"));
        assert_eq!(manager.plugins().count(), 0);
        // SAFETY: failed retirement intentionally retained this callback. Late
        // producer calls stay valid but are silenced after deactivation.
        assert!(!unsafe { &*retained }.active.load(Ordering::Acquire));
        assert!(matches!(
            manager.unload("test.stateful"),
            Some(Event::Rejected { .. })
        ));
        assert!(manager.unload("test.stateful").is_none());
        assert_eq!(INSTANCE_DESTROYS.load(Ordering::SeqCst), 0);
    }
}
