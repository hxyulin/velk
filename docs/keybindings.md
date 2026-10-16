# Keybindings

User bindings run the same commands as the command palette. They take precedence
inside their matching scope, including over native menu shortcuts and the active
Vim keymap. They do not introduce scripting or event-triggered commands.

Run **Keybindings: Open configuration** from the command palette
(`Primary+Shift+P`). This opens `keybindings.json` beside your user `settings.json`,
creating an empty configuration if necessary. `VELK_KEYBINDINGS_PATH` overrides
its location. `Primary` means Command on macOS and Control elsewhere.

```json
{
  "bindings": [
    { "keys": "Primary+k Primary+s", "command": "velk.file.save-all" },
    { "keys": "Primary+Shift+j", "command": "velk.keybindings.open",
      "when": { "scope": "window" } },
    { "keys": "F8", "command": null },
    { "keys": "Alt+l", "command": "velk.edit.right", "repeat": true },
    { "keys": "F9", "command": "velk.edit.insert-text", "input": "hello",
      "when": { "mode": "insert" } }
  ]
}
```

Save the file to apply it; Velk checks for changes every two seconds. **Keybindings:
Reload configuration** reloads immediately. Invalid JSON or bindings show a banner
and preserve the last valid configuration. Deleting the file restores defaults.
The file is strict JSON: comments, trailing commas, and unknown properties are errors.

## Keys and precedence

A space separates chord strokes; each stroke combines modifiers and a key with
`+`. Modifiers are `Primary`, `Ctrl` / `Control`, `Cmd` / `Super` / `Meta`,
`Alt` / `Option`, and `Shift`. Key names are case insensitive. Use an explicit
`Shift` modifier for shifted keys.

Supported named keys are `Space`, `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`,
`Left`, `Right`, `Up`, `Down`, `Home`, `End`, `PageUp`, `PageDown`, and `F1`–`F24`.
Single characters are also accepted. Spell the plus key `Plus`.
Key matching follows the logical key reported by the platform keyboard layout.

Bindings override defaults without removing the command from menus. A null
command consumes the key without performing an action. For the same keys in
matching contexts, the last rule wins. Window bindings run before editor bindings.
A consumed binding does not fall through if its command is unavailable or unknown;
Velk reports the problem instead.

Chords contain up to four strokes. After each prefix, there is one second to
continue; the pending chord appears above the editor. Escape, a pointer press,
focus or buffer changes, and document changes cancel it. A failed final stroke
is tried as a fresh key; consumed prefix strokes are not replayed. A short binding
and a longer chord sharing its prefix cannot coexist in overlapping contexts.
Key repeat is disabled by default; set `"repeat": true` for repeatable actions.
Repeated prefix presses never advance a chord.

## Contexts

`when` is optional. Its fields are combined:

- `scope`: `"editor"` by default; only the focused document editor receives it.
  `"window"` also works in panels and with no buffers. Use window scope carefully
  for printable keys because it can intercept panel and terminal input.
- `language`: matches the shared command context's language ID. Currently the
  host identifies `"rust"`; other files have no language ID.
- `mode`: matches the active keymap's stable mode ID. Standard editing uses
  `"insert"`. Bundled Vim uses `"normal"`, `"insert"`, `"visual"`,
  `"visual_line"`, and `"visual_block"`.

Custom bindings pause during IME composition and while modal settings, plugin
menus, the command palette, command prompts/results, or close confirmations are
open. Those controls retain their own keyboard behavior.

## Commands and input

Run **Keybindings: Show available commands** to open a scratch reference listing
current host and loaded plugin command IDs, labels, and effective shortcuts.
Plugin loading, file language, and selection can change command availability.
The command palette also displays configured shortcuts.

Host command IDs include `velk.file.save`, `velk.file.save-all`, `velk.file.new`,
`velk.buffer.close`, `velk.buffer.next`, `velk.buffer.previous`,
`velk.commands.open`, and the three `velk.keybindings.*` commands described above.
Editing commands use `velk.edit.*`: `undo`, `redo`, `select-all`, `backspace`,
`delete`, `left`, `right`, `up`, `down`, `select-left`, `select-right`,
`select-up`, `select-down`, `line-start`, `line-end`, `document-start`,
`document-end`, `indent`, `outdent`, `newline`, `insert-text`, `copy`, `cut`,
and `paste`. Indentation, newline, and backspace reuse the selected editing
providers. `insert-text` requires a string `input`.

Plugin commands use `plugin-id:command-id`. A bare plugin command ID is accepted
when unique. Optional string `input` supplies a plugin command's input directly,
bypassing its prompt. Without it, commands that declare an input show their usual
prompt. Picker commands open their interactive picker and do not accept fixed
input. Other host commands reject fixed input. Binding a command does not grant
additional plugin capabilities or bypass lifecycle and revision checks.

Configuration is limited to 256 KiB, 512 rules, and 4,096 bytes of input per rule.
