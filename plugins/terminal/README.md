# Terminal

Use a **Ghostty terminal emulator** inside Velk with an owned pseudo-terminal (PTY).

## Shell

The plugin uses `VELK_TERMINAL_SHELL`, then `SHELL`, to select the shell. The shell runs with the permissions of the editor process.

Terminal workers and their PTY sessions are stopped when the plugin unloads.

## Settings

Configure this plugin through **Settings → Plugins** or its **Settings** button
in Settings > Features. Select Apply changes to restart the feature. See the
[plugin settings guide](../../docs/plugin-settings.md#bundled-integrations) for
options, defaults, and environment-variable precedence.
