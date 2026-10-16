//! Standard service/request names shared by host and plugin adapters.
/// Paired requests for plugin-versioned restart state. Snapshot takes empty
/// input; restore receives the owned snapshot, bounded to 1 MiB by the host.
pub const LIFECYCLE_SNAPSHOT_HOOK: &str = "velk.lifecycle.snapshot.v1";
pub const LIFECYCLE_RESTORE_HOOK: &str = "velk.lifecycle.restore.v1";
/// Empty request with a bounded UTF-8 JSON `PluginMetadata` reply.
pub const METADATA_HOOK: &str = "velk.plugin.metadata.v1";
/// Resolved settings snapshot, using the existing settings JSON representation.
pub const SETTINGS_HOOK: &str = "velk.plugin.settings.v1";
pub const MARKUP_HOOK: &str = "velk.markup.render";
pub const LANGUAGE_HOOK: &str = "velk.language-server.definition";
pub const SEMANTIC_HOOK: &str = "velk.semantic.session";
