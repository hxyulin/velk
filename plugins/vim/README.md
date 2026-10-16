# Vim

Select **Vim** in Settings > Keymap provider to enable this opt-in keymap.
Each buffer starts in Normal mode and keeps its own mode and pending command.
The status bar shows `NORMAL`, `INSERT`, `VISUAL`, `V-LINE`, and pending counts or prefixes.

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

This is an initial subset, not full Vim emulation. Visual Block, multiple cursors, registers,
yank/put, operator–motion combinations (`dw`, `d$`), text objects, command-line
mode, repeat (`.`), Vim insert-session undo grouping, block cursors, and automatic
indentation for `o`/`O` are not implemented. Word characters use Unicode identifier
continuations; configurable Vim `iskeyword` rules are not implemented. Movement
uses grapheme boundaries and the editor's logical lines, including a final empty
line when the document ends with a newline.
