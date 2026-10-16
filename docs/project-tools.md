# Quick Open and workspace search

The bundled `velk.quick-open` and `velk.workspace-find` plugins share scan machinery
in `plugins/project`, but have separate identities, workers, commands, and unload
lifecycles. Disable or replace either plugin independently.

Choose the project with **Open folder…** (Cmd/Ctrl+Shift+O) first. Opening a
single file alone does not select a project folder.

- **Cmd/Ctrl+P** opens Quick Open. Type a fuzzy file path, use Up/Down, then Enter
  or click a result to open it. Empty queries list project files.
- **Cmd/Ctrl+Shift+F** opens Find in Workspace. Type literal text; case sensitivity
  and whole-word controls update the query. Results show path, line, column, and
  matching text. Enter or click opens the file at the match.
- **Escape** closes a picker and cancels its background query. Typing replaces the
  previous query after a 100 ms debounce. Late replies cannot replace newer ones.

Both use the selected project folder, without changing the process working
directory. Traversal and file reads use host services. Reads use a published
workspace context, avoiding a UI round trip for each filesystem operation.
Changes of project root invalidate result acceptance.

Scans honor project-local `.gitignore`, `.ignore`, and `.rgignore` rules, including
nested rules and negation. They skip `.git` and symlinks. Ancestor/global ignore
files outside the selected folder are not read. Workspace search prefers the
current text of open file-backed buffers over disk contents. Binary and invalid
UTF-8 files are skipped; untitled buffers are outside the project scan.

The first version rescans per query. There is no persistent index to invalidate.
Scans check at most 20,000 entries and return at most 1,000 results. Content searches
read at most 32 MiB in total. Individual host file/buffer reads retain the 1 MiB
limit. Unreadable paths, oversized reads, malformed ignore rules, and limits are
reported in the picker notice. Search is per line; multiline matching and regex
are not supported. The result pane displays a small window around keyboard
selection rather than creating every result row.

## Picker contract

Commands declare `Handler::Picker { hook, mode }`. `PickerMode::Files` uses a query
field; `Content` additionally exposes the existing search query's case/word flags.
The host owns presentation, focus, keyboard actions, source/generation guards,
and navigation. Plugins register a named bytes request and return `PickerReply`.

`Begin { id, context, query }` queues a worker job and returns `Pending`.
`Poll { id }` returns `Pending` or `Ready { items, notice }`. Results use the shared
command choice model with absolute `ChoiceAction::Location` targets. The host
validates results and native output limits before rendering them. Providers should
return from request callbacks promptly and join owned workers before unload.

Cancellation uses `velk.picker.cancel.v1` with `PickerCancel { plugin, id }` through
the existing event contract. IDs are monotonic per window; providers also use an
internal cancellation ticket. Old cancellation requests cannot cancel a newer
query or another provider. Root/SDK/contract versions remain at 1.
