# Zed Terminal Session Restoration Requirements

## Background

Warp Terminal has a feature named Session Restoration. Its documentation describes restoring the previous session's windows, tabs, panes, and the last few Blocks in each pane when the app starts again. Warp stores that previous-session data locally in SQLite, overwrites it with the latest session when the app quits, allows the feature to be disabled, and documents how users can clear the saved database.

References:

- Warp Session Restoration: https://docs.warp.dev/terminal/sessions/session-restoration
- Warp Blocks: https://docs.warp.dev/terminal/blocks

This document describes the equivalent product requirement for standalone `zed-terminal`.

## Goal

When `zed-terminal` is launched normally, it should be able to restore the user's previous terminal workspace so they can continue from the last visible state instead of rebuilding tabs, panes, directories, and context manually.

The restored workspace should include:

- Window count and window-level state where practical.
- Tab order and active tab.
- Split pane tree inside each tab.
- Active pane inside each tab.
- Each pane's startup identity: profile, shell, working directory, title, and command metadata.
- Terminal scrollback or buffer contents from the previous run.
- Enough visual state to make the restored pane recognizable immediately after launch.

## Product Semantics

Session restoration must be honest about process lifetime. After the app exits, the original shell, command, SSH session, editor, or long-running process is usually gone unless a separate persistent PTY/session backend exists.

For the first implementation, restoration should mean:

- Restore the previous layout and visible terminal history.
- Start a fresh shell or configured startup command for each restored pane.
- Preserve the previous buffer as restored history, not as evidence that the original process is still alive.
- Visually or textually separate restored historical content from new live output.

It must not imply:

- Reattaching to the original shell process.
- Resuming `vim`, `top`, `ssh`, `cargo watch`, or other interactive processes as live processes.
- Replaying user input automatically.

## User-Facing Behavior

Default startup behavior:

- If session restoration is enabled and a valid saved session exists, restore it.
- If no saved session exists, fall back to existing startup config behavior.
- If explicit launch arguments are supplied, those arguments should take precedence over restoration unless the user explicitly requests restoration.

Suggested controls:

- Setting: `restore_previous_session` (implemented for layout restore)
- Setting: `restore_terminal_buffer` (implemented as an explicit false-only field for now)
- Setting: `restore_buffer_line_limit`
- Action: `Restore Previous Session`
- Action: `Clear Saved Session`
- Action: `Open Session Restoration Settings`
- Optional CLI: `--restore-session` (implemented for layout restore)
- Optional CLI: `--no-restore-session` (implemented for layout restore)
- Optional CLI: `--clear-restored-session`

Suggested default:

- Enable layout restoration by default once storage and privacy controls are in place. The standalone app currently defaults `restore_previous_session` to `true` for layout-only restoration.
- Consider making buffer restoration opt-in if saved output may contain secrets.

## What To Restore

Minimum viable restore data:

- Saved timestamp.
- App/build version.
- Windows.
- Tabs per window.
- Pane split tree per tab.
- Active window, active tab, and active pane.
- Per pane:
  - profile name, if launched from a profile
  - shell program and args
  - command, if command-backed
  - working directory
  - custom title
  - scrollback text snapshot
  - cursor position or viewport-following mode if available

Nice-to-have restore data:

- ANSI attributes for restored buffer text.
- Search state.
- Scroll position.
- Tab colors/icons if configured.
- Pane sizes and flex ratios.
- Last exit/status metadata for command-backed panes.

## What Not To Restore Initially

Do not include these in the MVP:

- Live process resurrection.
- PTY state reattachment.
- Alternate-screen app state as an active application.
- Hidden environment variable values.
- Raw shell history beyond what is already visible in the terminal buffer.
- Secrets redaction inside buffer snapshots, unless a dedicated redaction mechanism is designed.

Alternate-screen programs need explicit handling. If a user quits while `vim`, `less`, `top`, or a full-screen TUI is active, MVP restoration should save a readable snapshot or omit that pane's buffer with a clear reason. It should not relaunch the TUI automatically.

## Storage Requirements

The saved session should live under the standalone terminal data directory, not inside the source tree.

Candidate files:

- `session.json` for layout metadata.
- `session.sqlite` if buffer snapshots need indexed storage.
- `session-buffers/` if buffer chunks are stored separately.

The storage format should support:

- Atomic writes.
- Versioning/migrations.
- Corruption recovery.
- Size limits.
- Clearing all saved session data.
- Excluding session data from support bundles by default.

## Privacy And Safety

Terminal buffers often contain secrets: tokens, command output, private paths, environment dumps, logs, and customer data.

Required controls:

- A clear setting to disable all session restoration.
- A separate setting to disable buffer restoration while keeping layout restoration.
- A command/action to clear saved session data.
- Documentation of the storage path.
- Support bundles must not include saved terminal buffers by default.
- Diagnostics should report only metadata such as enabled state, saved session age, window/tab/pane counts, and storage size.

## Current Zed Terminal Context

Relevant current implementation details:

- `terminal_view` already has persistence for terminal items, working directory, custom title, and pane layout metadata.
- Existing terminal persistence does not appear to persist raw terminal buffer or scrollback contents.
- Standalone `zed_terminal` recently moved toward `TerminalTab` owning panes, which is a useful foundation for restoring per-tab pane trees.
- Support bundle code currently avoids terminal buffer contents, which aligns with the privacy requirement.

Implication:

- Layout/session metadata restoration can build on existing pane and item serialization patterns.
- Buffer restoration requires a new snapshot API from the terminal/terminal view layer plus storage and privacy controls.

## Proposed MVP

Phase 0: Storage and privacy foundation

- Document saved session paths through `zed-terminal --paths`.
- Add a metadata-only session report that can be opened in-app or printed with `--session-report`.
- Add a confirmed in-app clear action and `--clear-saved-session` CLI.
- Keep support bundles limited to session path/file metadata; do not include raw saved session or terminal buffer contents.
- Do not enable actual layout or buffer restoration until settings, persistence format, and corruption behavior are designed and tested.

Phase 1: Layout-only restoration

- Current status as of 2026-06-08: the first layout-only slice is implemented for the standalone `zed_terminal` app.
- Implemented: valid `data/session/session.json` files restore top-level tabs, per-tab pane layout, active tab, active pane, working directory, and custom title metadata on normal launch.
- Implemented: `terminal.json` controls automatic layout restore with `restore_previous_session`; old configs remain compatible because the default is `true`.
- Implemented: `restore_terminal_buffer` exists in `terminal.json` and the schema, but the only supported value is `false`; `true` is rejected until buffer restoration exists.
- Implemented: explicit launch arguments bypass automatic restoration and fall back to the requested startup behavior unless `--restore-session` is supplied.
- Implemented: `--restore-session` forces layout restore for the current launch, and `--no-restore-session` disables layout restore for the current launch.
- Implemented: real-window smoke and release-check coverage verify creating tabs/panes with keyboard shortcuts, saving the session file, relaunching, restoring the tab-first hierarchy, disabling restore from config, and forcing restore from CLI.
- Not yet implemented in this phase: profile identity, explicit shell identity, command-backed pane identity, buffer contents, and an in-app Restore Previous Session action/settings UI.
- Persist windows, tabs, panes, active selection, profile/shell/cwd/title metadata.
- Restore fresh shells into the previous layout.
- Add settings and clear command.
- Add diagnostics that report session metadata only.

Phase 2: Text buffer restoration

- Persist bounded scrollback text per pane.
- Restore text as historical content separated from live shell output.
- Enforce line and byte limits.
- Add opt-in or explicit setting for buffer restoration.

Phase 3: Rich buffer restoration

- Preserve ANSI styling and basic screen state.
- Improve alternate-screen handling.
- Restore scroll position.
- Add package/release checks and visual smoke coverage.

## Acceptance Criteria

Layout restoration:

- Open `zed-terminal`, create multiple tabs and split panes, change active tab and pane, quit, reopen.
- The same tab order, split layout, active tab, and active pane are restored.
- Each restored pane starts in the expected working directory and shell/profile context.
- Explicit launch arguments bypass restoration unless `--restore-session` is supplied.
- Corrupt saved session data is ignored with a visible diagnostic/log entry, and startup still succeeds.

Buffer restoration:

- Run commands that emit output, quit, reopen.
- The previous output is visible in the restored pane.
- New shell output is distinguishable from restored historical output.
- Saved buffer size obeys configured limits.
- Clearing saved session data removes restored buffers and layout.
- Disabling buffer restoration keeps layout restoration but drops saved output.

Privacy:

- Saved session storage path is documented.
- Support bundle generation does not include raw restored buffer data.
- Diagnostics do not print terminal buffer contents.

## Open Questions

- Should buffer restoration be enabled by default, or should only layout restoration be enabled by default?
- Should restoration apply to every launch, or only launches without explicit CLI/startup commands?
- How should command-backed tabs behave after restoration: start a shell, rerun the command, or show history only?
- Should alternate-screen panes be snapshotted, skipped, or restored as plain text?
- Is JSON enough for the MVP, or should buffer restoration start with SQLite to avoid later migration churn?
- Should restored historical content be selectable/searchable through the normal terminal buffer search?
