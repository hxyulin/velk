//! Independently owned undo sessions and immutable snapshots. Only bytes and
//! opaque pointers cross this boundary; destruction remains in the allocating image.
use crate::{ByteSink, Bytes};
use std::ffi::c_void;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UndoApi {
    pub create: unsafe extern "C" fn(u64, Bytes, *mut *mut c_void) -> i32,
    pub request: unsafe extern "C" fn(*mut c_void, Bytes, ByteSink, *mut c_void) -> i32,
    pub snapshot: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    pub encode: unsafe extern "C" fn(*mut c_void, ByteSink, *mut c_void) -> i32,
    pub destroy: unsafe extern "C" fn(*mut c_void) -> i32,
    pub destroy_snapshot: unsafe extern "C" fn(*mut c_void) -> i32,
}
pub const MAX_UNDO_BYTES: usize = 512 * 1024 * 1024;
