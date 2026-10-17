# Undo history

Velk assigns one undo provider session to each document, shared by its views.
The bundled `velk.undo.tree` plugin supplies the default tree implementation. Undo/redo acts on
that history from the focused pane; other panes' selections follow each edit.
Typing and repeated deletion group until a caret move, another command, a view
switch, a save, or one second of inactivity. Paste and IME commits are separate
undo steps. Programmatic multi-edit transactions stay atomic.

Editing after undo keeps the previous future as another branch. Ordinary redo
follows the most recently edited or visited branch. Alternative branches are
available through Core's linked Rust `redo_branches` and `redo_branch` APIs;
a branch picker is not implemented in the shell.

## Provider boundary

`velk-core-contracts::history` defines providers, document-owned sessions,
immutable snapshots, reversible patches, travel proposals, and merge hints.
Core re-exports this contract as `velk_core::history`. The default tree engine
lives in `plugins/undo/engine`; the native plugin is `plugins/undo`. The engine
has no dependency on Core's document implementation, RXUI, or the plugin SDK.

The named native service is `velk.undo.session`, contract
`velk.contract.undo-session`, version 1. The SDK registers it with
`registrar.undo::<Provider>()`. Only byte views and opaque allocations cross
its C ABI. The host has no SDK lifecycle dependency. Commits send changed text
only, not a flattened document. Snapshot capture is cheap; encoding runs on a
worker against independently owned immutable data.

Settings exposes **Undo provider** (`undo_provider`). A change applies to newly
opened documents; live documents retain their history session. The application
resolves required factories before file/recovery loading. If the default native
service is missing or disabled, it uses the same bundled engine as a linked
fallback. Core itself has no built-in undo engine.

Unloading removes a provider from discovery and from future document selection.
Existing sessions, snapshots, and explicitly retained factory leases pin their
code and remain usable. This allows undo and recovery after logical unload.
They do not reference the destroyed plugin instance. The image can be released
when its final lease/session/snapshot drops.

Archives identify their provider and contain its opaque serialized state. The
application resolves the original provider for restore, rather than interpreting
its bytes with whichever provider is currently selected. If a required custom
provider is unavailable or disabled, recovery reports the failure and preserves
its on-disk snapshot. Incompatible native code is never replaced with another
provider's history decoder.

## RAM and recovery## RAM and recovery

History uses reversible text patches, separate from the live Rope. By default,
a document retains at most 128 history entries and 64 MiB of patch data and
entry overhead. Inactive branch leaves are pruned first, then old ancestors.
An individual edit larger than the budget may have no retained undo entry.
Recovery/save snapshots held by background workers can keep data alive beyond
these limits until their work completes.

Recovery and Reload Window serialize the exact retained tree, including every
replacement in a transaction and the preferred redo branches. Restoring a rename
or a multi-caret edit therefore preserves the mapping of peer carets during
subsequent undo/redo. Encoding does not scan pairs of historical document
snapshots to discover edits.

## Optional disk undo

Set `VELK_UNDO_DIR` to a directory outside the project before starting Velk:

```sh
VELK_UNDO_DIR="$HOME/.local/state/velk/undo" \
  cargo xtask run --release -- path/to/file.rs
```

After a successful file save, the save worker atomically writes that snapshot's
undo tree to the directory. Reopening the same canonical file path loads the
archive on the file worker only if its text matches the current file. Different
on-disk text starts fresh history. Missing archives are normal; damaged or
unreadable archives are logged and do not prevent opening the file. Archive
write failures are logged independently of a successful source-file save.

Archives survive normal application exit and are separate from crash recovery.
They retain prior and deleted text. Files use opaque names derived from canonical
paths; new archives use private temporary-file permissions. Remove the directory
to clear persistent history, and unset the variable to disable it. No directory
is created when the feature is disabled. There is no archive cleanup UI or
background garbage collector yet.

This is persistent storage for the bounded tree captured at Save, not unlimited
history or automatic paging of older entries from disk. Unsaved work and newer
history continue to use recovery. A file changed away from an archived state
will not inherit its undo tree. Archives and recovery use an internal format
that may change during early development without migration.
