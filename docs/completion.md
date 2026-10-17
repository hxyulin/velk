# Completion providers

`velk.completion` is the bundled completion reducer and owns
`velk.completion.complete` (Ctrl+Space). `velk.completion.words` and `velk.lsp`
contribute independently. Neither source requires the completion plugin to load,
and completion does not require either source. A replacement reducer can consume
the same topic without copying provider implementations.

## Boundaries

| Component | Responsibility |
| --- | --- |
| `plugins/completion/contracts` | Completion topic and default command/hook names |
| Completion plugin | Default merge policy and source labels |
| Contributor plugins | Produce candidates and deferred resolve payloads |
| Command contracts | Feature-neutral contribution declarations, collection snapshots, and choices |
| Application adapter | Discover loaded sources, schedule commands, cancel, and validate source/view/revision/generation |
| Editor widget | Caret popup, local prefix filtering, selection, keyboard input, and accessibility |
| Core | Authoritative text and normal undoable transactions |

The application and platform SDK do not depend on `velk-completion-contracts`.
The default native popup remains a host presentation adapter. This change makes
providers and merge policy replaceable; arbitrary plugin-rendered popups are not
part of this API.

## Optional integration versus dependencies

There are three distinct relationships:

- A Rust build dependency on `velk-completion-contracts` supplies protocol constants.
  It does not load a plugin or require the default completion implementation.
- A required manifest dependency is necessary for a plugin to work and participates
  in installation, loading, unloading, and orphan calculation.
- A contribution is an optional integration discovered among already loaded
  plugins. It does not install, enable, or retain its producer or consumer.

Completion uses the third relationship. Adding optional `velk.lsp` manifest edges
would tie completion to a specific source and require maintaining a list of every
community provider. Contribution topics allow providers unknown to the reducer
author to participate without dependency cycles or reverse dependencies.

Rust Analyzer still requires `velk.lsp` to run its server. Its completions arrive
through LSP's single contributor, using the existing workspace connection, not a
second Rust Analyzer process. Disabling completion leaves diagnostics and other
LSP commands available. Disabling LSP leaves document-word completion available.

## Writing a source

Implement `Plugin::contributions()` and return a `commands::contributions::Contribution`
with topic `velk_completion_contracts::TOPIC`. Its command uses either:

- `Handler::Request { hook }`, receiving the ordinary serialized command `Context`;
- `Handler::Semantic { payload }`, routed to the active document's semantic service.

Return `Outcome::Completions { items }`. Candidates contain a title, optional
detail, and an edit or deferred semantic action. Buffer edits carry a buffer ID and
revision and work for untitled documents. Semantic actions can lazily resolve
additional edits, such as imports, before applying a transaction. Contributor
commands cannot prompt, bind shortcuts, open pickers, or recursively collect.

A source should use explicit language restrictions where appropriate. Semantic
sources are unavailable when the document has no ready semantic service. The
built-in word source reads a bounded host buffer snapshot and checks its revision;
it never reads the saved file as a substitute for unsaved text.

## Collection and lifetime

`Handler::Collect { topic, hook }` discovers up to 32 matching loaded declarations.
Request contributors run independently; semantic contributors share the document's
existing command slot and run serially. The reducer receives
`commands::contributions::Collection` snapshots as each source finishes. Results
include authenticated plugin IDs, generations, command IDs, and isolated errors.
The host coalesces reducer updates while a previous reducer call is running.
Oversized batches share the native 1 MiB request budget; candidate tails are
trimmed before transport so one large source does not block the others.

The default reducer keeps declaration order, adds source details, and caps the
combined list at 1,000 items. It wraps candidate actions in `Contributed` so a
subsequent resolve or edit checks the actual source generation. Results from a
retired source cannot be accepted. Equal labels from different sources remain
separate, and selection survives incremental updates by label and source.

Typing inside the identifier prefix starts a fresh collection against the new
revision. Old choices can remain visible while refreshing but cannot be accepted.
Moving away, dismissing, switching documents/views, or closing the document
cancels the collection. Terminal semantic events release the command slot even
when their collection has already been replaced. Results are guarded by the
originating buffer, view, path, revision, caret, and reducer generation.

## Current limits

Completion is explicitly invoked. There is no automatic trigger-character policy,
snippet expansion, item documentation, cross-provider deduplication, configurable
source priority, or `filterText`/`sortText` metadata in the common choice model yet.
The host popup filters titles by identifier prefix. LSP retains its existing
server-provided ordering within its batch. Plugin lifecycle operations dismiss the collection. Provider generation checks at
acceptance also reject results from a source replaced outside that flow.

The first extraction deliberately retains shared popup/session mechanics in the
platform. A separate frontend protocol is appropriate when there is a concrete
second presentation, rather than moving RXUI types across the plugin boundary.
