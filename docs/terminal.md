# Terminal plugin

`velk.terminal` contributes a bottom-panel terminal through the existing named
panel request hook. Open **Terminal: Toggle bottom panel** in the command palette,
or press **Control+backtick** on macOS/Linux. Panels: Toggle visibility also shows it
alongside the other open slots. Click the terminal to focus it. The dedicated
toggle focuses it when opening and returns focus to the editor when closing.

The shell is created lazily when the grid is first rendered/sized. Loading the
plugin alone starts no shell. Closing/hiding the panel keeps the session running;
reopening it shows the latest screen. The initial working directory is the
explicit Open Folder selection, otherwise the initial file's nearest Git/Cargo
root or parent directory. Changing the tree root does not change an already
running shell's directory. A new session uses the latest context.

The shell is `$VELK_TERMINAL_SHELL`, otherwise `$SHELL`, otherwise `/bin/sh`, with
`-i`. The PTY advertises `TERM=xterm-256color`, `COLORTERM=truecolor` and
`TERM_PROGRAM=Velk`. No Ghostty application installation is required.

## Engine and ownership

The emulator is Ghostty's **libghostty-vt**, rather than an embedded AppKit view
or a separate terminal application. The `ghostty-vt` Rust adapter is pinned to
`0.7.2+ghostty.1.3.2-main.b40acce`; its sys crate pins Ghostty commit
`b40acce58dcf77df52231c3798ea58e924647c89` and verifies its source archive hash.
The upstream C interface is still evolving, so upgrades are deliberate.

Ghostty parses output, handles screen state/reflow/scrollback and alternate
screens, resolves cell styling, encodes keyboard events and paste, and produces
terminal query replies. `portable-pty` provides the shell/PTY. These dependencies
are linked into the built-in native plugin pack, not the editor binary. The
host contains only a generic fixed-cell renderer and owned input/snapshot data.
The plugin API remains a named byte hook, and the root C ABI stays at 1.

One plugin-owned worker creates and owns Ghostty's thread-affine objects and the
PTY. A `mio` readiness loop wakes for PTY data or queued input; idle sessions
only time out every 250 ms to reap exited children. Writes are nonblocking and
request writable readiness only while bytes remain queued. Output snapshots
coalesce at 8 ms; only the latest frame is retained. Revision-based reads are idempotent so a
cancelled check or hidden panel cannot consume and lose the final frame. While a streaming panel is
visible the host collects changes every 8 ms asynchronously. Unchanged replies
produce no UI updates. Hidden terminal panels stop fast host checks while the
plugin keeps reading output. Files retains its independent 200 ms invalidation
schedule, and terminal frames preserve other panels without cloning their trees
or rescanning directories. These are scheduling targets, not a measured 120 Hz
or input-to-display latency guarantee.

Unloading/restarting Terminal stops and joins its worker, closes the PTY,
terminates the owned shell and active foreground process group, and reaps the
shell. Restart starts with an empty emulator and launches a new shell when shown.
Reload Window also creates a new terminal session; live processes, scrollback and
shell state are not serialized. Hiding the panel is the operation that preserves
the running session. Detached daemons are not managed by this plugin.

## Interaction and current limits

Supported: ordinary keyboard input, Ctrl+C and other control keys, arrows,
navigation/function keys, Unicode committed text, IME preedit/commit, paste,
true-color foreground/background, bold/italic/faint, underline, strike/overline,
scrollback with the wheel, alternate-screen applications, and PTY resize when the
window changes. Ghostty's paste encoder strips unsafe control bytes and handles
bracketed paste. Command+V uses native paste on macOS; Command+Shift+V /
Control+Shift+V also paste. Control+V remains terminal input. Global palette,
reload and folder shortcuts remain available while the terminal is focused.

This first version has one session per plugin instance and a fixed 320-logical-
pixel panel. The grid is bounded to 200 columns by 60 rows, 4,096 styles, 128 UTF-8
bytes per grapheme cell and the panel protocol's 1 MiB payload. Input batches are
at most 64 events / 64 KiB of text, the host and plugin queues each hold at most
128 items, and pending PTY writes are capped at 256 KiB. Exceeding a frame limit
shows a status message; input/worker failures are visible and require restart.

Selection/copy, clickable links, terminal mouse reporting, images, configurable
fonts/themes, resizable panel dividers and multiple terminal tabs are future
work. The cursor is a steady block, so there are no idle blink redraws. Clipboard
and window-management requests from terminal escape sequences are ignored.
The PTY readiness/cancellation integration currently supports Unix (macOS/Linux);
Windows needs a ConPTY readiness/cancellation implementation before it is usable.

## Building and validation

The native library requires Zig **0.16.x** at build time. Prepare pinned inputs
once, without changing system packages or toolchain settings:

```sh
cargo xtask setup-terminal
cargo xtask build --release
cargo xtask run --release -- path/to/file
cargo xtask test
VELK_TERMINAL_SHELL=/bin/sh cargo xtask test-features --release
```

The setup helper uses Python 3 and curl, caches the pinned Zig wheel from PyPI
(with checksum verification) and Ghostty's checksum-pinned official source
archive under `~/.cache/velk/toolchains`. `VELK_TOOLCHAIN_DIR` overrides that cache.
The helper downloads build inputs only. `xtask` automatically supplies cached
`ZIG` and `GHOSTTY_SOURCE_DIR` paths to Cargo, respecting explicit overrides.
For direct Cargo commands, set those environment variables to an installed Zig
0.16 binary and the matching Ghostty source. A network-enabled sys build can
also obtain the pinned source itself.

Tests exercise real PTY command output/cwd, ANSI styling, Unicode widths,
alternate screens, resize, Ctrl+C, shell reaping, lazy startup and fresh restart.
The native pack smoke sends input through the C ABI, collects an owned rendered
grid, unloads the session, rejects its retired handle and reloads the plugin.
