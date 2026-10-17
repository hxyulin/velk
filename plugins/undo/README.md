# Undo tree

Bundled document undo provider. Retains branches, exact reversible patches, and
bounded RAM history. Its immutable snapshots serialize on recovery/save workers.
Core owns live text, state identities, revisions, and view rebasing; it validates
travel proposals before applying them. The provider never mutates a document.

Sessions and snapshots own their state independently of the plugin instance.
Unloading removes the factory from discovery, while existing documents retain
their history and pin the native code until their last reference is released.
The same linked engine provides the fallback when the native service is unavailable
and is available to embedders without the SDK.

## History browser

The separately enabled `velk.undo.browser` plugin provides **Undo: Show history
tree** in the command palette. It shows the active buffer's retained branches,
current and saved states, character counts, and short change excerpts. Activate a
leaf state to restore it; ancestors have a **Restore state** child. This changes the buffer without saving it; ordinary Undo and
Redo continue to work. The panel updates after edits, saves, and buffer switches.

The browser uses the platform history inspection/navigation operations and works
with any undo provider that exposes a tree. It does not read provider archives.
History limits still apply: pruned states cannot be displayed or restored.
