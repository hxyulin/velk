# Built-in feature settings

Open Settings > Features to configure the built-in editor services. Search matches
setting names, descriptions, keys, and feature names. Controls support booleans,
bounded integers, text, and fixed choices. Reset removes an override and restores
its default.

Changes save through the settings writer. Select **Apply changes** when a feature
has pending configuration. Applying settings restarts that feature's active
sessions; for a terminal this terminates its shell. Editing the settings file
requires saving it and using Reload Window. There is no settings-file watcher.

All features are linked into the executable and initialized at startup. There is
no plugin enable/disable or installation UI. The internal `plugins` settings key
and namespaced IDs remain implementation details of the existing settings store.

## Storage and validation

Only user overrides are stored, under exact plugin IDs:

```json
{
  "plugins": {
    "velk.files.backend": {
      "show_hidden": false
    },
    "velk.terminal": {
      "shell": "/bin/sh",
      "scrollback_bytes": 20000
    },
    "velk.language-server.rust-analyzer": {
      "executable": "/opt/tools/rust-analyzer"
    }
  }
}
```

Other editor settings share the same file.
Unknown plugin IDs and keys survive writes and resets. Undeclared keys are not
sent to a plugin, and a plugin receives only the values under its own ID.

Invalid edits through controls are rejected before storage changes. If an
existing JSON file contains an invalid declared value, the plugin starts with
its complete validated defaults. Its Settings section shows the error; correct
or reset the value and restart to apply it. The invalid stored override remains
available for correction. A plugin's own configuration-handler failure rejects
that plugin's load and appears in its Settings section.

There is no settings-file watcher. After editing `settings.json` as a document,
save it and use **Reload Window** to read it again. In-page edits apply to the
current window's preferences and are delivered on the next plugin load/restart.

Settings are configuration, not capability grants. They do not enable optional
permissions, and no extra capability is required for receiving a plugin's own
settings. Native plugins retain ambient process access; this is not a sandbox.
Do not use ordinary settings as secret storage.

## Bundled integrations

- **File tree backend:** `show_hidden` defaults to `true`, preserving the existing tree
  behavior. Turning it off excludes dot-prefixed files and directories.
- **Terminal:** `shell` defaults to an empty string, preserving the
  `VELK_TERMINAL_SHELL`, `SHELL`, `/bin/sh` fallback. A configured executable
  receives the existing `-i` argument. `scrollback_bytes` defaults to `10000`
  and accepts integers from `0` through `100000000`. This is an approximate byte
  budget, pruned at page granularity; `0` disables scrollback.
- **Rust Analyzer:** `executable` defaults to an empty string, preserving
  `VELK_LSP_CONFIG`, `VELK_RUST_ANALYZER`, and normal executable lookup. A nonempty
  setting overrides the executable selected by those existing mechanisms; their
  other configuration fields remain intact.
- **Search, Markdown, and generic LSP:** no settings are declared yet. Search
  case/word options remain part of individual search requests, and language-server
  policy remains with each language definition rather than the LSP transport.

## Authoring a plugin

Declare a schema and handle its resolved snapshot with the safe SDK:

```rust
use velk_plugin_sdk::{Plugin, settings::{Definition, Kind, Values}};

#[derive(Default)]
struct Example {
    enabled: bool,
}

impl Plugin for Example {
    const ID: &'static str = "example.feature";
    const VERSION: &'static str = "1";

    fn settings() -> Vec<Definition> {
        vec![Definition {
            key: "enabled".into(),
            title: "Enable feature".into(),
            description: "Enable this plugin's optional feature.".into(),
            kind: Kind::Boolean,
            default: false.into(),
        }]
    }

    fn configure(&mut self, values: &Values) -> Result<(), String> {
        self.enabled = values["enabled"].as_bool().ok_or("expected a boolean")?;
        Ok(())
    }

    fn describe(&mut self) -> String {
        "Example feature".into()
    }
}
```

`settings()` supplies static declarations and should have no side effects.
Local keys contain lowercase ASCII letters, digits, underscores, or hyphens;
the host supplies the plugin namespace. Titles and descriptions are plain text.
The host checks schema uniqueness, defaults, bounds, and string choices.

The SDK automatically publishes declarations through plugin metadata and adds
`velk.plugin.settings.v1`, an optional request using the existing bounded bytes
contract. The host resolves overrides and invokes it on the serialized plugin
worker before feature hooks or lifecycle restoration. JSON bytes are copied into
the receiving library; Rust containers and borrowed pointers do not cross the
ABI. No fields were added to the root C ABI.

`configure` receives every declared key with an effective value. Validate any
additional semantic constraints before mutating plugin state. Keep creation
side effects idle until configuration or feature requests provide their inputs.
Lifecycle snapshots should restore runtime state without overwriting the host
configuration. The current host configures each new instance once; live change notifications
and per-setting apply modes are deferred.

Schemas allow at most 64 settings, with text limits up to 16 KiB and at most 64
unique string choices per setting. Resolved configuration is bounded to 256 KiB
per plugin, within the existing metadata and hook limits. The queued aggregate
plugin override map is bounded to 1 MiB.

Legacy plugins without settings continue loading. The native API is experimental;
rebuild editor and plugin artifacts together when adopting the new metadata.
See [plugin API](plugin-api.md) and [built-in feature services](plugin-api.md).
