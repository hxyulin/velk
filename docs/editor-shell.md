# Editor shell

Velk starts with the editor and a compact bottom status bar. The bar shows save
and semantic status and provides Settings and Plugins buttons. Enabling the left
activity bar moves those buttons into it. There are no empty file/search panels
and no built-in filesystem scanning or search backend.

Settings opens as a modal with Command+, on macOS and Control+, elsewhere. It can
hide either bar, including both; the shortcut remains available. Escape, the
close button, or clicking the backdrop dismisses Settings and restores editing
focus. Settings and Plugins are mutually exclusive modals. F2 still opens Plugins.
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

## Feature plugins

This change establishes host chrome, not a plugin panel API. Files, search, and
other optional tools should be supplied by plugins as those features are built.
The intended boundary is:

- The host owns layout slots, selection, focus, accessibility, scrolling, and
  rendering. Visible rows should be virtualized using host controls.
- Plugins contribute panel descriptors and versioned data snapshots/deltas, and
  receive actions such as expanding a directory or submitting a search query.
- Filesystem traversal, watching, indexing, and searching run off the UI thread.
  Results must be bounded, cancellable, and tagged with a query/generation so late
  replies cannot revive a closed panel or an unloaded plugin.
- Rendering reads host-owned cached data. It must not synchronously call a plugin
  for each row or frame. Unload removes a plugin's contributions and cancels its
  outstanding work before releasing its code.

The named registry provides the discovery/contract boundary for that future
work. It does not currently publish panel contributions or connect plugins to
these bars. We should choose a concrete panel contract while implementing the
first real feature, and measure result transfer, UI preparation, and interaction
latency with realistic workloads. A C ABI alone is not a performance guarantee;
unbounded payloads, blocking callbacks, and eager row construction would dominate
the cost of a function call.
