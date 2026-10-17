# Completion

`velk.completion` owns the completion command and the default merge policy.
Ctrl+Space collects the `velk.completion.source` contributions of loaded plugins.
Sources are optional: neither this plugin nor LSP requires the other to load.
The separate `velk.completion.words` contributor suggests words from the active
buffer, including untitled documents, without starting a language server.

The host routes normal request or semantic commands, publishes partial result
snapshots to the reducer, and renders the returned caret suggestions. The reducer
preserves source order and source generations. Failed sources do not hide the
others. Accepting a result still uses the host's revision and view checks and
normal undoable document transactions.

Providers use the constants from `velk-completion-contracts` and declare
`Plugin::contributions()`. The protocol library is a build dependency; it does not
install or enable the default completion plugin. See `docs/completion.md` for the
boundaries and current limits.
