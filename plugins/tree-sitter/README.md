# Tree-sitter

Bundled, replaceable syntax-highlighting service. The plugin owns the Rust
grammar, incremental parser, syntax trees, and highlight queries. Each buffer
has an independent session on its syntax worker. Queries cover requested
viewports and can be cancelled when newer document or viewport work arrives.

The editor owns document revisions, scheduling, stale-result projection, and
styling. Disabling this plugin leaves plain text and semantic services available.
