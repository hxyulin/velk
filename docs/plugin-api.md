# Plugin API

## Crate boundaries

- `velk-plugin-contracts` owns common definitions: plugin IDs, metadata, access
  declarations, settings schemas and values, language-server definitions, and
  standard service/request names. It has no loader, native ABI, authoring trait,
  UI, or executor dependency. Its current Rust implementation uses `std` and
  Serde; it is not yet a `no_std` SDK or a Wasm interface definition.
- `velk-plugin-api` owns native C-compatible tables, entry points, status codes,
  and ABI limits. `wire` contains shared native codecs that copy borrowed C views
  into receiver-owned values. Its `contracts` module describes native tables,
  rather than the transport-independent models in `velk-plugin-contracts`.
- `velk-plugin-sdk` owns the safe authoring trait, registration adapters, export
  macros, panic containment, and plugin-side lifecycle. It re-exports common
  definitions and settings at their existing paths; `contracts` also exposes the
  shared crate. Existing bundled plugins need no import migration.
- `velk-plugin-host` owns native loading, scheduling, generations, discovery, and
  restart. It consumes shared contracts and native codecs directly, without
  depending on the authoring SDK. Its existing metadata/settings re-exports remain.

Feature models remain in their domain crates, including `velk-markup`,
`velk-semantic`, `velk-panels`, `velk-search`, and `velk-editing`. A future adapter should reuse
their service semantics and common definitions instead of depending on native
function tables. No Wasm runtime or new feature API is introduced by this split.

The root ABI and named contract versions stay at 1 during internal development;
callback layouts may change. Rebuild the host, SDK, and bundled plugins together.
Standard hook names and plugin-ID helpers are available from `velk-plugin-api`
through re-exports.

## Interactive editing service

`velk-editing` carries bounded editing context and transaction proposals. A native
provider implements the SDK's stateless `EditingBehavior` and registers through
`Registrar::editing`. The host supplies a direct retirement-aware lease to the
editor; typing does not queue behind lifecycle work or LSP requests. See
[interactive editing](interactive-editing.md) for roles, limits, input routing,
provider settings, and the bundled indentation and autopairs implementations.

## Workspace and buffer host services

`Plugin::attach_host` receives a per-instance `Host` handle. The host exposes
workspace context, project-scoped disk reads, buffer enumeration and text,
versioned edits, and normal open/create/select/save/close operations through C
callbacks. These service calls require the corresponding capability declaration;
native code still has ambient process access. See [host services](host-services.md)
for request semantics, limits, scheduling, and unload behavior.

## Host diagnostics service

The host attaches a process-lifetime `DiagnosticsSink` through the optional
`velk.diagnostics.attach.v1` service. Its C callbacks provide level checks, log
emission, a shared clock, and timing events. The SDK exposes safe wrappers through
`velk_plugin_sdk::diagnostics`. Only integers and borrowed UTF-8 byte views cross
this boundary; the host owns log filtering, formatting, buffering, and output.
See [plugin instrumentation](profiling.md#instrumenting-plugins) for usage and
limits. Plugins must join their workers before unload. Both the root ABI and
internal diagnostics contract stay at version 1 during development; rebuild the
host and plugins together after contract changes.

## Metadata and access declarations

Plugins have a stable machine ID and a separate human-readable name. The short
`describe()` summary belongs in lists. Use `README` for the full Markdown detail
page, much like a crate's README on crates.io:

```rust
use velk_plugin_sdk::{CapabilityDeclaration, Plugin, Registrar};

#[derive(Default)]
struct Example;

impl Plugin for Example {
    const ID: &'static str = "author.example";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Example Plugin";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Read the current document to compute results.",
        ),
        CapabilityDeclaration::optional(
            "network.http",
            "Fetch extra documentation; local results work without it.",
        ),
    ];

    fn describe(&mut self) -> String {
        "A short plain-text summary of this plugin".into()
    }

    fn register(_reg: &mut Registrar<Self>) -> Result<(), String> {
        // Register the services, requests, events or contributions you provide.
        Ok(())
    }
}
```

`NAME` defaults to the ID, and `README` and `CAPABILITIES` default to empty for
existing SDK implementations. All bundled plugins supply explicit names and
READMEs. The Plugins page retains host-owned metadata for plugins that ran earlier
in the current session, including after unload.

## Capabilities

A capability declares **access the plugin uses**, with a reason that users can
read. Registered hooks separately describe **what the plugin provides**. An ID
such as `velk.panels.v1` is a contribution contract, not an access permission.

- **Required**: the plugin needs this access for its core functionality. A future
  permission-enforcing host should refuse activation if required access is denied.
- **Optional**: the plugin can operate without this access, with reduced features
  or a local fallback. A future permission-enforcing host should communicate which
  optional capabilities were granted before the plugin uses them.

Host-service calls check these declarations. Native plugins run in the editor
process and have its ambient permissions. This API does not implement consent,
grant negotiation, feature switches, or a sandbox. The endpoint rejects calls
whose capability was not declared; that does not restrict direct native OS
access. Optional capabilities are not plugin dependencies or optional hook
registrations.

Capability IDs use dot-separated lowercase ASCII segments containing letters,
digits, hyphens or underscores. Use existing IDs consistently:

- `workspace.read`: query the working directory and selected project root.
- `filesystem.project.read`: read files and directories within the selected project.
- `editor.buffer.read`: enumerate buffers and inspect their current text.
- `editor.buffer.write`: request validated, undoable document edits.
- `editor.buffer.manage`: open, create, select, save, and close buffers.
- `filesystem.read`: read files, directories or metadata.
- `filesystem.watch`: observe filesystem changes.
- `environment.read`: inspect process environment variables.
- `process.spawn`: start processes, including shells and language servers.
- `network.http`: make HTTP requests (an example for future providers).

Custom dotted IDs are supported; the host does not interpret unknown declarations
as grants. Each ID may appear only once, with a nonempty purpose and an explicit
required/optional choice. Declare the access your plugin and its subprocesses use;
these IDs do not impose OS-level resource or path restrictions. The project-read
host service validates paths within the selected root.

## Markdown rendering

The detail pane renders the README using the selected Markdown provider, including
headings, bold, italic, monospace code, code blocks, lists and strikethrough.
Links are styled as text; they do not open automatically. The bundled renderer
shows HTML literally and preserves image alt text without fetching images.
The bundled `velk.markdown` CommonMark plugin is the default, and can be disabled
or replaced. Select another loaded provider under Settings > Markdown provider,
or set `"markdown_provider": "community.markdown"` in user settings, then run
Reload Window. This selection applies to both plugin READMEs and hover documentation.
Multiple providers may coexist; only the selected ID handles requests. If that
provider is unavailable, the host shows literal source rather than silently
choosing another provider. Rendering errors also fall back to source in the UI.

The list keeps the short plain-text summary instead of displaying Markdown source.
There is no arbitrary CSS or HTML styling API; the host theme controls appearance.

## Command declarations

Plugins declare command palette actions through `Plugin::commands()`. The shared
`commands` types are re-exported by the SDK and plugin contracts; they live in
`velk-commands` without UI or transport dependencies. Commands route to a named
plugin request handler or the active document's semantic backend. See the
[command guide](plugin-commands.md) for authoring, navigation outcomes, bundled
LSP/Rust actions, and lifecycle rules.

## Optional documentation model

`velk-markup` is a compact styled-text contract for the editor's documentation
surfaces, not a universal markup language model. It supports text runs with
emphasis, code, heading levels and link destinations. Structural tables, images,
custom block layout and interactive elements are outside this contract. The
bundled provider flattens supported Markdown structures into those runs; the host
controls visual styling.

The SDK does not enable its `markup` feature by default. Plugins using the standard
documentation model opt in explicitly:

```toml
velk-plugin-sdk = { path = "../../crates/plugin-sdk", features = ["markup"] }
```

This exposes `hooks::RenderMarkdown` and the `markup` re-export. Plugins that do
not enable it have no SDK dependency on `velk-markup`. Community extensions can
maintain their own model crate and versioned named request/event contracts using
the generic registration APIs.
Unknown contracts remain discoverable, but require a matching consumer and
presentation adapter; the editor does not automatically render an arbitrary model.
This opt-out does not change native plugin trust or provide a sandbox.

## Native wire contract and compatibility

The SDK automatically reserves `velk.plugin.metadata.v1` as a standard
`BYTES_CONTRACT` v1 request hook. It takes empty input and replies with UTF-8 JSON:

```json
{
  "name": "Example Plugin",
  "readme": "# Example\n\nFull **Markdown** documentation.",
  "commands": [],
  "capabilities": [
    {
      "id": "editor.buffer.read",
      "reason": "Read the current document to compute results.",
      "optional": false
    }
  ]
}
```

The JSON is serialized by the SDK and copied into host-owned memory through the
existing bytes request ABI. No Rust strings, vectors or trait objects cross the
native boundary. The root ABI and standalone/pack entry symbols remain version 1.
Older plugins without this hook still load; their metadata name falls back to the
ID, and they have no README or declared capabilities. A present but invalid
metadata hook rejects that instance through the normal load-error path. The SDK
validates declarations before registering them, and the host validates responses.

Limits: 256 UTF-8 bytes for a single-line name, 256 KiB of README source, 64
capabilities, 128 bytes per capability ID, 1,024 bytes per single-line reason, and
512 KiB for the complete encoded JSON. NULs in README text, duplicate capability
IDs, unknown JSON fields and malformed capability IDs are rejected. Limits are
per plugin, including each selected member of a pack. Metadata is discovered
after instance creation; inspecting an untrusted native plugin is not sandboxed.

## Plugin settings

Plugins can contribute declarative settings through `Plugin::settings` and
receive resolved snapshots through `Plugin::configure`. The editor owns storage,
validation, standard controls, reset behavior, and explicit restart-to-apply.
See [plugin settings](plugin-settings.md) for the user workflow, storage format,
bundled integrations, SDK example, and contract limits.
