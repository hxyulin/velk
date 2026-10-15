# Edit transactions and saved checkpoints

`Buffer::apply_transaction` and `Editor::apply_transaction` accept multiple
replacements against one original document revision. The editor wrapper publishes
one UI/syntax update; the buffer groups the entire operation into one undo entry.
Typing, paste, deletion, and IME commits use the same transaction path internally.
This change adds the mechanism without introducing indentation, auto-pairs, or
other new editing behavior.

```rust
use velk_editor_widget::{Buffer, TextEdit, Transaction};

let mut buffer = Buffer::new("first\nsecond\n");
buffer.apply_transaction(Transaction {
    revision: buffer.revision(),
    edits: vec![
        TextEdit { range: 0..0, text: "    ".into() },
        TextEdit { range: 6..6, text: "    ".into() },
    ],
    selection: None,
})?;
// Both insertions are undone together.
buffer.undo();

```

All ranges use Unicode scalar offsets in the **original** document. Edits can be
supplied in any order; the buffer sorts them and applies them from right to left.
Overlapping ranges and identical starting offsets are rejected, including two
insertions at the same position. Combine such insertions explicitly. Adjacent
ranges with different starting offsets are allowed. CRLF/CR replacement text
normalizes to LF.

Validation and editing occur against a candidate Rope snapshot. An obsolete base
revision, invalid range, overlap, or invalid final selection leaves the document,
selection, dirty state, undo/redo history, and syntax edit log unchanged. No-op
replacements do not advance revisions, dirty the document, or discard redo.

An optional final `Selection` uses scalar offsets in the resulting document.
Without it, both original endpoints map through replacements with right affinity:
positions inside or at the edges of a replacement follow its inserted text.
Selections retain their direction. The final endpoints snap forward to grapheme
boundaries, including when a protocol edit splits or joins a grapheme. Undo/redo
restore the exact original/final selections.

## Revisions and history

A transaction is one undo step, but every effective primitive replacement receives
its own monotonically increasing revision. Undo emits the inverses in reverse
order; redo emits forward edits in original application order. These ordered
coordinates remain valid for incremental Tree-sitter parsing and stale highlight
projection. Consumers must treat `revision()` as a version token, not an undo
count. A future LSP document version can advance once per transaction or use
monotonic buffer versions; it must still convert scalar positions into negotiated
LSP coordinates.

Undo history is bounded to 128 transactions, while the syntax edit log retains
128 primitive edits. A transaction larger than that log still undoes/redoes as a
unit; syntax parsing and stale color projection fall back safely when their full
coordinate history is unavailable.

## Saved state

`checkpoint()` returns an opaque document/content-state identity. A saver captures
it alongside an immutable Rope clone, then calls `mark_saved(checkpoint)` only
after successfully persisting that snapshot. Checkpoints from another buffer are
rejected. Successful save completion does not change the selection, document
revision, or undo history.

`is_dirty()` compares history identities in constant time. Undoing/redoing back to
the saved state clears dirty, and editing while a save is in flight leaves newer
changes dirty. History branches have distinct identities even when revision
numbers or text lengths happen to match. Independently retyping the same bytes as
the saved document still counts as dirty; no document-wide equality scan runs on
input. Loaded buffers start clean. A saved checkpoint outlives eviction of its undo
entry without retaining the old Rope.
