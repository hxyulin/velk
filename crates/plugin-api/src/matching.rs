use crate::Bytes;
/// Borrowed UTF-8 input and caller-owned output last only this call. The callback
/// is context-free, reentrant and must not perform I/O or wait on host services.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MatchingApi {
    pub scores: unsafe extern "C" fn(Bytes, *const Bytes, usize, *mut MatchScore) -> i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MatchScore {
    pub matched: u32,
    pub score: [u64; 4],
}
