# Zed Terminal App Plan

## Goal

Build a standalone terminal application similar to Windows Terminal while reusing Zed's existing terminal stack:

- `crates/terminal` for PTY, terminal state, ANSI parsing, search, selection, and shell spawning.
- `crates/terminal_view` for GPUI rendering, input handling, terminal tab item behavior, context menus, and terminal settings integration.
- `crates/workspace` / `crates/ui` for pane tabs, split panes, focus management, key dispatch, and shared Zed component styling.

The implementation should make the smallest practical change to existing Zed code. The preferred shape is a new crate that composes existing public APIs. Existing crates should only be changed when an API must be exposed for reuse.

## Proposed Crate

Add a new binary crate:

```text
crates/zed_terminal/
  Cargo.toml
  src/main.rs
```

Workspace member:

```toml
"crates/zed_terminal",
```

Default run target can stay `crates/zed`; this app is launched explicitly:

```sh
cargo run -p zed_terminal
```

## Architecture

```text
zed_terminal binary
  |
  | initializes minimal app services
  v
GPUI Application
  |
  | owns a single app window
  v
Workspace
  |
  | center pane is used as the terminal tab strip
  v
Pane
  |
  | each tab item is a TerminalView
  v
TerminalView
  |
  | owns Entity<Terminal>
  v
Terminal
  |
  | PTY/shell via project::Project terminal creation APIs
  v
system shell / configured shell
```

## Visual Parity With Zed

The standalone app must not treat `terminal_view` as a self-contained renderer. The official Zed terminal depends on app-level initialization performed by the Zed binary:

- `gpui::Application::with_assets(Assets)` installs the same embedded asset source used by Zed.
- `Assets::load_fonts(cx)` registers embedded fonts, including `.ZedMono` and `.ZedSans`.
- `theme_settings::init(theme::LoadThemes::All(Box::new(Assets)), cx)` loads bundled themes and icon themes.
- user themes from `paths::themes_dir()` should be loaded and watched so custom themes behave like Zed.
- `SettingsStore::watch_settings_files(...)` must load the user's `settings.json` before the first terminal window is rendered.
- window background appearance and text rendering mode must be synchronized from `WorkspaceSettings` and the active theme.

If these steps are skipped, settings still reference `.ZedMono` and `.ZedSans`, but GPUI cannot resolve those embedded font aliases. The terminal then falls back to a platform font with different glyph metrics, so `terminal_element` computes a different `cell_width` from the measured width of `m`. That directly causes wide-looking glyph spacing and broken table alignment. Similarly, loading only `LoadThemes::JustBase` bypasses Zed's bundled/user theme choices, so the terminal background and foreground colors differ from the official terminal.

The app should therefore match Zed's initialization context first, and only then compose `Workspace`, `TerminalPanel`, and `TerminalView`.

## Initialization

The new app should follow a lightweight subset of `crates/zed/src/main.rs` instead of copying the full editor startup. Resource, font, settings, and theme initialization must still match Zed because terminal rendering depends on them.

Required initialization:

- `zlog::init()` / `env_logger` fallback for logging.
- `gpui_platform::application().with_assets(assets::Assets).run(...)`.
- `component::init()`.
- `menu::init()` and `zed_actions::init()` so actions are registered.
- `release_channel::init(...)`.
- `settings::init(cx)` and `SettingsStore::watch_settings_files(...)`.
- `Assets::load_fonts(cx)` before creating terminal views, so `.ZedMono` / `.ZedSans` match Zed.
- `theme_settings::init(theme::LoadThemes::All(Box::new(Assets)), cx)`.
- Load and watch user themes from `paths::themes_dir()`.
- Apply `WorkspaceSettings::text_rendering_mode` and active theme window background appearance on startup and when settings change.
- `client::init(...)`, `Project::init(...)`.
- `workspace::init(app_state.clone(), cx)`.
- `editor::init(cx)` because `terminal_view` uses `Editor` for rename/edit UI and shared editor settings.
- `terminal_view::init(cx)`.

Then create:

- `RealFs`.
- `LanguageRegistry`.
- `Client`.
- `UserStore`.
- `WorkspaceStore`.
- `AppSession`.
- `NodeRuntime`.
- `AppState`.
- a local empty `Project`.
- one `Workspace`.
- one initial terminal via `TerminalPanel::add_center_terminal(...)`.

## Tab Model

Use `Workspace` center pane tabs for v1.

Benefits:

- Existing tab strip, close buttons, tab switching, context menus, split pane handling, drag/drop behavior, and focus behavior are reused.
- `TerminalView` already implements `workspace::Item`.
- `TerminalPanel::add_center_terminal` already creates a `TerminalView` and inserts it into the active pane.

Initial mappings:

- New tab: dispatch `workspace::NewTerminal`.
- Close tab: dispatch `pane::CloseActiveItem`.
- Next tab: dispatch `pane::ActivateNextItem`.
- Previous tab: dispatch `pane::ActivatePreviousItem`.
- Split right/down: dispatch `pane::SplitRight` / `pane::SplitDown`.

Windows Terminal style default keybindings can be added in the app:

```text
ctrl-shift-t       workspace::NewTerminal
ctrl-shift-w       pane::CloseActiveItem
ctrl-tab           pane::ActivateNextItem
ctrl-shift-tab     pane::ActivatePreviousItem
ctrl-shift-5       pane::SplitRight
alt-shift-plus     pane::SplitDown
ctrl-,             zed::OpenSettingsFile
```

Platform-specific Zed keymaps should still be loaded so existing terminal/editor key dispatch keeps working.

## Settings

V1 should reuse Zed's existing `settings.json` and `terminal` settings. This immediately enables:

- shell/profile command through `terminal.shell`;
- working directory through `terminal.working_directory`;
- font family/size/weight/features;
- cursor shape/blinking;
- scrollback;
- copy-on-select;
- environment variables;
- path hyperlink regexes;
- terminal bell behavior.

For the standalone app, the simplest settings UX is:

- `zed::OpenSettingsFile`: ensure `settings.json` exists, then open it with the system default app.
- `zed::OpenSettings`: initially alias to `OpenSettingsFile`.

Later, a dedicated settings UI can be added inside `zed_terminal` without pulling the full Zed settings UI:

- profiles list;
- default profile;
- shell command + arguments;
- starting directory;
- font controls;
- theme selection;
- keybinding editor subset.

Open decision: whether the standalone app should share Zed's config directory or call `paths::set_custom_data_dir(...)` early and use a separate `Zed Terminal` config/data root. Sharing Zed config minimizes changes and maximizes reuse; a separate root avoids surprising users who run both apps.

## Profiles

Windows Terminal's profile model should be layered on top of existing Zed terminal settings rather than replacing them.

V1:

- one default profile backed by existing `terminal.shell`;
- new tabs open that default profile.

V2:

- add a new `zed_terminal` setting section in `settings_content` only if necessary;
- define profile records:

```json
{
  "zed_terminal": {
    "default_profile": "PowerShell",
    "profiles": [
      {
        "name": "PowerShell",
        "shell": {
          "program": "pwsh",
          "args": ["-NoLogo"]
        },
        "working_directory": "~"
      }
    ]
  }
}
```

Until profile settings exist, profile selection can be deferred. The core tabbed terminal app does not require a new settings schema.

## Existing Code Changes

Target: no behavioral changes to existing Zed.

Allowed small changes if compilation requires them:

- expose a constructor/helper already used internally by `terminal_view`;
- expose terminal tab creation if `TerminalPanel::add_center_terminal` becomes insufficient;
- add a workspace dependency entry for the new crate.

Avoid:

- copying `crates/zed/src/main.rs`;
- modifying `Terminal` internals;
- forking `TerminalView`;
- pulling full AI/collab/debugger/editor feature initialization into the terminal app.

## Implementation Phases

1. Minimal app skeleton
   - new `zed_terminal` crate;
   - opens a GPUI window;
   - creates empty local project/workspace;
   - creates one terminal tab;
   - loads settings and keymaps.

2. App polish
   - title bar/menu text and a standalone window title instead of Workspace's empty-project title;
   - Windows Terminal style keybindings;
   - better empty/error state if shell spawn fails;
   - settings file action.

3. Dedicated settings UX
   - lightweight settings view for terminal-only settings;
   - profile editor;
   - import/export profile JSON if needed.

4. Optional separation
   - decide whether to share Zed config or use a separate app data dir;
   - app icon/name/build packaging.

## Verification

For each implementation stage:

```sh
cargo check -p zed_terminal
cargo run -p zed_terminal
```

Manual verification:

- first terminal spawns;
- `ctrl-shift-t` opens another tab;
- `ctrl-tab` / `ctrl-shift-tab` switch tabs;
- `ctrl-shift-w` closes the active tab;
- split actions still work;
- changes to `settings.json` update terminal behavior after reload or file watch event.
