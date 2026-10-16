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
