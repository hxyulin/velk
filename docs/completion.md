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
| Completion plugin | Default merge policy, source labels, and replaceable similarity matcher |
| Contributor plugins | Declare activation support and produce candidates and deferred resolve payloads |
| Command contracts | Feature-neutral contribution declarations, collection snapshots, and choices |
| Application adapter | Discover loaded sources, schedule commands, cancel, and validate source/view/revision/generation |
| Editor model | View-local snippet sessions and mirrored placeholder editing |
| Editor widget | Caret popup, local matching-service calls, selection, keyboard input, and accessibility |
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

The default reducer uses each candidate's `sort_text` for ascending lexical order,
keeps declaration order for ties, adds source details, and caps the combined list
at 1,000 items. It wraps candidate actions in `Contributed` so a
subsequent resolve or edit checks the actual source generation. Results from a
retired source cannot be accepted. Equal labels remain separate. Explicit selection follows the source generation, command, and candidate ID across
incremental updates, even when display labels change. Automatic selection follows
the best similarity score; equal-quality arrivals preserve the current choice.

Typing inside the identifier prefix starts a fresh collection against the new
revision. Old choices can remain visible while refreshing but cannot be accepted.
Moving away, dismissing, switching documents/views, or closing the document
cancels the collection. Terminal semantic events release the command slot even
when their collection has already been replaced. Results are guarded by the
originating buffer, view, path, revision, caret, and reducer generation.

## Candidate metadata

`Outcome::Completions` carries `velk_completion_contracts::Item` values. Each has a
normal `choice` (display title, detail and validated action), plus:

- `id`: provider-owned identity stable across refreshes; exclude revision-specific
  edits and transient resolve tokens. Distinguish candidates with equal labels.
- `filter_text`: text matched against the current identifier prefix, independently
  of the display title. Matching supports exact text, prefixes, close typos and fuzzy
  subsequences.
- `sort_text`: provider ranking key, compared lexically by the default reducer.

The platform's completion item is a presentation contract, separate from generic
picker choices. `Item::from(choice)` uses the title for all three fields when a
source has no distinct metadata. The reducer namespaces IDs by contributor command;
source plugin and generation are retained in the action. LSP preserves server
`filterText` and `sortText` and derives identity from label, detail, kind and label
details. Document words use their text as identity. Metadata is bounded and
validated before presentation.

## Similarity ranking

The bundled completion plugin ranks against `filter_text`, independently of display labels:

1. Exact case-sensitive text, then exact case-insensitive text.
2. Prefix matches, preferring shorter completions.
3. Close typo matches, including adjacent swaps, missing or extra characters, and
   substitutions. Fewer edits win, then shorter length difference.
4. Fuzzy subsequences scored by Nucleo, the matcher also used by Quick Open.

Provider `sort_text` order breaks equal-quality matches. An empty query keeps
provider order. Typo tolerance permits one edit from four characters and two from
eight; shorter queries use exact/prefix/subsequence matching to avoid typo noise.
Unicode matching is supported. Typo distance compares the query with a candidate
prefix so an untyped completion suffix is not treated as an error. Fuzzy and typo
work is bounded to 128 query characters; longer identifiers still support exact
and prefix matches.

The `completion_matching_provider` preference selects the local ranking service,
independently of the collector and contributors. `velk.completion` is the default.
A replacement implements `commands::completion::matching::Policy` and registers
with `Registrar::matching::<Policy>()`. Return one optional four-part score per
candidate: `None` filters it out, lower scores rank first, and equal scores retain
provider order. The editor owns selection and revision guards regardless of policy.

The native service receives borrowed UTF-8 query/candidate views and caller-owned
score storage through `MatchingApi`. It runs synchronously on the caller with no
worker queue, JSON encoding, LSP request, or host-service call. Policies must be
reentrant and perform bounded CPU work only; native plugins remain trusted code.
The host pins the library, excludes retirement during calls, validates result
shape, and falls back to case-insensitive prefix filtering on failure or retirement.
Provider selection applies to the next completion session. A custom policy changes
ranking of returned candidates; it cannot change a source's own result cap.

New sessions and typing select the best-ranked candidate. A better late result,
including an exact match, replaces an automatic selection. Up/Down navigation
makes selection explicit and preserves that candidate as providers finish, while
it remains available. Typing recomputes the best match and still requires fresh,
revision-checked edits before acceptance.

The document-word source uses the same matcher before its 200-item cap, keeping
the best matches across the buffer rather than the first matching words. It
excludes the token under the caret, while allowing exact words from other
occurrences. Matching ranks candidates a provider actually returns; it cannot
recover candidates filtered out by an external language server.

## Automatic invocation

The default completion plugin enables automatic completion. In its Settings page:

- `automatic` enables or disables automatic invocation; Ctrl+Space remains explicit.
- `typing` starts suggestions after two identifier characters. Disable it to keep
  character triggers without starting on ordinary words.

Sources declare `Triggers` as their contribution's topic-owned `metadata`:
`typing`, a `characters` list, and `dynamic_characters`. The word source declares
only typing. LSP declares typing and dynamic characters; it checks the running
server's `completionProvider.triggerCharacters` before sending any character-triggered
LSP request. This supports server-advertised characters without a hardcoded list
in the editor. An unsupported character returns no candidates and sends no LSP
completion request. Custom static sources can advertise their own characters.

Only committed, single-character keyboard insertions activate this policy. Paste,
IME commits, undo, programmatic edits, and modal Vim commands do not start a new
session. The host waits 150 ms after the latest insertion and calls the collector's
optional `automatic` policy hook with a feature-neutral `contributions::Activation`:
document context, inserted text, identifier prefix, and matching loaded contribution
declarations. The hook returns `Option<String>`: an opaque command input, or null
to decline. The host does not interpret completion trigger metadata or policy input.
Activation checks the originating view, buffer, revision, caret, dismissal epoch,
provider generation, and modal/busy state before starting a collection.

The completion contract encodes automatic input as `Invocation::Typing` or
`Invocation::Character`. Providers receive it in `Context::input`. Explicit requests
and active-session refreshes have no automatic input. LSP maps character invocation
to trigger kind 2 with `triggerCharacter`; explicit, typing and refresh requests use
kind 1. The word source declines character invocations. Active sessions retain the
existing immediate refresh and stale-edit rejection behavior.

## Selected-item documentation

An item can supply `documentation: Some(Documentation { text, markdown })`.
Plain text is displayed literally; Markdown uses the selected documentation markup
provider, with a literal fallback while formatting runs or when the provider fails.
The popup shows a bounded, selectable, scrollable documentation panel beside the
list in wide views and below it in narrow views. Scrolling documentation does not
scroll the buffer. Formatting preserves the current item's documentation scroll.

Semantic contributors can instead supply `documentation_resolve`, an opaque
semantic command payload. The command must return `Outcome::Document`; set its
`language` to `Some("markdown")` for Markdown, or leave it unset for plain text.
The host resolves only the highlighted item, shares the normal semantic command
slot, and remembers the attempt for the current selection. Inline documentation
takes precedence over a resolve payload. Documentation bodies are limited to
64 KiB and resolve payloads to 16 KiB, in addition to existing transport budgets.

LSP preserves inline `CompletionItem.documentation`, advertises Markdown/plain-text
support and documentation resolution, and uses `completionItem/resolve` when a
selected item has no inline documentation and the server supports resolution.
Resolving documentation returns content only, even if the server also supplies
additional edits. Acceptance is a separate revision-checked action; it may resolve
the item again to obtain edits such as imports.

Selection, revision, caret, view, dismissal epoch, source identity and provider
generation guard both resolution and asynchronous formatting. Changing selection
or dismissing cancels pending documentation work. If acceptance happens while a
request is running, cancellation's terminal event releases the slot before the
acceptance resolve proceeds. A failed or empty response displays no documentation
and does not turn into an editor command or apply edits. Links are styled as text;
rendering does not fetch URLs or execute link actions.

## Snippet sessions

`ChoiceAction::Snippet` and `Outcome::Snippet` carry a `commands::snippet::SnippetEdit`:
one workspace edit, the document/replacement indices identifying its primary
insertion, and relative Unicode-scalar tabstops in that insertion's expanded text.
Positive indices navigate in ascending order; exactly one index-zero stop ends the
session. Repeated positive indices edit together through the normal multi-selection
transaction path. Ranges must be valid, mirrors must have identical defaults, and
the primary insertion must target the guarded source buffer.

LSP advertises snippet support and expands numbered stops (`$1`, `${1}`), defaults
(`${1:value}`), nested defaults, mirrors, escaped characters, and choice defaults
(`${1|one,two|}`, initially selecting `one`). `commands::snippet::expand` is also
available to other providers. Snippet variables and regular-expression transforms
are not supported; these candidates are declined instead of inserting raw syntax.
Expansion is bounded to 64 KiB, 256 stops, and 16 nesting levels.

The host validates and commits the primary insertion and additional edits such as
imports together. It adjusts tabstop positions for preceding edits and normalized
line endings. Failed or stale preparation leaves the whole batch untouched.
The insertion and imports form one undo step; placeholder edits use ordinary undo.

Tab advances and Shift+Tab returns to the previous placeholder, taking precedence
over indentation and a new completion popup while the snippet is active. The final
stop leaves a normal caret. Escape, explicit movement/selection commands, undo,
redo, programmatic edits, and edits from another view end navigation. Replacing a
parent default removes its nested stops. Sessions belong to a view; new views and
redo do not recreate them. Placeholder selection uses the existing selection and
multi-caret renderer.

## Current limits

There is no cross-provider deduplication or configurable source priority yet.
LSP still skips command-based items. Automatic IME completion and retriggering incomplete LSP lists with trigger
kind 3 are deferred. Plugin lifecycle operations dismiss the collection; provider
generation checks also reject results from retired sources.

The first extraction retains shared popup/session mechanics in the platform. A
separate frontend protocol is appropriate when there is a concrete second
presentation, rather than moving RXUI types across the plugin boundary.
