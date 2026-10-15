//! Experimental native ABI. No Rust containers or trait objects cross it.
use std::ffi::c_void;
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
    /// Optional Markdown capability. The input and sink context are borrowed for
    /// this call only. Emit one batch synchronously; no callback may escape.
    pub render_markdown: Option<RenderMarkdown>,
    /// Synchronous, borrowed definition query. The host copies the result.
    pub language_server: Option<QueryLanguageServer>,
    pub semantic_create: Option<CreateSemantic>,
    pub semantic_input: Option<SendSemantic>,
    pub semantic_destroy: Option<DestroySemantic>,
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

// Internal v1 semantic session protocol. All views are borrowed for their call.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
pub const DOCUMENT: u32 = 1;
pub const SAVED: u32 = 2;
pub const HOVER: u32 = 3;
pub const CANCEL_HOVER: u32 = 4;
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

/// Borrowed interpretation of author.[namespace.]name. Namespace may contain dots;
/// nesting has no implicit dependency, permissions, or ownership semantics.
#[derive(Debug, PartialEq, Eq)]
pub struct PluginId<'a> {
    pub author: &'a str,
    pub namespace: Option<&'a str>,
    pub name: &'a str,
}
pub fn parse_plugin_id(id: &str) -> Option<PluginId<'_>> {
    if id.len() >= 64 {
        return None;
    }
    if id.split('.').any(|s| {
        s.is_empty()
            || !s.as_bytes()[0].is_ascii_lowercase()
            || !s
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
    }) {
        return None;
    }
    let first = id.find('.')?;
    let last = id.rfind('.')?;
    Some(PluginId {
        author: &id[..first],
        namespace: (first != last).then(|| &id[first + 1..last]),
        name: &id[last + 1..],
    })
}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_flat_and_nested_namespaces_and_rejects_ambiguous_ids() {
        assert_eq!(
            parse_plugin_id("velk.markdown"),
            Some(PluginId {
                author: "velk",
                namespace: None,
                name: "markdown"
            })
        );
        assert_eq!(
            parse_plugin_id("velk.language-server.rust-analyzer"),
            Some(PluginId {
                author: "velk",
                namespace: Some("language-server"),
                name: "rust-analyzer"
            })
        );
        assert_eq!(
            parse_plugin_id("author.tools.language.server.rust"),
            Some(PluginId {
                author: "author",
                namespace: Some("tools.language.server"),
                name: "rust"
            })
        );
        for id in [
            "",
            "velk",
            ".velk.rust",
            "velk.rust.",
            "velk..rust",
            "Velk.rust",
            "velk.世界",
            "velk.a/b",
            "velk.-rust",
            "velk.1rust",
        ] {
            assert!(parse_plugin_id(id).is_none(), "accepted {id}");
        }
        assert!(parse_plugin_id(&format!("velk.{}", "a".repeat(59))).is_none());
    }
}
