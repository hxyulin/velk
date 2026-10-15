# Search

The first search capability operates on the current open buffer, including
unsaved edits. Matching is supplied by the bundled `velk.search.literal` native
plugin. The `velk-search` crate contains UI-independent query/result types,
validation, bounds, and the v1 wire protocol. It depends on neither RXUI nor the
search engine. The root plugin ABI and `Plugin` trait are unchanged.

Command+F on macOS or Control+F elsewhere opens and focuses the host find bar.
A small existing selection seeds the query. Search is literal, case-insensitive
by default, with Match case and Whole word options. Enter/Shift+Enter or the
Next/Previous buttons wrap through results. Command/Control+G and its Shift
variant also navigate while focus is within the find bar. Escape closes the bar
and returns focus to the editor. Navigation selects and reveals the active match;
it does not edit the document or add undo history. Match endpoints inside an
extended grapheme expand to its outer boundaries. Non-active matches are not
painted in this first version, so search does not replace semantic annotations.

`search_backend` in the existing settings JSON selects a plugin ID; its default is
`velk.search.literal`. A provider must register the named byte request
`velk.search.buffer.v1`. Missing/unloaded providers produce an inline notice;
there is no search engine fallback linked into the app. Plugin lifecycle changes
refresh an open search. Settings are read at startup as described in
[editor-shell.md](editor-shell.md).

## Scheduling and lifetime

Only a cheap Rope snapshot clone is taken on the UI thread. The host's background
task debounces query/document changes for 50 ms, binds the configured provider,
and uploads UTF-8 in at most 64 KiB chunks. JSON encoding, upload, result decoding,
and range validation all run off the UI thread. Bounded JSON byte requests reuse
the existing generic C ABI. A future binary adapter can replace this encoding
without changing the neutral query/result model.

Begin supersedes older work. Append streams one buffer snapshot; Start queues
matching on one plugin-owned worker. The shared plugin dispatch worker only
handles short upload/control/poll calls. The host checks Poll every 8 ms only
while a search is active, with a 15-second polling deadline. There is no idle
polling or per-frame plugin call. Poll consumes the ready result. Cancel retires
the matching ID; stale cancellation cannot cancel a newer request. Closing or
changing the query cancels the host task and requests backend cancellation.
Plugin destruction cancels pending work and joins its worker before unload.

Requests/results carry an ID and document revision. The host also checks the
current revision before applying results or navigating, so late results cannot
move the selection in newer text. Closing search prevents results from reopening
it. Result storage belongs to the host independently of the loaded library.

Buffers are limited to 64 MiB, queries to 1024 UTF-8 bytes, and results to 10,000
ordered, non-overlapping Unicode scalar ranges. The UI reports `+` when more
matches exist. The backend uses the Rust `regex` crate with an escaped literal,
Unicode case matching and Unicode word boundaries; it offers no regex query
syntax. Canonically equivalent composed/decomposed text is not normalized.
Regex compilation memory is bounded. Cancellation is checked between matches;
an in-progress no-match scan can finish before cancellation is observed. Unload
waits on the background plugin worker, never the UI thread.

## Frontends and backends

The capability defines search data and lifecycle. A backend plugin owns matching
policy and execution. The current frontend is a small host adapter using RXUI
controls; it does not implement the search algorithm. A future frontend plugin
could contribute controls, actions, result layouts, and navigation behavior using
the host's eventual UI contribution contract. No Rust view object or callback is
passed through the C ABI, and no frontend plugin API is claimed here.

Project search needs a separate scope/resource model, streaming batches, file
identity, previews, and workspace lifecycle. It should share appropriate query
types without pretending those semantics are equivalent to buffer ranges. Files,
indexing, and project search remain optional plugin features.
