# Diagnostics

A bundled, replaceable Problems panel and diagnostic navigation commands.

- **Diagnostics: Show Problems** opens the bottom Problems panel.
- **Diagnostics: Next diagnostic** (`F8`) and **Previous diagnostic** (`Shift+F8`)
  move through diagnostics across workspace files, wrapping at the ends.
- Activate a problem row with a click or Enter to reveal its location.

The panel groups diagnostics by document and includes severity, line, column,
message, and available source/code. It refreshes while diagnostics arrive, edits
invalidate old results, or providers retire. All severities participate in navigation.
Multiple diagnostics at one position share a navigation stop.

The host retains workspace publications, including unopened files reported by a
language server. Closing a buffer preserves results that describe its saved text.
Opening a problem uses normal buffer deduplication and jump history. Open targets
require the exact revision; disk targets require matching file contents before
navigation. Unversioned server publications describe disk text and are hidden
from a dirty buffer whose contents differ. The panel does not start its own
analysis: coverage depends on what the workspace language server publishes.
The host exports at most 1,000 diagnostics and bounds display strings; truncation
is shown explicitly. Full diagnostic messages remain available through editor
hover. The plugin uses the standard host-rendered panel and command contracts,
with no RXUI dependency or private LSP connection.

## Filtering

Click the **Filter** row to cycle between all severities, errors and warnings,
and errors only. The command palette also provides **Cycle problem severity**,
**Filter problems by text**, **Filter problems by source**, and **Reset problem
filters**. Text matches the path, message, or code; source matches the diagnostic
source. Matches are case-insensitive, filters combine, and empty input clears
that field. These commands change the current plugin instance's filters.

The Diagnostics plugin's user settings set the default `severity`, `source`, and
`text` filters. `F8` and `Shift+F8` follow the same filtered results as the panel.
**Next error** and **Previous error** restrict navigation to errors while keeping
the text and source filters. Reset restores all severities and clears both fields.
Filter changes invalidate old panel actions even if the resulting list is empty.

Saved-file results are checked against disk before each Problems read. External
replacement, edits, deletion, or loss of access invalidate them without waiting
for another server publication. Recreating a file does not revive old results.
Exact open-buffer results continue to describe that buffer's own revision.
