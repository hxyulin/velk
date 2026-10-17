# Plugin panels

Panels are optional, host-rendered contributions discovered through the named
`velk.ui.panels.v1` byte-request hook. The shared `velk-panels` crate contains
serializable data, validation and bounded encoding/decoding. It has no RXUI,
plugin-host or filesystem dependency. The root native plugin ABI remains 1;
`Plugin` has no panel-specific method.

The built-in `velk.files` plugin provides a real, lazy filesystem tree. See
[files.md](files.md) for root selection, behavior and limits. Open Panels with the
shell button, **Panels: Toggle visibility** in the palette, or Command+B /
Control+B. Panels start hidden.

## Contribution contract

Register an ordinary SDK request handler for `hooks::Request(velk_panels::HOOK)`.
Decode `velk_panels::Request` and encode a `velk_panels::Reply`:

- `Describe { context }` requests the complete contribution. Context contains an
  optional absolute native document path, an explicit workspace root overriding
  root inference, whether the host has initialized view
  state, and expanded node IDs retained across refresh/restart.
- `Expand { panel, revision, node, expanded }` asks for an unloaded branch's
  contents. Already loaded branches expand and collapse locally. Expanded IDs
  allow providers to restore previously opened descendants lazily.
- `Invoke { panel, revision, node, action }` activates a leaf's opaque action.
- `Poll { id }` collects asynchronous work; `Cancel { id }` retires it.
- `Changes` checks a provider's bounded invalidation signal. Opt in with
  `Snapshot.watch = true`; return `Reply::Unchanged` or `Reply::Invalidated`. This
  check must not perform I/O, mutate the model, or supersede an active job. The
  host checks on its async executor every 200 ms and only updates the UI after
  invalidation. Automatic rediscovery waits for queued user actions to finish.
  Lifecycle changes cancel the check loop and reject stale replies.
- `Updates { revision }` collects a latest live snapshot from a provider with
  `Snapshot.stream = true`. Return Ready or Unchanged promptly. The host sends
  its highest panel revision for this provider; providers advance that revision
  for every new frame. Reads are idempotent and do not consume the frame, so
  cancellation or a hide/show transition cannot lose a final update. The host checks
  visible/open streaming panels every 8 ms on its async executor; hidden grids
  stop fast checks. Snapshots coalesce, and Unchanged causes no UI update. Slow
  invalidation checks retain their independent 200 ms schedule. Streamed updates
  cannot contain effects and only replace their provider, retaining other panels
  without copying their contents.
- `Input { panel, events }` queues ordered fixed-cell input and returns Ack. There
  is no revision comparison: keyboard streams target a live panel identity, not
  coordinates from a potentially obsolete snapshot. Generation-bound handles
  still retire input connections on unload/restart. Resize, committed text/paste,
  named/control keys and wheel scroll are owned values. No callback or input
  processing runs in the renderer.

A handler can return `Reply::Ready { snapshot }` immediately, or
`Reply::Pending { id }` after queueing owned work. Subsequent polls return the same
job ID until ready. `Reply::Cancelled` ends the exchange. The host polls at 8 ms
intervals using its async executor and cancels on a 30-second timeout. A new
Describe can supersede previous work; providers must reject retired job IDs.
Lifecycle changes invalidate host tasks and handles. A dropped host task alone
does not guarantee delivery of Cancel, so providers also need bounded work,
supersession and unload cancellation.

`Snapshot` describes the provider's complete set of panels. Each panel has a
provider-local ID, title, preferred `Left`, `Right` or `Bottom` slot, revision and
content. Content is `Text`, `Tree` or a bounded fixed-cell `Grid`. Trees use preorder depth and stable node
IDs. The first node starts at depth zero, depth increases by at most one, and
only a branch can have children. A node's `expanded` flag initializes host state
only. `lazy` is valid only for a branch, and requests Expand when opened. Later
snapshots retain expansion/selection for surviving IDs. Unknown expanded IDs
are retained while lazy branches exist, so descendants can be restored when a
collapsed parent is reopened after plugin restart.

The host qualifies panel IDs with plugin ID, so two providers can both call a
panel `files`. Multiple panels in a slot appear as tabs. Host controls own fonts,
colors, accessibility, focus, layout and scrolling. Side slots are 280 logical
pixels wide; the bottom slot is 180 pixels high for text/trees and 320 for grids.
Resizing and moving panels
are not yet exposed. Contributions cannot draw arbitrary native UI or run
callbacks during rendering.

The plugin must validate panel ID, revision, node and action against its current
model before applying a request. Its reply replaces its complete set of panels,
while preserving other providers. A lower revision within the same generation
is rejected. A new generation can restart revision numbering. Leaves without
actions only select; branches toggle expansion. Pointer and Enter/Space button
activation share the same path. This is a virtual list of buttons, not a full
accessibility tree navigation implementation.

## Fixed-cell surfaces

`Grid` is renderer-neutral cell data; see [terminal.md](terminal.md) for its first
provider. Sparse cells carry row/column, grapheme text, width and a style index.
Styles carry sRGB foreground/background and decoration flags. The host validates
bounds, row order, overlap, style references and cursor position. Current bounds
are 200 columns, 60 rows, 4,096 styles and 128 UTF-8 bytes per grapheme, within the
same 1 MiB message limit. Input is bounded before queueing.

The host paints the grid with RXUI/Astrelis, converts sRGB to linear colors,
caches shaped/prepared runs, and places Unicode graphemes at their supplied cell
coordinates. It owns focus, clipboard paste, IME composition and text commits.
Preparing a visible grid can queue a size change through its bounded input
channel; it performs no plugin or filesystem call. Input delivery and live
snapshot collection use independent async tasks so continuous output does not
block keystrokes or directory actions. The root plugin ABI and Plugin trait are
unchanged.

## Host effects

An Invoke reply can include one-shot `Effect::OpenFile { path }` or
`Effect::RevealBuffer { location }` values. A buffer location contains a session
buffer ID, exact revision, and Unicode-scalar offset. Reveal rejects closed or
edited targets, selects the existing buffer, and records navigation history. The host
validates the entire snapshot before applying effects, removes effects before
storing the contribution, and opens files through its normal asynchronous buffer
path. Canonical path deduplication and unsaved buffers retain normal behavior.
Discovery and Expand replies cannot request effects. Refresh, rendering and
session restoration never replay them.

`NativePath` transports Unix bytes or Windows UTF-16 units without lossy UTF-8
conversion. Only absolute paths in the receiving platform's encoding are
accepted, with no NUL and at most 32,768 units. Labels are separate display text;
never reconstruct a filesystem path from a label. Plugins remain trusted native
code; this protocol does not sandbox filesystem access.

## Scheduling, ownership and limits

Discovery happens after plugin lifecycle operations and on explicit refresh.
Binding, native calls, decoding and structural validation run outside the UI
thread. The UI retains owned snapshots and cached visible row indices. RXUI's
fixed-height virtual list constructs visible rows plus overscan. Rendering,
scrolling, collapsing and expanding loaded branches make no plugin calls.
Tree labels stay on one line with ellipsis overflow. Leading indentation is capped
at 35% of the viewport width; trailing padding stays fixed so deep nodes remain readable.

Hook handlers run on the serialized plugin worker and must return quickly.
Filesystem traversal belongs on the provider's own cancellable worker; returning
Pending keeps unrelated plugin calls moving. No plugin-owned callback or memory
is retained in the view. Plugins must stop/join their workers before unload.
Cooperative cancellation cannot interrupt a blocked operating system syscall.

Every payload is capped at 1 MiB. A provider can return at most 16 panels,
10,000 total tree nodes, depth 32, IDs/actions of 128 bytes, labels of 256 bytes,
text panels of 64 KiB and 16 effects. IDs must be unique within their scope.
Across providers a window accepts at most 64 panels and 20,000 tree nodes.
The host bounds pending panel actions and file opens to 32 each. Malformed or
oversized contributions are rejected independently, with errors in the shell.
Full snapshots are suitable for this prototype; larger datasets will need a
paged/delta contract. Push delivery and custom panel content are not implemented.

Reload Window preserves visibility, closed slots, selected tabs, expanded
branches, row selection, explicit workspace root and the initial document path
used as project context.
Data and handles are rediscovered from new plugin instances. Scroll offsets are
transient. The host retains view state for up to 64 contributed panels, allowing
unload/reload/restart to restore expansion without retaining native code.

Validation covers malformed paths/trees, lazy reads, refresh restoration,
cancellation, unusual filenames, bounds and host buffer queuing. The native
pack smoke in `cargo xtask test-plugins` expands a real directory, collects an
open-file effect, unloads with outstanding work, verifies stale-handle rejection
observes a filesystem invalidation, and reads host-owned snapshots after unload.

The bundled `velk.problems` provider contributes a bottom Problems tree through
this same contract. Rows group workspace diagnostics by document; leaf actions
use `RevealDiagnostic` for revision-checked buffers or content-checked file opens. Its Show Problems command selects only
its own panel without reopening other closed slots. See
[the Diagnostics plugin](../plugins/problems/README.md).
