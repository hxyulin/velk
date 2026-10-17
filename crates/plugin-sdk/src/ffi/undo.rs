//! Undo sessions are independent of the lifecycle instance and stay alive until
//! their document and immutable worker snapshots have released them.
use super::{guard, registry::Registrar};
use crate::{Plugin, api::*};
use std::{ffi::c_void, sync::Arc};
use velk_core_contracts::history::{Provider, Restore, Session, Snapshot};
impl<P: Plugin> Registrar<P> {
    pub fn undo<U: Provider + Default>(&mut self) -> Result<(), String> {
        // SAFETY: all contexts are independently owned; no pointer borrows P.
        unsafe {
            self.register_raw(
                velk_core_contracts::history::HOOK,
                velk_core_contracts::history::CONTRACT,
                1,
                SERVICE,
                UndoApi {
                    create: create::<U>,
                    request,
                    snapshot,
                    encode,
                    destroy,
                    destroy_snapshot,
                },
                (),
            )
        }
    }
}
unsafe fn input<'a>(bytes: Bytes) -> Result<&'a [u8], ()> {
    if bytes.length > MAX_UNDO_BYTES || bytes.length > 0 && bytes.data.is_null() {
        return Err(());
    }
    // SAFETY: validated borrowed-call byte view, including empty initialization.
    Ok(if bytes.length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bytes.data, bytes.length) }
    })
}
unsafe fn reply(result: Result<Vec<u8>, String>, sink: ByteSink, context: *mut c_void) -> i32 {
    let (bytes, status) = match result {
        Ok(bytes) => (bytes, OK),
        Err(error) => (error.into_bytes(), INVALID),
    };
    if bytes.len() > MAX_UNDO_BYTES {
        return INVALID;
    }
    // SAFETY: sink and context are borrowed only for this synchronous call.
    unsafe {
        sink(
            context,
            Bytes {
                data: bytes.as_ptr(),
                length: bytes.len(),
            },
            status,
        )
    }
}
unsafe extern "C" fn create<U: Provider + Default>(
    root: u64,
    archive: Bytes,
    output: *mut *mut c_void,
) -> i32 {
    guard(|| {
        if output.is_null() {
            return INVALID;
        }
        let Ok(bytes) = (unsafe { input(archive) }) else {
            return INVALID;
        };
        let provider = U::default();
        let session = if bytes.is_empty() {
            provider.open(root)
        } else {
            let Ok(archive) = serde_json::from_slice::<Restore>(bytes) else {
                return INVALID;
            };
            let Ok(session) = provider.restore(archive) else {
                return INVALID;
            };
            session
        };
        // SAFETY: opaque allocation is released by destroy in this image.
        unsafe {
            output.write(Box::into_raw(Box::new(session)).cast());
        }
        OK
    })
}
unsafe extern "C" fn request(
    session: *mut c_void,
    bytes: Bytes,
    sink: ByteSink,
    context: *mut c_void,
) -> i32 {
    guard(|| {
        if session.is_null() {
            return INVALID;
        }
        let Ok(bytes) = (unsafe { input(bytes) }) else {
            return INVALID;
        };
        // SAFETY: the caller serializes access to this uniquely owned session.
        let session = unsafe { &mut *session.cast::<Box<dyn Session>>() };
        let result = serde_json::from_slice(bytes)
            .map_err(|e| e.to_string())
            .and_then(|request| session.request(request))
            .and_then(|reply| serde_json::to_vec(&reply).map_err(|e| e.to_string()));
        unsafe { reply(result, sink, context) }
    })
}
unsafe extern "C" fn snapshot(session: *mut c_void, output: *mut *mut c_void) -> i32 {
    guard(|| {
        if session.is_null() || output.is_null() {
            return INVALID;
        }
        // SAFETY: session is serialized; the snapshot owns all its data.
        let session = unsafe { &*session.cast::<Box<dyn Session>>() };
        let Ok(snapshot) = session.snapshot() else {
            return INVALID;
        };
        unsafe {
            output.write(Box::into_raw(Box::new(snapshot)).cast());
        }
        OK
    })
}
unsafe extern "C" fn encode(snapshot: *mut c_void, sink: ByteSink, context: *mut c_void) -> i32 {
    guard(|| {
        if snapshot.is_null() {
            return INVALID;
        }
        // SAFETY: snapshots are immutable Send + Sync and remain allocated.
        let snapshot = unsafe { &*snapshot.cast::<Arc<dyn Snapshot>>() };
        let result = snapshot
            .encode()
            .and_then(|value| serde_json::to_vec(&value).map_err(|e| e.to_string()));
        unsafe { reply(result, sink, context) }
    })
}
unsafe extern "C" fn destroy(session: *mut c_void) -> i32 {
    guard(|| {
        if session.is_null() {
            return INVALID;
        }
        // SAFETY: called exactly once for this image's independently owned allocation.
        unsafe {
            drop(Box::from_raw(session.cast::<Box<dyn Session>>()));
        }
        OK
    })
}
unsafe extern "C" fn destroy_snapshot(snapshot: *mut c_void) -> i32 {
    guard(|| {
        if snapshot.is_null() {
            return INVALID;
        }
        // SAFETY: code and allocation stay pinned until the final snapshot drops.
        unsafe {
            drop(Box::from_raw(snapshot.cast::<Arc<dyn Snapshot>>()));
        }
        OK
    })
}
