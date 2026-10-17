# Feature backends and plugin dependencies

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

Declare `plugin.call` in the frontend's capabilities and the backend's exact ID
in its manifest dependencies. Store the per-instance `Host` from `attach_host`.
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
    let output = host.call_plugin(project::FILES_PROVIDER, project::HOOK, input)?;
    serde_json::from_slice(&output).map_err(|e| e.to_string())
}
```

Poll the pending ID and translate the resulting locations into your frontend's
presentation. Forward cancellation when the user dismisses or supersedes work.
`velk_plugin_sdk::backend::Tasks` supplies a bounded latest-request worker with
cancellation, result polling, optional idle invalidation checks, and joined
teardown. Bundled frontend modules are complete examples.

Calls go through the existing C host-services barrier and bounded plugin queue,
never the UI. The host verifies the caller's generation and declared direct
dependency, then wraps the request in `contracts::backend::Call`, containing its
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

Each `plugins.json` entry records distribution information separately from its
plugin's capability requirements:

```json
{
  "id": "example.file-picker",
  "library": "artifacts/<digest>/libexample.dylib",
  "digest": "<digest>",
  "pack": false,
  "dependencies": ["velk.project.files"],
  "install_reason": "explicit"
}
```

`dependencies` is a list of exact required IDs. The graph rejects missing,
duplicate, and cyclic edges before loading code. Dependencies load before their
consumers. Selecting one frontend loads its dependency closure; explicitly
disabled dependencies stay disabled and their consumers report a load failure.
Enabling that dependency explicitly permits it to load again. Restarting a
backend restarts its active consumers; disabling it stops those consumers first.
Unrelated plugins keep their generations. Low-level `Manager::unload` refuses a
provider with active dependents; the runtime uses the coordinated cascade.

`install_reason` is `explicit`, `bundled`, or `dependency`. Omitted values mean
`explicit`; omitted dependencies mean an empty list. These are normal manifest
authoring defaults. Enabling/disabling is separate from installation intent and
does not rewrite it. The current local distribution manifest supplies this
information; there is no package installer or uninstall command yet.

The plugin manager offers an installed view grouped by those reasons and a
dependency tree. Shared subgraphs appear once with references on later edges;
details link to both requirements and consumers. Disabled and failed plugins
remain visible through the manifest inventory.

`Manifest::orphaned_dependencies` calculates dependency-only entries unreachable
from an explicit or bundled root. It provides the basis for future automatic
removal: removing one frontend must retain shared backends and anything manually
installed. This release does not delete plugins or artifacts automatically.
Multiple logical plugins may share one native image, so artifact removal must
also account for every remaining manifest entry using that image.

Internal contract and ABI versions remain 1 during early development.
