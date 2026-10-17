# Snippets

Bundled, replaceable snippet expansion and transform provider (`velk.snippets`).
Select a provider with the editor's `snippet_provider` setting. This plugin needs
no filesystem, process, or buffer capabilities: variables come from explicit
completion context.

Supports numbered placeholders, nested defaults, mirrors, choices, named variables,
and bounded Rust-regex transforms with capture formatting and case conversion.
Unknown variables become editable placeholders. JavaScript regex lookarounds and
backreferences are unsupported. Invalid templates fail before document edits.

The editor retains revision validation, transactions, undo, placeholder tracking,
and choice controls. LSP supplies raw templates and snapshot variables; it has no
runtime or build dependency on this plugin. A session retains its selected provider
lease; retiring that provider makes subsequent transform evaluation fail safely.
