# Research: How do established tiling window managers structure their hotkey/keybinding foundation?

> **TL;DR**: The field is split cleanly in two. i3, GlazeWM, AeroSpace, whkd and skhd all key their binding table on a **free-form command string parsed at load time** — nobody in this survey uses a closed enum of unit variants, because every one of them needs a parameter (`workspace 3`, `focus --workspace 1`, `resize --width -2%`). The parsers differ in rigour but not in shape: GlazeWM feeds the string to **clap** inside a `Deserialize` impl so a typo makes `serde_yaml::from_str` fail and rejects the **whole file**; AeroSpace parses it with its own CLI arg parser and accumulates a **diagnostic list**, applying the rest of the config and showing a GUI message window; i3 does **not** validate the command at load time at all — `command = string` in `config.spec` — and a typo surfaces on the first keypress as an i3-nagbar reading "The configured command for this shortcut could not be run successfully."; whkd's command is a raw shell line it never inspects; skhd fails the entire file, and its hot-reload frees the old bindings *before* re-parsing, so a bad edit leaves you with **zero** bindings. **Binding to a user-named entity is proven and there are exactly three answers to a missing name**: AeroSpace **creates it** (`Workspace.get(byName:)` inserts on miss) and in `config-version = 1` even *derives* its `persistent-workspaces` list from the names it finds in bindings; GlazeWM **errors at press time** and pops a "Non-fatal error" dialog ("Workspace with name 'x' doesn't exist or is already active."); komorebi **silently does nothing** (`if let Some(...) = self.monitor_workspace_index_by_name(name)` with no `else`). AeroSpace additionally validates the *name itself* at parse time against a 21-entry reserved list. On duplicates, i3 and AeroSpace are the only ones that report: i3's `check_for_duplicate_bindings` sets `has_errors` and spawns nagbar; AeroSpace emits `"'alt-h' Binding redeclaration"` keyed on a modifier-order-normalised description. GlazeWM and skhd are silently first-wins; whkd prints to a stderr its own sample config hides. **On GUI editors, the tiling-WM field has nothing to copy**: GlazeWM's tray offers only "reload config"/"show config folder"; komorebi ships `komorebi-shortcuts`, a **read-only** egui viewer of the whkdrc with a filter box and no capture; AeroSpace has a tray menu only. The only real design is **PowerToys**, and it is worth copying wholesale: the KBM editor holds a named Win32 event (`PowerToys_KeyboardManager_Event_EditorWindow`) for the editor window's entire lifetime, which the separate engine process checks at the top of its hook and returns `0` — the source comment reads *"Signaled ... event to suspend the KBM engine"* — while the narrower capture state is armed only when `currentUIWindow == GetForegroundWindow()`, and the Settings-side picker **disposes the hook entirely on window deactivation** and rebuilds it on activation. Capture works on an already-registered combo because a `WH_KEYBOARD_LL` hook that returns `1` prevents the OS ever generating `WM_HOTKEY`. `Win+L` and `Ctrl+Alt+Del` are the one place a hardcoded reserved list exists (`ShortcutErrorType::WinL` / `::CtrlAltDel`, checked on every keystroke during capture); everything else relies on a live `RegisterHotKey`/`UnregisterHotKey` probe against `ERROR_HOTKEY_ALREADY_REGISTERED`. **Nobody solves the config-layer question** — no manager here has both a layered config and a GUI that writes to it.

## Findings

### Scope and source selection

This covers the *binding table* — how a combination is spelled, what it points at, how that pointer is validated, and what happens when it is wrong — read from parsers and command definitions rather than from user guides. The managers are the five named in the brief plus their hotkey daemons: **GlazeWM** (Windows/macOS, YAML, low-level hook), **AeroSpace** (macOS, TOML, Carbon `RegisterEventHotKey`), **komorebi** + **whkd** (Windows, custom DSL, low-level hook), **i3** (X11, the origin of the idiom, custom recursive parser), and **yabai** + **skhd** (macOS, custom DSL, `CGEventTap`). **Microsoft PowerToys** is carried through as the only first-party source with a shipping GUI key-capture editor and the only one that uses `RegisterHotKey` the way Mosaix does. yabai itself has no binding layer at all — its README states keyboard shortcuts are "optionally set" using "skhd and other third-party software", and again that "Keyboard shortcuts can be defined with skhd or any other suitable software you may prefer" ([yabai README](https://github.com/asmvik/yabai/blob/master/README.md); note the repository moved from `koekeishiya/yabai` to `asmvik/yabai`, and `koekeishiya/skhd` likewise to `asmvik/skhd`).

Where a claim could not be verified from source it is marked as such. Nothing here was executed.

### The comparison

| | Binding syntax | Parameterised commands | What parses the command | Typo behaviour | Duplicate detection | Modes / chords | GUI editor |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **GlazeWM** | YAML list of `{commands: [...], bindings: [...]}`; combo is a `+`-joined string | **Yes**, GNU-style flags: `focus --workspace 1`, `resize --width -2%` | `clap`'s `InvokeCommand::try_parse_from` called from a hand-written `Deserialize` impl | **Whole file rejected.** Fatal error dialog at startup; on reload a "Non-fatal error" dialog and the previous config is kept | **None.** Hook picks the longest match; `process_event` then re-resolves with `.find()` — first config entry wins | **Binding modes** (`binding_modes:`, `wm-enable-binding-mode --name`). No chords | **No.** Tray has "reload config" and "show config folder" only |
| **AeroSpace** | TOML table under `[mode.<name>.binding]`; key is the combo, value is a command string or array | **Yes**, positional: `workspace 3`, `move-node-to-workspace A`, `layout tiles horizontal vertical` | Its own `parseCmdArgs` / `lexAndParseShell` | **Diagnostic accumulated**, that one binding becomes `.empty`, rest of config still applies, GUI message window lists every error | **Yes.** `"'<combo>' Binding redeclaration"`, keyed on a modifier-order-normalised description | **Binding modes** (`mode service`), `on-mode-changed` callback, mode shown in the tray text. No chords | **No.** Tray menu only |
| **komorebi + whkd** | whkdrc: `alt + h : <shell command>`; komorebi itself has no binding table | **Yes** — it's a shell line, so anything: `komorebic focus-named-workspace code` | **Nothing.** whkd never inspects the command; it writes the line to a persistent `pwsh`/`cmd` stdin | Runs and fails as a shell command. whkdrc *syntax* errors reject the whole file with a message that discards the parse error | In-process only: `win-hotkeys` returns `RegistrationFailed` for an identical combo; whkd prints "ignoring this binding and continuing..." to stderr | `.pause` toggles all bindings; no named modes, no chords | **Read-only.** `komorebi-shortcuts` is an egui list of the whkdrc with a filter box |
| **i3** | `bindsym $mod+1 workspace number $ws1` | **Yes**, positional, arbitrary | **Nothing at load time** (`command = string` in `config.spec`); parsed by `parse_command` on each press | Config loads clean; on press, i3-nagbar: "The configured command for this shortcut could not be run successfully." | **Yes.** `check_for_duplicate_bindings` sets `has_errors` → i3-nagbar "You have an error in your i3 config file!" | **Modes** (`mode "resize" { ... }`), displayed by i3bar. No chords | **No** |
| **yabai + skhd** | skhdrc: `<mode> < <mod>-<key> : <shell command>` | **Yes** — shell line | **Nothing** (shell) | Runs and fails as a shell command | **None.** `table_add` keeps the existing entry — silent first-wins | **Modes** (`:: name @`), `@` captures unbound keys too. No chords | **No** |
| **PowerToys KBM** | JSON remap table, edited only through the GUI | N/A (remaps, not commands) | N/A | N/A | Editor-side validation with typed `ShortcutErrorType` values | **Chords** (2 action keys, opt-in "Allow chords" switch). No modes | **Yes** — the only one |
| **Mosaix (today)** | `[hotkeys]` table, `snap-left = "ctrl+alt+left"` | **No** — closed enum of unit `Command` variants | serde, on the enum | Unknown key rejects the file | **Yes**, post-merge, naming both commands | None | Planned |

Read across the table: **not one manager in the field keys its binding table on a closed enum of unit variants.** Every one carries a parameter, and every one pays for it with a parser and a class of runtime failure Mosaix does not currently have.

### The literal syntax, side by side

**GlazeWM** — `resources/assets/sample-config.yaml`. The binding entry is a two-field object; a *list* of commands runs in sequence, and a *list* of bindings gives the same commands multiple triggers:

```yaml
keybindings:
  # Shift focus in a given direction.
  - commands: ['focus --direction left']
    bindings: ['alt+h', 'alt+left']

  # Resize focused window by a percentage or pixel amount.
  - commands: ['resize --width -2%']
    bindings: ['alt+u']

  # Change to a workspace defined in `workspaces` config.
  - commands: ['focus --workspace 1']
    bindings: ['alt+1']

  # Move focused window to a workspace defined in `workspaces` config.
  - commands: ['move --workspace 1', 'focus --workspace 1']
    bindings: ['alt+shift+1']
```
— [`resources/assets/sample-config.yaml`](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)

**AeroSpace** — `docs/config-examples/default-config.toml`. The combo is the TOML *key*; the command is the *value*, a string or an array of strings:

```toml
[mode.main.binding]
    alt-slash = 'layout tiles horizontal vertical'
    alt-h = 'focus left'
    alt-shift-h = 'move left'
    alt-minus = 'resize smart -50'
    alt-1 = 'workspace 1'
    alt-a = 'workspace A' # In your config, you can drop workspace bindings that you don't need
    alt-shift-a = 'move-node-to-workspace A'
    alt-shift-semicolon = 'mode service'

# 'service' binding mode declaration.
[mode.service.binding]
    esc = ['reload-config', 'mode main']
    r = ['flatten-workspace-tree', 'mode main'] # reset layout
    f = ['layout floating tiling', 'mode main'] # Toggle between floating and tiling layout
```
— [`docs/config-examples/default-config.toml`](https://github.com/nikitabobko/AeroSpace/blob/main/docs/config-examples/default-config.toml)

Note `alt-a = 'workspace A'`. `A` is not an index; it is a user-chosen workspace *name*. That is the closest analogue in the field to Mosaix's "apply the saved layout called `writing`".

**whkd** (komorebi's hotkey daemon) — `docs/whkdrc.sample`. The command after the `:` is a shell line:

```
.shell powershell

alt + h                 : komorebic focus left
alt + shift + oem_4     : komorebic cycle-focus previous # oem_4 is [
alt + oem_plus          : komorebic resize-axis horizontal increase
alt + 1                 : komorebic focus-workspace 0
alt + shift + 1         : komorebic move-to-workspace 0
alt + p                 : komorebic toggle-pause
```
— [`docs/whkdrc.sample`](https://github.com/LGUG2Z/komorebi/blob/master/docs/whkdrc.sample)

whkd also supports a per-application dispatch block, which is a shape nobody else has:

```
alt + q [
    # Default is a keyword which will apply to all apps
    # If you only have Default, this is the same as doing "alt + q : komorebic close"
    Default       : komorebic close

    # Ignore is a keyword which will skip running the hotkey for the given process
    Google Chrome : Ignore
]
```
— [whkd README](https://github.com/LGUG2Z/whkd/blob/master/README.md)

**i3** — `etc/config`. `bindsym <combo> <command...>`, command unquoted to end of line:

```
bindsym Mod1+1 workspace number $ws1
bindsym Mod1+Shift+1 move container to workspace number $ws1
bindsym Mod1+r mode "resize"

mode "resize" {
        bindsym $left       resize shrink width 10 px or 10 ppt
        bindsym $down       resize grow height 10 px or 10 ppt
        # back to normal: Enter or Escape or Mod1+r
        bindsym Return mode "default"
        bindsym Escape mode "default"
        bindsym Mod1+r mode "default"
}
```
— [`etc/config`](https://github.com/i3/i3/blob/next/etc/config)

**skhd** — the grammar, as the maintainer states it:

```
hotkey       = <mode> '<' <action> | <action>
mode         = 'name of mode' | <mode> ',' <mode>
action       = <keysym> '[' <proc_map_lst> ']' | <keysym> '->' '[' <proc_map_lst> ']'
               <keysym> ':' <command>          | <keysym> '->' ':' <command>
               <keysym> ';' <mode>             | <keysym> '->' ';' <mode>
keysym       = <mod> '-' <key> | <key>
command      = command is executed through '$SHELL -c' and
               follows valid shell syntax.
->           = keypress is not consumed by skhd
```
— [skhd README, "Configuration"](https://github.com/asmvik/skhd/blob/master/README.md)

**Mosaix, for contrast** — the combo is the value, the command is the key, and the command is a closed enum:

```toml
[hotkeys]
snap-left = "ctrl+alt+left"
```
— [`crates/mosaix-config/src/schema.rs`](../../crates/mosaix-config/src/schema.rs), `Command` (16 unit variants) and `hotkeys: BTreeMap<Command, KeyCombo>`

Mosaix is the only entry in the table whose *key* is the command. That inversion is what makes `BTreeMap<Command, KeyCombo>` possible and is exactly what a parameterised command breaks: `apply-layout("writing")` and `apply-layout("code")` are two distinct keys, so the map's key type stops being a plain enum.

### What parses the command string, and what a typo does

The four answers are genuinely different, and the difference is load-bearing.

**GlazeWM: clap, inside a `Deserialize` impl — whole-file rejection.** `InvokeCommand` is a `clap::Parser` enum, and the config path reaches it through a hand-written deserializer that prepends a fake argv[0]:

```rust
impl<'de> Deserialize<'de> for InvokeCommand {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    // Clap expects an array of string slices where the first argument is
    // the binary name/path. When deserializing commands from the user
    // config, we therefore have to prepend an additional empty argument.
    let unparsed = String::deserialize(deserializer)?;
    let unparsed_split = iter::once("").chain(unparsed.split_whitespace());

    InvokeCommand::try_parse_from(unparsed_split).map_err(|err| {
      // Format the error message and remove the "error: " prefix.
      let err_msg = err.apply::<KindFormatter>().to_string();
      serde::de::Error::custom(err_msg.trim_start_matches("error: "))
    })
  }
}
```
— [`packages/wm-common/src/app_command.rs`, lines 265–281](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/app_command.rs)

This is the strictest design in the survey and the cheapest to build: the CLI (`glazewm command ...`), the IPC surface, and the config file all share one grammar, and clap generates the validation, the flag parsing, and the error text. The cost is that one bad command string fails `serde_yaml::from_str(&config_str)?` in `UserConfig::read`, so **the whole file is rejected** ([`packages/wm/src/user_config.rs`, `fn read`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/user_config.rs); the same function carries a `// TODO: Improve error formatting of serde_yaml errors.`). The consequences differ by moment:

- At startup, `UserConfig::new(config_path)?` propagates out of `start_wm`, and `main` treats it as terminal: *"If unable to start the WM, the error is fatal and a message dialog is shown."* — `dispatcher.show_error_dialog("Fatal error", &err.to_string())` ([`packages/wm/src/main.rs`, lines 71–80](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/main.rs)).
- On reload, `reload_config` calls `config.reload()?`, whose first act is `Self::read(&self.path)?`; if that fails, `self.value` is never assigned, so **the previously loaded config stays in effect** and the error reaches the main loop's tail, which shows `dispatcher.show_error_dialog("Non-fatal error", &err.to_string())` ([`reload_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/reload_config.rs); [`main.rs`, lines 288–292](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/main.rs)).

That last property is the good half of whole-file rejection: a bad hot-reload cannot leave the user hotkey-less.

**AeroSpace: its own parser, diagnostics accumulated, config still applied.** `parseShellOfCommandsForConfig` appends the failure to a diagnostic list and substitutes an empty command for that binding — it does not abort:

```swift
func parseShellOfCommandsForConfig(_ raw: OrderedJson, _ backtrace: ConfigBacktrace, _ c: inout ConfigParserContext) -> Shell<any Command> {
    if let rawString = raw.asStringOrNil {
        return parseCommand(rawString, allowExecAndForget: true, allowEval: false).toResult().toParsedConfig(backtrace).getOrNil(appendErrorTo: &c.errors) ?? .empty
```
— [`Sources/AppBundle/config/parseConfig.swift`, lines 184–187](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/parseConfig.swift)

Only a small set of failures are marked `preventConfigReload: true` (TOML syntax errors, an unreadable file, an ambiguous config location). `ParseConfigResult.allowReloadConfig` is `errors.allSatisfy { !$0.preventConfigReload }`, and the reload path applies the config whenever that holds — *while simultaneously* showing a message window listing every diagnostic:

```swift
    if !args.noGui {
        let lines = errors + warnings
        switch true {
            case !errors.isEmpty:
                let msg = failedToParseMsg(configUrl: result.configUrl, errorsCount: parseResult.errors.count, warningsCount: parseResult.warnings.count, lines: lines)
                MessageModel.shared.message = Message(body: msg, containsWarnings: containsWarnings)
    ...
    if parseResult.allowReloadConfig && !args.dryRun {
        TrayMenuModel.shared.lastReloadConfigContainedWarnings = containsWarnings
        resetHotKeys()
        config = parseResult.config
```
— [`Sources/AppBundle/command/impl/ReloadConfigCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/command/impl/ReloadConfigCommand.swift)

The header text is `"Failed to parse '<path>'. N error(s). M warning(s)"`, followed by every diagnostic with its backtrace. This is the most usable failure mode in the survey: the user gets a list, not the first error, and the working bindings keep working.

**i3: nothing at load time.** The config grammar terminates the binding rule with an untyped string:

```
state BINDCOMMAND:
  ...
  command = string
      -> call cfg_binding($bindtype, $modifiers, $key, $release, $border, $whole_window, $exclude_titlebar, $command)
```
— [`parser-specs/config.spec`, lines 454–465](https://github.com/i3/i3/blob/next/parser-specs/config.spec)

The command is stored verbatim and handed to the *command* parser on every press. `run_binding` copies it, calls `parse_command`, and inspects the result:

```c
    if (result->parse_error) {
        char *pageraction;
        sasprintf(&pageraction, "i3-sensible-pager \"%s\"\n", errorfilename);
        char *argv[] = {
            NULL, /* will be replaced by the executable path */
            "-f", config.font.pattern,
            "-t", "error",
            "-m", "The configured command for this shortcut could not be run successfully.",
            "-b", "show errors", pageraction,
            NULL};
        start_nagbar(&command_error_nagbar_pid, argv);
```
— [`src/bindings.c`, `run_binding`, lines 883–899](https://github.com/i3/i3/blob/next/src/bindings.c)

The commands parser deliberately distinguishes this class: `"We set parse_error to true to distinguish this from other errors. i3-nagbar is spawned upon keypresses only for parser errors."` ([`src/commands_parser.c`, lines 339–341](https://github.com/i3/i3/blob/next/src/commands_parser.c)). So i3 accepts every binding at load and pays for it with a per-press dialog — which, notably, is a *worse* place to learn about a typo than either GlazeWM's or AeroSpace's.

i3's *config* parser is nevertheless the most forgiving in the survey at the line level. On an unparseable line it prints the offending line with an underline pointing at the parser position, then:

```c
            context->has_errors = true;

            /* Skip the rest of this line, but continue parsing. */
            while ((size_t)(walk - input) <= len && *walk != '\n') {
                walk++;
            }
```
— [`src/config_parser.c`, lines 410–416](https://github.com/i3/i3/blob/next/src/config_parser.c)

and at the end `parse_file_inner` spawns i3-nagbar with `"You have an error in your i3 config file!"` and an "edit config" button wired to `i3-sensible-editor "<path>" && i3-msg reload` ([`config_parser.c`, lines 505–530 and 818–840](https://github.com/i3/i3/blob/next/src/config_parser.c)). Per-line skip plus one aggregate nagbar is the same shape as AeroSpace's diagnostic list.

**whkd and skhd: the command is a shell line, never inspected.** whkd's parser (chumsky) treats everything after the `:` as opaque text (`let command = take_until(choice((comment, text::newline(), end())))`), and `HkmData::register` closes over the string and writes it to a long-lived shell's stdin ([`parser/src/lib.rs`, lines 47–50](https://github.com/LGUG2Z/whkd/blob/master/parser/src/lib.rs); [`src/main.rs`, lines 52–89](https://github.com/LGUG2Z/whkd/blob/master/src/main.rs)). A typo in `komorebic focus-lft` is a shell failure at press time and nothing more.

whkd's handling of a *grammar* error is worth recording as an anti-pattern, because it throws the error away:

```rust
    parser()
        .parse(contents)
        .map_err(|_error| WhkdError::Parse(path.clone()))
```
— [`parser/src/lib.rs`, lines 21–23](https://github.com/LGUG2Z/whkd/blob/master/parser/src/lib.rs)

and the binary then does `whkd_parser::load(&home).unwrap_or_else(|_| panic!("could not load whkdrc from {home:?}"))` ([`src/main.rs`, line 40](https://github.com/LGUG2Z/whkd/blob/master/src/main.rs)). Whole-file rejection with a message that names only the path.

skhd's is worse in a specific way. Its parser is fail-fast (`if (parser->error) break;`) and frees the mode map on error, returning `false` ([`src/parse.c`, `parse_config`, lines 468–500](https://github.com/asmvik/skhd/blob/master/src/parse.c)). But its hot-reload callback frees the *live* bindings before re-parsing:

```c
static HOTLOADER_CALLBACK(config_handler)
{
    debug("skhd: config-file has been modified.. reloading config\n");
    free_mode_map(&mode_map);
    free_blacklist(&blacklst);
    parse_config_helper(config_file);
}
```
— [`src/skhd.c`, lines 110–118](https://github.com/asmvik/skhd/blob/master/src/skhd.c)

So saving a syntactically bad skhdrc leaves the user with **no bindings at all** until they fix it, and the only feedback is a `stderr` line of the form `#<line>:<col> <message>` from `parser_report_error` ([`src/parse.c`, lines 593–601](https://github.com/asmvik/skhd/blob/master/src/parse.c)). GlazeWM's ordering — parse first, swap only on success — is the correct one and is worth stating as a rule.

### Binding to a user-named entity: three answers, all shipped

This is Mosaix's open question, and it is not novel. Three managers let a binding name a user-created string, and all three do something different when the name is missing.

**AeroSpace: create it on demand, and treat the binding as a declaration.** `workspace A` resolves through `Workspace.get(byName:)`, which is an insert-on-miss:

```swift
    @MainActor static func get(byName name: String) -> Workspace {
        if let existing = workspaceNameToWorkspace[name] {
            return existing
        } else {
            let workspace = Workspace(name)
            workspaceNameToWorkspace[name] = workspace
            return workspace
        }
    }
```
— [`Sources/AppBundle/tree/Workspace.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/tree/Workspace.swift)

There is no "does this workspace exist" check anywhere in `WorkspaceCommand.run` — the only failure path is "already focused" ([`Sources/AppBundle/command/impl/WorkspaceCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/command/impl/WorkspaceCommand.swift)). And under `config-version = 1`, AeroSpace goes further and **derives the set of names that should stay alive from the bindings themselves**:

```swift
        config.persistentWorkspaces = (config.modes.values.lazy
            .flatMap { (mode: Mode) -> [HotkeyBinding] in Array(mode.bindings.values) }
            .flatMap { (binding: HotkeyBinding) -> [String] in
                let commands = binding.commands.flatten()
                return commands.filterIsInstance(of: WorkspaceCommand.self).compactMap { $0.args.target.val.workspaceNameOrNil()?.raw } +
                    commands.filterIsInstance(of: MoveNodeToWorkspaceCommand.self).compactMap { $0.args.target.val.workspaceNameOrNil()?.raw }
            }
            + (config.workspaceToMonitorForceAssignment).keys)
            .toOrderedSet()
```
— [`Sources/AppBundle/config/parseConfig.swift`, lines 280–288](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/parseConfig.swift)

In `config-version = 2` this became an explicit `persistent-workspaces = [...]` key in the config, listed in the shipped default. That migration is itself informative: the maintainer moved *away* from inferring the entity set from bindings and toward declaring it once.

What AeroSpace does validate at parse time is the **name**, against a reserved list:

```swift
    public static func parse(_ raw: String) -> ResOrStr<WorkspaceName> {
        // reserved names
        if raw == "focused" || raw == "non-focused" ||
            raw == "visible" || raw == "invisible" || raw == "non-visible" ||
            raw == "active" || raw == "non-active" || raw == "inactive" ||
            raw == "back-and-forth" || raw == "back_and_forth" || raw == "previous" ||
            raw == "prev" || raw == "next" ||
            raw == "monitor" || raw == "workspace" ||
            raw == "monitors" || raw == "workspaces" ||
            raw == "all" || raw == "none" ||
            raw == "mouse" || raw == "target"
        {
            return .failure("'\(raw)' is a reserved workspace name")
        }
        if raw.isEmpty { return .failure("Empty workspace name is forbidden") }
        if raw.contains(",") { return .failure("Workspace names are not allowed to contain comma") }
        if raw.starts(with: "_") { return .failure("Workspace names starting with underscore are reserved for future use") }
        if raw.starts(with: "-") {
            // The syntax conflicts with CLI options. E.g. list-windows --workspace -foo
            return .failure("Workspace names starting with dash are disallowed")
        }
        if raw.rangeOfCharacter(from: .whitespacesAndNewlines) != nil {
            return .failure("Whitespace characters are forbidden in workspace names")
        }
        return .success(WorkspaceName(raw))
    }
```
— [`Sources/Common/model/WorkspaceName.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/Common/model/WorkspaceName.swift)

Three of those rules exist because the name shares a namespace with the command grammar (`prev`, `next`, `all`, leading `-`), which is a direct consequence of positional command syntax.

**GlazeWM: error at press time, with a dialog.** The command struct types the field as a plain `Option<String>`, so nothing is checked at load:

```rust
pub struct InvokeFocusCommand {
  #[clap(long)]
  pub direction: Option<Direction>,
  ...
  #[clap(long)]
  pub workspace: Option<String>,
```
— [`packages/wm-common/src/app_command.rs`, lines 310–322](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/app_command.rs)

At invoke time, `focus_workspace` resolves the target, falls through to `activate_workspace(Some(&name), ...)`, and that requires a matching entry in the config's `workspaces:` list:

```rust
  let found_config = match workspace_name {
    Some(workspace_name) => config
      .inactive_workspace_configs(&state.workspaces())
      .into_iter()
      .find(|config| config.name == workspace_name)
      .with_context(|| {
        format!(
          "Workspace with name '{workspace_name}' doesn't exist or is already active."
        )
      }),
```
— [`packages/wm/src/commands/workspace/activate_workspace.rs`, lines 89–100](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/workspace/activate_workspace.rs)

That `anyhow` error propagates out of `wm.process_event(PlatformEvent::Keybinding(...))` into the `tokio::select!` tail in `main`, which logs it and pops `show_error_dialog("Non-fatal error", ...)`. GlazeWM has a **second** instance of the same pattern for a different entity — binding modes:

```rust
  let binding_mode = config
    .value
    .binding_modes
    .iter()
    .find(|config| name == config.name)
    .with_context(|| {
      format!("No binding mode found with the name '{name}'.")
    })?;
```
— [`packages/wm/src/commands/general/enable_binding_mode.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/enable_binding_mode.rs)

So GlazeWM's answer is consistent: **the named entity must be declared elsewhere in the config, the binding is not the declaration, and a dangling reference is a runtime error with a visible dialog** — never a config error.

**komorebi: silent no-op.** komorebi has a full family of named-workspace commands (`focus-named-workspace`, `move-to-named-workspace`, `send-to-named-workspace`, `named-workspace-layout`, `named-workspace-custom-layout`, `named-workspace-rule`, …) reachable from a whkdrc binding as `komorebic focus-named-workspace code`. Every one of them is written the same way:

```rust
            SocketMessage::FocusNamedWorkspace(ref name) => {
                if let Some((monitor_idx, workspace_idx)) =
                    self.monitor_workspace_index_by_name(name)
                {
                    self.focus_monitor(monitor_idx)?;
                    self.focus_workspace(workspace_idx)?;
                }
            }
```
— [`komorebi/src/process_command.rs`, lines 1269–1276](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/process_command.rs)

There is no `else`. A binding naming a workspace that does not exist does nothing, logs nothing, and returns success to `komorebic`. `SendContainerToNamedWorkspace` (line 850), `MoveContainerToNamedWorkspace` (line 863), `NamedWorkspaceLayoutCustom` (line 1023), `NamedWorkspaceTiling` (line 1030) and `NamedWorkspaceLayout` (line 1037) are all identical in shape. komorebi's counterweight is an explicit declaration command, `komorebic ensure-named-workspaces <monitor> <names...>` (`SocketMessage::EnsureNamedWorkspaces(usize, Vec<String>)`), which users are expected to run at startup.

**And one manager deliberately avoids the problem: komorebi identifies custom layouts by path, not by name.** The saved-layout case closest to Mosaix's is `komorebic named-workspace-custom-layout <workspace> <path>`, whose argument is documented as *"JSON or YAML file from which the custom layout definition should be loaded"* and typed `PathBuf` ([`komorebic/src/main.rs`, `struct NamedWorkspaceCustomLayout`, lines 311–318](https://github.com/LGUG2Z/komorebi/blob/master/komorebic/src/main.rs)). Layouts in komorebi are files; only workspaces get names.

### Modes, chords, and prefixes

**Four of the five managers have modal bindings; none has a chord.** The one chord implementation in the whole survey is PowerToys Keyboard Manager's, and it is opt-in per mapping: *"Shortcuts can be created with one or more modifiers and two non-modifier keys. These are called 'chords'. In order to create a chord, select Edit to open the dialog to record the shortcut using the keyboard. Once opened, toggle on the Allow chords switch."* ([Keyboard Manager docs, "Shortcuts with chords"](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager)). The editor's capture code caps it at two action keys and shifts the oldest out ([`KeyboardHookHelper.cs`, `KeyDown`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorUI/Helpers/KeyboardHookHelper.cs)).

The modal designs:

- **i3** — `mode "resize" { ... }` in the config, entered with `mode "resize"` and left with `mode "default"`. i3's own default config binds `Return`, `Escape` and `Mod1+r` back to default, i.e. three escapes for one mode. The mode name is broadcast on the IPC `mode` event and shown by i3bar. The userguide's statement: *"You can have multiple sets of bindings by using different binding modes. When you switch to another binding mode, all bindings from the current mode are released and only the bindings defined in the new mode are valid."* ([i3 userguide, "Binding modes"](https://i3wm.org/docs/userguide.html); [`etc/config`, lines 172–196](https://github.com/i3/i3/blob/next/etc/config)).
- **AeroSpace** — modes are the top-level structure of the binding table, not a nested block: `[mode.main.binding]` is mandatory (`parseModes` emits `"Please specify 'main' mode"` if it is absent, [`Sources/AppBundle/config/Mode.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/Mode.swift)). Switching is `mode <name>`, and it works by toggling the *enabled* flag on already-created Carbon hotkeys rather than by filtering at dispatch:

  ```swift
  @MainActor func activateMode_nonCancellable(_ targetMode: String?) async {
      let targetBindings = targetMode.flatMap { config.modes[$0] }?.bindings ?? [:]
      for binding in targetBindings.values where !hotkeys.keys.contains(binding.descriptionWithKeyCode) {
          hotkeys[binding.descriptionWithKeyCode] = HotKey(key: binding.keyCode, modifiers: binding.modifiers, keyDownHandler: { ... })
      }
      for (binding, key) in hotkeys {
          key.isEnabled = targetBindings.keys.contains(binding)
      }
  ```
  — [`Sources/AppBundle/config/HotkeyBinding.swift`, lines 34–49](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/HotkeyBinding.swift)

  This is the closest analogue in the field to what Mosaix would have to do with `RegisterHotKey`/`UnregisterHotKey`, because Carbon's `RegisterEventHotKey` is likewise OS-arbitrated. Note the design decision: hotkeys are created lazily and never destroyed on mode change, only paused. Mode state **is** visible — `updateTrayText()` prefixes the menu-bar text with the uppercased mode name whenever it is not `main`:

  ```swift
  TrayMenuModel.shared.trayText = (activeMode?.takeIf { $0 != mainModeId }?.first.map { "(\($0.uppercased())) " } ?? "") + ...
  ```
  — [`Sources/AppBundle/ui/TrayMenuModel.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/ui/TrayMenuModel.swift)

  There is also an `on-mode-changed` config hook, present (empty) in the shipped default config.

- **GlazeWM** — `binding_modes:` is a sibling of `keybindings:`, entered with `wm-enable-binding-mode --name resize` and left with `wm-disable-binding-mode --name resize`. Selection happens at dispatch, and a mode **replaces** the base table entirely rather than layering on it:

  ```rust
    pub fn active_keybinding_configs(
      &self,
      binding_modes: &[wm_common::BindingModeConfig],
      is_paused: bool,
    ) -> impl Iterator<Item = KeybindingConfig> {
      let source_configs = if let Some(first_mode) = binding_modes.first() {
        &first_mode.keybindings
      } else {
        &self.value.keybindings
      }
  ```
  — [`packages/wm/src/user_config.rs`, lines 359–372](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/user_config.rs)

  The same function is where the pause interlock lives, and it is the neatest bit of design in GlazeWM's binding layer: when paused, it returns only the configs containing `InvokeCommand::WmTogglePause`, *"so that unpausing remains possible."* Mode changes emit `WmEvent::BindingModesChanged`, which `main` reacts to by calling `keybinding_listener.update(...)` with the new key set, and which is also published on IPC as a subscribable event (`SubscribableEvent::BindingModesChanged`) for the status bar.

- **skhd** — modes are declared `:: name` with an optional `@` meaning *"capture keypresses regardless of being bound to an action"*, and a binding switches mode with `;` instead of `:` ([skhd README](https://github.com/asmvik/skhd/blob/master/README.md)). A binding can also be scoped to several modes at once (`mode = 'name' | <mode> ',' <mode>`). Referencing an undeclared mode is a **hard config error** — see below.

- **whkd** has no modes. It has `.pause <combo>`, a global on/off toggle for every binding, plus a `.pause_hook` shell command run on toggle ([whkd README](https://github.com/LGUG2Z/whkd/blob/master/README.md)).

**Is a one-combo-to-one-command table painting Mosaix into a corner?** On this evidence: no, but only because every modal implementation here is a *filter over a set of tables*, not a change to the shape of a binding. i3, AeroSpace and GlazeWM all keep "one combo → one command list" and add a layer of indirection above it (which table is live). Adding modes later means adding a mode dimension to the table and a re-registration step on mode change — both of which Mosaix already has the machinery for, since it re-registers on config reload. Chords are the genuinely structural change, and nobody in the tiling field has them.

### Conflict and duplicate detection

**i3 is the strictest, and it checks twice.** During keysym→keycode translation it scans for an existing binding with the same keycode, modifiers, and release flag and logs `"Duplicate keybinding in config file:\n keysym = %s, keycode = %d, state_mask = 0x%x\n"` ([`src/bindings.c`, lines 599–614](https://github.com/i3/i3/blob/next/src/bindings.c)). Then, after every config file (and every `include`d file) is parsed, `check_for_duplicate_bindings` does an O(n²) pass over the whole binding list:

```c
void check_for_duplicate_bindings(struct context *context) {
    Binding *bind, *current;
    TAILQ_FOREACH (current, bindings, bindings) {
        TAILQ_FOREACH (bind, bindings, bindings) {
            if (bind == current) { break; }
            if (!binding_same_key(bind, current)) { continue; }
            context->has_errors = true;
            ...
                ELOG("Duplicate keybinding in config file:\n  state mask 0x%x with keysym %s, command \"%s\"\n",
                     current->event_state_mask, current->symbol, current->command);
```
— [`src/bindings.c`, lines 784–806](https://github.com/i3/i3/blob/next/src/bindings.c)

`has_errors` is what spawns i3-nagbar, so a duplicate binding is a **user-visible config error**, not a log line. `binding_same_key` also normalises: it compares symbols case-insensitively (`strcasecmp`) and refuses to compare a `bindsym` against a `bindcode` ([`src/bindings.c`, lines 747–774](https://github.com/i3/i3/blob/next/src/bindings.c)).

**AeroSpace detects duplicates and normalises modifier order.** Because the combo is a TOML key, literal duplicates are already impossible; the check exists for the aliasing case (`alt-shift-h` vs `shift-alt-h`), and it works because the map is keyed on a canonical description built from the parsed modifier set and keycode:

```swift
        if let binding {
            if result.keys.contains(binding.descriptionWithKeyCode) {
                c.errors.append(.init(backtrace, "'\(binding.descriptionWithKeyCode)' Binding redeclaration"))
            }
            result[binding.descriptionWithKeyCode] = binding
        }
```
— [`Sources/AppBundle/config/HotkeyBinding.swift`, lines 96–102](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/HotkeyBinding.swift)

Note that it reports **and then overwrites** — last wins, with a diagnostic. Note also that the check is per-mode, since `parseBindings` is called once per `[mode.X.binding]` table; two modes may bind the same combo, which is the point of modes.

**GlazeWM has no duplicate detection at all, and resolves first-wins.** The listener buckets every binding by its trigger key without deduplicating:

```rust
  fn create_keybinding_map(
    keybindings: &[Keybinding],
  ) -> HashMap<Key, Vec<Keybinding>> {
    let mut keybinding_map = HashMap::new();
    for keybinding in keybindings {
      keybinding_map
        .entry(*keybinding.trigger_key())
        .or_insert_with(Vec::new)
        .push(keybinding.clone());
    }
    keybinding_map
  }
```
— [`packages/wm-platform/src/keybinding_listener.rs`, lines 221–233](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/keybinding_listener.rs)

On a press it filters candidates whose every key is held, takes `.max_by_key(|keybinding| keybinding.keys().len())` — longest match wins, which is how `alt+shift+h` beats `alt+h` — and rejects if any modifier group *not* in the binding is held. The resulting `KeybindingEvent` carries the `Keybinding`, and the WM then maps it back to commands:

```rust
      PlatformEvent::Keybinding(keybinding_event) => {
        // Find the keybinding config that matches this keybinding.
        let commands = config
          .active_keybinding_configs(...)
          .find(|kb_config| {
            kb_config.bindings.contains(&keybinding_event.0)
          })
          .map(|kb_config| kb_config.commands.clone());
```
— [`packages/wm/src/wm.rs`, lines 87–101](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/wm.rs)

`.find` is first-match, so with two `alt+h` entries the **earlier one in the file** runs and the later is dead. Nothing warns.

**skhd is silently first-wins, by way of its hash table:**

```c
void table_add(struct table *table, void *key, void *value)
{
    struct bucket **bucket = table_get_bucket(table, key);
    if (*bucket) {
        if (!(*bucket)->value) {
            (*bucket)->value = value;
        }
    } else {
        *bucket = table_new_bucket(key, value);
        ++table->count;
    }
}
```
— [`src/hashtable.h`, lines 91–102](https://github.com/asmvik/skhd/blob/master/src/hashtable.h)

An existing entry with a non-null value is left untouched. No diagnostic.

**whkd detects duplicates in its hotkey library, and reports to a stderr its own recommended launch hides.** `win-hotkeys` computes an id from the combo and refuses a second registration:

```rust
        // Check if already exists
        if self
            .hotkeys
            .values()
            .any(|vec| vec.iter().any(|hotkey| hotkey.generate_id() == id))
        {
            return Err(RegistrationFailed);
        }
```
— [`src/manager.rs`, lines 59–66](https://github.com/iholston/win-hotkeys/blob/main/src/manager.rs), with `#[error("Hotkey registration failed. Hotkey is already in use.")] RegistrationFailed` in [`src/error.rs`](https://github.com/iholston/win-hotkeys/blob/main/src/error.rs)

whkd's response is per-binding and non-fatal:

```rust
        }) {
            eprintln!(
                "Unable to bind '{:?} + {}' to '{}' (error: {error}), ignoring this binding and continuing...",
                self.mod_keys, self.vkey, self.command
            );
        }
```
— [`src/main.rs`, lines 80–86](https://github.com/LGUG2Z/whkd/blob/master/src/main.rs)

Per-binding partial success is the right policy — it is the same policy Mosaix's `start_hotkeys` already implements. The weakness is the channel: komorebi's own sample whkdrc reloads whkd with `Start-Process whkd -WindowStyle hidden` ([`docs/whkdrc.sample`](https://github.com/LGUG2Z/komorebi/blob/master/docs/whkdrc.sample)), so in the documented configuration that `eprintln!` has no visible console.

**One dangling-reference case is a hard config error: skhd's modes.** Referencing an undeclared mode is fatal to the whole file:

```c
    if (!mode && token_equals(identifier, "default")) {
        mode = find_or_init_default_mode(parser);
    } else if (!mode) {
        parser_report_error(parser, identifier, "undeclared identifier\n");
        return;
    }
```
— [`src/parse.c`, lines 244–250](https://github.com/asmvik/skhd/blob/master/src/parse.c); the same message is emitted for a `;`-activation naming an unknown mode at [line 129](https://github.com/asmvik/skhd/blob/master/src/parse.c)

This is the one shipped precedent for "the binding names a user-created entity, and a missing name is a **load-time** error." It works because a skhd mode is declared in the same file, by a `::` line — exactly the relationship a Mosaix `[layouts]` table would have with a `apply-layout = "writing"` binding.

**How an OS refusal is reported — and the one manager that does it worst.** Only two systems in the survey use OS-arbitrated registration at all.

- **GlazeWM, komorebi/whkd and skhd do not.** GlazeWM installs a single `WH_KEYBOARD_LL` hook and matches in the callback (`platform_impl::KeyboardHook::new(...)` in [`keybinding_listener.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/keybinding_listener.rs)); `win-hotkeys` likewise (`SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)` in [`src/hook.rs`](https://github.com/iholston/win-hotkeys/blob/main/src/hook.rs)); skhd uses a `CGEventTap`. A hook has no concept of an OS refusal — it simply shadows whatever it matches. There is nothing to report and nothing to negotiate.
- **AeroSpace does, through Carbon — and swallows the failure.** `HotKey(key:modifiers:keyDownHandler:)` is `soffes/HotKey` 0.2.1, pinned exactly in `Package.swift`. Its registration path is:

  ```swift
  		// Ensure registration worked
  		guard registerError == noErr, eventHotKey != nil else {
  			return
  		}
  ```
  — [`Sources/HotKey/HotKeysController.swift`](https://github.com/soffes/HotKey/blob/master/Sources/HotKey/HotKeysController.swift)

  No log, no throw, no return value. AeroSpace's call site (`hotkeys[binding.descriptionWithKeyCode] = HotKey(...)`) has no error handling either. So if macOS or another application already owns a combo, **the AeroSpace binding silently does nothing and nothing tells the user.** This is the single most instructive failure in the survey, because it is exactly the failure Mosaix's per-binding `HotkeyRegistrationResult` exists to prevent.
- **PowerToys does, and built a whole subsystem for it** — see below.

**Reserved / never-bindable lists.** Only PowerToys maintains one, and it has exactly two entries:

```cpp
    ShortcutErrorType IsShortcutIllegal(Shortcut shortcut)
    {
        // Win+L
        if (shortcut.winKey != ModifierKey::Disabled && shortcut.ctrlKey == ModifierKey::Disabled && shortcut.altKey == ModifierKey::Disabled && shortcut.shiftKey == ModifierKey::Disabled && shortcut.actionKey == 0x4C)
        {
            Logger::info(L"Illegal shortcut detected: Win+L");
            return ShortcutErrorType::WinL;
        }

        // Ctrl+Alt+Del
        if (shortcut.winKey == ModifierKey::Disabled && shortcut.ctrlKey != ModifierKey::Disabled && shortcut.altKey != ModifierKey::Disabled && shortcut.shiftKey == ModifierKey::Disabled && shortcut.actionKey == VK_DELETE)
        {
            Logger::info(L"Illegal shortcut detected: Ctrl+Alt+Del");
            return ShortcutErrorType::CtrlAltDel;
        }

        return ShortcutErrorType::NoError;
    }
```
— [`src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditorHelpers.cpp`, lines 138–155](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditorHelpers.cpp)

These are two of twenty values in `ShortcutErrorType`, which is otherwise all shape rules (`ShortcutStartWithModifier`, `ShortcutAtleast2Keys`, `ShortcutOneActionKey`, `ShortcutMaxShortcutSizeOneActionKey`, …) ([`ShortcutErrorType.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/ShortcutErrorType.h)). The user-facing statement is unambiguous: *"⊞ Win+L and Ctrl+Alt+Del cannot be remapped as they are reserved by the Windows OS."* ([Keyboard Manager docs, "Important"](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager)).

Everything else PowerToys learns from the OS rather than from a table. The runner's `HotkeyConflictManager` classifies a combo as `InAppConflict` (another PowerToys module owns it) or `SystemConflict`, and the system test is a live register/unregister probe:

```cpp
        // Use a unique ID for this test registration
        const int hotkeyId = 0x0FFF; // Arbitrary ID for temporary registration

        // Try to register the hotkey with Windows, using nullptr instead of a window handle
        if (!RegisterHotKey(nullptr, hotkeyId, modifiers, hotkey.key))
        {
            // If registration fails with ERROR_HOTKEY_ALREADY_REGISTERED, it means the hotkey
            // is already in use by the system or another application
            if (GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED)
            {
                return true;
            }
        }
        else
        {
            // If registration succeeds, unregister it immediately
            UnregisterHotKey(nullptr, hotkeyId);
        }

        return false;
```
— [`src/runner/hotkey_conflict_detector.cpp`, `HasConflictWithSystemHotkey`, lines 334–383](https://github.com/microsoft/PowerToys/blob/main/src/runner/hotkey_conflict_detector.cpp)

`GetAllConflicts` also has a documented fallback that is worth copying verbatim in spirit: after checking the in-app map, the system map, and the successfully-registered map, it concludes *"If all the above conditions are ruled out, a system-level conflict is the only remaining explanation."* and attributes the conflict to `moduleName = L"System"` ([lines 155–161](https://github.com/microsoft/PowerToys/blob/main/src/runner/hotkey_conflict_detector.cpp)). The Win32 documentation supports treating the probe as authoritative but not exhaustive: *"Typically, RegisterHotKey also fails if the keystrokes specified for the hot key have already been registered for another hot key. However, some pre-existing, default hotkeys registered by the OS (such as PrintScreen, which launches the Snipping tool) may be overridden by another hot key registration when one of the app's windows is in the foreground."*, and separately *"The F12 key is reserved for use by the debugger at all times, so it should not be registered as a hot key."* and *"Keyboard shortcuts that involve the WINDOWS key are reserved for use by the operating system."* ([RegisterHotKey, winuser.h](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)).

komorebi's docs make the same point from the user side, about the Windows key specifically: *"However, `whkd` is a very simple hotkey daemon, and notably, does not include workarounds for Microsoft's restrictions on hotkey combinations that can use the `Windows` key. If using hotkey combinations with the `Windows` key is important to you, I suggest that ... you use AutoHotKey to handle your key bindings."* ([`docs/installation.md`, lines 17–24](https://github.com/LGUG2Z/komorebi/blob/master/docs/installation.md)).

### Why komorebi delegates hotkeys to a separate binary at all

The maintainer states the split as a design boundary, not an accident, in the first paragraphs of the getting-started guide:

> `komorebi` is a tiling window manager for Windows that is comprised of two main binaries, `komorebi.exe`, which contains the window manager itself, and `komorebic.exe`, which is the main way to send commands to the tiling window manager.
>
> **It is important to note that neither `komorebi.exe` nor `komorebic.exe` handle key bindings, because `komorebi` is a tiling window manager and not a hotkey daemon.**
>
> — [`docs/installation.md`, lines 1–10](https://github.com/LGUG2Z/komorebi/blob/master/docs/installation.md)

The mechanics that make it work: `komorebic.exe` is a complete `clap` CLI over a Unix-socket message enum (`SocketMessage`), so any hotkey daemon that can run a command line can drive komorebi. whkd's contribution is a combo→shell-line table and a persistent shell process; it holds no komorebi state. The cost is visible in the failure modes catalogued above — komorebi has no idea whether a binding exists, whkd has no idea whether a command is valid, and neither can report on the other's problem. The upside is that users who need Win-key bindings can swap whkd for AutoHotkey without touching the WM, which the docs explicitly recommend, and komorebi ships `komorebic.lib.ahk` in the repo root to support it.

### GUI editors in the tiling-WM field: there are none

Checked directly:

- **GlazeWM.** The tray menu is an enum of five items: `ReloadConfig`, `ShowConfigFolder`, `ToggleWindowAnimations`, `RunOnStartup`, `Exit` ([`packages/wm/src/sys_tray.rs`, lines 19–27](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/sys_tray.rs)). The companion app in the org is **Zebar**, described as *"a tool for creating customizable and cross-platform taskbars, desktop widgets, and popups"* — a status bar, not a settings UI (glzr-io repository list; the org's other repos are `glazewm-js`, an IPC client library, plus shared tooling). GlazeWM does have a runtime config-mutating command, `wm-update-workspace-config`, but it is in-memory only — `workspace.set_config(updated_config)` with no file write anywhere in the function ([`packages/wm/src/commands/workspace/update_workspace_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/workspace/update_workspace_config.rs)).
- **komorebi.** Two egui apps ship in-tree. `komorebi-gui` is a live state/appearance panel — its `main.rs` imports `GlobalState`, `BorderStyle`, `StackbarMode` and drives them over `SocketMessage`; it contains no binding code. `komorebi-shortcuts` is closer, and it is **read-only**: a struct literally named `Quicklook`, holding `whkdrc: Option<Whkdrc>` and a `filter: String`, rendering a two-column egui grid of `binding.keys.join(" + ")` against `binding.command`, filtered by substring. Ninety-eight lines, no capture, no write path, and `whkd_parser::load(&home).ok()` — a parse failure silently produces an empty window ([`komorebi-shortcuts/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi-shortcuts/src/main.rs)).
- **AeroSpace.** A SwiftUI tray menu (`TrayMenuModel`, `TrayItem`) showing workspaces, the active mode, an enable toggle, and accessibility-permission state. No editor. Config errors are surfaced in a `MessageModel` window (see `reloadConfig_nonCancellable` above).
- **i3, skhd, whkd, yabai.** None.

**So the only shipping GUI key-capture editor relevant to Mosaix is Microsoft's**, and it is worth reading closely because it solves precisely the problem the brief poses.

### PowerToys Keyboard Manager: capturing a keystroke that is already live

PowerToys uses **two layers of suspension**, at two different scopes.

**Layer 1 — a named event that suspends the whole engine, held for the editor window's lifetime.** The KBM engine and the KBM editor are separate processes. When the editor window is created it takes an RAII lock on a named Win32 event:

```cpp
inline void CreateEditShortcutsWindowImpl(HINSTANCE hInst, KBMEditor::KeyboardManagerState& keyboardManagerState, MappingConfiguration& mappingConfiguration, std::wstring keysForShortcutToEdit, std::wstring action)
{
    Logger::trace("CreateEditShortcutsWindowImpl()");
    auto locker = EventLocker::Get(KeyboardManagerConstants::EditorWindowEventName.c_str());
    if (!locker.has_value())
    {
        Logger::error(L"Failed to lock event {}. {}", KeyboardManagerConstants::EditorWindowEventName, get_last_error_or_default(GetLastError()));
    }

    Logger::trace(L"Signaled {} event to suspend the KBM engine", KeyboardManagerConstants::EditorWindowEventName);
```
— [`KeyboardManagerEditorLibrary/EditShortcutsWindow.cpp`, lines 72–81](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditShortcutsWindow.cpp) (identical code at [`EditKeyboardWindow.cpp`, lines 120–129](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditKeyboardWindow.cpp))

The event name is `L"PowerToys_KeyboardManager_Event_EditorWindow"` ([`common/KeyboardManagerConstants.h`, line 8](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/common/KeyboardManagerConstants.h)). The engine, in its own process, opens the same event at startup and checks it as the *second* statement of its hook handler:

```cpp
intptr_t KeyboardManager::HandleKeyboardHookEvent(LowlevelKeyboardEvent* data) noexcept
{
    if (loadingSettings)
    {
        return 0;
    }

    // Suspend remapping if remap key/shortcut window is opened
    if (editorIsRunningEvent != nullptr && WaitForSingleObject(editorIsRunningEvent, 0) == WAIT_OBJECT_0)
    {
        return 0;
    }
```
— [`KeyboardManagerEngineLibrary/KeyboardManager.cpp`, lines 301–311](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEngineLibrary/KeyboardManager.cpp); the mouse hook does the same at lines 178–180 with the comment *"Suspend while the remap key/shortcut editor window is capturing input, mirroring HandleKeyboardHookEvent."*

So **yes: PowerToys suspends its live remapping for the whole time the editor is open**, cross-process, via a named kernel object, with a zero-timeout `WaitForSingleObject` on the hot path.

**Layer 2 — a per-dialog capture state, armed only when the capture window is foreground.** The editor process runs its own `WH_KEYBOARD_LL` hook and switches its behaviour on a state enum whose variants are documented individually:

```cpp
    enum class KeyboardManagerUIState
    {
        // If set to this value then there is no keyboard manager window currently active that requires a hook
        Deactivated,
        // If set to this value then the detect key window is currently active and it requires a hook
        DetectSingleKeyRemapWindowActivated,
        // If set to this value then the detect shortcut window in edit keyboard window is currently active and it requires a hook
        DetectShortcutWindowInEditKeyboardWindowActivated,
        // If set to this value then the edit keyboard window is currently active and remaps should not be applied
        EditKeyboardWindowActivated,
        // If set to this value then the detect shortcut window is currently active and it requires a hook
        DetectShortcutWindowActivated,
        // If set to this value then the edit shortcuts window is currently active and remaps should not be applied
        EditShortcutsWindowActivated
    };
```
— [`KeyboardManagerEditorLibrary/KeyboardManagerState.h`, lines 30–45](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.h)

The foreground check is in `CheckUIState`, and its comment says exactly why:

```cpp
        // If the UI state is a detect window then we also have to ensure that the UI window is in focus.
        // GetForegroundWindow can be used here since we only need to check the main parent window and not the sub windows within the content dialog. Using GUIThreadInfo will give more specific sub-windows within the XAML window which is not needed.
        else if (currentUIWindow == GetForegroundWindow())
        {
            return true;
        }
```
— [`KeyboardManagerState.cpp`, lines 23–52](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.cpp)

and the two fallthrough branches immediately below encode the degradation: if the state is `DetectShortcutWindowActivated` but the window is *not* foreground, a query for `EditShortcutsWindowActivated` still returns true — i.e. **capture disarms on blur while suspension stays on.** The hook decision itself:

```cpp
Helpers::KeyboardHookDecision KeyboardManagerState::DetectShortcutUIBackend(LowlevelKeyboardEvent* data, bool isRemapKey)
{
    // Check if the detect shortcut UI window has been activated
    if ((!isRemapKey && CheckUIState(KeyboardManagerUIState::DetectShortcutWindowActivated)) || ...)
    {
        ...
        // Add the key if it is pressed down
        if (data->wParam == WM_KEYDOWN || data->wParam == WM_SYSKEYDOWN)
        {
            SelectDetectedShortcut(data->lParam->vkCode);
        }
        // Remove the key if it has been released
        else if (data->wParam == WM_KEYUP || data->wParam == WM_SYSKEYUP)
        {
            ResetDetectedShortcutKey(data->lParam->vkCode);
        }

        // Suppress the keyboard event
        return Helpers::KeyboardHookDecision::Suppress;
    }

    // If the detect shortcut UI window is not activated, then clear the shortcut buffer if it isn't empty
    ...
```
— [`KeyboardManagerState.cpp`, lines 379–421](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.cpp)

Note the second branch: leaving the capture window **clears the partially-built combination**, so a blur does not leave a half-captured shortcut behind.

**Why this captures an already-registered global hotkey.** The mechanism is not "unregister first" — it is hook precedence. A `WH_KEYBOARD_LL` procedure that returns a nonzero value swallows the event before the system's hotkey matching sees it, so no `WM_HOTKEY` is ever posted to whoever registered the combination. PowerToys' shared hook does exactly that:

```cpp
                    s_instance->keyboardEventCallback(ev);
                    return 1;
                }
            }
        }
        return CallNextHookEx(NULL, nCode, wParam, lParam);
```
— [`src/common/interop/KeyboardHook.cpp`, `HookProc`](https://github.com/microsoft/PowerToys/blob/main/src/common/interop/KeyboardHook.cpp)

The same file shows the multiplexing design: a single process-wide `SetWindowsHookEx(WH_KEYBOARD_LL, ...)` installed once, with a static `std::unordered_set<KeyboardHook*> instances`, iterated per event and dispatched to the first instance whose `isActiveCallback()` returns true. Every capture control in PowerToys Settings shares that one hook.

**Scope: modal dialog, not settings page.** In the legacy editor the capture surface is a `ContentDialog` — `ShortcutControl::CreateDetectShortcutWindow` builds `ContentDialog detectShortcutBox;`, and the enclosing button handler sets `keyboardManagerState->SetUIState(KBMEditor::KeyboardManagerUIState::DetectShortcutWindowActivated, editShortcutsWindowHandle);` immediately before opening it. On accept and on cancel the handlers call `keyboardManagerState.ResetUIState()` and then restore `EditShortcutsWindowActivated` / `EditKeyboardWindowActivated` ([`ShortcutControl.cpp`, lines 37–39, 84–86, 971–976, 1022–1031, 1092–1101](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/ShortcutControl.cpp)).

Because every key including `Enter` and `Escape` is suppressed during capture, the documented way out of the dialog is a **long press**: *"Once you select Select, a dialog window will open in which you can enter the key or shortcut, using your keyboard. Once you're satisfied with the output, hold Enter to continue. To leave the dialog, hold Esc."* ([Keyboard Manager docs, "How to select a key for remapping"](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager)). That is what `KeyDelay` and `HandleKeyDelayEvent` in `KeyboardManagerState` implement.

**The redesigned editor uses a different scope.** The WinUI 3 editor shipped as default for new installs in PowerToys 0.100 replaces the modal dialog with an explicit **record toggle** per field. `KeyboardHookHelper` is a singleton whose `ActivateHook(IKeyboardHookTarget target)` first calls `CleanupHook()` — so at most one field can be recording — and the toggle's `_Unchecked` handler tears the hook down:

```csharp
        public void ActivateHook(IKeyboardHookTarget target)
        {
            CleanupHook();
            _activeTarget = target;
            _currentlyPressedKeys.Clear();
            _keyPressOrder.Clear();
            ...
                _keyboardHook = new HotkeySettingsControlHook(KeyDown, KeyUp, () => true, (key, extraInfo) => true);
```
— [`KeyboardManagerEditorUI/Helpers/KeyboardHookHelper.cs`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorUI/Helpers/KeyboardHookHelper.cs); toggles at [`Controls/UnifiedMappingControl.xaml.cs`, `TriggerKeyToggleBtn_Checked` / `_Unchecked`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorUI/Controls/UnifiedMappingControl.xaml.cs)

Note the `() => true` for `isActive`: the new editor drops the foreground check and relies on the toggle plus process-lifetime engine suspension instead. It also enforces shape limits during capture rather than on save — at most four modifiers and five keys total, with `_activeTarget.OnInputLimitReached()` shown inline — and normalises modifier variants so only one of `LShift`/`RShift` is ever displayed.

### PowerToys Settings' "Activation shortcut" picker

This is the control every PowerToys module's settings page uses, and it answers the blur question most directly of all: it **destroys the hook on window deactivation.**

```csharp
        private void ShortcutDialog_SettingsWindow_Activated(object sender, WindowActivatedEventArgs args)
        {
            args.Handled = true;
            if (args.WindowActivationState != WindowActivationState.Deactivated && (hook == null || hook.GetDisposedState() == true))
            {
                // If the PT settings window gets focused/activated again, we enable the keyboard hook to catch the keyboard input.
                hook = new HotkeySettingsControlHook(Hotkey_KeyDown, Hotkey_KeyUp, Hotkey_IsActive, FilterAccessibleKeyboardEvents);
            }
            else if (args.WindowActivationState == WindowActivationState.Deactivated && hook != null && hook.GetDisposedState() == false)
            {
                // If the PT settings window lost focus/activation, we disable the keyboard hook to allow keyboard input on other windows.
                hook.Dispose();
                hook = null;
            }
        }
```
— [`src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs`, lines 793–807](https://github.com/microsoft/PowerToys/blob/main/src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs)

Within the window, capture is gated to the open dialog by a plain bool: the hook's `isActive` delegate is `Hotkey_IsActive() => _isActive`, `_isActive = true` is set at the end of `ShortcutDialog_Opened`, and `ShortcutDialog_Closing` sets `_isActive = false`. The control's `Loaded`/`Unloaded` handlers construct and dispose the hook because *"because of virtualization in e.g. a ListView, the control can go through several Loaded / Unloaded cycles."*

Three details in this file are worth stealing outright:

1. **Accessibility escape hatch.** `FilterAccessibleKeyboardEvents` returns `false` (do not capture) for `Tab` when no modifiers are involved, so keyboard users can still focus out of the control, and returns `false` whenever `FocusManager.GetFocusedElement(...)` is a `Button` — i.e. when the dialog's own Save/Cancel has focus. Escape maps to "clear and disable Save" rather than being captured as a binding.
2. **Modifier bookkeeping across the dialog boundary.** `ShortcutDialog_Opened` records which modifiers were already physically held via `GetAsyncKeyState` into `_modifierKeysOnEntering`, and when the user releases one of those the control synthesises a matching key-up to the system with a sentinel `dwExtraInfo` (`ignoreKeyEventFlag`) that its own hook then ignores — *"Any keyevent with the extraInfo set to this value will be ignored by the keyboard hook and sent to the system instead."* Without this, opening the dialog with Alt held leaves the OS believing Alt is still down after the dialog eats the key-up.
3. **Validation and conflict feedback happen per keystroke, not on save.** `Hotkey_KeyDown` clears `c.ConflictMessage`/`c.HasConflict`, disables the primary button while the combination is empty or a single key, and once a valid combination exists calls `CheckForConflicts(lastValidSettings)`. The conflict surfaces as a tooltip on the Edit button, a colour change on the key visual (`KeyVisualShouldShowConflict = !IgnoreConflict && HasConflict`), and an entry in a dedicated `ShortcutConflictWindow` — and there is an explicit `IgnoreConflict` opt-out (`HotkeyConflictIgnoreHelper.IsIgnoringConflicts(hotkeySettings)`), so the user can save a conflicting binding deliberately.

**Presenting a combination the OS will not allow: warn *before* the attempt.** Both PowerToys paths check as the user types, not after saving. The KBM editor runs `EditorHelpers::IsShortcutIllegal` inside `BufferValidationHelpers` while the shortcut is being assembled ([`BufferValidationHelpers.cpp`, line 320](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/BufferValidationHelpers.cpp)) and turns `WinL`/`CtrlAltDel` into a message. The Settings picker runs `CheckForConflicts` on every keydown. When a conflict is only *partially* blocking, PowerToys asks rather than refuses — `Dialog::PartialRemappingConfirmationDialog` is a two-button `ContentDialog` with `IDS_CONTINUE_BUTTON` / `IDS_CANCEL_BUTTON` ([`Dialog.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/Dialog.cpp)).

One limit is documented rather than enforced: `Win+L` and `Ctrl+Alt+Del` are handled below the low-level hook, so the guard is a table, not a probe. And even the probe cannot see them — `RegisterHotKey` will not report them as conflicts because they are not registered hotkeys at all.

### Config layers, and where a GUI edit should land

Two of the six systems have a layered config file model, and **neither has a GUI or a documented write target.**

- **i3** has `include <pattern>` (`state INCLUDE: pattern = string -> call cfg_include($pattern)`, [`parser-specs/config.spec`, lines 103–106](https://github.com/i3/i3/blob/next/parser-specs/config.spec)). The userguide states the semantics: *"i3 expands pattern using shell-like word expansion, specifically using the wordexp(3) C standard library function"*; *"Variables are shared between all config files, but beware of the following limitation: You can define a variable and use it within an included file. You cannot use (in the parent file) a variable that was defined within an included file."*; and *"Implementation-wise, i3 does not currently construct one big configuration from all include directives. Instead, i3's config file parser interprets all configuration directives in its parse_file() function... This means the evaluation order of files forms a tree, or one could say i3 uses depth-first traversal."* ([i3 userguide, "Include directive"](https://i3wm.org/docs/userguide.html)). Notably, `check_for_duplicate_bindings(context)` runs at the end of *each* `parse_file_inner`, so duplicate detection sees the accumulated binding list — a binding in an included file that collides with one in the parent is caught. There is no GUI and no config-writing command; `i3-msg` changes are runtime-only.
- **skhd** has `.load "<file>"`, *"treated as an absolutepath if the filename begins with '/' otherwise the file is relative to the path of the config-file it was loaded from"*, processed recursively by `parser_do_directives` ([skhd README; `src/parse.c`, lines 604–636](https://github.com/asmvik/skhd/blob/master/src/parse.c)). Loaded files are added to the hotloader's watch list so editing any of them triggers the same destructive reload described earlier. No GUI.
- **GlazeWM, AeroSpace, whkd, komorebi** have no include mechanism. GlazeWM reads one path (`~/.glzr/glazewm/config.yaml`, overridable by `--config` or `GLAZEWM_CONFIG_PATH`). AeroSpace searches two well-known locations and treats *finding both* as a fatal `ambiguousConfigError` rather than merging them ([`Sources/AppBundle/config/ConfigFile.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/ConfigFile.swift)).
- **PowerToys** has no layering either: each module owns one `settings.json`, and KBM owns a single remap JSON that the editor rewrites wholesale.

The nearest thing to an answer anywhere in the survey is GlazeWM's `wm-update-workspace-config`, which mutates config **in memory only and never writes the file** — so on the next reload the user's YAML wins and the runtime change is lost. That is a deliberate-looking choice (it makes runtime tweaking safe) but it is not a solution to the layered-write question.

**This is a genuine gap, not an oversight in the search.** No manager in this survey has both (a) a config that can be composed from multiple sources and (b) a UI or command that writes bindings back. Mosaix will be designing this without precedent.

## What this implies for Mosaix

**(a) Should the binding table key a closed command enum, or a parsed command string? Neither, exactly — key it on a small closed enum that carries a payload, and do not build a command-string parser.**

The field's unanimity on parsed strings is real but it is an artefact of a constraint Mosaix does not share. i3, GlazeWM, AeroSpace, whkd and skhd all needed *one* grammar to serve a CLI, an IPC surface, and a config file at once; the string is the wire format, and the config just reuses it. GlazeWM makes this explicit — `InvokeCommand` is a `clap::Parser` and the config deserializer literally fabricates an argv. Mosaix has no CLI and no IPC surface today. Adopting a command-string grammar would mean building and testing a parser purely to serve a config file that TOML already parses, and inheriting the failure class that comes with it: GlazeWM rejects the whole file on one typo, i3 defers the error to a nagbar on first press, whkd and skhd never validate at all.

What Mosaix must give up is the *unit-variant* enum, because `BTreeMap<Command, KeyCombo>` cannot express two bindings for the same verb with different arguments. The minimal change that preserves everything valuable is to make `Command` a data-carrying enum with an externally-tagged or adjacently-tagged serde representation, keeping the flat form for the fifteen existing unit variants and adding a table form for the parameterised ones — for example `apply-layout = { name = "writing", key = "ctrl+alt+w" }` alongside `snap-left = "ctrl+alt+left"`, or a separate `[hotkeys.apply-layout]` sub-table keyed by layout name. The exact spelling is a design decision; the invariant to keep is that **serde still rejects an unknown command at load, so a typo in the verb remains a config error that names the file and the line, and never becomes a silent no-op or a press-time dialog.** That property is strictly better than what any of the five managers achieves, and it is worth protecting.

The corollary is that Mosaix's existing duplicate detection must keep working across the change. Today `duplicate_binding` iterates `resolved.hotkeys` pairwise after the merge and reports both commands ([`crates/mosaix-config/src/validate.rs`](../../crates/mosaix-config/src/validate.rs)). Detection **after** overlay merge is the right scope and puts Mosaix ahead of GlazeWM (none, silent first-wins) and skhd (none, silent first-wins), level with AeroSpace (per-mode, normalised) and i3 (post-include, nagbar). Keep it, and make sure a parameterised command's identity participates in the comparison so `apply-layout("writing")` and `apply-layout("code")` are distinct commands but two bindings of `apply-layout("writing")` are still a duplicate.

**(b) Is there a proven pattern for binding to a user-named entity, and what should the field do when the name is missing? Yes, and Mosaix should take skhd's answer, not AeroSpace's or komorebi's.**

Three shipped behaviours exist and they are not equally good:

- **komorebi's silent no-op is the worst available option** and should be excluded outright. `if let Some(...) = self.monitor_workspace_index_by_name(name)` with no `else`, repeated across a dozen commands, means a mistyped layout name produces no window movement, no message, no log line, and a success exit code. This is the exact failure mode `CLAUDE.md`'s "prefer explicit failure over silent misbehaviour" rules out.
- **AeroSpace's create-on-demand works for workspaces because a workspace is definitionally empty until used.** A *saved layout* is not: `apply-layout = "wrting"` cannot conjure a layout, so there is nothing to create. Create-on-demand does not transfer.
- **GlazeWM's press-time error dialog is honest but late**, and its own history argues against it: GlazeWM has the same dangling-reference bug class twice (workspaces, binding modes) and handles both by popping a modal dialog while the user is mid-keystroke.
- **skhd's is the one that fits.** A skhd mode is declared in the same file the bindings live in, and referencing an undeclared one is `parser_report_error(parser, identifier, "undeclared identifier\n")` — a **load-time** error. A Mosaix saved layout is the same kind of entity: a name the user created, resolvable at config-validation time.

So: **a binding naming a layout that does not exist should be a validation error, reported by `mosaix-config`'s existing `ValidationError` machinery alongside `DuplicateBinding` and `DuplicateFingerprint`, naming the file, the binding, and the missing layout.** This requires that the set of layout names be knowable during validation — i.e. layouts must be declared in the config directory that `validate()` already walks, not discovered later from disk at runtime. If layouts end up living outside that walk, the fallback (in descending order of quality) is: a warning at load plus a visible failure at press time, GlazeWM-style; never komorebi's silence.

Two secondary rules follow from the same sources. **Validate the name itself at parse time, against a reserved list** — AeroSpace's `WorkspaceName.parse` rejects 21 reserved words plus empty, comma-containing, whitespace-containing, leading-`_` and leading-`-` names, and three of those rules exist purely because the name shares a namespace with a command grammar. Mosaix's TOML-keyed form has fewer such collisions, but empty, whitespace-only, and duplicate-modulo-case names should still be rejected where the name is defined. And **decide the precedence between an explicit declaration and a binding reference now**: AeroSpace shipped binding-derived `persistent-workspaces` in `config-version = 1` and moved to an explicit `persistent-workspaces = [...]` key in version 2. That migration is the field telling you which way it goes.

**(c) Is there a proven GUI key-capture design to copy, and does it suspend live registrations? Yes — PowerToys' — and yes, at two scopes. Copy both.**

There is nothing to learn from the tiling managers here: GlazeWM's tray offers "show config folder", komorebi's `komorebi-shortcuts` is a 98-line read-only list with a filter box, and AeroSpace has a tray menu. Mosaix would be the first tiling window manager in this survey to ship a GUI hotkey editor.

The design to copy has five parts, all of which map onto Mosaix's `RegisterHotKey` architecture more cleanly than onto PowerToys' hook architecture:

1. **A coarse suspension held for the editor's lifetime.** PowerToys signals a named event for as long as the editor window exists, and the engine checks it with a zero-timeout wait at the top of its hook, returning `0`. Mosaix's equivalent is stronger and simpler: because it uses `RegisterHotKey`, it can genuinely **unregister every binding while the editor window is open and re-register on close**, which is what `HotkeyRegistrations::stop()` plus a fresh `start_hotkeys()` already does on config reload. That is not a workaround — it is the same operation AeroSpace performs on every mode change and on every `reloadConfig` (`resetHotKeys()` then `activateMode_nonCancellable`), and the same one it performs to enable and disable individual bindings via `key.isEnabled`. Unregistering is also the only way capture can work at all for Mosaix, because Mosaix has no low-level hook to swallow the keystroke ahead of the OS; `RegisterHotKey` is arbitrated by the OS and there is no `return 1`.
2. **A fine-grained capture arm scoped to a modal dialog.** `SetUIState(DetectShortcutWindowActivated, hwnd)` on open, `ResetUIState()` on both accept and cancel. A capture that is armed for a whole settings page rather than one dialog is a page on which no key does anything.
3. **Disarm on blur, and clear the partial capture.** `currentUIWindow == GetForegroundWindow()` in `CheckUIState`, plus the `else` branch of `DetectShortcutUIBackend` that resets `detectedShortcut` when the capture window is not active. The Settings-side control goes further and disposes the hook entirely on `WindowActivationState.Deactivated`, rebuilding it on activation. For Mosaix, "disarm on blur" is about the capture buffer; the *registration* suspension should be tied to the editor window's lifetime, not its focus, so a user who alt-tabs away mid-edit does not suddenly have live hotkeys firing into another app.
4. **Modifier bookkeeping across the dialog boundary.** Snapshot `GetAsyncKeyState` for Shift/Ctrl/Alt/Win on dialog open into a `_modifierKeysOnEntering` set, and synthesise the matching key-up when one of those is released, tagged with a sentinel `dwExtraInfo` the capture path ignores. Without this the OS thinks a modifier is still held after the dialog eats its key-up. Mosaix needs the equivalent even without a hook, because `RegisterHotKey` capture will have to read key state directly.
5. **Warn before the attempt, not after, and allow an override.** Validate on every keystroke: the shape rules (`ShortcutStartWithModifier`, `ShortcutAtleast2Keys`, `ShortcutOneActionKey`) and the reserved pair (`WinL`, `CtrlAltDel`) inline, with the Save button disabled while the combination is incomplete. For OS ownership, use PowerToys' probe verbatim — `RegisterHotKey(nullptr, id, mods, vk)`, check `GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED`, `UnregisterHotKey` on success — and classify the result the way `HotkeyConflictManager` does, into "another Mosaix binding owns this" and "the system or another application owns this", with the documented fallback that an unexplained refusal is attributed to the system. Keep PowerToys' `IgnoreConflict` escape hatch: the user is allowed to save a conflicting binding on purpose.

For the reserved list specifically: **maintain a table for `Win+L` and `Ctrl+Alt+Del` only**, exactly as PowerToys does, and rely on the OS for everything else. Those two are the only combinations the probe cannot detect, because they are never registered hotkeys — they are handled below the hook and below `RegisterHotKey`. Add `F12` to a *warning* (not a block) on the strength of the Win32 documentation: *"The F12 key is reserved for use by the debugger at all times, so it should not be registered as a hot key."* Do not attempt to enumerate the Windows-key shortcuts; the docs say only that they are "reserved for use by the operating system", and komorebi's own guidance is to hand Win-key bindings to AutoHotkey rather than fight for them.

**(d) On config layers, there is no precedent and Mosaix must decide alone.** No manager in this survey has both a layered config and a UI that writes to it. i3 and skhd have includes but no GUI and no config-writing command; GlazeWM, AeroSpace, komorebi and PowerToys have a single file per concern. The one adjacent data point is GlazeWM's `wm-update-workspace-config`, which mutates config in memory and deliberately never writes the file — so runtime edits are lost on the next reload. That is a defensible position for a *runtime tweak* but not for a GUI whose entire purpose is persistence. Whatever Mosaix chooses, the choice must be visible in the UI at the moment of editing, because the resolved value the user sees and the file that would receive the write are different objects whenever a profile is active — and there is no shipped design anywhere in the field that makes that distinction for them.

## Sources

**GlazeWM**

- [`resources/assets/sample-config.yaml`](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml) — the `keybindings:` and `binding_modes:` sections; `commands:`/`bindings:` list shape; `focus --workspace 1`, `move --workspace 1`, `resize --width -2%`, `wm-enable-binding-mode --name resize`
- [`packages/wm-common/src/app_command.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/app_command.rs) — `InvokeCommand` as a `clap::Parser` enum; the hand-written `Deserialize` impl calling `try_parse_from` with a fabricated argv (lines 265–281); `InvokeFocusCommand.workspace: Option<String>` and `InvokeMoveCommand.workspace: Option<String>` (lines 310–383); `WmEnableBindingMode { name: String }`
- [`packages/wm-common/src/parsed_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/parsed_config.rs) — `KeybindingConfig { bindings, commands }`; `BindingModeConfig`; `deserialize_bindings` splitting on `+` and parsing each `Key`; `WorkspaceConfig { name, display_name, bind_to_monitor, keep_alive }`
- [`packages/wm-platform/src/keybinding_listener.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/keybinding_listener.rs) — `WH_KEYBOARD_LL` hook, no `RegisterHotKey`; `MODIFIER_GROUPS`; longest-match dispatch via `max_by_key`; `create_keybinding_map` bucketing duplicates without detection; `update()` and `enable()`
- [`packages/wm/src/wm.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/wm.rs) — `process_event`'s `PlatformEvent::Keybinding` arm resolving a fired keybinding back to commands with `.find()` (first-wins)
- [`packages/wm/src/user_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/user_config.rs) — `read()` doing `serde_yaml::from_str(&config_str)?` (whole-file rejection) with the error-formatting TODO; `reload()` reading before assigning; `active_keybinding_configs` selecting the binding-mode table and the pause interlock returning only `WmTogglePause` configs
- [`packages/wm/src/main.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/main.rs) — fatal `show_error_dialog("Fatal error", ...)` on startup config failure; the `tokio::select!` loop's tail `show_error_dialog("Non-fatal error", ...)`; `keybinding_listener.update(...)` on `UserConfigChanged` / `BindingModesChanged` / `PauseChanged`
- [`packages/wm/src/commands/workspace/activate_workspace.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/workspace/activate_workspace.rs) — `workspace_config()` and the `"Workspace with name '{workspace_name}' doesn't exist or is already active."` context
- [`packages/wm/src/commands/workspace/focus_workspace.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/workspace/focus_workspace.rs) — `WorkspaceTarget::Name(String)` resolution and the activate-then-focus fallback
- [`packages/wm/src/commands/general/enable_binding_mode.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/enable_binding_mode.rs) — `"No binding mode found with the name '{name}'."`
- [`packages/wm/src/commands/general/reload_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/reload_config.rs) — `config.reload()?` as the first step, so a failed parse leaves the old config in effect
- [`packages/wm/src/commands/workspace/update_workspace_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/workspace/update_workspace_config.rs) — the runtime config mutation that is in-memory only, with no file write
- [`packages/wm/src/sys_tray.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/sys_tray.rs) — the complete tray menu: `ReloadConfig`, `ShowConfigFolder`, `ToggleWindowAnimations`, `RunOnStartup`, `Exit`

**AeroSpace**

- [`docs/config-examples/default-config.toml`](https://github.com/nikitabobko/AeroSpace/blob/main/docs/config-examples/default-config.toml) — `[mode.main.binding]` / `[mode.service.binding]`; `alt-1 = 'workspace 1'` and `alt-a = 'workspace A'`; command arrays; the key and modifier vocabulary; `key-mapping.preset`; `persistent-workspaces`; `on-mode-changed`
- [`Sources/AppBundle/config/HotkeyBinding.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/HotkeyBinding.swift) — `resetHotKeys()`; `activateMode_nonCancellable` creating `HotKey` objects lazily and toggling `isEnabled` per mode; `parseBindings` emitting `"'<combo>' Binding redeclaration"` keyed on `descriptionWithKeyCode`; `parseBinding` splitting on `-`
- [`Sources/AppBundle/config/Mode.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/Mode.swift) — `parseModes` requiring a `main` mode and rejecting unknown keys inside a mode table
- [`Sources/AppBundle/config/parseConfig.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/parseConfig.swift) — `ParseConfigResult { config, errors, warnings }` and `allowReloadConfig`; `preventConfigReload` reserved for TOML/IO failures; `parseShellOfCommandsForConfig` substituting `.empty` on a bad command; the `config-version = 1` derivation of `persistentWorkspaces` from binding commands (lines 280–288)
- [`Sources/AppBundle/command/parseCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/command/parseCommand.swift) — `lexAndParseShell()` → `parseCmdArgs` → `toCommand()`
- [`Sources/AppBundle/command/impl/ReloadConfigCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/command/impl/ReloadConfigCommand.swift) — the GUI message window on errors/warnings; `"Failed to parse '<path>'. N error(s). M warning(s)"`; `resetHotKeys()` before swapping in the new config
- [`Sources/AppBundle/command/impl/WorkspaceCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/command/impl/WorkspaceCommand.swift) — no existence check; the only failure is "already focused"
- [`Sources/AppBundle/tree/Workspace.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/tree/Workspace.swift) — `Workspace.get(byName:)` inserting on miss
- [`Sources/Common/model/WorkspaceName.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/Common/model/WorkspaceName.swift) — the 21-word reserved list plus empty/comma/underscore/dash/whitespace rules
- [`Sources/AppBundle/config/ConfigFile.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/config/ConfigFile.swift) — two candidate paths, `ambiguousConfigError` when both exist, no include mechanism
- [`Sources/AppBundle/ui/TrayMenuModel.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/ui/TrayMenuModel.swift) — `updateTrayText()` prefixing the menu-bar text with the uppercased active mode; `lastReloadConfigContainedWarnings`
- [`Package.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Package.swift) — `soffes/HotKey` pinned at `exact: "0.2.1"`
- [`soffes/HotKey`, `Sources/HotKey/HotKeysController.swift`](https://github.com/soffes/HotKey/blob/master/Sources/HotKey/HotKeysController.swift) — `RegisterEventHotKey` with `guard registerError == noErr, eventHotKey != nil else { return }`, i.e. a silently swallowed registration failure
- [AeroSpace guide](https://nikitabobko.github.io/AeroSpace/guide) — "Binding modes" (*"When you switch to a different binding mode, all the bindings from the current mode are deactivated..."*) and the config-location statement *"If the config is found in more than one location then the ambiguity is reported."*

**komorebi and whkd**

- [`docs/installation.md`](https://github.com/LGUG2Z/komorebi/blob/master/docs/installation.md) — *"neither `komorebi.exe` nor `komorebic.exe` handle key bindings, because `komorebi` is a tiling window manager and not a hotkey daemon"*; the whkd Windows-key caveat and the AutoHotKey recommendation
- [`docs/whkdrc.sample`](https://github.com/LGUG2Z/komorebi/blob/master/docs/whkdrc.sample) — `.shell powershell`; `alt + h : komorebic focus left`; `alt + 1 : komorebic focus-workspace 0`; the `-WindowStyle hidden` reload line
- [`komorebi/src/process_command.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/process_command.rs) — `FocusNamedWorkspace` (line 1269), `SendContainerToNamedWorkspace` (850), `MoveContainerToNamedWorkspace` (863), `NamedWorkspaceLayoutCustom` (1023), `NamedWorkspaceTiling` (1030), `NamedWorkspaceLayout` (1037) — every one an `if let Some(...)` with no `else`; `EnsureNamedWorkspaces` (1399)
- [`komorebi/src/core/mod.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/core/mod.rs) — the `SocketMessage` variants carrying named-workspace strings
- [`komorebic/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebic/src/main.rs) — `NamedWorkspaceCustomLayout { workspace: String, path: PathBuf }`, i.e. layouts identified by file path rather than name
- [`komorebi-shortcuts/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi-shortcuts/src/main.rs) — the entire read-only `Quicklook` viewer; `whkd_parser::load(&home).ok()`
- [`komorebi-gui/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi-gui/src/main.rs) — a live state/border/stackbar panel over `SocketMessage`, with no binding surface
- [whkd README](https://github.com/LGUG2Z/whkd/blob/master/README.md) — the whkdrc example, `.shell` / `.pause` / `.pause_hook`, the per-application `[ Default : ... / Ignore ]` block, and the statement that the format is "heavily inspired by `skhd` and `sxhkd`"
- [`parser/src/lib.rs`](https://github.com/LGUG2Z/whkd/blob/master/parser/src/lib.rs) — the chumsky grammar; `command = take_until(...)` treating the command as opaque; `.map_err(|_error| WhkdError::Parse(path.clone()))` discarding the parse error
- [`src/main.rs`](https://github.com/LGUG2Z/whkd/blob/master/src/main.rs) — `HkmData::register` writing the command to a persistent shell stdin; `"Unable to bind ... ignoring this binding and continuing..."` on stderr; the `panic!` on a failed whkdrc load
- [`iholston/win-hotkeys`, `src/manager.rs` and `src/hook.rs` and `src/error.rs`](https://github.com/iholston/win-hotkeys/blob/main/src/manager.rs) — in-process duplicate check returning `RegistrationFailed`; `SetWindowsHookExW(WH_KEYBOARD_LL, ...)`; the `"Hotkey registration failed. Hotkey is already in use."` message

**i3**

- [`etc/config`](https://github.com/i3/i3/blob/next/etc/config) — `bindsym Mod1+1 workspace number $ws1`; `move container to workspace number $ws1`; the full `mode "resize" { ... }` block with three exits
- [`parser-specs/config.spec`](https://github.com/i3/i3/blob/next/parser-specs/config.spec) — `state BINDING` and `state BINDCOMMAND` with `command = string` (no command validation at load); `state MODENAME` / `MODEBRACE` / `MODE`; `state INCLUDE: pattern = string -> call cfg_include($pattern)`
- [`src/bindings.c`](https://github.com/i3/i3/blob/next/src/bindings.c) — the in-translation duplicate check (lines 599–614); `binding_same_key` (747–774) and `check_for_duplicate_bindings` (784–806) setting `has_errors`; `run_binding` spawning i3-nagbar with *"The configured command for this shortcut could not be run successfully."* (859–900)
- [`src/config_parser.c`](https://github.com/i3/i3/blob/next/src/config_parser.c) — per-line error reporting with an underlined position and *"Skip the rest of this line, but continue parsing."*; `start_config_error_nagbar` with the "edit config" action; `parse_file_inner` calling `check_for_duplicate_bindings` after every file
- [`src/commands_parser.c`](https://github.com/i3/i3/blob/next/src/commands_parser.c) — `result->parse_error = true` and *"i3-nagbar is spawned upon keypresses only for parser errors."*
- [`src/key_press.c`](https://github.com/i3/i3/blob/next/src/key_press.c) — `get_binding_from_xcb_event` then `run_binding`
- [i3 userguide](https://i3wm.org/docs/userguide.html) — "Include directive" (`wordexp(3)`, variable-scoping limitation, depth-first traversal); "Binding modes"; *"If the workspace does not exist yet, it will be created."*

**yabai and skhd**

- [yabai README](https://github.com/asmvik/yabai/blob/master/README.md) — yabai has no binding layer; *"Keyboard shortcuts can be defined with skhd or any other suitable software you may prefer."* (repository moved from `koekeishiya/yabai`)
- [skhd README](https://github.com/asmvik/skhd/blob/master/README.md) — the full hotkey grammar; the mode declaration grammar including `@`; `.load` and `.blacklist` (repository moved from `koekeishiya/skhd`)
- [`src/parse.c`](https://github.com/asmvik/skhd/blob/master/src/parse.c) — `"undeclared identifier"` for an unknown mode (lines 129 and 248); `parse_config` fail-fast with `free_mode_map` on error; `parser_report_error` writing `#line:col message` to stderr; `parser_do_directives` recursing through `.load`
- [`src/skhd.c`](https://github.com/asmvik/skhd/blob/master/src/skhd.c) — `config_handler` freeing the live mode map *before* re-parsing, so a bad edit leaves no bindings
- [`src/hashtable.h`](https://github.com/asmvik/skhd/blob/master/src/hashtable.h) — `table_add` preserving an existing non-null value, i.e. silent first-wins on duplicate bindings

**Microsoft PowerToys and Win32**

- [`src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.h) — the six-variant `KeyboardManagerUIState` with per-variant comments; `KeyboardHookDecision { ContinueExec, Suppress, SkipHook }`; `AllowChord`; `CheckUIState`'s documented focus requirement
- [`KeyboardManagerState.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/KeyboardManagerState.cpp) — `CheckUIState` with `currentUIWindow == GetForegroundWindow()` and the two "not in focus" fallthroughs (lines 23–52); `DetectShortcutUIBackend` suppressing every key while armed and clearing the buffer when not (379–421)
- [`KeyboardManagerEditorLibrary/ShortcutControl.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/ShortcutControl.cpp) — `SetUIState(DetectShortcutWindowActivated, ...)` before opening the `ContentDialog`; `ResetUIState()` then restoring the parent window state on accept and cancel
- [`KeyboardManagerEditorLibrary/EditShortcutsWindow.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditShortcutsWindow.cpp) and [`EditKeyboardWindow.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditKeyboardWindow.cpp) — `EventLocker::Get(EditorWindowEventName)` and the log line *"Signaled ... event to suspend the KBM engine"*
- [`KeyboardManagerEngineLibrary/KeyboardManager.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEngineLibrary/KeyboardManager.cpp) — `editorIsRunningEvent` created from `KeyboardManagerConstants::EditorWindowEventName`; the `WaitForSingleObject(..., 0) == WAIT_OBJECT_0 → return 0` guard at the top of both the keyboard and mouse hook handlers
- [`common/KeyboardManagerConstants.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/common/KeyboardManagerConstants.h) — `EditorWindowEventName = L"PowerToys_KeyboardManager_Event_EditorWindow"`
- [`KeyboardManagerEditorLibrary/EditorHelpers.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/EditorHelpers.cpp) — `IsShortcutIllegal` with the hardcoded `Win+L` and `Ctrl+Alt+Del` checks (lines 138–155)
- [`KeyboardManagerEditorLibrary/ShortcutErrorType.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/ShortcutErrorType.h) — the twenty typed validation errors including `WinL` and `CtrlAltDel`
- [`KeyboardManagerEditorLibrary/BufferValidationHelpers.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/BufferValidationHelpers.cpp) — `IsShortcutIllegal` invoked during buffer validation, not on save
- [`KeyboardManagerEditorLibrary/Dialog.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorLibrary/Dialog.cpp) — `PartialRemappingConfirmationDialog`, a Continue/Cancel `ContentDialog`
- [`KeyboardManagerEditorUI/Helpers/KeyboardHookHelper.cs`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorUI/Helpers/KeyboardHookHelper.cs) — the redesigned editor's singleton hook; `ActivateHook` calling `CleanupHook` first; the 4-modifier/5-key limit with `OnInputLimitReached()`; chord handling capped at two action keys; modifier-variant normalisation
- [`KeyboardManagerEditorUI/Controls/UnifiedMappingControl.xaml.cs`](https://github.com/microsoft/PowerToys/blob/main/src/modules/keyboardmanager/KeyboardManagerEditorUI/Controls/UnifiedMappingControl.xaml.cs) — record-toggle arming instead of a modal dialog; `SetDropDownsEnabled(..., false)` during recording
- [`src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs`](https://github.com/microsoft/PowerToys/blob/main/src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs) — `ShortcutDialog_SettingsWindow_Activated` disposing and rebuilding the hook on window blur/focus (lines 793–807); `_isActive` gating via `Hotkey_IsActive`; `_modifierKeysOnEntering` snapshot from `GetAsyncKeyState` and the `ignoreKeyEventFlag` synthetic key-ups; `FilterAccessibleKeyboardEvents` letting Tab/Shift+Tab and focused Buttons through; per-keystroke `ComboIsValid` / `CheckForConflicts`; `IgnoreConflict`
- [`src/common/ManagedCommon/HotkeySettingsControlHook.cs`](https://github.com/microsoft/PowerToys/blob/main/src/common/ManagedCommon/HotkeySettingsControlHook.cs) — the shared capture hook wrapper used by every PowerToys "Activation shortcut" picker
- [`src/common/interop/KeyboardHook.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/common/interop/KeyboardHook.cpp) — one process-wide `SetWindowsHookEx(WH_KEYBOARD_LL, ...)` multiplexed over a static instance set; `return 1` to suppress the event, which is what lets capture take an already-registered global hotkey
- [`src/runner/hotkey_conflict_detector.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/runner/hotkey_conflict_detector.cpp) — `HotkeyConflictType::{NoConflict, InAppConflict, SystemConflict}`; `HasConflictWithSystemHotkey` probing with `RegisterHotKey(nullptr, ...)` and `ERROR_HOTKEY_ALREADY_REGISTERED` then unregistering; `GetAllConflicts`' documented fallback *"a system-level conflict is the only remaining explanation"*
- [PowerToys Keyboard Manager documentation](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager) — *"⊞ Win+L and Ctrl+Alt+Del cannot be remapped as they are reserved by the Windows OS."*; the shortcut shape rules; "Shortcuts with chords" and the "Allow chords" switch; *"hold Enter to continue. To leave the dialog, hold Esc."*; the "Use the new editor" toggle and PowerToys 0.100 default
- [RegisterHotKey function (winuser.h)](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) — *"Keyboard shortcuts that involve the WINDOWS key are reserved for use by the operating system."*; *"Typically, RegisterHotKey also fails if the keystrokes specified for the hot key have already been registered for another hot key. However, some pre-existing, default hotkeys registered by the OS ... may be overridden ..."*; *"The F12 key is reserved for use by the debugger at all times, so it should not be registered as a hot key."*

**Mosaix (for the comparison row only)**

- [`crates/mosaix-config/src/schema.rs`](../../crates/mosaix-config/src/schema.rs) — the 16-variant unit `Command` enum; `KeyCombo` with its uppercase canonicalisation comment ("duplicate-binding detection depends on this"); `hotkeys: BTreeMap<Command, KeyCombo>` on `BaseConfig`, `ProfileConfig` and `ResolvedConfig`
- [`crates/mosaix-config/src/validate.rs`](../../crates/mosaix-config/src/validate.rs) — `ValidationError::DuplicateBinding` naming both commands; `duplicate_binding` run against each resolved config after overlay merge
- [`crates/mosaix-platform-windows/src/hotkeys.rs`](../../crates/mosaix-platform-windows/src/hotkeys.rs) — per-binding `HotkeyRegistrationResult`; *"Registration is per-binding partial-success, not all-or-nothing"*; `HotkeyRegistrations::stop()` unregistering and joining the message-pump thread
