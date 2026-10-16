# Rust Analyzer

Add **Rust language support** through `rust-analyzer`.

Requires the **Language Server Protocol** plugin (`velk.lsp`) and a `rust-analyzer` executable on your path.

## Configuration

Set `VELK_RUST_ANALYZER` to override the executable. Set `VELK_LSP_CONFIG` to load a JSON language-server configuration file.

The LSP transport plugin starts and manages the server process.

## Settings

Configure this plugin through **Settings → Plugins** or its **Settings** button
in the plugin manager. Changes apply after an explicit restart. See the
[plugin settings guide](../../docs/plugin-settings.md#bundled-integrations) for
options, defaults, and environment-variable precedence.
