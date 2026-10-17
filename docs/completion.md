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

Return `Outcome::Completions { items, continuation }`; use `None` when no refresh state is needed. Candidates contain a title, optional
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

The default reducer groups sources by configured priority, then uses each source's
`sort_text` for ascending lexical order, keeping declaration order for ties. It adds
source details and caps the combined list at 1,000 items after priority and duplicate
handling. It wraps actions in `Contributed` so subsequent resolves and edits check
the actual source generation. Results from retired sources cannot be accepted.
Explicit selection follows the source generation, command, and candidate ID across
incremental updates. Automatic selection follows the best similarity score, using
the reducer's source order to break ties even when a preferred source arrives late.

Typing inside the identifier prefix starts a fresh collection against the new
revision. Old choices can remain visible while refreshing but cannot be accepted.
Moving away, dismissing, switching documents/views, or closing the document
cancels the collection. Terminal semantic events release the command slot even
when their collection has already been replaced. Results are guarded by the
originating buffer, view, path, revision, caret, and reducer generation.

## Incomplete lists and continuation

The host wraps contributor input in `commands::contributions::Input`, serialized
inside `Context::input` for request hooks or passed as the semantic command input.
It contains the optional topic-owned `activation` string, a `refresh` flag, and an
optional opaque `continuation` value. `Invocation::decode` unwraps the activation
for completion sources. Direct explicit requests may omit input.

A source returns its continuation with `Outcome::Completions`. The host stores it
by plugin ID, generation, and command, then returns it only to that source when
refreshing the same collection, document, and view. Empty result sets can still
carry a continuation. Complete responses and errors clear it; dismissing the
collection discards it. Superseded requests retain the last valid state, and late
responses cannot replace it. Tokens are bounded to 16 KiB and survive bounded
collection transport when candidate tails are trimmed. The host never interprets
the token as LSP or other feature data.

LSP preserves `CompletionList.isIncomplete` as the completion protocol's
`Continuation::Incomplete` marker. Refreshes with that marker send trigger kind 3
(`TriggerForIncompleteCompletions`), as described by the
[LSP completion specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_completion).
Arrays, null responses, and complete lists clear the marker. A fresh explicit or
automatic session starts without it. The editor continues requesting fresh edits
for complete lists too, since stale revision-specific edits cannot be accepted.

## Source priority and duplicates

The completion plugin exposes two user-wide settings:

- `source_order`: comma-separated plugin IDs, highest priority first. The default
  is `velk.lsp, velk.completion.words`; unlisted sources follow in declaration order.
  An empty value uses declaration order throughout. Up to 32 distinct IDs are allowed.
- `deduplicate`: enabled by default. Keep the highest-priority copy of equivalent
  concrete actions from different sources. Disable it to keep every source's result.

These settings apply after restarting the completion plugin, like its other
settings. Similarity still ranks exact/prefix/typo/fuzzy quality first; source order
breaks equal-quality ties. Higher-priority sources receive the combined item budget
first. Server `sort_text` values are compared within their source, rather than
assuming ranking keys from unrelated servers share a scale.

Equivalence requires the same title, filter text, overload detail, and concrete
action, including target, revision, ranges, replacement text, additional edits, and
snippet stops. Workspace edit document/replacement order is normalized for the
comparison. File-backed document words use UTF-8 workspace edits, allowing exact
matches with concrete LSP edits without comparing different target representations.
Untitled buffers retain buffer-ID edits.

Repeated entries within a single source remain available for overloads. Different
imports, edits, details, or snippet stops remain distinct. Deferred semantic actions
also stay separate: resolve tokens can hide additional edits, and comparing labels
or opaque payloads cannot prove equivalence. The reducer does not resolve every
candidate just to deduplicate it. Documentation comes from the retained candidate.

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

The completion contract encodes automatic activation as `Invocation::Typing` or
`Invocation::Character` inside the contributor input envelope. Explicit requests
and active-session refreshes have no automatic activation. LSP maps character
invocation to trigger kind 2 with `triggerCharacter`; incomplete-list refreshes use
kind 3, and other invocations use kind 1. The word source declines character invocations. Active sessions retain the
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

## Replaceable frontends

The editor setting `completion_frontend` selects a presentation service separately
from sources, merging, and `completion_matching_provider`. It applies to the next
completion session without restarting the plugin:

- `velk.completion`: the default menu with candidate details and documentation
  alongside or below it according to viewport width.
- `velk.completion.compact`: up to ten title-only rows without documentation.

Both are bundled native providers. Custom providers implement
`commands::completion::frontend::Provider` and register with
`Registrar::completion_frontend::<YourFrontend>()`. This is a synchronous,
reentrant, stateless service; implementations must perform bounded CPU work and
must not wait for the plugin worker or host services.

`Snapshot` contains already filtered/ranked candidate titles and details, the
selected visible index, viewport dimensions, readiness, and whether documentation
exists. It contains no edit payloads, source handles, or document text. `View`
chooses displayed rows, their titles/details, list width, and hidden/below/alongside
documentation dimensions. Row indices identify the original ranked candidates;
rows must retain ranked order. A frontend can choose its visible window without changing ranking or acceptance.
The selected candidate must appear in the returned rows. The host renders these
controls using its theme and accessibility roles. Arrow keys, Enter/Tab, Escape,
pointer acceptance, focus, snippet precedence, and checked document edits remain
shared behavior.

The platform command contracts own these presentation primitives because the
editor widget consumes them. Feature contribution and activation protocols remain
in `velk-completion-contracts`. No RXUI objects or Rust trait objects cross the
native ABI: a named C table exchanges bounded JSON in caller-owned storage.
The host validates row identities, unique indices, selected-row visibility, text
sizes, finite dimensions, and transport bounds. There are at most 1,000 input
candidates and 32 output rows; presentation text is truncated to 1,024 Unicode
scalars per field before dispatch. Frontend output is cached until its snapshot
or selected provider changes. Retirement prevents further native calls; failure,
invalid output, or an unavailable provider uses a minimal title/detail menu.
The session releases its code lease on acceptance or dismissal. Pointer handlers
also check session epoch, target revision/position, and candidate identity.

## Signature help and commit characters

LSP declares `velk.lsp.signature-help` with Ctrl+Shift+Space. It displays the
server's callable signature, highlights the active parameter separately, and
shows a bounded documentation excerpt. Alt+Up/Down cycles overloads; Escape or
clicking away dismisses it. Once invoked, the session refreshes on text/caret
changes and ends when the server returns no signature. Initial automatic trigger
characters are deferred; invoking the command does not require the Completion
plugin. The panel stays hidden while a completion menu is active.

`Outcome::SignatureHelp` carries a neutral `commands::signature::Help`, including
Unicode-scalar active-parameter ranges. LSP converts its UTF-16 label offsets
independently of negotiated document position encoding. Invalid/split offsets
omit highlighting. Requests use normal document/view/revision/provider guards;
a dismissal epoch prevents late replies from reopening the popup. Signature
refreshes share semantic scheduling with completion and documentation. Signature
documentation currently uses a plain text excerpt.

Completion items may declare up to 32 punctuation commit characters. Only a
ready selected suggestion and a single committed keyboard character can accept
this way. Paste, IME input, stale lists, and unlisted characters follow ordinary
editing. LSP advertises commit-character support, uses per-item declarations in
preference to server defaults, and preserves explicit empty overrides. Malformed
multi-character values, whitespace, and identifier characters are excluded.
Deduplication preserves candidates with differing commit behavior.

The host carries the typed character through the existing edit guard, including
asynchronous completion resolution. It replays the character only after the edit
successfully commits; stale/cancelled/failed edits do not replay it. Plain edits
then use the normal interactive pairing provider. A function snippet that already
inserted `(` consumes that opener once and retains the argument placeholder;
other commit characters finish snippet navigation before ordinary insertion.
A separately inserted commit character uses ordinary editing undo, so undoing it
and the accepted completion can require two steps. Completion plus its imports
remain one checked transaction.

## Current limits

LSP still skips command-based items. Automatic IME completion is deferred.
Deduplication is conservative for deferred actions and differing position encodings. Plugin lifecycle operations dismiss the collection; provider
generation checks also reject results from retired sources.

The frontend currently describes a caret menu using shared controls. Arbitrary
RXUI trees, custom input handlers, inline ghost text, and plugin-defined styling
are outside this contract.
