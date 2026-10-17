# Plugin interfaces and file types

A plugin ID identifies one implementation. An interface ID identifies a shared
semantic protocol. Access capabilities describe host operations a plugin uses;
they are independent of both interface implementation and consumption.

The first interface bindings cover File Tree, project file queries, and workspace
text search. Their models remain in feature-owned contract libraries. The host
routes opaque requests and does not import those libraries.

## Distribution declarations

Declare interfaces in each `plugins.json` entry. These declarations can be read
without executing native code:

```json
{
  "interfaces": {
    "implements": [{
      "interface": "velk.project.files",
      "title": "Project file queries",
      "hook": "community.files.query",
      "version": 1
    }],
    "requires": []
  }
}
```

A consumer declares a requirement instead:

```json
{
  "interfaces": {
    "implements": [],
    "requires": [{
      "interface": "velk.project.files",
      "version": 1,
      "default_provider": "velk.project.files"
    }]
  }
}
```

The publisher supplies these fields alongside library, digest, packaging, and
ordinary exact-ID dependencies. `velk_plugin_sdk::interfaces` exposes their Rust
types. A native implementation registers its advertised hook with
`Registrar::provide(hooks::Request(...), ...)`. It must implement the semantic
request/reply protocol from the feature's shared library. Interface identity and
version are a declaration of that obligation, not proof of behavioral correctness.
The host checks that its advertised version-1 byte-request hook actually exists.

Consumers declare `plugin.call` and call
`Host::call_interface(interface, encoded_request)` on an owned background worker.
The authenticated consumer envelope, per-consumer sessions, cancellation, release,
and generation rules are the same as `Host::call_plugin`. Ordinary exact-ID calls
remain useful for dependencies that require a specific implementation.

## Selection and lifecycle

Plugins > Interfaces lists installed backend implementations and the existing
platform service selectors. Backend selections persist in user settings:

```json
{
  "interface_bindings": {
    "velk.project.files": "community.fast-files"
  }
}
```

The host resolves requirements into dependency edges before loading code. A
selected provider must declare the requested interface and version. Invalid
selections and dependency cycles fail before loading. Defaults apply only when
there is no explicit binding; an unavailable explicit selection is an error.
Disabled providers remain disabled and their consumers cannot load.

Selecting a running backend from the Interfaces page restarts running plugins,
including their document-service attachments. This first implementation uses a
coordinated restart rather than switching beneath active backend sessions.
The dependency view and enable operations resolve the same bindings. Restarting
or disabling a bound backend follows its resolved consumers. Retired generations
cannot route new calls. Rebinding does not migrate backend session state.

Existing typed platform services (undo, syntax, editing, matching, completion
presentation, snippets, Markdown, and buffer search) also have selectors on this
page. They retain their dedicated transports, settings, and documented lifetime
rules. The generic byte-request interface routing currently covers feature
backends; it does not turn every typed native service into a byte request.

## File identity and associations

Plugins declare file types independently of syntax and language-server providers:

```rust
fn file_types() -> Vec<velk_plugin_sdk::file_types::FileType> {
    vec![velk_plugin_sdk::file_types::FileType {
        id: "rust".into(),
        name: "Rust".into(),
        extensions: vec!["rs".into()],
        filenames: vec![],
    }]
}
```

The bundled `velk.language.rust` plugin declares Rust identity. Tree-sitter and
rust-analyzer remain independent implementations; the language plugin requires
neither. Syntax sessions accept language IDs and providers reject unsupported IDs.
Command contexts, semantic snapshots, highlighting, and the status bar use the
window's loaded file-type declarations.

Exact filenames take precedence over case-insensitive extensions. Conflicting
language IDs for the same association leave the file unclassified until the user
chooses an override. User settings accept exact names and `*.extension` keys:

```json
{
  "file_associations": {
    "*.custom": "rust",
    "RustSource": "rust"
  }
}
```

The target must be a loaded declared type. Arbitrary globs, shebang detection,
content sniffing, and per-project overrides are deferred. Removing a language
plugin removes its associations; plugin lifecycle refreshes existing document
views and semantic attachments. Documents can open before providers load and
remain editable without a recognized file type.
