//! Native syntax sessions. No Rust allocation or parser object crosses this ABI.
use super::Bytes;
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SyntaxPoint {
    pub byte: usize,
    pub line: usize,
    pub column: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SyntaxEdit {
    pub revision: u64,
    pub replaced_start: usize,
    pub replaced_end: usize,
    pub inserted: usize,
    pub start: SyntaxPoint,
    pub old_end: SyntaxPoint,
    pub new_end: SyntaxPoint,
}
#[repr(C)]
pub struct SyntaxDocument {
    pub revision: u64,
    pub text: Bytes,
    pub edits: *const SyntaxEdit,
    pub edit_count: usize,
}
#[repr(C)]
pub struct SyntaxHighlight {
    pub start: usize,
    pub end: usize,
    pub capture: Bytes,
}
pub type SyntaxSink = unsafe extern "C" fn(*mut c_void, *const SyntaxHighlight, usize) -> i32;
/// Borrowed for one synchronous call; the plugin must not retain this callback
/// or context. Nonzero means stop work without publishing a partial result.
pub type SyntaxCancelled = unsafe extern "C" fn(*mut c_void) -> bool;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SyntaxApi {
    /// Create an independent session. Language is UTF-8; currently `rust`.
    /// Success writes one non-null session; failure retains no session/activity.
    pub create: unsafe extern "C" fn(Bytes, *mut *mut c_void) -> i32,
    /// Serialized on a background worker. Text and edits are borrowed for the
    /// call. OK means parsed; CANCELLED leaves the previous result unpublished.
    pub update: unsafe extern "C" fn(
        *mut c_void,
        *const SyntaxDocument,
        SyntaxCancelled,
        *mut c_void,
    ) -> i32,
    /// Scalar range within the last successful update. Exactly one synchronous
    /// sink call on success, even for zero captures. Order defines precedence.
    pub highlights: unsafe extern "C" fn(
        *mut c_void,
        usize,
        usize,
        SyntaxCancelled,
        *mut c_void,
        SyntaxSink,
        *mut c_void,
    ) -> i32,
    /// Called once, on the owning worker, even after provider retirement. The
    /// host pins library code until this function returns.
    pub destroy: unsafe extern "C" fn(*mut c_void) -> i32,
}

pub const SYNTAX_CANCELLED: i32 = 6;
