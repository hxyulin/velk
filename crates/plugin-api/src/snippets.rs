use crate::Bytes;
/// Context-free, reentrant local service. JSON input and output storage are
/// borrowed only for this call. Output encodes `Result<Response, String>`. No host callbacks or ambient I/O are permitted.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SnippetApi {
    pub evaluate: unsafe extern "C" fn(Bytes, *mut u8, usize, *mut usize) -> i32,
}
