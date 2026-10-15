# Files plugin

`velk.files` replaces the mock tree and ships in the built-in plugin pack. Run
`cargo xtask run --release -- <file>` and open Panels with Command+B / Control+B.
Click a folder to expand or collapse it, and click a file to open or select its
buffer. Existing unsaved buffers are preserved. The panel's refresh button
reloads directory contents; Plugins can unload or restart Files independently.

The initial file determines a fixed root for the window: prefer its nearest Git
ancestor (`.git` directory or worktree file), otherwise its nearest Cargo project,
otherwise its parent directory. The plugin canonicalizes this path on its worker.
Switching buffers does not move the tree. Reload Window preserves this source
context and expanded branches. A separate Open Folder command is not implemented.

Initial discovery reads only the root directory. A subdirectory is read when
opened, then cached. Collapsing or reopening a loaded branch requires no I/O.
Refresh clears the cache and reloads expanded branches; descendants below a
collapsed parent wait until that parent is reopened. Plugin restart restores the
same expansion behavior using stable path-based IDs. There is no filesystem
watcher yet, so use Refresh after external changes.

Directories sort before files, then by native filename. Hidden files are shown;
Git ignore rules are not applied. Symlinks display an arrow and remain leaves,
so directory cycles cannot trigger recursive traversal. A symlink to a regular
file can be opened through the host's canonical file path handling. Selecting a
directory symlink reports the usual file-open error. Control characters in labels
are replaced and long labels truncated; native action paths retain their exact
bytes/UTF-16 units. Errors reading a folder appear as rows inside that folder.

The plugin owns one worker for directory I/O and snapshot preparation. Native
hooks only queue work or poll results, keeping the shared plugin dispatcher and
UI thread available. Requests are cancellable and unload joins the worker. As
with other blocking filesystem APIs, cancellation cannot interrupt a stalled
kernel filesystem call.

This implementation has explicit prototype bounds: 1,000 examined entries per
directory, 1,800 cached entries, 2,000 snapshot rows, and depth 32. Limit notices
are visible. Refresh resets the cache budget. These bounds keep snapshots small;
very large trees will need paging or deltas rather than raising limits without
measuring. Rows are virtualized by the host. No indexing, search, watching,
renaming, deletion, creation or drag-and-drop is performed.

The shared [panel protocol](panels.md) contains generic lazy expansion and file
open effects; the editor does not contain a special Files-plugin API.
