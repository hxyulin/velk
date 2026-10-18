# Completion

`velk.completion` owns the completion command and the default merge policy.
Ctrl+Space collects the `velk.completion.source` contributions of loaded plugins.
Sources are optional: neither this plugin nor LSP requires the other to load.
The separate `velk.completion.words` contributor suggests words from the active
buffer, including untitled documents, without starting a language server.

The host routes normal request or semantic commands, publishes partial result
snapshots to the reducer, and renders the returned caret suggestions. The reducer
sorts by provider ranking keys and preserves source generations and candidate identity. Failed sources do not hide the
others. Accepting a result still uses the host's revision and view checks and
normal undoable document transactions.

Providers use the constants from `velk-completion-contracts` and declare
`Plugin::contributions()`. The protocol library is a build dependency; it does not
load an independently installed completion plugin. See `docs/completion.md` for the
boundaries and current limits.

Automatic completion starts after two identifier characters or a supported
provider trigger character, with a 150 ms initial debounce. The `automatic` and
`typing` settings can disable automatic invocation or restrict it to character
triggers. Ctrl+Space always remains available. LSP checks character triggers
against its running server; the word source does not activate on punctuation.
Providers supply separate display titles, filter text, sort text and stable IDs.

Selected suggestions can show plain-text or Markdown documentation in a bounded,
scrollable panel. Markdown uses the editor's selected markup provider. LSP fetches
missing documentation only for the highlighted item, without applying completion
edits until acceptance.

Matching prioritizes exact text and prefixes, tolerates small typos (including
adjacent swaps), and supports fuzzy subsequences through Nucleo. Automatic
selection follows the best match; explicit keyboard navigation stays stable as
sources finish. The word source applies the same scoring before its result cap.

The matching service is replaceable separately through the editor's
`completion_matching_provider` setting. The bundled matcher runs locally through
a pinned native lease; typing does not wait for the plugin worker.

LSP function completions can insert snippets. Tab and Shift+Tab navigate numbered
placeholders, repeated placeholders edit together, and the final stop returns to
normal editing. Escape or undo ends the session. Imports and snippet insertion
remain one checked undoable transaction. Snippet variables resolve from explicit
provider context. Choice placeholders open a keyboard/pointer menu, and bounded
Rust-regex transforms update derived occurrences when leaving their source stop.
See `docs/completion.md` for syntax and resolver limits.

`source_order` selects provider priority using comma-separated plugin IDs; the
default prefers LSP over document words. `deduplicate` keeps the preferred copy
of equivalent concrete edits across sources. Different imports, overload details,
and unresolved actions stay separate. Restart the completion plugin after changing
these settings. Similarity remains the primary ranking criterion.

Incomplete LSP lists retain a source-local continuation so further typing uses
trigger kind 3 until the server returns a complete list. Dismissal, source retirement,
and a fresh completion session do not reuse the old continuation.

Presentation is independently replaceable through the editor setting
`completion_frontend`. The default `velk.completion` frontend displays details and
documentation; `velk.completion.compact` displays up to ten title-only rows.
Changes apply to the next completion session. Both use the same providers,
matching, keyboard behavior, and checked edit application. Custom native plugins
register a stateless `commands::completion::frontend::Provider` with
`Registrar::completion_frontend`; no RXUI types cross the native boundary.

Contributors may declare punctuation commit characters on their items. The host
accepts a ready selection before replaying a typed character after the edit
succeeds, preserving snippet placeholders and normal autopair behavior. Signature
help is a separate LSP-declared command (Ctrl+Shift+Space), independent of this plugin.
