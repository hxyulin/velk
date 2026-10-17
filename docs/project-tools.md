# Quick Open and workspace search

The bundled `velk.quick-open` and `velk.workspace-find` plugins are default picker
frontends. They depend on `velk.project.files` and `velk.project.search`, respectively.
The backend plugins own indexing, matching, project reads, and cancellation;
The feature-owned `velk-project-contracts` library defines their data protocol. Each consuming frontend
has independent backend state. Disable or replace either frontend independently,
or implement another frontend without linking or copying the backend. See
[feature backends](feature-backends.md) for authoring and dependency declarations.

Choose the project with **Open folder…** (Cmd/Ctrl+Shift+O) first. Opening a
single file alone does not select a project folder.

- **Cmd/Ctrl+P** opens Quick Open. Type a fuzzy file path, use Up/Down, then Enter
  or click a result to open it. Empty queries list project files.
- **Cmd/Ctrl+Shift+F** opens Find in Workspace. Type literal text; case sensitivity
  and whole-word controls update the query. Results show path, line, column, and
  matching text. Enter or click opens the file at the match.
- **Escape** closes a picker and cancels its background query. Typing replaces the
  previous query immediately. Late replies cannot replace newer ones.

Both use the selected project folder, without changing the process working
directory. Traversal and file reads use host services. Reads use a published
workspace context, avoiding a UI round trip for each filesystem operation.
Changes of project root invalidate result acceptance.

Scans honor project-local `.gitignore`, `.ignore`, and `.rgignore` rules, including
nested rules and negation. They skip `.git` and symlinks. Ancestor/global ignore
files outside the selected folder are not read. Workspace search prefers the
current text of open file-backed buffers over disk contents. Binary and invalid
UTF-8 files are skipped; untitled buffers are outside the project scan.

Quick Open builds a complete eligible file index once per picker session, then
filters paths in memory using [Nucleo](https://github.com/helix-editor/nucleo)'s
Unicode-aware fuzzy matcher with path scoring and smart case matching. There is
no entry cutoff: the 1,000-result display limit is applied only after every
indexed path has been matched. The notice shows the total matches and indexed
file count. Reopening the picker rebuilds the index, picking up new files and
ignore-rule changes. File watching while the picker is open is not implemented.
Typing while indexing changes the pending query without restarting traversal.

Workspace search refreshes the eligible file list per query and reads at most
32 MiB of text, returning at most 1,000 matches. Individual host file/buffer reads
retain the 1 MiB limit. Unreadable paths, oversized reads, malformed ignore rules,
and limits are reported in the picker notice. Search is per line; multiline
matching and regex are not supported.

The picker has a fixed frame, input height, status area, and result row height.
It retains previous results while filtering and accepts a selection only once
the current query completes. A small window around keyboard selection avoids
creating every result row.

## Picker contract

Commands declare `Handler::Picker { hook, mode }`. `PickerMode::Files` uses a query
field; `Content` additionally exposes the existing search query's case/word flags.
The host owns presentation, focus, keyboard actions, source/generation guards,
and navigation. Plugins register a named bytes request and return `PickerReply`.

`Begin { session, id, context, query }` queues a worker job and returns `Pending`.
The session stays constant while the picker is open; reopening uses a new one.
`Poll { id }` returns `Pending` or `Ready { items, notice }`. Results use the shared
command choice model with absolute `ChoiceAction::Location` targets. The host
validates results and native output limits before rendering them. Providers should
return from request callbacks promptly and join owned workers before unload.

Cancellation uses `velk.picker.cancel.v1` with `PickerCancel { plugin, id }` through
the existing event contract. IDs are monotonic per window; providers also use an
internal cancellation ticket. Old cancellation requests cannot cancel a newer
query or another provider. Root/SDK/contract versions remain at 1.
