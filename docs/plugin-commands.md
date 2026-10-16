# Plugin commands

Loaded plugins contribute commands to the command palette using `Plugin::commands()`.
Open it with Command+Shift+P on macOS, Ctrl+Shift+P elsewhere, or F3. Search for an
action and press Enter. The host owns discovery, input, asynchronous scheduling,
navigation, and presentation; plugins own their behavior.

Commands have a stable dotted ID, a title, an optional language filter, and one
of two handlers. Identity includes the owning plugin ID, so two plugins may use
the same local command ID. Declarations are copied into plugin metadata and
validated during load. Unloading a plugin removes its commands.

User [keybindings](keybindings.md) invoke these same declarations using
`plugin-id:command-id`. A binding can supply a string `input` to bypass a declared
prompt; ordinary invocation retains the prompt. Pickers keep their interactive
query flow and reject fixed input. Keybindings retain the existing generation,
capability, availability, and revision checks.

## Plugin request handlers

Use a named handler for an action performed by the plugin itself:

```rust
use velk_plugin_sdk::{Plugin, Registrar, commands, hooks};

#[derive(Default)]
struct Example;

impl Plugin for Example {
    const ID: &'static str = "example.commands";
    const VERSION: &'static str = "1";

    fn commands() -> Vec<commands::Declaration> {
        vec![commands::Declaration {
            id: "example.show-context".into(),
            title: "Example: Show caret position".into(),
            language: None,
            input: None,
            shortcut: None,
            handler: commands::Handler::Request {
                hook: "example.show-context".into(),
            },
        }]
    }

    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::CommandHandler { name: "example.show-context" },
            |_: &mut Self, context: commands::Context| {
                Ok(commands::Outcome::Message {
                    text: format!("Caret: {:?}", context.position),
                })
            },
        )
    }

    fn describe(&mut self) -> String { "Example command provider".into() }
}
```

Context contains an optional file path, language, revision, and Unicode scalar
caret offset. A command with no language filter can run with no buffers open;
its document context is then empty. `Context::input` contains a submitted prompt
value when the declaration requests one. Source text is not copied into the context.
The SDK serializes context/results over the existing bounded bytes-request ABI.
A declaration naming a missing or incompatible request hook rejects plugin load.

Handlers run serially on the plugin worker, outside the UI thread. Keep them
short: this is not an executor for long-lived asynchronous jobs. Retired native
handles cannot execute after unload. The editor also rejects results after the
source buffer, revision, caret, or owning plugin generation has changed.

## Semantic commands

A declaration with `Handler::Semantic { payload }` routes through the active
buffer's semantic service. The payload belongs to that backend; the host does
not interpret LSP methods or rust-analyzer extensions. Commands are disabled
until the semantic service is ready. One command can be outstanding per service;
hover and diagnostics have separate result slots.

The LSP plugin currently accepts payloads shaped as:

```json
{
  "method": "textDocument/definition",
  "result": "locations",
  "capability": "/definitionProvider"
}
```

`capability` is a JSON pointer into the initialized server capabilities, or null
when the extension has no capability flag. Unsupported capabilities produce a
command error without sending a request. Supported response kinds include `locations`, `macro_expansion`, `text_edits`,
`workspace_edit`, `code_actions`, and `completion`, plus the adapters for resolved
actions and completion items. Parameters contain the current document URI and caret
position using the negotiated encoding; references also include declarations.
Other semantic backends can define their own payload schema and return the same
neutral outcomes. They must complete each accepted command with `Event::Command`.

The bundled LSP plugin declares:

- Go to definition.
- Go to type definition.
- Go to implementation.
- Find references.
- Format document.
- Rename symbol.
- Code actions and quick fixes.
- Complete at caret.

The Rust Analyzer definition plugin declares:

- Rust: Go to parent module, using `experimental/parentModule`.
- Rust: Expand macro, using `rust-analyzer/expandMacro`.

Rust actions are filtered to Rust documents. The LSP transport owns request IDs,
encoding conversion, response parsing, timeouts, and cancellation. Editing the
source cancels pending requests, and the UI discards replies after navigation or
provider retirement. A command times out after ten seconds. Server command execution, file resource operations, and task execution are not
implemented. Text refactorings use the [shared edit proposal](workspace-edits.md).

Protocol references: [LSP definition](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_definition)
and [rust-analyzer extensions](https://rust-analyzer.github.io/book/contributing/lsp-extensions.html).

## Results and limits

`velk-commands` contains the UI-independent data shared by contracts, semantic
services, and presentation adapters. Outcomes are:

- `Edit`: a workspace text-edit proposal, validated and applied by the host.
- `Choices`: searchable results whose selection applies an edit or requests
  backend resolution, using the original source and provider guard.
- `Message`: a status message.
- `Locations`: one target opens directly; multiple targets open a chooser. Targets
  use absolute local file paths, zero-based lines/columns, and an explicit UTF-8,
  UTF-16, or Unicode scalar encoding. Existing buffers retain unsaved text.
- `Document`: a separate, scrollable text result, used for macro expansion. The
  source document is unchanged. The current view displays plain monospace text;
  the optional language is available to other consumers.

The host rejects invalid target positions and partial code points. Non-file LSP
URIs produce an error; virtual source navigation needs a future document provider.

Limits are 128 declarations per plugin, 256 bytes per single-line title, 16 KiB
per backend payload, 10,000 locations, and 4 MiB of encoded output. Overall plugin
metadata remains bounded to 512 KiB. Command declarations describe available
actions; access requirements remain capability declarations and are not enforced
permissions for trusted native plugins.

The semantic-session contract remains version 1 during internal development. Its C
records carry bounded command payloads, submitted input, and results. The root
native ABI, language-definition contract, and generic bytes-request contract also
remain version 1. Rebuild the host and plugins together when their contracts change.

## Prompts, shortcuts, and navigation history

Set a declaration's `input` to a prompt label to collect a string before dispatch.
The source and provider must still match when the user submits it. Enter submits;
Escape or Close cancels. Input is limited to 1,024 bytes.

`shortcut` accepts a character, `Space`, or `F1` through `F24`, plus `primary`,
`control`, `alt` and `shift` flags. `primary` means Command on macOS and Control
elsewhere. Shortcuts share the palette's handler and availability checks. Host
commands take precedence; among plugins declaring the same effective shortcut,
the first loaded plugin in the host's deterministic order wins. Other commands
remain available from the palette. User keymap overrides are not implemented.

Bundled shortcuts:

- F12: Go to definition.
- F2: Rename symbol.
- F4: Plugin manager.
- Alt+Shift+F: Format document.
- Primary+period: Code actions and quick fixes.
- Ctrl+Space: Complete at caret.
- Alt+Left / Alt+Right: Navigate back / forward.

Navigation records successful provider-directed jumps. The bounded history follows
text edits and removes closed buffers. Back/forward do not create another jump.
Normal tab switches and file-dialog opens do not add entries. History is local
to the window and is not persisted across Reload Window.

## Panels and existing buffer locations

`Outcome::ShowPanel { panel }` selects a panel local to the command's owning
plugin, opens its slot, and preserves the closed state of other slots. It cannot
select another plugin's panel. The contribution must already be discovered.

`Outcome::RevealBuffer { location }` navigates to a Unicode-scalar offset in an
existing session buffer ID at an exact revision. The host rejects changed or
closed targets before switching buffers. Navigation preserves unsaved text,
records the origin in jump history, and restores editor focus. This result also
supports untitled buffers and avoids reconstructing file paths from labels.

`Outcome::RevealDiagnostic` accepts a `DiagnosticLocation` with either an exact
buffer target or a path/content fingerprint and a Unicode-scalar offset. File
targets use the normal asynchronous open flow and reject changed contents before
selecting an existing buffer or opening a new one. Panel effects support the same
location through `Effect::RevealDiagnostic`.

The bundled Diagnostics plugin declares Show Problems, Next Diagnostic (`F8`),
and Previous Diagnostic (`Shift+F8`). Navigation includes every severity, follows
document title then source position, and wraps. Duplicate diagnostics at the
same offset share a navigation stop. It reads the current host snapshot for each
command rather than relying on the panel's cached rows.

The Diagnostics plugin also declares text/source filters, severity cycling/reset,
and explicit next/previous-error commands. Ordinary diagnostic navigation uses
the panel's filters; error navigation keeps the text/source filters and restricts
severity to errors. See [Problems filtering](../plugins/problems/README.md#filtering).
