# Velk

Velk is a desktop text editor built with Rust and
[RXUI](https://github.com/hxyulin/rxui), currently under active development.

Version **0.0.0-dev** is an initial name reservation for the Velk project.
This package has no dependencies, public API, or executable and does not contain
the editor implementation. Do not use it as an editor library or installable app.

The editor is developed separately in the same workspace, with rope-backed text
editing, syntax highlighting, and built-in Vim editing and language-server
support. The package contents and API may change completely in future versions.

This is an explicitly versioned prerelease. Cargo does not select prereleases
for ordinary stable-version dependency requirements; using one requires opting
in to a matching prerelease requirement. This initial version may be yanked later.

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
