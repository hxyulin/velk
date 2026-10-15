//! Experimental native ABI. No Rust containers or trait objects cross it.
use std::ffi::c_void;
pub const ABI_VERSION: u32 = 1;
pub const ENTRY_SYMBOL: &[u8] = b"velk_plugin_entry_v1\0";
pub const OK: i32 = 0;
pub const INCOMPATIBLE: i32 = 1;
pub const INVALID: i32 = 2;
pub const BUFFER_TOO_SMALL: i32 = 3;
pub const PANICKED: i32 = 4;
/// All functions run serially on the plugin worker. Callers provide valid buffers
/// and instances. Plugins must not retain borrowed pointers, spawn background work,
/// or register thread-local destructors: v1 has no asynchronous lifetime protocol.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub abi_version: u32,
    pub struct_size: u32,
    /// UTF-8, NUL-terminated, including a terminator within the array.
    pub id: [u8; 64],
    pub version: [u8; 32],
    pub create: Option<unsafe extern "C" fn(*mut *mut c_void) -> i32>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    /// Copy UTF-8 into caller storage. Set required length even on insufficient
    /// capacity. The returned bytes contain no terminator and no plugin pointers.
    pub describe: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut usize) -> i32>,
    /// Optional Markdown capability. The input and sink context are borrowed for
    /// this call only. Emit one batch synchronously; no callback may escape.
    pub render_markdown: Option<RenderMarkdown>,
}
pub type Entry = unsafe extern "C" fn(u32, *mut PluginApi, usize) -> i32;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RUNS: usize = 65_536;
pub const BOLD: u32 = 1;
pub const ITALIC: u32 = 2;
pub const CODE: u32 = 4;
pub const STRIKETHROUGH: u32 = 8;
pub const LINK: u32 = 16;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bytes {
    pub data: *const u8,
    pub length: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StyledRun {
    pub text: Bytes,
    pub link: Bytes,
    pub flags: u32,
    pub heading: u8,
}
pub type RunSink = unsafe extern "C" fn(*mut c_void, *const StyledRun, usize) -> i32;
pub type RenderMarkdown =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, RunSink, *mut c_void) -> i32;
