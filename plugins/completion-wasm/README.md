# Wasm completion matching

The same ranking algorithm as the default completion plugin, executed in an
isolated Wasmtime instance with fuel and memory limits. No filesystem, network,
WASI, host callbacks, or shared memory is available to this guest.

Build and stage with `cargo xtask plugins --wasm`. Select
`velk.completion.wasm` under Plugins > Interfaces > Completion matching provider.
The native provider remains the default. Failed, busy, and retired guests use the
completion session's existing prefix fallback.
