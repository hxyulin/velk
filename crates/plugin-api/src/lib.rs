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

/// Optional paired request hooks for owned, plugin-versioned restart state.
/// Snapshot accepts empty input; restore accepts the snapshot (at most 1 MiB).
pub const LIFECYCLE_SNAPSHOT_HOOK: &str = "velk.lifecycle.snapshot.v1";
pub const LIFECYCLE_RESTORE_HOOK: &str = "velk.lifecycle.restore.v1";

/// Optional SDK-to-host timing bridge. Callbacks are process-owned and thread
/// safe. Borrowed UTF-8 is copied before returning; no Rust objects cross the ABI.
pub const DIAGNOSTICS_HOOK: &str = "velk.diagnostics.attach.v1";
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceSink {
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
    pub attach: unsafe extern "C" fn(*const TraceSink) -> i32,
}

/// Optional JSON metadata request; empty input, UTF-8 PluginMetadata response.
/// Uses BYTES_CONTRACT v1 without changing the root native ABI.
pub const METADATA_HOOK: &str = "velk.plugin.metadata.v1";

/// Optional settings snapshot request using the v1 bytes contract.
pub const SETTINGS_HOOK: &str = "velk.plugin.settings.v1";
