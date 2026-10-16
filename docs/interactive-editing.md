# Interactive editing providers

`velk-editing` defines renderer-independent editing intents, bounded logical-line
context, absolute Unicode scalar selections, tracked pairs, and edit decisions.
One provider is selected per role: indentation or pairing. Providers can decline;
the editor retains normal text input when a provider is absent or fails.

The native `velk.editing.provider` contract exposes a pure synchronous C callback
at `velk.editing.indentation` or `velk.editing.pairing`. Register a stateless
`EditingBehavior` with `Registrar::editing`. Resolved plugin settings arrive in
requests. Callbacks must be reentrant and fast, perform no I/O, acquire no plugin
instance locks, and retain no input pointers. This trusted native interface does
not enforce a time budget or provide sandboxing.

Interactive evaluation bypasses the lifecycle worker. A host lease pins the
library and excludes generation retirement while a callback runs; retirement
invalidates leases before instance destruction. Old leases decline immediately.
Only bounded JSON byte views and synchronous reply callbacks cross the ABI.
Internal contract versions stay at 1; rebuild host, SDK, and plugins together.

Context and inserted text are each limited to 64 KiB, with at most 4,096
replacements, 256 tracked pairs, and 4 MiB per encoded request/reply. Positions
refer to the original document; final selections and newly tracked pairs refer
to the result. Providers cannot edit outside their supplied context. The host
validates decisions before applying a Core transaction and remains responsible
for revisions, undo, IME, focus, lifecycle, and recovery.

## Editor integration

`indentation_provider` and `pairing_provider` select plugin IDs in Settings.
Provider selection applies to existing and newly opened buffers. Disabling,
unloading, or restarting a provider invalidates its old leases. Plugin setting
changes take effect when that plugin is restarted, matching other plugin settings.

Tab/Shift+Tab, Enter, and Backspace are editing intents only inside the focused
editor. Ordinary committed keyboard text can use pairing; paste and IME commits
remain literal and preedit is never rewritten. Missing, declining, or failing
providers use the editor's original behavior (four-space Tab, literal newline,
normal deletion; Shift+Tab retains focus navigation without an indenter).

Every accepted proposal is one Core transaction with optional final selection.
The host transforms tracked pair endpoints through text edits and invalidates
pairs when either delimiter is replaced. Undo, redo, and provider changes clear
pair tracking rather than claiming that restored or pre-existing brackets were
inserted by the current provider. Tracking is local to a buffer and not persisted.

## Bundled indentation

`velk.indentation` handles Tab and Shift+Tab. Tab at a caret inserts to the next
configured tab stop; a selection indents each affected line. Shift+Tab removes
up to one indentation unit, including existing tabs and partial space prefixes.
A selection ending at a line's beginning excludes that line. Edits retain the
selection direction and form one undo step.

Plugin settings `tab_width` (1–16, default 4) and `use_tabs` (default false) control
inserted text. Columns are logical Unicode scalar columns with tabs expanded to
configured stops; they are not font- or glyph-width measurements.

Enter preserves the leading whitespace before the caret and adds an indentation
unit after `(`, `[`, or `{`. Before a matching close, it inserts an indented blank
line and places the caret inside. Existing whitespace before the close is
replaced so it does not accumulate. These initial rules are lexical; strings,
comments, and language-specific continuation alignment are not parsed.

## Bundled autopairs

`velk.autopairs` inserts matching `()`, `[]`, and `{}` for ordinary typing and
wraps selected text, preserving selection direction. It avoids inserting a close
before identifier text. `pair_quotes` defaults to false; enabling it adds single
and double quotes with basic identifier/escape guards. These rules are lexical,
so language-aware quote, string, comment, and lifetime handling remains deferred.
