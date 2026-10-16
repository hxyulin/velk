# Plugin host services

Plugins can query the workspace and ask the editor to operate on buffers through
`velk_plugin_sdk::Host`. Each instance receives its own handle in
`Plugin::attach_host`, after metadata discovery and before settings configuration
or lifecycle restore. Store the handle in the plugin; clones are thread-safe.
There is no process-global current plugin, including in multi-plugin packs.

## Capabilities

| Declaration | Host requests |
| --- | --- |
| `workspace.read` | `Workspace`: current working directory and selected project root |
| `filesystem.project.read` | `ReadFile`, `ReadDirectory`: disk contents inside the project |
| `editor.diagnostics.read` | `Diagnostics`: current revision-matched diagnostics across open buffers |
| `editor.buffer.read` | `Buffers`, `ReadBuffer`: metadata and current unsaved text |
| `editor.buffer.write` | `EditBuffer`, `ApplyEdit`: versioned document edits |
| `editor.buffer.manage` | `OpenBuffer`, `NewBuffer`, `SelectBuffer`, `SaveBuffer`, `CloseBuffer` |

The service checks the calling instance's declarations before dispatch. Required
and optional declarations both allow calls today; there is no consent UI or
optional-grant negotiation. An undeclared request returns an error. Other existing
feature contracts still use their own supplied contexts and validated outcomes.

This is an API boundary, not a native-code sandbox. Native plugins retain ambient
process access and can bypass the service with Rust filesystem or OS calls.
`filesystem.read` continues to describe that broader access; it does not imply
`filesystem.project.read` for this service.

## Workspace and project reads

`Workspace` distinguishes the process's current directory from the folder selected
in the editor. Selecting another folder does not change the process cwd.
Project reads use the selected folder and fail if none is selected; there is no
implicit cwd fallback. A plugin that only needs project reads can declare
`filesystem.project.read` without also declaring `workspace.read`.

Relative paths resolve against the project root. Absolute paths are accepted only
when they resolve inside it. The host canonicalizes the root and requested path,
rejecting parent traversal and symlinks that resolve outside the root. This check
is not a race-proof security boundary against concurrent filesystem changes.
Directory results use project-relative paths, are sorted, and report symlinks
without traversing them. Reads require a regular file and return disk bytes,
not unsaved editor contents. Use `ReadBuffer` for the latter.

File reads and buffer text replies are limited to 1 MiB. Directory reads return sorted pages of at most 4,096 entries. Pass `after: None`
for the first page, then the last entry's project-relative path for the next page.
Continue until an empty page; there is no total directory-entry limit. Listings
are not atomic snapshots, so concurrent filesystem changes can affect traversal. Native request
JSON is limited to 1 MiB and response JSON to 4 MiB, including encoding overhead.
An oversized buffer or encoded response returns an explicit error. Paths in this
JSON contract must be representable as UTF-8.

## Buffer operations

`Buffers` returns buffers in tab order, with stable IDs, file paths where present,
titles, active state, dirty state, revision, and selection. IDs are scoped to the
current window session. `ReadBuffer` returns this metadata and the current text,
including unsaved changes. A closed ID is an error and is never reused for a new
buffer in that session.

`NewBuffer` creates an untitled document and returns `Created { id }`. Nonempty
initial text is a normal undoable edit and marks the document dirty.
`OpenBuffer` requires an absolute path and reuses the normal asynchronous opening
and file deduplication flow. `SelectBuffer` selects an existing ID. Saving a
file-backed buffer preserves the current selection of tabs; saving an untitled
buffer selects it and opens the normal Save As dialog.

`CloseBuffer` uses the same close policy as closing a tab: dirty documents prompt,
and cancelling leaves them open. Closing the final buffer leaves the window open.
There is no force-close or discard flag. Saves retain atomic replacement and
external-change protection.

`EditBuffer` takes an ID, an expected revision, and the existing command
`TextEdit` model with explicit position encoding. It supports file-backed and
untitled documents, rejects stale revisions and invalid or overlapping ranges,
and commits a single undo step. `ApplyEdit` reuses the existing multi-document
workspace-edit preparation, validation, and undo behavior.

Operations return `Accepted` after dispatch through the normal host flow.
For opens, saves, multi-document edits, and dirty closes, this acknowledges the
request rather than promising completion: file I/O can fail and the user can
cancel a dialog. Results and notices appear through the existing editor UI;
query buffer state again when needed. Buffer mutations return a busy error during
conflicting operations, composition, or close confirmation.

## SDK use and scheduling

```rust
use velk_plugin_sdk::{host::{Request, Response}, Host};

fn project_file(host: &Host) -> Result<Vec<u8>, String> {
    match host.request(Request::ReadFile { path: "Cargo.toml".into() })? {
        Response::File { bytes } => Ok(bytes),
        _ => Err("unexpected host response".into()),
    }
}
```

Implement `fn attach_host(&mut self, host: Host)` in `Plugin` and store the handle
in an `Option<Host>` field. Declare each capability the plugin uses. The host
attaches services automatically; no extra registrar call is needed.

Calls are synchronous and can wait for the UI. Call from plugin request handlers
or owned background workers. Never call from UI callbacks or the pure interactive
editing provider: those paths must not block waiting for themselves. Filesystem
reads run on the calling plugin worker, not on the UI thread. Long scans should
use a plugin-owned worker so they do not occupy the shared plugin dispatch thread.
This service does not introduce another executor or cross-ABI Rust futures.

The host uses a bounded request queue and copies owned JSON through a C callback
table. Retained handles keep only host-owned endpoint storage alive. Unload retires
the endpoint before plugin destruction; shutdown closes the bridge before joining
the plugin worker. Waiting calls fail, and queued retired requests cannot mutate
the editor. Plugins must still stop and join their workers before unload.
Internal contracts remain at version 1; rebuild the host, SDK, and native plugins
together after changes.

`SaveBuffer.expected_revision` is `None` for ordinary saves. Autosave supplies
`Some(revision)` to require a file-backed buffer at that exact revision.
See [buffer events and autosave](buffer-events.md) for post-commit subscriptions.

## Diagnostic reads

`Diagnostics` returns owned `DiagnosticDocument` records keyed by session buffer
ID and revision. Each diagnostic includes a Unicode-scalar range, zero-based
logical line and scalar column, severity, message, source, and code. Ranges are
aligned to grapheme boundaries so navigation does not get stuck inside a combining
sequence. The host
retains the latest semantic diagnostics per buffer. Closed documents, failed or
stopped services, and results whose revision differs from current text are
omitted; editing invalidates results immediately, before the backend responds.

A response contains at most 1,000 diagnostics across the window and reports
`truncated` when more exist. Display strings are bounded to 512 characters for
messages, 64 for source/code, and 256 for document titles. Control characters are
replaced with spaces. These are display summaries; editor hover retains the full
message. Replies contain no document text or filesystem contents.

The bundled Diagnostics plugin reads through this service on the serialized
plugin worker, outside rendering. Its ordinary 200 ms panel invalidation check
compares current records with the last described panel. No separate LSP session
or plugin-owned UI is involved. Requests use the existing host C FFI and retain
contract version 1.
