# Open buffers

The command line accepts `velk [file]`. A window can hold multiple
file-backed or untitled buffers, shown in a horizontally scrollable tab bar above the editor.
Only the selected editor is mounted and rendered. Inactive buffers keep their
Rope, selection, undo/redo history, scroll offset, syntax state, semantic service,
find query and disk-save baseline in memory.

| Action | Shortcut |
| --- | --- |
| New untitled document | Command+N / Control+N |
| Save As | Command+Shift+S / Control+Shift+S |
| Save All | Command+Option+S / Control+Alt+S |
| Open another file | Command+O on macOS, Control+O elsewhere |
| Save selected buffer | Command+S / Control+S |
| Close selected buffer | Command+W / Control+W |
| Next buffer | Control+Tab |
| Previous buffer | Control+Shift+Tab |
| Close window | Command+Shift+W / Control+Shift+W |
| Quit | Command+Q / Control+Q |

The bar also provides Open and Close tab buttons. The same actions appear in the
command palette. On macOS and Windows, native File and Edit menus route through
RXUI's commands; Quit uses the editor's unsaved-changes flow. Linux keeps the
in-window commands without requesting RXUI's unsupported native menu attachment.

Opening uses the native file picker. File reads, UTF-8 decoding, newline
normalization and disk-baseline hashing run on the blocking pool. Failed opens
leave the current buffer untouched. A resolved path already open in the window
selects its existing tab, including opening the same target through a symlink.
Each tab has a stable internal ID; closing a tab never reuses its ID.

A dot marks an unsaved buffer. Save snapshots the selected buffer, then writes
asynchronously. Switching while saving is supported, and different files can
save concurrently. Completion updates the original buffer's disk baseline and
checkpoint; edits after the snapshot remain dirty. Closing/reloading waits for
pending file operations to finish so an in-flight write is never abandoned by
removing its owning buffer. IME composition prevents tab switching; if a file
finishes opening during composition, the current composition is preserved and
an explanatory notice asks the user to finish it before retrying.

Closing a dirty tab asks Save / Don't Save / Cancel. Closing the last tab keeps the window open with no active document. The empty
editor area offers New File and Open File; panels, plugins, settings and the
workspace root remain available. Close Window and Quit are separate actions. Closing a window or quitting checks **all** dirty buffers. Earlier
discard approvals leave their buffers in memory until all prompts succeed;
cancelling a later prompt retains the entire buffer set. A failed save, or newer
edits made during a save, aborts closing.

## Ownership and background work

The window owns shared plugins, panel contributions, settings and palette state.
Each `BufferState` owns an editor entity, file identity, subscription, pending
save, semantic service and find state. Buffer-local async callbacks capture a
stable ID and resolve it on completion. A closed buffer rejects late results;
equal document revisions in different tabs cannot alias diagnostics or hover.
Closing the buffer shuts down its semantic service and drops its subscriptions
and task handles. Switching tabs keeps these services alive.

The current search provider accepts one job at a time. Only the visible buffer
runs find work. Switching cancels outgoing work and re-runs the incoming buffer's
query with a window-wide unique request ID, preserving its selection. Inactive
buffers do not compete for the shared provider. The query and open/closed find
bar state remain per-buffer.

Reload Window transfers every buffer's unsaved text, history/checkpoints,
selection, scroll, original disk baseline and find state. It restores the tab
order and selected buffer along with the existing window/menu/plugin state.
Syntax/rendering caches and semantic services rebuild in the replacement process.
This remains a private process handoff; independent persistent recovery is
described in [document-foundation.md](document-foundation.md).

See [document-foundation.md](document-foundation.md) for untitled documents,
Save As, Save All, persistent recovery, stable IDs, and disk-change handling.
Current limits: one editor pane and no tab reordering. The semantic service still owns one document per session;
multiple Rust buffers can therefore start multiple language-server sessions.
Sharing one LSP connection across a workspace is a separate backend improvement.
