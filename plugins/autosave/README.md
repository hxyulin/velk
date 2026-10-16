# Autosave

Save file-backed buffers after editing pauses. Enable `enabled` in plugin settings
and restart the plugin. It defaults to off. `delay_ms` defaults to 1,000 ms.

Untitled documents keep using editor recovery until you choose a file path.
Autosave never opens Save As, discards changes, or overwrites disk conflicts.
It saves through the host with the exact observed revision, preserving focus,
undo history, and normal save validation. Saving an older snapshot does not
cancel a newer pending edit. Busy operations retry after the configured delay;
file failures use the editor's normal notice and wait for another edit.

The owned worker sleeps between deadlines and joins on unload. Disabling or
restarting cancels pending timers; already accepted saves may finish normally.
