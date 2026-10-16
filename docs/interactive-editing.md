# Interactive editing providers

`velk-editing` defines renderer-independent editing intents, bounded logical-line
context, absolute Unicode scalar selections, tracked pairs, and edit decisions.
One provider is selected per role: indentation, pairing, or optional keymap.
Providers can decline; an absent provider leaves ordinary editing available.

The native `velk.editing.provider` contract exposes a pure synchronous C callback
at `velk.editing.indentation`, `velk.editing.pairing`, or `velk.editing.keymap`. Register a stateless
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
providers generally use the editor's original behavior (four-space Tab, literal newline,
normal deletion; Shift+Tab retains focus navigation without an indenter).
If an active provider fails or exceeds a limit while indenting a selection, the
selection remains untouched and the failure is logged. Tab must not replace the
selected document text with spaces in that case.

Every accepted proposal is one Core transaction with optional final selection.
The host transforms tracked pair endpoints through text edits and invalidates
pairs when either delimiter is replaced. Undo, redo, and provider changes clear
pair tracking rather than claiming that restored or pre-existing brackets were
inserted by the current provider. Tracking is local to a buffer and not persisted.

## Keymap providers

`keymap_provider` selects a modal input provider. The default empty string means
standard editing. Select `velk.vim` to enable the bundled Normal/Insert/Visual keymap;
see its [supported keys and limitations](../plugins/vim/README.md).

`Intent::Key` carries committed keyboard text or a named key, modifiers, and
host-owned per-buffer `input::State`. An empty key initializes a buffer. Keymap
requests carry selection and document length but no document text or pair list,
so their evaluation is independent of the size of selected lines.
`Decision::Input` returns a bounded state and list of editor actions. A response
with `handled: false` retains state while allowing ordinary input to continue.
Only keymap requests may return input responses; they cannot return text proposals.

State contains a stable `mode` ID, a status label, at most 4 KiB of opaque provider data, and flags to
capture text and clamp the caret to line characters. A response contains at most
16 actions, with counts limited to 1–10,000. The editor model applies motions,
line/character deletion, line insertion, undo, and redo to the authoritative Core
buffer. Character movement and deletion preserve extended grapheme boundaries.
Whole-line or counted-character deletions form one undo step. Undo and redo clear
pair tracking. Key interpretation, counts, prefixes, and mode transitions stay in
the plugin. The status label is visible in the normal status bar, or in the notice
area when that bar is hidden.

The mode ID uses at most 64 lowercase ASCII letters, digits, underscores, or
hyphens. An empty ID leaves the mode unspecified; the host uses `insert` for
non-capturing input and `command` for capturing input. User [keybindings](keybindings.md)
match this ID rather than the display label, which may include counts or prefixes.

State is local to each buffer, reset when its provider is replaced or deselected,
and is not serialized into recovery data. Unloaded provider leases decline and
release modal capture. If an active capturing provider fails validation or
execution, the editor consumes input and logs the error to avoid inserting
command text. Selecting Standard editing restores ordinary input.

Committed keyboard text is interpreted once, after native keyboard layout and
dead-key processing. Named keys are handled before default navigation. Insert
mode passes ordinary text, indentation, pairing, and IME through to their existing
paths. Normal mode suppresses preedit and IME commits. Native clipboard paste
remains literal, and application shortcuts retain their existing routing. The
keymap only participates in document editor input; settings and picker fields
retain their standard controls.

### Visual selections

Keymaps can start characterwise or linewise selection with `Action::Select`,
change its kind, swap endpoints, cancel it, delete it, or change it. Subsequent
motion actions extend the selection. The editor model owns the logical anchor
and cursor and projects them into Core's directed, exclusive-end selection.
Characterwise projection includes both endpoint graphemes; linewise projection
includes complete logical lines. Vertical movement retains the preferred column.
Delete/change produces one Core transaction, and linewise deletion at EOF removes
the preceding newline when needed to avoid leaving a spurious empty line.

The renderer receives the logical caret position separately from the selected
range. This keeps status position, painting, and scroll reveal on the active
endpoint even when the selected range ends on the next line. Selected newlines
paint a small cell at the line end, including otherwise empty lines. Selection
highlighting uses the existing GPU path and remains independent of text shaping.
Clipboard commands use the projected Core selection, so native Copy includes the
last selected grapheme. External text or selection changes invalidate modal
anchors and restore Normal mode; they cannot leave a stale renderer position.

### Block selections and multiple carets

`SelectionKind::Block` projects the logical anchor/cursor rectangle into a
revision-bound `SelectionSet` in the editor model. A set contains disjoint row
ranges and their logical carets plus a primary index. Core retains the primary
selection and the authoritative text/history; all row edits use a single Core
transaction. Selection sets are view state and are not persisted in recovery.
The first implementation uses grapheme columns, counts a tab as one column, and
clips short rows at EOL. It does not add virtual spaces. Blocks are limited to
4,096 rows, and inserted text across a multi-selection transaction to 4 MiB.

`InsertAtSelections` creates row insertion carets at the start or end of each
range; block change creates carets after deleting each range. Literal typing,
paste, Backspace, and Delete use the shared set edit path. Matching multiline
paste distributes its lines across rows. Undo/redo and navigation end multi-caret
insertion. External revision or primary-selection changes invalidate the set.
Per-row carets snap to final grapheme boundaries after text changes.

The renderer receives an immutable, sorted region snapshot. It finds the region
for each visible logical line and paints its selected range and caret through
the existing line cache. Scroll reveal, status, and IME positioning use the primary
caret. Invisible block rows do not force shaping or GPU preparation. During IME
preedit only the primary composition is drawn; committing inserts at every caret.
Indentation and pairing proposals currently remain single-caret operations;
multi-caret Tab/Enter and text use their literal fallbacks.

Keymap state may declare up to eight `control_keys`, each a single character.
The editor binds these in its focused command scope so a provider can claim
Ctrl chords before ancestor or application shortcuts. Vim claims Ctrl+V/Q in
Normal and Visual modes and releases them in Insert mode. This avoids conflicts
with application paste/quit without changing keyboard behavior in other controls.


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

Typing a closing delimiter skips only a matching tracked close at the caret.
Backspace removes both delimiters only between an empty tracked pair. Skipping
changes only the caret; paired deletion is one undo step. The most recent 256
pairs per buffer are tracked; older pairs fall back to literal editing.

## Configuration example

The same values are available in Settings. Provider selection applies immediately;
restart a plugin after changing its own options.

```json
{
  "keymap_provider": "velk.vim",
  "indentation_provider": "velk.indentation",
  "pairing_provider": "velk.autopairs",
  "plugins": {
    "velk.indentation": { "tab_width": 4, "use_tabs": false },
    "velk.autopairs": { "pair_quotes": false }
  }
}
```

Disable either plugin in the plugin manager to use plain editing for that role,
or select another loaded plugin implementing the same role. Settings and plugin
manager surfaces retain ordinary Tab focus navigation.
