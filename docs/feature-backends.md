# Internal feature backends

These backends and frontends are built into the editor. The `plugins/` directory
names and internal routing IDs do not describe independently installed code.
There is no dynamic plugin loader or provider-selection UI. See
[built-in services](plugin-api.md) for the current direction.

Features have three responsibilities:

- **Core contracts** describe queries, results, identities, cancellation, and
  lifecycle. They contain no filesystem implementation or renderer types.
- **Backend plugins** own I/O, indexing, matching, caches, and watchers. They return
  domain data rather than panels, picker choices, or window actions.
- **Frontend plugins** interpret user actions and turn backend results into the
  host's shared panel and picker models. The host retains focus, accessibility,
  native rendering, command dispatch, and document navigation.

Feature contracts are owned alongside their plugins: `plugins/files/contracts`
and `plugins/project/contracts` are independent Rust libraries. Only frontend
and backend plugins depend on them; the editor, host, central plugin contracts,
and SDK do not. Future feature protocols should follow this ownership model.
A feature may define a different protocol without changing the platform.
These libraries provide compile-time types, not executable runtime plugins.
The host only needs the generic routing and presentation interfaces.

The default implementations remain in small source modules in the existing
plugin crates. They are separate logical plugins even when packaged in one
native library. A third-party frontend depends on the contracts and SDK, not
on the bundled implementation crate or its filesystem/matcher dependencies.
The current frontend surface is still host-rendered panels and pickers; this
change does not introduce arbitrary plugin-owned RXUI views.

| Feature | Shared contract | Backend ID and hook | Default frontend |
| --- | --- | --- | --- |
| File tree | `velk_files_contracts` | `velk.files.backend`, `velk.files.tree` | `velk.files` |
| Fuzzy project files | `velk_project_contracts` | `velk.project.files`, `velk.project.query` | `velk.quick-open` |
| Workspace text search | `velk_project_contracts` | `velk.project.search`, `velk.project.query` | `velk.workspace-find` |
| Buffer text search | `velk_search` | `velk.search.literal`, `velk.search.backend` | Host find bar |

The file-tree backend returns a revision, preorder entries, stable node IDs,
lossless native paths, lazy-directory state, notices, and an optional resolved
file path. Describe restores expanded branches; Expand and Resolve require the
current tree revision. Changes consumes a debounced watcher invalidation.
Describe, Expand, and Resolve return a pending job ID, followed by Poll/Cancel.
The frontend chooses panel position, titles, row actions, and file-open effects.
`show_hidden` is a backend setting under `velk.files.backend`.

Project backends accept Begin with a session, query ID, and search query, plus
Poll and Cancel. A new session refreshes the file index; query changes within a
session reuse it. Results contain locations, relative display paths, optional
source previews, and a notice. They contain no command choices or picker mode.
The existing ignore rules, complete file indexing, Nucleo ranking, unsaved-buffer
preference, result limits, and stale workspace/revision guards remain in effect.

Buffer search retains its bounded Begin/Append/Start/Poll/Cancel upload protocol
and scalar-offset matches. Each plugin frontend has a separate session from the
host find bar. It can show matches in another panel or drive its own commands.

## Calling a backend

Declare `plugin.call` in the frontend's capabilities and an interface requirement
in its manifest. The selected implementation resolves into a dependency edge; see
[built-in feature services](plugin-api.md). Exact-ID dependencies remain available when a
consumer specifically needs one implementation. Store the per-instance `Host` from `attach_host`.
From an owned background worker:

```rust
use velk_plugin_sdk::Host;
use velk_project_contracts as project;

fn begin(host: &Host, session: u64, id: u64) -> Result<project::Reply, String> {
    let request = project::Request::Begin {
        session,
        id,
        query: project::Query {
            text: "src".into(),
            ..Default::default()
        },
    };
    let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let output = host.call_interface(project::FILES_INTERFACE, input)?;
    serde_json::from_slice(&output).map_err(|e| e.to_string())
}
```

Poll the pending ID and translate the resulting locations into your frontend's
presentation. Forward cancellation when the user dismisses or supersedes work.
`velk_plugin_sdk::backend::Tasks` supplies a bounded latest-request worker with
cancellation, result polling, optional idle invalidation checks, and joined
teardown. Bundled frontend modules are complete examples.

Calls go through the existing C host-services barrier and bounded plugin queue,
never the UI. The host resolves the declared interface and selected provider, verifies the caller's
generation and resolved dependency, then wraps the request in `contracts::backend::Call`, containing its
host-authenticated plugin ID and generation. A backend decodes that envelope
with `backend::Sessions<T>` and handles the inner domain request. Each consumer
has independent state; query IDs can overlap across frontends. A new generation
replaces that consumer's previous state. At most 16 consumers are retained per
backend. Frontend unload emits `velk.backend.release` to release its sessions,
workers, caches, and watches; a late release cannot delete a newer generation.

A backend request callback must only queue or poll work and return promptly.
Calling another plugin synchronously from a dispatch callback is rejected,
because the shared worker cannot re-enter itself. Run such calls on an owned
background worker. Retirement or runtime shutdown interrupts pending host calls,
allowing workers to join without waiting for the dispatcher to process them.
Native plugins remain trusted in-process code; these checks are lifecycle and
contract checks, not a sandbox.

## Dependency graph and install intent
