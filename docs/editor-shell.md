# Editor shell

Closing the last document keeps the window open. The empty editor area provides
New File and Open File actions, with no hidden document; window features and the
workspace root remain available.

Velk starts with a buffer tab bar, the editor and a compact bottom status bar. The bar shows save
and semantic status and provides Settings and Plugins buttons. Enabling the left
activity bar moves those buttons into it. Panels start hidden. The Files plugin reads the initial project root and loads subfolders only when opened.

Settings opens as a modal with Command+, on macOS and Control+, elsewhere. It can
hide either bar, including both; the shortcut remains available. Escape, the
close button, or clicking the backdrop dismisses Settings and restores editing
focus. Settings and Plugins are mutually exclusive modals. F4 still opens Plugins.
Save notices/errors remain visible even with the status bar hidden.

The current appearance settings are `status_bar` (default true) and `activity_bar`
(default false). They are stored as JSON:

```json
{
  "status_bar": true,
  "activity_bar": false
}
```

On macOS the file is `~/Library/Application Support/Velk/settings.json`. Other
platforms use `$XDG_CONFIG_HOME/velk/settings.json`, falling back to
`~/.config/velk/settings.json`. `VELK_SETTINGS_PATH` overrides the entire file path,
including for isolated development previews. Settings are read before the native
application starts; changes apply immediately in memory and save asynchronously
using a temporary file and atomic replacement. Writes are serialized and rapid
changes coalesced. Unknown keys are preserved. Malformed/unreadable files are not
overwritten: the modal reports the error and edits apply to this window only.
Reset appearance restores only the two layout options.

See [buffers.md](buffers.md) for opening, switching, closing and saving multiple
files, keyboard shortcuts and reload behavior.

## Feature plugins

Plugins can now contribute host-rendered panels through the named
`velk.ui.panels.v1` request hook. Left, right and bottom slots support tabbed tree
or text snapshots. The built-in `velk.files` plugin supplies a lazy filesystem
tree with native filesystem watching; see [files.md](files.md) for folder selection,
refresh and limits. Panels start hidden. Open them with the
Panels button, Command+B / Control+B, or **Panels: Toggle visibility** in the
palette. Closing a slot restores editing focus. The launcher moves into the
activity bar when enabled and remains reachable by shortcut with both bars hidden.

The host owns layout, scrolling, virtualized row controls and view state. Snapshot
requests run off the UI thread, and lifecycle changes invalidate pending replies.
See [panels.md](panels.md) for the contract, limits, authoring example, ownership,
reload behavior and current restrictions.

## Command palette

Open the palette with Command+Shift+P on macOS, Control+Shift+P elsewhere, or F3.
It is a modal and does not resize the editor. It can replace Settings or Plugins,
including when both shell bars are hidden. Typing filters command captions using
case-insensitive subsequence matching (`stgs` finds Settings); contiguous matches
rank ahead of matches with gaps. Up/Down wrap through the results, Enter runs the
highlighted enabled command, and Escape or a backdrop click restores editor
focus. Buttons can also be clicked or activated after Tab navigation. Disabled
commands remain visible, and Enter on a disabled result or an empty list does
nothing. Reopening starts with an empty query.

The catalogue includes Terminal: Toggle bottom panel (Control+backtick), Open file, Open folder (Command+Shift+O / Control+Shift+O),
Panels: Refresh, New File, Save, Save As, Save All, Close buffer, Next/Previous buffer,
Find in file, Settings, Plugins, Show hover,
Plugins: Load from disk, Plugins: Unload all, Close window, and Quit Velk. Command
entries wrap the same RXUI actions as existing buttons and shortcuts, sharing
their current availability and callbacks. Closing or quitting still uses the
unsaved-changes flow. File search, plugin lifecycle operations, and saves retain
their existing asynchronous implementations. Palette filtering only examines
this small host-owned catalogue and adds no worker, dependency, or plugin calls
per keystroke. The catalogue is only built while the palette is open; at most
eight results are constructed, following keyboard selection through longer lists.

Stable command IDs are separate from captions. These are host commands for now;
plugin command contributions are not implemented by the palette. Plugin restart
and Reload Window are described in [reloading.md](reloading.md). A future contribution contract can expose owned command descriptors and
route invocation through the plugin worker, without passing plugin callbacks
onto the UI thread or changing the native plugin ABI.

The palette also exposes Diagnostics commands to start/stop a trace capture and
add a marker. See [profiling.md](profiling.md) for capture limits, structured logs,
plugin instrumentation and sampling builds.


The built-in `velk.terminal` plugin adds a Ghostty-powered fixed-cell bottom
panel. Its shell starts when shown, and hiding the panel retains its live
session. See [terminal.md](terminal.md) for build prerequisites, keyboard input,
ownership, unload/reload behavior and prototype limits.
