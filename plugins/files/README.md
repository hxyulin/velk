# Files

Browse your project in a lazy, expandable **file tree**.

- Expand directories as needed instead of scanning the whole project.
- Watch directories for changes.
- Open files from the sidebar.

This is the default frontend over `velk.files.backend`. The backend owns directory
reads, caches, and watches; other frontend plugins can reuse it through the shared
file-tree contract. Each frontend has independent expansion and query state.

## Settings

Configure **File tree backend** through **Settings → Plugins** or its **Settings** button
in the plugin manager. Changes apply after an explicit restart. See the
[plugin settings guide](../../docs/plugin-settings.md#bundled-integrations) for
options, defaults, and environment-variable precedence.
