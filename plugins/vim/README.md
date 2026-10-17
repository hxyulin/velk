# Vim

Select **Vim** in Settings > Keymap provider to enable this opt-in keymap.
Each editor view starts in Normal mode and keeps its own mode and pending command.
The status bar shows `NORMAL`, `INSERT`, `VISUAL`, `V-LINE`, `V-BLOCK`, and pending counts or prefixes.

| Keys | Action |
| --- | --- |
| `h j k l`, arrows | Move by characters or logical lines |
| `w b e` | Next word, previous word, end of word |
| `0 ^ $` | Start of line, first nonblank, end of line |
| `gg G`, `12gg`, `12G` | First line, last line, numbered line |
| `i a I A` | Insert, append, insert at first nonblank, append at line end |
| `o O` | Open an empty line below or above and enter Insert mode |
| `Escape`, `Ctrl+[` | Return to Normal mode; cancel pending input |
| `x`, `dd` | Delete characters or whole lines |
| `u`, `Ctrl+r` | Undo or redo |
| `Ctrl+v`, `Ctrl+q` | Enter or leave Visual Block mode |
| `I`, `A` in Visual Block | Insert before or append after each row selection |
| `v`, `V` | Enter characterwise or linewise Visual mode; repeat the same key to exit |
| Motions in Visual mode | Extend the selection, including the character under the cursor |
| `o` in Visual mode | Swap the anchor and cursor |
| `d`, `x`, Delete in Visual mode | Delete the selection and return to Normal mode |
| `c` in Visual mode | Change the selection and enter Insert mode |

Counts such as `3w`, `5j`, `2x`, and `3dd` are supported, capped at 10,000.
Unrecognized Normal-mode keys cancel a pending prefix and are consumed.
Insert mode uses the editor's ordinary text, IME, indentation, and autopairs paths.
Native application shortcuts and clipboard paste remain available.

Characterwise selection includes whole graphemes at both endpoints. Linewise
selection includes every touched logical line, including its newline. The
existing selection highlight paints both; selected newlines have a visible cell
so blank lines are visible too. The caret follows the active endpoint separately
from the selected range. `v` and `V` switch selection kinds without losing the
anchor. Escape exits at the active endpoint. Counts and `gg`/`G` work in either
Visual mode. Native Copy uses the inclusive selected range; native paste replaces
it literally. Pointer selection or an external edit ends Visual mode so stale
anchors cannot affect later edits. Selection state is local to the buffer and
is reset on provider replacement or deselection.

Visual Block uses **logical grapheme columns**. Tabs count as one column,
matching the existing logical movement model; proportional and wide glyphs do not
form a pixel-aligned rectangle. Short rows clip to their end without adding
virtual spaces or padding. A block may cover at most 4,096 logical rows. Each row
has an independent range and caret; the active endpoint controls scrolling while
selecting. Block insert/change begins with the first row as the primary caret.

`c`, `I`, and `A` enter Insert mode with a caret on every selected row. Keyboard
text, IME commits, Tab, and Enter apply to all row carets in one transaction per
edit. Backspace/Delete also operate on every caret and stop at line boundaries.
Escape keeps the primary caret and returns to Normal mode. Navigation and
undo/redo finish multi-caret insertion. Pointer selection or external edits clear
the extra carets. Multi-caret typing uses literal text; indentation and pairing
providers currently participate in single-caret editing only. IME preedit is
shown at the primary caret, and its commit is inserted at every caret.

Native Copy joins selected rows with newlines. Native paste distributes one line
per row when line and caret counts match; otherwise it inserts the full text at
each row. It replaces a selected block and returns to Normal mode, or keeps
existing Insert-mode carets active. Multi-row insertion is limited to 4 MiB of
total inserted UTF-8 text per transaction. An oversized selection or edit is
rejected without partially changing the document.

The keymap claims `Ctrl+v` and `Ctrl+q` while the editor is focused in Normal or
Visual modes, ahead of application paste/quit shortcuts. It releases those keys
in Insert mode. Menu and other native clipboard actions retain their standard
behavior. The mode and selections remain local to each editor view.

This is an initial subset, not full Vim emulation. Registers,
yank/put, operator–motion combinations (`dw`, `d$`), text objects, command-line
mode, repeat (`.`), Vim insert-session undo grouping, block cursors, and automatic
indentation for `o`/`O` are not implemented. Word characters use Unicode identifier
continuations; configurable Vim `iskeyword` rules are not implemented. Movement
uses grapheme boundaries and the editor's logical lines, including a final empty
line when the document ends with a newline.
