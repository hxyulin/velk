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

Server sessions are stopped on unload. The native library stays resident because process-wide signal handlers may outlive a session.

Language-server stderr is drained into the host diagnostics service. The host owns
filtering, colors, and output; external stderr text is recorded at INFO with source
`velk_plugin_lsp::server`. Long lines are truncated to 4 KiB.
