# Plugin edit proposals

Plugins return `Outcome::Edit { edit }` using the shared `velk-commands` model.
The host owns document lookup, validation, transactions, undo, recovery and saving.
An LSP response is translated inside the LSP plugin; no protocol objects enter
the editor or Core.

A `WorkspaceEdit` contains `DocumentEdit` records, each with an absolute local
file path, an optional host revision, and replacements. A replacement contains
start/end positions and inserted text. Positions use zero-based logical lines
and explicitly named UTF-8, UTF-16, or Unicode scalar columns. All replacements
in one document refer to its original contents, not the result of preceding edits.

The host captures open-buffer identities and revisions when invoking a command.
It prepares the proposal on the blocking executor, resolving paths and reading
any closed targets through the normal file-loading code. It converts positions
to Core transactions and validates every target before mutating any buffer.

Before committing, the host rechecks the source context, provider generation,
target identities and revisions, active saves/composition, and document limits.
Changes to any target abort the complete proposal. Invalid ranges, partial code
points, overlapping edits, equal starting offsets, duplicate canonical paths,
and missing files also reject the complete proposal. Provider revision numbers
are host revisions; the LSP adapter checks its own document version before
translating it to a host revision.

Successful edits to each document form one undo step. Undo is buffer-local;
there is no cross-file undo group. Existing buffers keep their unsaved text and
history. Closed targets open as dirty buffers, with the original file contents
available through Undo. Changes remain unsaved until the user saves them; the
normal disk-conflict checks still apply. An edit originating from an empty
window makes its first affected document visible.

## Current limits

- Existing text files only: file creation, renaming and deletion are rejected.
- No automatic saves or server-initiated `workspace/applyEdit`.
- At most 256 target documents, 10,000 replacements, and 4 MiB of encoded output.
- The current LSP implementation runs one server session per buffer. It has no
  authoritative version for another buffer's unsaved text. Unversioned edits to
  another dirty buffer are rejected; versioned proposals must match the host
  revision. Save those targets before retrying a cross-file LSP refactoring.
- Closed, unversioned files use the contents read during preparation. The host
  does not claim those contents match a server's earlier analysis snapshot.
  They are loaded without writing to disk, and saving still checks for external
  changes against the loaded file baseline.
- Change annotations requiring a preview are not supported. The client does not
  advertise annotation support, and annotated replacements are rejected.

## Bundled LSP actions

**Format document** applies the formatter's text replacements to the active
buffer. Initial formatting options are four spaces with spaces enabled; there
is no format-on-save policy.

**Rename symbol** prompts for a new name and requests `textDocument/rename` at
the caret. The server validates the name and rename location. Multi-file text
edits use the same host preflight and transaction path.

**Code actions and quick fixes** requests actions at the caret, including current
diagnostics covering that position. The result picker supports local filtering,
arrow keys, Enter and Escape. Edit-bearing actions can apply immediately; actions
with resolvable data use `codeAction/resolve`. Disabled actions and actions requiring
server command execution are excluded. This first version does not execute
server tasks or apply resource operations.

**Complete at caret** explicitly requests completions. The searchable picker
shows the returned candidate set; filtering it does not send another LSP request.
Completion can resolve additional text edits lazily. Primary replacement and
additional edits, including imports, apply as one transaction and undo step.
Insert/replace items use their replacement range. Without a supplied range, the
host proposal replaces the identifier prefix preceding the caret.

Completion is plain-text only. The client advertises no snippet support, and
snippet or command-based items are excluded. Automatic suggestions, snippet tab
stops, completion documentation, and parameter hints are future work. Invoke
completion again after editing to obtain a fresh candidate set.

See [plugin commands](plugin-commands.md) for shortcuts, prompts, result handling,
and the native contract version.
