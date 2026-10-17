# Open buffers

The command line accepts `velk [file]`. A window can hold multiple
file-backed or untitled buffers, shown in a horizontally scrollable tab bar above the editor.
The active pane selects a document from these tabs. Several panes can show the
same document or different documents. Hidden documents retain their text,
history, last view state, semantic service, find query, and disk-save baseline.

| Action | Shortcut |
| --- | --- |
| New untitled document | Command+N / Control+N |
| Save As | Command+Shift+S / Control+Shift+S |
| Save All | Command+Option+S / Control+Alt+S |
| Open another file | Command+O on macOS, Control+O elsewhere |
| Save selected buffer | Command+S / Control+S |
| Split editor right | Command+\ / Control+\ |
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
Each `BufferState` owns a shared document, its view entities and subscriptions,
file identity, pending save, semantic service and find state. Buffer-local async callbacks capture a
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
original disk baseline and find state. It also restores the split tree, divider
positions, each view's selection and scroll, tab order, and focused pane along
with the existing window/menu/plugin state.
Syntax/rendering caches and semantic services rebuild in the replacement process.
This remains a private process handoff; independent persistent recovery is
described in [document-foundation.md](document-foundation.md).

See [document-foundation.md](document-foundation.md) for untitled documents,
Save As, Save All, persistent recovery, stable IDs, and disk-change handling.
## Multiple views

Use **Split editor right** or **Split editor down** in the command palette or
native View menu. Splits can nest, support draggable dividers, and allow up to
16 views per window. **Focus next editor view** cycles through panes.
**Close editor view** or a pane header's close button removes that view; the
last pane remains available. Closing a view keeps its document open, including
unsaved changes. **Close buffer** closes the document and all its views, asking
about unsaved changes once.

Each view has its own caret, selection, scroll, Vim mode, and transient input
state. Text, history, saved state, and diagnostics belong to the document.
Undo from either pane uses the shared history and rebases the other pane's
selection. Save and editing commands target the focused pane. Selecting a tab
changes only that pane. Async caret-sensitive replies are rejected if focus
has moved to another view, even when it shows the same document and position.

Recovery stores text and history once per document, plus the layout and each
visible view's state. Language-server connections are shared by provider and
workspace; adding a view does not start another connection. Tab reordering is
not implemented.

## External file reconciliation

The existing two-second document maintenance cycle checks every open file,
including inactive buffers. Changed clean files up to 1 MiB reload automatically
when the editor is ready and has no active composition. The reload is one undoable
transaction on the authoritative document: revisions advance, the current
selection is clamped to the new text, syntax/search/annotations update, and the
existing semantic session receives the new snapshot and saved notification.
Buffer subscribers receive the reconciled saved state, and recovery captures it.

Dirty, unavailable, or larger files retain their contents and expose a warning
above the buffer with the disk conflict actions. **Dismiss** hides the warning
for that buffer's remaining lifetime, including after switching tabs or later disk
changes. Closing and reopening the file creates a buffer with warnings enabled.
Dismissal does not clear the conflict, permit an unsafe save, or stop reconciliation. A file or revision change during background I/O cancels applying
the reload; starting IME composition also prevents replacement. Undoing a reload
restores the prior text as a dirty local change. Large-file automatic reload is
deferred until a streaming reconciliation policy is available.
