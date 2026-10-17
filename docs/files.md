# Files plugin

`velk.files` ships in the built-in plugin pack. Run
`cargo xtask run --release -- <file>` and open Panels with Command+B / Control+B.
Click a folder to expand or collapse it, and click a file to open or select its
buffer. Existing unsaved buffers are preserved. The `velk.files` frontend depends on `velk.files.backend`, which owns directory
caches and watches. Other frontends can reuse that backend through the shared
[file-tree protocol](feature-backends.md), without linking or copying its implementation.
The hidden-file setting now belongs to **File tree backend**.

Plugins can unload or restart Files independently; restarting the backend also
restarts its active frontends.

The initial file determines the default root: prefer its nearest Git ancestor
(`.git` directory or worktree file), otherwise its nearest Cargo project,
otherwise its parent directory. **Open folder…** in the command palette, or
Command+Shift+O / Control+Shift+O, opens a native folder picker. The selected
folder becomes the exact root, including a subfolder inside a larger repository.
Existing buffers stay open. Cancel leaves the tree unchanged. Switching buffers
does not move the tree; plugin restart and Reload Window preserve the selected
folder and expanded branches.

Initial discovery reads only the root directory. A subdirectory is read when
opened, then cached. Collapsing or reopening a loaded branch requires no I/O.
The `notify` dependency supplies native filesystem notifications. Only loaded
directories receive nonrecursive watches, plus a watch on the root's parent to
catch root removal/replacement. The parent is not listed and sibling events are
filtered out. Unopened descendants are not scanned or watched by the plugin.

Creation, removal and rename events invalidate the tree automatically. Ordinary
content and metadata writes are ignored because this panel only displays names
and structure. Events coalesce into one invalidation bit: a 100 ms quiet period,
with a 500 ms maximum debounce under a sustained stream. The host checks that bit
every 200 ms on its async executor; the frontend also checks the backend on its
owned worker every 200 ms; idle checks perform no directory I/O and no
UI updates. Native event delivery can add backend-specific latency.

An update reloads the root and expanded branches. Descendants below a collapsed
parent wait until that parent is reopened. Watches on discarded cached branches
are removed. Directory deletion/replacement retires its native registration so
reopening or recreation can attach a fresh watch. Stable path IDs preserve
expansion and selection for surviving rows. Watching updates the tree only;
it does not reload open buffers or overwrite unsaved text.

The panel has no refresh button. **Panels: Refresh** in the palette remains a
manual rescan, useful when a filesystem does not deliver reliable notifications.
Read failures and watcher errors appear in the tree. Restarting Files recreates
the watcher. The native watcher and its callbacks are stopped before unload
returns; the plugin's owned worker is joined as well.

Directories sort before files, then by native filename. Hidden files are shown;
Git ignore rules are not applied. Symlinks display an arrow and remain leaves,
so directory cycles cannot trigger recursive traversal. A symlink to a regular
file can be opened through the host's canonical file path handling. Selecting a
directory symlink reports the usual file-open error. Control characters in labels
are replaced and long labels truncated; native action paths retain exact native
bytes/UTF-16 units.

Directory reads, watcher setup/teardown and snapshot preparation run on the
plugin worker. Hook calls only queue work, poll results or check the invalidation
bit, keeping the shared dispatcher and UI available. Cancellation cannot
interrupt a stalled kernel filesystem call.

Prototype bounds remain 1,000 examined entries per directory, 1,800 cached
entries, 2,000 snapshot rows and depth 32, with visible limit notices. Refresh
resets the cache budget. Larger trees will need paging/deltas. Rows remain
virtualized by the host. Indexing, Git ignore support, file-management actions
and drag-and-drop are not implemented.

The shared [panel protocol](panels.md) contains generic lazy expansion,
invalidation and file-open effects; the editor has no Files-specific plugin API.
