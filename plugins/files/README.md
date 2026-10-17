# Files

Browse your project in a lazy, expandable **file tree**.

- Expand directories as needed instead of scanning the whole project.
- Watch directories for changes.
- Open files from the sidebar.

This is the default frontend over `velk.files.backend`. The backend owns directory
reads, caches, and watches; other frontend plugins can reuse it through the shared
file-tree contract. Each frontend has independent expansion and query state.

## File operations

The command palette provides **Files: Create file**, **Files: Create directory**,
**Files: Rename or move current file**, and **Files: Delete file or empty directory**.
Enter paths relative to the selected project folder. Parent directories must exist.
Creating a file opens it; renaming preserves the open document, unsaved changes,
undo history, and views, and restarts language support for the new path.
Existing destinations are never replaced. File moves must stay on one filesystem;
safe directory moves currently require Unix.

Deletion requires typing `delete <project-relative path>` in the prompt. Close
all documents under that path first. Only files and empty directories can be
deleted; recursive deletion is deliberately unavailable. File operations go through
the host's `filesystem.project.write` capability so document identity, recovery,
and plugin notifications stay consistent.

## Settings

Configure **File tree backend** through **Settings → Plugins** or its **Settings** button
in the plugin manager. Changes apply after an explicit restart. See the
[plugin settings guide](../../docs/plugin-settings.md#bundled-integrations) for
options, defaults, and environment-variable precedence.
