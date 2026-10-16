# Indentation

Replaceable default indentation provider. Tab advances a caret to the next
configured tab stop, or indents selected lines. Shift+Tab removes up to one
indent unit from the affected lines. A selection ending at the beginning of a
line excludes that line. Direction and text selection are preserved through edits.

Settings: `tab_width` (1–16, default 4) and `use_tabs` (default false).
Restart this plugin after changing its settings. Each action is one undo step.

Enter preserves leading whitespace. An opening bracket adds an indentation unit;
when its matching closing bracket follows the caret, Enter inserts an indented
blank line and aligns the closing bracket. These are lexical rules, not a syntax-
aware language formatter. Strings and comments have no special parsing yet.
