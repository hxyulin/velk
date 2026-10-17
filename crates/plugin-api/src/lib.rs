//! Experimental native ABI. No Rust containers or trait objects cross it.
//! Common models live in velk-plugin-contracts; wire adapters copy native views.
use std::ffi::c_void;
pub mod undo;
pub use undo::{MAX_UNDO_BYTES, UndoApi};
pub mod contracts;
pub use contracts::*;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bytes {
    pub data: *const u8,
    pub length: usize,
}
pub const ABI_VERSION: u32 = 1;
pub const ENTRY_SYMBOL: &[u8] = b"velk_plugin_entry_v1\0";
pub const OK: i32 = 0;
pub const INCOMPATIBLE: i32 = 1;
pub const INVALID: i32 = 2;
pub const BUFFER_TOO_SMALL: i32 = 3;
pub const PANICKED: i32 = 4;
pub const NOT_FOUND: i32 = 5;
/// Process-global hooks cannot be unregistered safely; deactivate instances but
/// retain this code image for process lifetime. Reloads of the same digest reuse it.
pub const RESIDENT_CODE: u32 = 1;
/// Lifecycle/input calls run serially on the host plugin worker. Markdown borrows
/// last one call. Semantic sessions may emit from their own reactor until their
/// destroy function returns; destruction must cancel and join all session work.
/// Retain only the explicit semantic sink/context, never input pointers. No work
/// or unload-sensitive TLS destructors may outlive session/plugin destruction.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub flags: u32,
    /// UTF-8, NUL-terminated, including a terminator within the array.
    pub id: [u8; 64],
    pub version: [u8; 32],
    pub create: Option<unsafe extern "C" fn(*mut *mut c_void) -> i32>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    /// Copy UTF-8 into caller storage. Set required length even on insufficient
    /// capacity. The returned bytes contain no terminator and no plugin pointers.
    pub describe: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut usize) -> i32>,
    /// Registration discovery is fixed; feature tables live behind named contracts.
    pub hook_count: Option<unsafe extern "C" fn(*mut c_void) -> usize>,
    pub hook_info: Option<unsafe extern "C" fn(*mut c_void, usize, *mut HookInfo) -> i32>,
    pub get_hook:
        Option<unsafe extern "C" fn(*mut c_void, usize, Bytes, u32, usize, *mut c_void) -> i32>,
}
pub type Entry = unsafe extern "C" fn(u32, *mut PluginApi, usize) -> i32;

/// A pack enumerates metadata/factories, never creates instances during discovery.
/// Every member's table obeys the ordinary v1 plugin contract. The host retains
/// the whole image if any advertised member requires resident code.
pub const PACK_ENTRY_SYMBOL: &[u8] = b"velk_plugin_pack_entry_v1\0";
pub const MAX_PACK_MEMBERS: usize = 256;
pub type PackMember = unsafe extern "C" fn(usize, u32, *mut PluginApi, usize) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginPackApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub member_count: usize,
    pub member: Option<PackMember>,
}
pub type PackEntry = unsafe extern "C" fn(u32, *mut PluginPackApi, usize) -> i32;

pub use velk_plugin_contracts::{PluginId, parse_plugin_id};

pub const REQUEST: u32 = 1;
pub const EVENT: u32 = 2;
pub const SERVICE: u32 = 3;
pub const CONTRIBUTION: u32 = 4;
pub const MAX_HOOKS: usize = 256;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HookInfo {
    pub name: [u8; 128],
    pub contract: [u8; 128],
    pub version: u32,
    pub kind: u32,
    pub table_size: usize,
}

/// Optional SDK-to-host logging and timing bridge. Callbacks are process-owned and thread
/// safe. Borrowed UTF-8 is copied before returning; no Rust objects cross the ABI.
pub const DIAGNOSTICS_HOOK: &str = "velk.diagnostics.attach.v1";
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DiagnosticsSink {
    /// Levels: 1 error, 2 warn, 3 info, 4 debug, 5 trace.
    pub log_enabled: unsafe extern "C" fn(level: u32) -> bool,
    /// Target and message must be valid UTF-8, at most 256 and 8192 bytes.
    /// Views last only this call. The host owns filtering and output.
    pub log: unsafe extern "C" fn(level: u32, target: Bytes, message: Bytes),
    pub enabled: unsafe extern "C" fn() -> bool,
    pub now_ns: unsafe extern "C" fn() -> u64,
    pub next_id: unsafe extern "C" fn() -> u64,
    /// kind: 0 complete CPU scope, 1 instant, 2 async begin, 3 async end.
    /// Async ends retain original start_ns; duration_ns gives their end offset.
    pub record: unsafe extern "C" fn(
        kind: u32,
        id: u64,
        name: Bytes,
        target: Bytes,
        start_ns: u64,
        duration_ns: u64,
    ),
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DiagnosticsApi {
    pub attach: unsafe extern "C" fn(*const DiagnosticsSink) -> i32,
}

pub use velk_plugin_contracts::hooks::*;

#[allow(unsafe_code)]
pub mod wire;

/// Thread-safe host callbacks. Attach borrows the table; retaining the opaque
/// context keeps it allocated after retirement, when calls return an error.
/// Requests copy JSON and invoke the sink exactly once before returning. They
/// may block on the UI: never call from UI/interactive editing callbacks.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostServices {
    pub context: *mut c_void,
    pub retain: unsafe extern "C" fn(*mut c_void),
    pub release: unsafe extern "C" fn(*mut c_void),
    pub request: unsafe extern "C" fn(*mut c_void, Bytes, ByteSink, *mut c_void) -> i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub context: *mut c_void,
    pub attach: unsafe extern "C" fn(*mut c_void, *const HostServices) -> i32,
}

mod syntax;
pub use syntax::*;

mod matching;
pub use matching::*;

/// Reentrant UI-local presentation. JSON views are bounded by the frontend contract;
/// all pointers borrow caller storage for this call, with no retained context.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CompletionFrontendApi {
    pub present: unsafe extern "C" fn(Bytes, *mut u8, usize, *mut usize) -> i32,
}
