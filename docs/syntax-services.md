# Syntax services

Syntax highlighting is a replaceable service. `velk-syntax` defines the shared
Rust provider/session interfaces, revisioned worker requests, viewport demand,
and immutable indexed highlight results. It has no parser or grammar dependency.
The contract is also available as `velk_plugin_contracts::syntax` and
`velk_plugin_sdk::syntax`.

The bundled `velk.tree-sitter` plugin implements Rust highlighting. It owns
Tree-sitter, the Rust grammar and highlight queries, incremental parse trees, and
capture precedence. The editor executable does not link these implementations.
The plugin is enabled by default and can be disabled, unloaded, or replaced.
Select its replacement with the user-wide `syntax_provider` setting. Missing or
disabled providers leave text unhighlighted; semantic services remain available.

## Ownership and scheduling

The document remains authoritative in Core. A syntax session receives a revision,
a text snapshot, and the available parser-independent edits. It must fall back
to a fresh parse when incremental history is incomplete. Offsets in highlights
are Unicode scalar positions; edit points also include UTF-8 byte coordinates.

Each syntax-enabled buffer owns a coalescing background worker and an independent
provider session. Opening, updating, querying, and destroying a session happen on
that worker, including grammar initialization. Document and viewport mailboxes
retain only the latest requests. Newer document work cancels stale parsing;
newer viewport work cancels stale queries without restarting parsing. No parsing,
querying, or native text transfer runs on the UI thread or the general plugin
lifecycle worker.

The worker queries requested viewport ranges and publishes immutable indexed
captures. The widget performs interval lookups, projects highlights around newer
edits and IME preedit, and applies host styling. Semantic annotations are merged
through the existing annotation model. Syntax providers emit capture names rather
than renderer colors or RXUI objects.

Loading or replacing a provider refreshes existing buffers. Unchanged plugin
generations preserve session identity. Provider changes discard cached syntax
and create new workers; late results are checked against worker identity,
document revision, and viewport generation. Retirement rejects further calls on
old sessions. Their library code stays pinned until session destruction finishes.

## Native SDK

Implement `syntax::Provider` and `syntax::Session`, then register a default,
reentrant factory with `registrar.syntax::<Factory>()`. The factory is independent
of the plugin instance. Every session has one serialized owner and must implement
`Send`; it never accesses editor UI state.

The named service is `velk.syntax.session`, with contract
`velk.contract.syntax-session`, version 1. `SyntaxApi` in `velk-plugin-api`
translates between the safe Rust interfaces and a C table. Only function pointers,
opaque session pointers, scalar coordinates, and borrowed byte/array views cross
the boundary. Rust traits, Rope allocations, and syntax trees stay within their
own image. Calls and callbacks are synchronous; the provider must not retain
borrowed input, cancellation callbacks, or result sinks. The SDK catches panics
at the boundary. Native plugins remain trusted code.

The native adapter currently copies the full UTF-8 document on each background
update, including incremental ones, with a 64 MiB limit and up to 4,096 edit
records. Incremental edits reuse the parse tree. Queries return at most 65,536
captures per viewport range; names are bounded to 128 ASCII identifier bytes.
The host validates returned ranges and capture names before publication. This
copying cost is off the input thread, but remains a consideration for large files.

Syntax highlighting and semantic services are separate contracts: syntax supplies
fast viewport colors, while `velk-semantic` supplies diagnostics, hover, and
language-server actions. Both can operate independently of Tree-sitter.
