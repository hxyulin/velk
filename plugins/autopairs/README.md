# Autopairs

Replaceable default pairing provider. Typing `(`, `[`, or `{` inserts a matching
close and places the caret inside. Typing an opener with text selected wraps the
selection and preserves its direction. Each insertion is one undo step.

`pair_quotes` (default false) also enables single and double quotes. Quotes after
identifier characters or an odd backslash escape are left literal. These lexical
checks do not parse strings, comments, or Rust lifetimes; quotes stay opt-in.
Restart this plugin after changing its settings.

Paste, IME commits, and programmatic edits are always literal. Pair tracking is
owned by the host, scoped to the buffer, and cleared on undo/redo or provider
changes. Brackets that were already in a file are not claimed as generated pairs.
