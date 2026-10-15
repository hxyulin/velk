# Language Server Protocol

Connect Velk to language servers using the **Language Server Protocol**.

Provides semantic sessions, diagnostics and Markdown hover documentation. A language definition plugin supplies the server executable, arguments and configuration.

## Lifecycle

Server sessions are stopped on unload. The native library stays resident because process-wide signal handlers may outlive a session.
