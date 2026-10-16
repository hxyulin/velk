# Buffer lifecycle events

Register `Registrar::on_buffer_event` and declare `editor.buffer.read` to receive
`velk_plugin_sdk::buffers::Event` values. The host delivers bounded JSON batches
through the existing named event ABI, after document operations have committed.
Event handlers run on the plugin worker: queue long work on an owned worker.

- `Snapshot { buffers }` replaces the consumer's known set of buffers. It is sent
  after plugin load/restart and when pending notifications need resynchronization.
- `Opened { buffer }` reports a newly added buffer.
- `Changed { buffer }` reports committed text changes, including typing, paste,
  IME commits, undo/redo, and plugin edits. Selection and preedit changes alone do
  not produce it. Several revisions may coalesce into the latest metadata.
- `Saved { buffer, saved_revision }` reports a successful disk save. The current
  buffer revision may be newer and still dirty.
- `Closed { id }` removes the buffer from the consumer's state.

These are post-commit state notifications, not a veto hook or a promise to invoke
plugins once per individual replacement. Plugins never run inside Core's atomic
transaction. IDs and revisions have the same meaning as the host-service API.
The dispatcher uses one background task per window, preserving lifecycle order
and applying backpressure outside the UI. It coalesces changes and replaces an
oversized pending queue with an authoritative snapshot. Failed delivery is logged.
An event subscriber must treat snapshots as authoritative and tolerate duplicate
or coalesced metadata. The host rejects subscriptions without the read declaration.

## Autosave

The bundled `velk.autosave` plugin is disabled by its `enabled` setting by default.
Enable it in Settings > Plugins > Autosave and restart the plugin. `delay_ms`
controls its idle delay (250–60,000 ms, default 1,000).

The plugin owns the debounce timer and scheduling policy. It skips untitled and
clean documents, cancels pending work on close/unload, and asks the host to save
only the observed revision. `SaveBuffer.expected_revision` makes the revision and
file-backed checks atomic with dispatch, preventing a stale timer from saving a
newer edit. Normal manual saves pass `None`.

The host retains save policy: disk-conflict checks, atomic replacement, undo
history, composition/close guards, and notices. Autosave does not open Save As or
overwrite conflicts. A failed save waits for the next edit; a busy host retries
without replacing a newer scheduled revision. Accepted saves can finish after a
plugin is disabled, just like other accepted host operations.
