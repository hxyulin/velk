# Documents, workspaces, recovery, and disk changes

Velk can start with `cargo xtask run` or `velk` without a file argument. It opens
an empty untitled document, or resumes the most recent recoverable workspace.
An explicit file argument still reports open errors before creating a window.

## Documents and destinations

Close/prompt decisions and Save All ordering live in Core's UI-independent
`lifecycle` module. The application supplies current document facts and adapts
the returned actions to RXUI dialogs and file jobs. See [Core boundaries](core.md).

Each document has a window-scoped numeric ID, independent of its optional disk
file. Opening a file resolves its canonical path for deduplication. Untitled
names are display labels: no placeholder files are created on disk. New File
creates a separate buffer, even when other untitled buffers have identical text.

- **New File**: Command/Ctrl+N.
- **Save**: Command/Ctrl+S; untitled documents open the native Save As picker.
- **Save As**: Command/Ctrl+Shift+S.
- **Save All**: Command/Ctrl+Alt+S (Command+Option+S on macOS).

All four actions are available in the File menu on supported platforms and the
command palette. The application accepts zero or one file argument.

Closing the final document leaves the window open with zero buffers and a
**No documents open** view offering New File and Open File. No hidden or replacement
untitled document is created. Buffer-specific commands are disabled, while window
commands, panels, plugins, settings and the workspace root remain available.
Command/Ctrl+W closes a buffer; Command/Ctrl+Shift+W explicitly closes the window.
Recovery and Reload Window preserve an empty workspace and its next document ID.

Save As changes the existing document's destination without changing its ID,
undo/redo history or selection. Its filename updates language detection. Saving
captures an immutable text snapshot and checkpoint; edits made while writing
remain unsaved. A destination already open in another buffer is rejected,
including symlink aliases. Other file operations cannot open a second copy while
a destination-changing save is pending. Ordinary background saves still allow
opening unrelated files. New files use no-clobber creation. Existing destinations use
an atomic replacement and content-baseline check; symlink targets are resolved
without replacing the symlink itself. New documents default to LF; copies of an
existing document retain its original newline convention. New files respect the
process umask on Unix, and existing files retain their permissions.

Save All visits dirty documents in tab order, without changing the selected tab.
It prompts for each untitled destination in turn. Cancelling a picker or failing
a write stops the remaining saves and leaves those documents dirty. An error
from a background tab is also reported on the visible document. Empty, clean
untitled documents are skipped. A Save All operation is a sequence of individual
file saves, not a multi-file atomic transaction.

Document IDs and their allocation counter survive recovery and Reload Window.
Closing a document never reuses its ID within the resumed workspace. The current
bound is 256 open documents per window.

## Workspace context

`Workspace` is host-owned and holds the explicit folder root independently of
the active document. Open Folder updates this context and the context sent to
panel providers. Switching buffers or using Save As does not change an explicit
root. Without an explicit root, providers retain their existing inference from
the workspace's initial file-backed document. An empty window can choose a
folder before creating or saving any file.

The root survives both recovery and Reload Window. Plugin access to generic
workspace/document services will be exposed through shared contracts in a later
platform milestone; this change does not introduce arbitrary plugin document
mutation or a second plugin event system.

## Recovery

Recovery stores snapshots separately from source files. It preserves all open
buffers, unsaved text, undo/redo state, saved checkpoints, disk baselines,
selection, scroll, tab order, the selected document, document IDs and workspace
root. It does not write unsaved changes into a user's files.

The recovery directory defaults to `recovery/` beside `settings.json`.
`VELK_RECOVERY_DIR` overrides it; an isolated `VELK_SETTINGS_PATH` also isolates
the default recovery directory. Each running window owns a separate snapshot
and OS file lock. Another process skips locked workspaces, so launching a second
editor does not duplicate the buffers of a live editor.

Changes schedule a coalesced snapshot after 200 ms. The dedicated writer adds a
200 ms coalescing window, encodes history, syncs a temporary file, then atomically
replaces the previous snapshot. A two-second maintenance interval also captures
selection and scroll changes. Unchanged document/workspace state skips encoding
and disk writes. Encoding and disk writes run off the UI thread;
document snapshots are not cloned on every input event. Recovery is periodic,
so a crash can lose the latest changes that have not reached a completed write.

On startup, the newest valid unlocked workspace is claimed and restored. An
explicitly requested file is selected if it is already recovered; otherwise it
opens alongside the recovered documents. If that would exceed the document
limit, startup asks the user to launch without a file and close a document first.
Other abandoned workspaces remain available to another launch.

A successful normal close removes that window's snapshot after all save/discard
prompts succeed. Cancelling a close keeps it. Application errors retain the
snapshot. Reload Window writes a durable recovery snapshot in the replacement
process before acknowledging readiness; the old process then retires its backup.

Unreadable, corrupt and unsupported snapshots are preserved, logged, and reported
in the editor. Recovery snapshots are limited to 128 MiB; an oversized snapshot
fails without replacing the previous good one and produces a recovery notice.
This is a private versioned recovery format, not a plugin storage API. Clean
shutdown currently does not retain a separate recent-workspace session catalogue.

## External changes

While a window is open, the host checks file metadata in the background every
two seconds. Content hashing runs only when metadata changes, or when a fresh
baseline needs checking. Replaced, modified, missing or inaccessible files show
a notice without changing the local editor contents.

- **Reload from disk** reads the current version. Dirty documents require explicit
  confirmation. If another edit occurs during the read, the result is rejected
  and local edits remain intact. A successful reload establishes a new clean
  buffer and disk baseline, preserving the document ID.
- **Open disk version** opens a detached, unsaved comparison copy in another tab.
  It cannot save over the original through ordinary Save. This is a comparison
  tab, not a structured diff or merge viewer.
- **Overwrite disk** requires confirmation and establishes a fresh disk baseline
  on the worker before the ordinary atomic save. If the file was deleted, it is
  recreated with no-clobber semantics and the original newline convention.
- **Save As** remains available to preserve both versions under different paths.

Ordinary Save still rejects changed disk contents even if the periodic check
has not run yet. Checks also apply to recovered buffers, using their original
disk baselines. This avoids treating newer external contents as the baseline
for unsaved recovered text. Metadata polling and the final save check do not
provide an OS-level compare-and-swap against another process racing the rename.
