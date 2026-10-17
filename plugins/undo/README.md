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
