//! Author-side semantic session lifecycle over the native ABI.
use super::guard;
use crate::api::wire::semantic::decode_input;
pub use crate::api::wire::semantic::{decode_event, with_event, with_input};
use crate::{Plugin, api::*, semantic as model};
use std::{ffi::c_void, sync::Arc};

struct HostSink {
    callback: SemanticSink,
    context: *mut c_void,
}
// SAFETY: the ABI host explicitly supplies a thread-safe sink/context, retained
// until session destroy joins this producer. No Rust borrow crosses the ABI.
unsafe impl Send for HostSink {}
unsafe impl Sync for HostSink {}
impl HostSink {
    fn publish(&self, event: model::Event) {
        let result = with_event(&event, |raw| unsafe { (self.callback)(self.context, raw) });
        if result.is_err() {
            let error = model::Event::Status(model::Status::Failed(
                "semantic event exceeds ABI limits".into(),
            ));
            let _ = with_event(&error, |raw| unsafe { (self.callback)(self.context, raw) });
        }
    }
}
struct OwnedSession {
    service: Arc<model::Service>,
}
pub(super) unsafe extern "C" fn create<P: Plugin, F>(
    instance: *mut c_void,
    definition: *const LanguageServerDefinition,
    sink: SemanticSink,
    context: *mut c_void,
    output: *mut *mut c_void,
) -> i32
where
    F: FnMut(&mut P, crate::LanguageServer) -> Box<dyn model::Backend> + 'static,
{
    if instance.is_null() || definition.is_null() || output.is_null() {
        return INVALID;
    }
    guard(|| {
        // SAFETY: serialized lifecycle call on a live plugin instance.
        let Ok(definition) = (unsafe { super::language_server::decode(&*definition) }) else {
            return INVALID;
        };
        let binding = unsafe { &mut *instance.cast::<super::registry::Binding<P, F>>() };
        let backend = (binding.handler)(unsafe { &mut *binding.plugin }, definition);
        let host = HostSink {
            callback: sink,
            context,
        };
        let Ok(service) = model::Service::start_observed(backend, move |event| host.publish(event))
        else {
            return INVALID;
        };
        let session = Box::new(OwnedSession { service });
        unsafe {
            output.write(Box::into_raw(session).cast());
        }
        OK
    })
}
pub(super) unsafe extern "C" fn input(instance: *mut c_void, raw: *const SemanticInput) -> i32 {
    if instance.is_null() || raw.is_null() {
        return INVALID;
    }
    guard(|| {
        let Ok(input) = (unsafe { decode_input(&*raw) }) else {
            return INVALID;
        };
        let service = unsafe { &(*instance.cast::<OwnedSession>()).service };
        match input {
            model::Input::Command { request, payload } => {
                if service.command(request, payload).is_err() {
                    return INVALID;
                }
            }
            model::Input::Document(d) => service.update(d),
            model::Input::Saved(d) => service.saved(d),
            model::Input::Hover(r) => service.hover(r),
            model::Input::CancelHover => service.cancel_hover(),
        };
        OK
    })
}
pub(super) unsafe extern "C" fn destroy(instance: *mut c_void) -> i32 {
    if instance.is_null() {
        return INVALID;
    }
    guard(|| {
        // Keep ownership intact on failure: the host retains library + sink.
        let session = unsafe { &*instance.cast::<OwnedSession>() };
        if session.service.shutdown_and_join().is_err() {
            return PANICKED;
        }
        unsafe {
            drop(Box::from_raw(instance.cast::<OwnedSession>()));
        }
        OK
    })
}
