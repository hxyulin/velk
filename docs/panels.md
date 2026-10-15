# Plugin panels

Panels are optional, host-rendered contributions discovered through the named
`velk.ui.panels.v1` byte-request hook. The shared `velk-panels` crate contains
serializable data, validation and bounded encoding/decoding. It has no RXUI,
plugin-host or filesystem dependency. The root native plugin ABI remains 1;
`Plugin` has no panel-specific method.

## Try the mock Files plugin

Build and launch with `cargo xtask run --release -- <file>`. The built-in pack now
includes `velk.demo.files`, an independently unloadable plugin which supplies
**Files (mock)**. Open Panels from the status/activity bar, the command palette's
**Panels: Toggle visibility**, or Command+B / Control+B. Panels start hidden.

The demo tree is hard-coded sample data unrelated to the open document. Branches
expand/collapse; rows can be selected. File rows have no action, and never open,
read, write or scan files. Refresh requests another snapshot. Unloading the demo
removes its contribution; loading or restarting it discovers it again. It shares
the built-in native image without sharing its logical instance/lifecycle.

## Contribution contract

Register an ordinary SDK request handler:

```rust
use velk_plugin_sdk::{Plugin, Registrar, hooks};

// Within Plugin::register:
reg.provide(hooks::Request(velk_panels::HOOK), |plugin, bytes| {
    match velk_panels::decode::<velk_panels::Request>(bytes)? {
        velk_panels::Request::Describe => {
            let snapshot = plugin.panel_snapshot();
            snapshot.validate()?;
            velk_panels::encode(&snapshot)
        }
        velk_panels::Request::Invoke { panel, revision, node, action } => {
            plugin.apply_panel_action(panel, revision, node, action)?;
            velk_panels::encode(&plugin.panel_snapshot())
        }
    }
})
```

`Snapshot` describes the provider's complete set of panels. Each panel has a
provider-local ID, title, preferred `Left`, `Right` or `Bottom` slot, revision and
content. Content is currently `Text` or `Tree`. A flat tree is a list (all depths
zero); nested trees use preorder depth and stable node IDs. The first node starts
at depth zero, depth increases by at most one, and only a branch can have children.
A node's `expanded` flag initializes host state only. Later snapshots retain the
user's expansion/selection for surviving IDs, even when the plugin restarts.

The host qualifies panel IDs with plugin ID, so two providers can both call a
panel `files`. Multiple panels in a slot appear as tabs. Host controls own fonts,
colors, accessibility, focus, layout and scrolling. Side slots are currently 280
logical pixels wide and the bottom slot is 180 pixels high; resizing and moving
panels are not yet exposed. These contributions cannot draw arbitrary native UI
or run callbacks during rendering.

`Invoke` is optional per leaf, via the node's opaque `action` string. Selecting a
leaf with an action submits panel ID, snapshot revision, node ID and action.
The plugin must validate these against its current model before mutating state.
Its reply replaces its complete set of panels, while preserving other providers.
A lower revision in an action reply for an existing panel is rejected within the same generation.
A new generation can restart revision numbering. Leaves without actions only
select; branches only toggle expansion. Pointer activation and the buttons'
Enter/Space activation share the host action path. This is a virtual list of
buttons, not a full accessibility tree navigation implementation.

## Scheduling, ownership and limits

Discovery happens after plugin lifecycle operations and on explicit refresh.
Binding, native calls, JSON decoding and structural validation run outside the UI
thread. The UI holds owned snapshots and a cached list of visible row indices.
RXUI's fixed-height virtual list constructs only visible rows plus overscan;
no plugin call is made for a row, a frame, scrolling or folder expansion.
Expansion rebuilds the bounded visible-index cache once per interaction.

Hook handlers run on the serialized plugin worker and should return quickly.
A real file provider should do traversal/watching/indexing on its own cancellable
worker, then return cached snapshots. This first contract is request/reply:
push updates, deltas, pending/poll responses and arbitrary custom panel content
are intentionally not implemented yet. Expensive work must not be performed in
a synchronous handler merely because it is off the UI thread.

Every payload is capped at 1 MiB. A provider can return at most 16 panels and
10,000 total tree nodes, with depth at most 32, IDs/actions at most 128 bytes,
labels at most 256 bytes and each text panel at most 64 KiB. IDs must be unique
within their scope. Across providers a window accepts at most 64 panels and
20,000 tree nodes. Malformed/oversized providers are rejected independently, with
errors shown in the shell. Full snapshots are suitable for this prototype;
large real datasets will need a paged/delta contract.

Lifecycle submission immediately invalidates outstanding panel tasks and clears
cached contributions/handles. Completion discovers the current catalog again.
An epoch check rejects old UI replies, and generation-bound hook handles reject
retired native instances. Dropping a host task does not reverse a native action
already executing. No plugin-owned callback or memory is retained in the view;
plugins still must stop/join their own workers before unload.

Reload Window preserves panel visibility, closed slots, selected tabs, expanded
branches and row selection. Data/handles are rediscovered from new plugin
instances. Scroll offsets are transient in this initial implementation. The host
keeps view state for up to 64 contributed panels, allowing unload/reload/restart
to restore expansion without retaining native code. Bar visibility settings
remain separate from panel visibility.

Validation includes contract rejection tests, host layout/virtualization/focus
checks, and the native pack smoke in `cargo xtask test-plugins`, which requests a
real DLL snapshot, unloads its provider, verifies stale-handle rejection and
reads the host-owned snapshot after unload.
