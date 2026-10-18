# Undo tree

Bundled document undo provider. Retains branches, exact reversible patches, and
bounded RAM history. Its immutable snapshots serialize on recovery/save workers.
Core owns live text, state identities, revisions, and view rebasing; it validates
travel proposals before applying them. The provider never mutates a document.

Sessions and snapshots own their state independently of the feature instance.
The same linked engine is available without the registration adapters. The
interactive undo-tree panel has been removed; ordinary undo/redo, branching
history, recovery snapshots, and optional disk history remain.
