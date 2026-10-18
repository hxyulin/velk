# Language Server Protocol

Connect Velk to language servers using the **Language Server Protocol**.

Provides semantic sessions, diagnostics and Markdown hover documentation. A language definition plugin supplies the server executable, arguments and configuration.

## Commands

Open the command palette with Command+Shift+P (macOS), Ctrl+Shift+P, or F3:

- Go to definition.
- Go to type definition.
- Go to implementation.
- Find references.
- Format document (Alt+Shift+F).
- Rename symbol (F2).
- Code actions and quick fixes (Command/Ctrl+period).
- Complete at caret (Ctrl+Space).

Commands use the active document and caret. Unsupported server capabilities show
an error. Definition navigation also supports F12. Use Alt+Left/Right to go back
or forward after a jump. Multiple targets open a chooser; existing buffers keep unsaved text.
Edits remain unsaved and use the host transaction/undo machinery. Rename and
code actions can affect multiple files; another dirty buffer requires a known
revision. Completion is explicitly invoked and plain-text only. See the
[command guide](../../docs/plugin-commands.md) and
[edit proposal guide](../../docs/workspace-edits.md).

## Lifecycle

The client shares one server process per canonical workspace root and effective
server configuration. Buffers attach to that client with independent document
versions, requests and cancellation. Closing a buffer sends didClose; closing the
last buffer keeps the client available for reuse. Provider unload/restart and
application shutdown stop and reap its clients, including idle ones.

On Unix, each server starts in its own process group. Shutdown first attempts the
LSP shutdown/exit exchange, then terminates any remaining group members, including
Cargo or compiler children that could retain build-directory locks. The same
cleanup runs when the transport is dropped unexpectedly. Children that detach
into another process group are outside this cleanup. Windows currently retains
direct server-process termination without process-group cleanup.

Cargo workspace members share their workspace root, while nested independent
Cargo projects remain separate. Compilation databases, compile flags and repository
markers support other project roots; an explicitly configured root takes
precedence. The client is language-neutral, but Velk currently bundles only a Rust
language definition and editor routing.

The language client is linked into the editor. Every file publication enters the editor's workspace
diagnostic store, including unopened files. Publications replace earlier results
for the same client and path; empty publications clear them. Failure or retirement
clears the client's results. Unversioned publications use saved file contents,
while versioned publications require the exact attached document version.

Language-server stderr is drained into the host diagnostics service. The host owns
filtering, colors, and output; external stderr text is recorded at INFO with source
`velk_plugin_lsp::server`. Long lines are truncated to 4 KiB.

Workspace collection respects the native 1 MiB encoded request limit. Diagnostic
lists are truncated with an explicit marker when necessary; a file publication
whose source snapshot cannot fit is omitted without stopping the server.

## Pull diagnostics

Servers advertising `diagnosticProvider` use `textDocument/diagnostic` for open
buffers. Velk requests on open, debounces edits and saves, and handles the server's
`workspace/diagnostic/refresh` request across attached documents. It reuses result
IDs and cached full reports for unchanged responses, cancels outdated requests,
rejects late results, and applies a ten-second deadline. Pulled results enter the
same host store and inline annotations as push diagnostics. Push and pull use
independent collections and merge without duplicate entries, since servers such
as rust-analyzer can pull native diagnostics while pushing compiler diagnostics.
Clearing one collection preserves the other. Servers without the pull capability
continue using push diagnostics.

This does not implement workspace pull, partial reports, related-document pull
reports, or dynamic diagnostic registration; those capabilities are not advertised.

Completion items preserve inline documentation and can lazily resolve missing
documentation for the selected suggestion. The client advertises Markdown and
plain text. Documentation resolution never applies returned edits; accepting a
completion remains a separate checked edit operation, including import resolution.

Ctrl+Shift+Space invokes signature help, which refreshes while active; Alt+Up/Down
cycles overloads and Escape dismisses it. Initial automatic signature triggers
are deferred. Completion commit characters use item overrides or server defaults
and are replayed through host editing after checked acceptance. Snippet openers
are consumed once. See [completion](../../docs/completion.md) for behavior and limits.

Snippet completions resolve file/document variables from their revisioned snapshot,
retain interactive choices, and carry placeholder transforms through native results.
Selected text is unavailable to semantic position requests and resolves empty;
clipboard, dates, and workspace variables have no implicit resolver. The supported
regex syntax and editing behavior are documented in [completion](../../docs/completion.md).

Snippet completions carry raw templates and explicit snapshot variables. The editor
expands them through its selected snippet provider on acceptance; LSP does not
depend on the bundled Snippets plugin.
