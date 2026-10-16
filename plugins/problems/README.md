# Diagnostics

A bundled, replaceable Problems panel and diagnostic navigation commands.

- **Diagnostics: Show Problems** opens the bottom Problems panel.
- **Diagnostics: Next diagnostic** (`F8`) and **Previous diagnostic** (`Shift+F8`)
  move through diagnostics across open buffers, wrapping at the ends.
- Activate a problem row with a click or Enter to reveal its location.

The panel groups diagnostics by document and includes severity, line, column,
message, and available source/code. It refreshes while diagnostics arrive, edits
invalidate old results, or buffers close. All severities participate in navigation.
Multiple diagnostics at one position share a navigation stop.

Only current diagnostics from open semantic documents are available; this does
not run a project-wide analysis. Positions are revision-checked before navigation.
The host exports at most 1,000 diagnostics and bounds display strings; truncation
is shown explicitly. Full diagnostic messages remain available through editor
hover. The plugin uses the standard host-rendered panel and command contracts,
with no RXUI dependency or private LSP connection.
