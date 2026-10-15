//! Experimental native ABI. No Rust containers or trait objects cross it.
use std::ffi::c_void;
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
