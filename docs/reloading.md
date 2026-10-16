# Plugin restart and Reload Window

The command palette exposes **Plugins: Restart active plugins** and **Reload
window**. Each loaded plugin also has a Restart button in the F4 Plugins modal.
Reload Window has a direct shortcut: Command+Option+R on macOS, Control+Alt+R
elsewhere. The direct shortcut works inside supported menus, preserving the open
menu; selecting a palette command closes that palette as normal.

## Plugin restart

Restart is one serialized plugin-worker operation. It snapshots optional state,
retires dependent semantic sessions, destroys the selected instances, creates new
generations, restores state, and reattaches services to the latest document.
Calls cannot interleave with teardown/recreation. Old hook handles become stale.
Restarting one plugin keeps sibling instances alive, including members sharing
its native pack. Restart-all selects only active IDs; deliberately unloaded
plugins stay unloaded. The buffer, undo/redo, layout, menus and search query stay
in the host. Search results are recomputed after lifecycle completion.

The operation reads the current validated manifest, so a staged newer artifact
can be selected. A missing manifest/active entry or failed state capture cancels
before any instance is retired. Artifact/factory/restore failures after teardown
are reported in Plugins; the document and unaffected instances remain alive.
A failed restore retires the new instance. Failed native teardown retains code
and blocks reactivation until the process exits.

Native image lifetime still follows the existing residency contract. Logical
restart does not promise to unmap resident code, nor a shared pack still used by
siblings. Reload Window creates a new process and releases the old process's
native code when it exits. Native plugins remain trusted, cooperative code;
restart is not a sandbox or a way to interrupt a hung native callback.

## Optional plugin state

Plugins may register a paired set of ordinary byte request hooks:

- `velk.lifecycle.snapshot.v1`: empty request, owned state response.
- `velk.lifecycle.restore.v1`: the captured state as input, acknowledgement response.

Constants are exported by `velk-plugin-api`; implementations use the existing
SDK `Registrar::provide(hooks::Request(...), ...)`. No root trait method or ABI
version change is required. With neither hook the plugin restarts from its
normal default state. Registering only one is an error. State is limited to
1 MiB per plugin, must contain no pointers or borrowed/native handles, and should
include a plugin-owned schema version. Snapshot must be observational: on an
error, the running instance must remain usable. Restore completes before semantic
sessions attach. Long-lived jobs/servers must still cancel and join on teardown;
serialize durable intent/configuration, not in-flight futures or callback state.

The same hooks carry opt-in state through Reload Window. Host service inputs
such as the current document are replayed; completed editing, save, and other
side-effecting user commands are never replayed. The built-ins currently rely on
host-owned configuration/document state and restart their workers/services fresh.

## Window handoff

Reload briefly blocks input with a progress modal and snapshots:

- Buffer text, directed selection, revision, undo/redo groups and saved identity.
- The original file target, line-ending style and disk hash used by safe saves.
- Horizontal/vertical scroll offsets and normal window geometry/maximized state.
- Current appearance preferences and enabled plugin IDs, including an empty set.
- Settings/Plugins/palette open state, palette query/selection, and search query,
  options and open state. Search results are recomputed without moving the
  restored document selection. Focus returns to the search input when open,
  otherwise the editor; modals apply their own autofocus/focus trap.
- Opt-in plugin state, copied on the plugin worker.

Rope/history snapshots are cheap clones on the UI thread. A blocking worker
encodes the current text plus history deltas, writes a versioned private temporary
session, and launches the current executable with the same single file argument.
Private environment variables identify the handoff files; the public CLI remains
`velk <file>`. Native filename encoding is preserved. The replacement restores
without rereading/rebaselining the document from disk, so external changes during
reload remain save conflicts. Syntax, diagnostics, hover cards, LSP sessions,
search results and pending requests are recreated rather than serialized.

The old window exits only after the replacement enters a second render frame.
This avoids treating native window creation as readiness before the initial UI
has painted. Failed startup or a 20-second readiness timeout kills/reaps the
replacement and leaves the old document usable with an error notice. Temporary
handoff files are removed after acknowledgement or failure. Reload is disabled
while saving, showing a native save dialog, writing settings, changing plugin
lifecycle, or composing IME text. It does not save the file or prompt to discard
unsaved edits.

This is an internal, same-build session handoff, not crash recovery or a stable
cross-version session format. Fullscreen/minimized state, exact focus within an
open modal and transient service popups are not persisted. Only one document
window is currently supported by the application.

## Multiple buffers

Reload transfers every open file-backed buffer and restores tab order and the
selected tab. Inactive buffers retain unsaved text, undo history, selection,
scroll, disk baseline and find state. Pending file opens or saves in any buffer
block reload. See [buffers.md](buffers.md) for buffer ownership and lifecycle.

Document IDs, the next-ID counter, and the explicit workspace root now survive
replacement alongside untitled buffers. The replacement persists its initial
recovery snapshot before acknowledging readiness; successful handoff retires the
old process’s recovery snapshot. See [document-foundation.md](document-foundation.md).
