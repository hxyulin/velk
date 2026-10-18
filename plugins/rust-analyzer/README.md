# Rust Analyzer

Add **Rust language support** through `rust-analyzer`.

Requires the **Language Server Protocol** plugin (`velk.lsp`) and a `rust-analyzer` executable on your path.

## Commands

In addition to the LSP plugin's standard navigation commands, this plugin declares:

- **Rust: Go to parent module**.
- **Rust: Expand macro**, displayed as a separate text result.

Use the command palette at the current caret. These actions appear for Rust
documents and require a ready language-server session. See the
[command guide](../../docs/plugin-commands.md).

## Configuration

Set `VELK_RUST_ANALYZER` to override the executable. Set `VELK_LSP_CONFIG` to load a JSON language-server configuration file.

The LSP transport plugin starts and manages the server process. Cargo checking is
enabled by default, including rust-analyzer's initial project check and checks on
save. Compiler diagnostics such as borrow errors depend on this integration;
native rust-analyzer diagnostics alone do not cover all compiler errors. An
explicit `VELK_LSP_CONFIG` can override `checkOnSave` in initialization options
and the `rust-analyzer` configuration section.

## Settings

Configure this plugin through **Settings → Plugins** or its **Settings** button
in Settings > Features. Select Apply changes to restart the feature. See the
[plugin settings guide](../../docs/plugin-settings.md#bundled-integrations) for
options, defaults, and environment-variable precedence.
