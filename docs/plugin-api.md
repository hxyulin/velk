# Plugin metadata and access declarations

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

These are declarations today. Native plugins run in the editor process and have
its ambient permissions. This API does not implement consent, grant negotiation,
feature switches, or a sandbox. Omitting a capability does not restrict a native
library's access. Optional capabilities are not plugin dependencies or optional
hook registrations.

Capability IDs use dot-separated lowercase ASCII segments containing letters,
digits, hyphens or underscores. Use existing IDs consistently:

- `editor.buffer.read`: inspect document text supplied by the host.
- `filesystem.read`: read files, directories or metadata.
- `filesystem.watch`: observe filesystem changes.
- `environment.read`: inspect process environment variables.
- `process.spawn`: start processes, including shells and language servers.
- `network.http`: make HTTP requests (an example for future providers).

Custom dotted IDs are supported; the host does not interpret unknown declarations
as grants. Each ID may appear only once, with a nonempty purpose and an explicit
required/optional choice. Declare the access your plugin and its subprocesses use;
these IDs do not confer resource or path restrictions.

## Markdown rendering

The detail pane renders the README using the active Markdown provider, including
headings, bold, italic, monospace code, code blocks, lists and strikethrough.
Links are styled as text; they do not open automatically. The bundled renderer
shows HTML literally and preserves image alt text without fetching images.
Disabling the Markdown provider falls back to the README's literal source.

The list keeps the short plain-text summary instead of displaying Markdown source.
There is no arbitrary CSS or HTML styling API; the host theme controls appearance.

## Native wire contract and compatibility

The SDK automatically reserves `velk.plugin.metadata.v1` as a standard
`BYTES_CONTRACT` v1 request hook. It takes empty input and replies with UTF-8 JSON:

```json
{
  "name": "Example Plugin",
  "readme": "# Example\n\nFull **Markdown** documentation.",
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
