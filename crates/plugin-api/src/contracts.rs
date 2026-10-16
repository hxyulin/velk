//! Built-in typed contracts layered on the generic registry. New contract
//! packages can define additional C tables without extending PluginApi.
use super::*;
pub use velk_plugin_contracts::hooks::{LANGUAGE_HOOK, MARKUP_HOOK, SEMANTIC_HOOK};
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
pub struct StyledRun {
    pub text: Bytes,
    pub link: Bytes,
    pub flags: u32,
    pub heading: u8,
}
pub type RunSink = unsafe extern "C" fn(*mut c_void, *const StyledRun, usize) -> i32;
pub type RenderMarkdown =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, RunSink, *mut c_void) -> i32;

// Internal v1 semantic session protocol. All views are borrowed for their call.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
pub const DOCUMENT: u32 = 1;
pub const SAVED: u32 = 2;
pub const HOVER: u32 = 3;
pub const CANCEL_HOVER: u32 = 4;
pub const COMMAND: u32 = 5;
pub const COMMAND_RESULT: u32 = 5;
pub const STATUS: u32 = 1;
pub const DIAGNOSTICS: u32 = 2;
pub const HOVER_RESULT: u32 = 3;
pub const REQUEST_FAILED: u32 = 4;
pub const STARTING: u32 = 1;
pub const READY: u32 = 2;
pub const FAILED: u32 = 3;
pub const STOPPED: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SemanticRequest {
    pub id: u64,
    pub revision: u64,
    pub position: usize,
}
#[repr(C)]
/// Path is native Unix bytes on Unix, UTF-8 elsewhere. Other views are UTF-8.
pub struct SemanticDocument {
    pub path: Bytes,
    pub language: Bytes,
    pub text: Bytes,
    pub revision: u64,
}
#[repr(C)]
pub struct SemanticInput {
    pub payload: Bytes,
    pub kind: u32,
    pub document: *const SemanticDocument,
    pub request: SemanticRequest,
}
#[repr(C)]
pub struct SemanticDiagnostic {
    pub start: usize,
    pub end: usize,
    pub severity: u32,
    pub message: Bytes,
    pub source: Bytes,
    pub code: Bytes,
}
#[repr(C)]
pub struct SemanticEvent {
    pub payload: Bytes,
    pub kind: u32,
    pub status: u32,
    pub capabilities: u32,
    pub revision: u64,
    pub request: SemanticRequest,
    pub text: Bytes,
    pub start: usize,
    pub end: usize,
    pub has_range: u32,
    pub has_hover: u32,
    pub markdown: u32,
    pub diagnostics: *const SemanticDiagnostic,
    pub diagnostic_count: usize,
}
/// May run on a plugin reactor thread. Host must synchronously copy borrowed
/// event data. Context stays valid through successful semantic_destroy, which
/// must join producers and finish all callbacks before returning.
pub type SemanticSink = unsafe extern "C" fn(*mut c_void, *const SemanticEvent) -> i32;
/// On success, return one non-null session. Failure must leave no retained sink
/// or running producer. Session functions are serialized by the host.
pub type CreateSemantic = unsafe extern "C" fn(
    *mut c_void,
    *const LanguageServerDefinition,
    SemanticSink,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
pub type SendSemantic = unsafe extern "C" fn(*mut c_void, *const SemanticInput) -> i32;
pub type DestroySemantic = unsafe extern "C" fn(*mut c_void) -> i32;

/// UTF-8 configuration borrowed only for query callback / session creation.
#[repr(C)]
pub struct LanguageServerDefinition {
    pub transport: Bytes,
    pub language: Bytes,
    pub executable: Bytes,
    pub arguments: *const Bytes,
    pub argument_count: usize,
    pub root: Bytes,
    pub initialization_options: Bytes,
    pub configuration: Bytes,
    /// Nonempty only for a failed definition query; other fields are then empty.
    pub error: Bytes,
}
pub type LanguageServerSink =
    unsafe extern "C" fn(*mut c_void, *const LanguageServerDefinition) -> i32;
pub type QueryLanguageServer =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, LanguageServerSink, *mut c_void) -> i32;

pub const MARKUP_CONTRACT: &str = "velk.contract.markup-render";
pub const LANGUAGE_CONTRACT: &str = "velk.contract.language-server";
pub const SEMANTIC_CONTRACT: &str = "velk.contract.semantic-session";
pub const BYTES_CONTRACT: &str = "velk.contract.bytes-request";
pub const EVENT_CONTRACT: &str = "velk.contract.bytes-event";
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MarkupApi {
    pub context: *mut c_void,
    pub render: RenderMarkdown,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LanguageApi {
    pub context: *mut c_void,
    pub query: QueryLanguageServer,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemanticApi {
    pub context: *mut c_void,
    pub create: CreateSemantic,
    pub input: SendSemantic,
    pub destroy: DestroySemantic,
}
pub type ByteSink = unsafe extern "C" fn(*mut c_void, Bytes, i32) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BytesApi {
    pub context: *mut c_void,
    pub call: unsafe extern "C" fn(*mut c_void, Bytes, ByteSink, *mut c_void) -> i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EventApi {
    pub context: *mut c_void,
    pub notify: unsafe extern "C" fn(*mut c_void, Bytes) -> i32,
}
