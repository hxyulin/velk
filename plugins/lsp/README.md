# Language Server Protocol

Connect Velk to language servers using the **Language Server Protocol**.

Provides semantic sessions, diagnostics and Markdown hover documentation. A language definition plugin supplies the server executable, arguments and configuration.

## Commands

Open the command palette with Command+Shift+P (macOS), Ctrl+Shift+P, or F3:

- Go to definition.
- Go to type definition.
- Go to implementation.
- Find references.

Commands use the active document and caret. Unsupported server capabilities show
an error. Multiple targets open a chooser; existing buffers keep unsaved text.
See the [command guide](../../docs/plugin-commands.md).

## Lifecycle

Server sessions are stopped on unload. The native library stays resident because process-wide signal handlers may outlive a session.
