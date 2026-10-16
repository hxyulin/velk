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
