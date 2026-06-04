use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs as std_fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use assets::Assets;
use clap::{ArgGroup, Parser, ValueEnum, ValueHint};
use client::{Client, UserStore};
use collections::HashMap;
use fs::RealFs;
use futures::StreamExt;
use gpui::{
    Action, App, AppContext as _, Axis, Bounds, Context, KeyBinding, Menu, MenuItem, Pixels,
    SharedString, SystemWindowTabController, Task, TaskExt, WeakEntity, Window, WindowBounds,
    WindowOptions, actions, px, size,
};
use language::LanguageRegistry;
use node_runtime::NodeRuntime;
use project::Project;
use reqwest_client::ReqwestClient;
use schemars::JsonSchema;
use serde::Deserialize;
use session::{AppSession, Session};
use settings::{KeybindSource, KeymapFile, KeymapFileLoadResult, Settings};
use task::{
    HideStrategy, RevealStrategy, RevealTarget, SaveStrategy, Shell, SpawnInTerminal, TaskId,
};
use terminal::Terminal;
use terminal_view::{TerminalView, default_working_directory, terminal_panel::TerminalPanel};
use theme::{ActiveTheme, ThemeRegistry};
use theme_settings::{ThemeSettings, load_user_theme};
use workspace::WorkspaceSettings;
use workspace::{AppState, Event as WorkspaceEvent, Workspace, WorkspaceStore};

actions!(
    zed_terminal,
    [
        OpenSettingsFile,
        OpenStartupConfigFile,
        OpenStartupConfigSchemaFile,
        OpenKeymapFile,
        OpenDefaultKeymapReferenceFile,
        OpenConfigDirectory,
        OpenDataDirectory,
        OpenLogFile,
        OpenLogsDirectory,
        OpenThemesDirectory,
        NewTerminalWindow,
        CloseTerminalWindow,
        MinimizeTerminalWindow,
        ZoomTerminalWindow,
        NewTerminalTab,
        DuplicateTerminalTab,
        ToggleFullScreen,
        ResizePaneLeft,
        ResizePaneRight,
        ResizePaneUp,
        ResizePaneDown,
        ResetPaneSizes,
        ClearDefaultStartupProfile
    ]
);

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Action)]
#[action(namespace = zed_terminal)]
#[serde(deny_unknown_fields)]
struct NewTerminalTabWithProfile {
    profile: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Action)]
#[action(namespace = zed_terminal)]
#[serde(deny_unknown_fields)]
struct NewTerminalSplitWithProfile {
    profile: String,
    direction: TerminalStartupSplitDirection,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Action)]
#[action(namespace = zed_terminal)]
#[serde(deny_unknown_fields)]
struct SetDefaultStartupProfile {
    profile: String,
}

const WINDOW_WIDTH: f32 = 1100.0;
const WINDOW_HEIGHT: f32 = 720.0;
const APP_TITLE: &str = TERMINAL_APP_NAME;
const TERMINAL_APP_NAME: &str = "Zed Terminal";
const TERMINAL_APP_NAME_LOWERCASE: &str = "zed-terminal";
const TERMINAL_KEYMAP_PATH: &str = "keymaps/zed-terminal.json";
const TERMINAL_STARTUP_CONFIG_FILE: &str = "terminal.json";
const TERMINAL_STARTUP_CONFIG_SCHEMA_FILE: &str = "terminal.schema.json";
const TERMINAL_DEFAULT_KEYMAP_REFERENCE_FILE: &str = "default-keymap.json";
const TERMINAL_PROFILE_COMMAND_PALETTE_MAX_RESULTS: usize = 100;

static TERMINAL_LOG_FILE: OnceLock<PathBuf> = OnceLock::new();
static TERMINAL_OLD_LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Debug, Parser)]
#[command(
    name = "zed-terminal",
    version,
    about = "Launch the standalone Zed terminal."
)]
#[command(group(
    ArgGroup::new("default_profile_command")
        .args(["set_default_profile", "clear_default_profile"])
))]
#[command(group(
    ArgGroup::new("startup_update_command")
        .args(["update_startup"])
))]
#[command(group(
    ArgGroup::new("startup_env_command")
        .args(["update_startup_env"])
))]
#[command(group(
    ArgGroup::new("profile_metadata_command")
        .args(["create_profile", "update_profile"])
))]
#[command(group(
    ArgGroup::new("profile_startup_command")
        .args(["update_profile_startup"])
))]
#[command(group(
    ArgGroup::new("profile_env_command")
        .args(["update_profile_env"])
))]
#[command(group(
    ArgGroup::new("profile_copy_command")
        .args(["copy_profile"])
))]
#[command(group(
    ArgGroup::new("profile_visibility_command")
        .args(["hide_profile", "show_profile"])
))]
struct Cli {
    #[arg(
        long = "user-data-dir",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath
    )]
    user_data_dir: Option<PathBuf>,

    #[arg(
        long = "config-dir",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath
    )]
    config_dir: Option<PathBuf>,

    #[arg(
        long = "paths",
        conflicts_with_all = [
            "list_profiles",
            "describe_profile",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor"
        ]
    )]
    print_paths: bool,

    #[arg(
        long = "paths-format",
        value_enum,
        requires = "print_paths",
        help = "Set the output format for --paths"
    )]
    paths_format: Option<TerminalPathsOutputFormat>,

    #[arg(
        long = "list-profiles",
        conflicts_with_all = [
            "describe_profile",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "List configured startup profiles without opening a terminal window"
    )]
    list_profiles: bool,

    #[arg(
        long = "all-profiles",
        requires = "list_profiles",
        conflicts_with_all = [
            "describe_profile",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor"
        ],
        help = "Include hidden startup profiles when listing profiles"
    )]
    all_profiles: bool,

    #[arg(
        long = "list-profiles-format",
        value_enum,
        requires = "list_profiles",
        help = "Set the output format for --list-profiles"
    )]
    list_profiles_format: Option<TerminalListProfilesOutputFormat>,

    #[arg(
        long = "describe-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "describe_startup",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Describe one startup profile from terminal.json without opening a terminal window"
    )]
    describe_profile: Option<String>,

    #[arg(
        long = "describe-profile-format",
        value_enum,
        requires = "describe_profile",
        help = "Set the output format for --describe-profile"
    )]
    describe_profile_format: Option<TerminalDescribeProfileOutputFormat>,

    #[arg(
        long = "describe-startup",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "describe_profile",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "update_profile_startup",
            "update_startup",
            "update_startup_env",
            "update_profile_env",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Describe the root startup layout from terminal.json without opening a terminal window"
    )]
    describe_startup: bool,

    #[arg(
        long = "describe-startup-format",
        value_enum,
        requires = "describe_startup",
        help = "Set the output format for --describe-startup"
    )]
    describe_startup_format: Option<TerminalDescribeStartupOutputFormat>,

    #[arg(
        long = "no-startup-config",
        conflicts_with_all = [
            "profile",
            "list_profiles",
            "describe_profile",
            "describe_startup",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor"
        ]
    )]
    no_startup_config: bool,

    #[arg(
        long = "print-startup-layout",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "describe_profile",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor"
        ],
        help = "Print the resolved startup layout without opening a terminal window"
    )]
    print_startup_layout: bool,

    #[arg(
        long = "startup-layout-format",
        value_enum,
        requires = "print_startup_layout",
        help = "Set the output format for --print-startup-layout"
    )]
    startup_layout_format: Option<TerminalStartupLayoutOutputFormat>,

    #[arg(
        long = "update-startup",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "update_profile_startup",
            "update_profile_env",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Update root startup fields in terminal.json without opening a terminal window"
    )]
    update_startup: bool,

    #[arg(
        long = "startup-working-directory",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath,
        requires = "startup_update_command",
        conflicts_with = "clear_startup_working_directory",
        help = "Set root working_directory for --update-startup"
    )]
    startup_working_directory: Option<PathBuf>,

    #[arg(
        long = "clear-startup-working-directory",
        requires = "update_startup",
        conflicts_with = "startup_working_directory",
        help = "Clear root working_directory for --update-startup"
    )]
    clear_startup_working_directory: bool,

    #[arg(
        long = "startup-command",
        value_name = "COMMAND",
        requires = "startup_update_command",
        conflicts_with_all = ["clear_startup_command", "startup_shell", "startup_shell_args"],
        help = "Set root command for --update-startup"
    )]
    startup_command: Option<String>,

    #[arg(
        long = "clear-startup-command",
        requires = "update_startup",
        conflicts_with = "startup_command",
        help = "Clear root command for --update-startup"
    )]
    clear_startup_command: bool,

    #[arg(
        long = "startup-title",
        value_name = "TITLE",
        requires = "startup_update_command",
        conflicts_with = "clear_startup_title",
        help = "Set root title for --update-startup"
    )]
    startup_title: Option<String>,

    #[arg(
        long = "clear-startup-title",
        requires = "update_startup",
        conflicts_with = "startup_title",
        help = "Clear root title for --update-startup"
    )]
    clear_startup_title: bool,

    #[arg(
        long = "startup-shell",
        value_name = "PROGRAM",
        requires = "startup_update_command",
        conflicts_with_all = ["clear_startup_shell", "startup_command"],
        help = "Set root shell program for --update-startup"
    )]
    startup_shell: Option<String>,

    #[arg(
        long = "startup-shell-arg",
        value_name = "ARG",
        requires = "startup_shell",
        allow_hyphen_values = true,
        help = "Append one shell argument for --startup-shell"
    )]
    startup_shell_args: Vec<String>,

    #[arg(
        long = "clear-startup-shell",
        requires = "update_startup",
        conflicts_with_all = ["startup_shell", "startup_shell_args"],
        help = "Clear root shell for --update-startup"
    )]
    clear_startup_shell: bool,

    #[arg(
        long = "update-startup-format",
        value_enum,
        requires = "update_startup",
        help = "Set the output format for --update-startup"
    )]
    update_startup_format: Option<TerminalStartupUpdateOutputFormat>,

    #[arg(
        long = "update-startup-env",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "update_profile_startup",
            "update_startup",
            "update_profile_env",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Update root environment variables in terminal.json without opening a terminal window"
    )]
    update_startup_env: bool,

    #[arg(
        long = "startup-env",
        value_name = "KEY=VALUE",
        requires = "startup_env_command",
        help = "Set one environment variable for --update-startup-env; repeat to set multiple"
    )]
    startup_env: Vec<String>,

    #[arg(
        long = "remove-startup-env",
        value_name = "KEY",
        requires = "update_startup_env",
        help = "Remove one environment variable for --update-startup-env; repeat to remove multiple"
    )]
    remove_startup_env: Vec<String>,

    #[arg(
        long = "clear-startup-env",
        requires = "update_startup_env",
        help = "Clear all root environment variables for --update-startup-env"
    )]
    clear_startup_env: bool,

    #[arg(
        long = "update-startup-env-format",
        value_enum,
        requires = "update_startup_env",
        help = "Set the output format for --update-startup-env"
    )]
    update_startup_env_format: Option<TerminalStartupEnvUpdateOutputFormat>,

    #[arg(
        long = "set-default-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "describe_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Set the default startup profile in terminal.json without opening a terminal window"
    )]
    set_default_profile: Option<String>,

    #[arg(
        long = "default-profile-format",
        value_enum,
        requires = "default_profile_command",
        help = "Set the output format for --set-default-profile and --clear-default-profile"
    )]
    default_profile_format: Option<TerminalDefaultProfileUpdateOutputFormat>,

    #[arg(
        long = "clear-default-profile",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Clear the default startup profile in terminal.json without opening a terminal window"
    )]
    clear_default_profile: bool,

    #[arg(
        long = "create-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "update_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Create a startup profile in terminal.json without opening a terminal window"
    )]
    create_profile: Option<String>,

    #[arg(
        long = "profile-display-name",
        value_name = "DISPLAY_NAME",
        requires = "profile_metadata_command",
        help = "Set display name metadata for --create-profile or --update-profile"
    )]
    profile_display_name: Option<String>,

    #[arg(
        long = "profile-description",
        value_name = "DESCRIPTION",
        requires = "profile_metadata_command",
        help = "Set description metadata for --create-profile or --update-profile"
    )]
    profile_description: Option<String>,

    #[arg(
        long = "profile-icon",
        value_name = "ICON",
        requires = "profile_metadata_command",
        help = "Set icon metadata for --create-profile or --update-profile"
    )]
    profile_icon: Option<String>,

    #[arg(
        long = "profile-color",
        value_name = "COLOR",
        requires = "profile_metadata_command",
        help = "Set color metadata for --create-profile or --update-profile"
    )]
    profile_color: Option<String>,

    #[arg(
        long = "create-profile-hidden",
        requires = "create_profile",
        help = "Create the startup profile hidden from profile lists and menus"
    )]
    create_profile_hidden: bool,

    #[arg(
        long = "create-profile-format",
        value_enum,
        requires = "create_profile",
        help = "Set the output format for --create-profile"
    )]
    create_profile_format: Option<TerminalStartupProfileCreationOutputFormat>,

    #[arg(
        long = "update-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Update startup profile display metadata in terminal.json without opening a terminal window"
    )]
    update_profile: Option<String>,

    #[arg(
        long = "clear-profile-display-name",
        requires = "update_profile",
        conflicts_with = "profile_display_name",
        help = "Clear display name metadata for --update-profile"
    )]
    clear_profile_display_name: bool,

    #[arg(
        long = "clear-profile-description",
        requires = "update_profile",
        conflicts_with = "profile_description",
        help = "Clear description metadata for --update-profile"
    )]
    clear_profile_description: bool,

    #[arg(
        long = "clear-profile-icon",
        requires = "update_profile",
        conflicts_with = "profile_icon",
        help = "Clear icon metadata for --update-profile"
    )]
    clear_profile_icon: bool,

    #[arg(
        long = "clear-profile-color",
        requires = "update_profile",
        conflicts_with = "profile_color",
        help = "Clear color metadata for --update-profile"
    )]
    clear_profile_color: bool,

    #[arg(
        long = "update-profile-format",
        value_enum,
        requires = "update_profile",
        help = "Set the output format for --update-profile"
    )]
    update_profile_format: Option<TerminalStartupProfileUpdateOutputFormat>,

    #[arg(
        long = "update-profile-startup",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Update startup fields for a profile in terminal.json without opening a terminal window"
    )]
    update_profile_startup: Option<String>,

    #[arg(
        long = "profile-working-directory",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath,
        requires = "profile_startup_command",
        conflicts_with = "clear_profile_working_directory",
        help = "Set working_directory for --update-profile-startup"
    )]
    profile_working_directory: Option<PathBuf>,

    #[arg(
        long = "clear-profile-working-directory",
        requires = "update_profile_startup",
        conflicts_with = "profile_working_directory",
        help = "Clear working_directory for --update-profile-startup"
    )]
    clear_profile_working_directory: bool,

    #[arg(
        long = "profile-command",
        value_name = "COMMAND",
        requires = "profile_startup_command",
        conflicts_with_all = ["clear_profile_command", "profile_shell", "profile_shell_args"],
        help = "Set command for --update-profile-startup"
    )]
    profile_command: Option<String>,

    #[arg(
        long = "clear-profile-command",
        requires = "update_profile_startup",
        conflicts_with = "profile_command",
        help = "Clear command for --update-profile-startup"
    )]
    clear_profile_command: bool,

    #[arg(
        long = "profile-title",
        value_name = "TITLE",
        requires = "profile_startup_command",
        conflicts_with = "clear_profile_title",
        help = "Set title for --update-profile-startup"
    )]
    profile_title: Option<String>,

    #[arg(
        long = "clear-profile-title",
        requires = "update_profile_startup",
        conflicts_with = "profile_title",
        help = "Clear title for --update-profile-startup"
    )]
    clear_profile_title: bool,

    #[arg(
        long = "profile-shell",
        value_name = "PROGRAM",
        requires = "profile_startup_command",
        conflicts_with_all = ["clear_profile_shell", "profile_command"],
        help = "Set shell program for --update-profile-startup"
    )]
    profile_shell: Option<String>,

    #[arg(
        long = "profile-shell-arg",
        value_name = "ARG",
        requires = "profile_shell",
        allow_hyphen_values = true,
        help = "Append one shell argument for --profile-shell"
    )]
    profile_shell_args: Vec<String>,

    #[arg(
        long = "clear-profile-shell",
        requires = "update_profile_startup",
        conflicts_with_all = ["profile_shell", "profile_shell_args"],
        help = "Clear shell for --update-profile-startup"
    )]
    clear_profile_shell: bool,

    #[arg(
        long = "update-profile-startup-format",
        value_enum,
        requires = "update_profile_startup",
        help = "Set the output format for --update-profile-startup"
    )]
    update_profile_startup_format: Option<TerminalStartupProfileStartupUpdateOutputFormat>,

    #[arg(
        long = "update-profile-env",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "update_profile_startup",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Update profile environment variables in terminal.json without opening a terminal window"
    )]
    update_profile_env: Option<String>,

    #[arg(
        long = "profile-env",
        value_name = "KEY=VALUE",
        requires = "profile_env_command",
        help = "Set one environment variable for --update-profile-env; repeat to set multiple"
    )]
    profile_env: Vec<String>,

    #[arg(
        long = "remove-profile-env",
        value_name = "KEY",
        requires = "update_profile_env",
        help = "Remove one environment variable for --update-profile-env; repeat to remove multiple"
    )]
    remove_profile_env: Vec<String>,

    #[arg(
        long = "clear-profile-env",
        requires = "update_profile_env",
        help = "Clear all environment variables for --update-profile-env"
    )]
    clear_profile_env: bool,

    #[arg(
        long = "update-profile-env-format",
        value_enum,
        requires = "update_profile_env",
        help = "Set the output format for --update-profile-env"
    )]
    update_profile_env_format: Option<TerminalStartupProfileEnvUpdateOutputFormat>,

    #[arg(
        long = "copy-profile",
        visible_alias = "duplicate-profile",
        value_names = ["SOURCE_NAME", "TARGET_NAME"],
        num_args = 2,
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "create_profile",
            "update_profile",
            "describe_profile",
            "remove_profile",
            "rename_profile",
            "hide_profile",
            "show_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Copy a startup profile in terminal.json without opening a terminal window"
    )]
    copy_profile: Vec<String>,

    #[arg(
        long = "copy-profile-format",
        value_enum,
        requires = "copy_profile",
        help = "Set the output format for --copy-profile"
    )]
    copy_profile_format: Option<TerminalStartupProfileCopyOutputFormat>,

    #[arg(
        long = "remove-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Remove a startup profile from terminal.json without opening a terminal window"
    )]
    remove_profile: Option<String>,

    #[arg(
        long = "remove-profile-format",
        value_enum,
        requires = "remove_profile",
        help = "Set the output format for --remove-profile"
    )]
    remove_profile_format: Option<TerminalStartupProfileRemovalOutputFormat>,

    #[arg(
        long = "rename-profile",
        value_names = ["OLD_NAME", "NEW_NAME"],
        num_args = 2,
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Rename a startup profile in terminal.json and update profile references without opening a terminal window"
    )]
    rename_profile: Vec<String>,

    #[arg(
        long = "rename-profile-format",
        value_enum,
        requires = "rename_profile",
        help = "Set the output format for --rename-profile"
    )]
    rename_profile_format: Option<TerminalStartupProfileRenameOutputFormat>,

    #[arg(
        long = "hide-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Hide a startup profile from menus and profile lists without opening a terminal window"
    )]
    hide_profile: Option<String>,

    #[arg(
        long = "show-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_profiles",
            "new_tab_profile_titles",
            "new_tab_profile_splits",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Show a startup profile in menus and profile lists without opening a terminal window"
    )]
    show_profile: Option<String>,

    #[arg(
        long = "profile-visibility-format",
        value_enum,
        requires = "profile_visibility_command",
        help = "Set the output format for --hide-profile and --show-profile"
    )]
    profile_visibility_format: Option<TerminalStartupProfileVisibilityOutputFormat>,

    #[arg(
        long = "validate-startup-config",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Validate terminal.json without opening a terminal window"
    )]
    validate_startup_config: bool,

    #[arg(
        long = "validate-startup-config-format",
        value_enum,
        requires = "validate_startup_config",
        help = "Set the output format for --validate-startup-config"
    )]
    validate_startup_config_format: Option<TerminalStartupConfigValidationOutputFormat>,

    #[arg(
        long = "print-startup-config-schema",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Print the JSON Schema for terminal.json without opening a terminal window"
    )]
    print_startup_config_schema: bool,

    #[arg(
        long = "init-config",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "print_startup_config_schema",
            "print_default_keymap",
            "validate_keymap",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Create missing standalone config files without opening a terminal window"
    )]
    init_config: bool,

    #[arg(
        long = "init-config-format",
        value_enum,
        requires = "init_config",
        help = "Set the output format for --init-config"
    )]
    init_config_format: Option<TerminalConfigInitializationOutputFormat>,

    #[arg(
        long = "doctor",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "validate_keymap",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Run read-only standalone terminal diagnostics without opening a terminal window"
    )]
    doctor: bool,

    #[arg(
        long = "doctor-format",
        visible_alias = "format",
        value_enum,
        requires = "doctor",
        help = "Set the output format for --doctor"
    )]
    doctor_format: Option<TerminalDoctorOutputFormat>,

    #[arg(
        long = "validate-keymap",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Validate the standalone default keymap and active keymap.json without opening a terminal window"
    )]
    validate_keymap: bool,

    #[arg(
        long = "validate-keymap-format",
        value_enum,
        requires = "validate_keymap",
        help = "Set the output format for --validate-keymap"
    )]
    validate_keymap_format: Option<TerminalKeymapValidationOutputFormat>,

    #[arg(
        long = "print-default-keymap",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "print_startup_config_schema",
            "init_config",
            "doctor",
            "validate_keymap",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "title",
            "new_tabs",
            "new_tab_titles",
            "new_tab_command_directories",
            "new_tab_command_titles",
            "new_tab_commands",
            "command"
        ],
        help = "Print the standalone default keymap without opening a terminal window"
    )]
    print_default_keymap: bool,

    #[arg(long = "profile", value_name = "NAME")]
    profile: Option<String>,

    #[arg(
        short = 'd',
        long = "working-directory",
        visible_alias = "cwd",
        visible_alias = "starting-directory",
        visible_alias = "startingDirectory",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath,
        conflicts_with = "directory"
    )]
    working_directory: Option<PathBuf>,

    #[arg(value_name = "DIRECTORY", value_hint = ValueHint::DirPath)]
    directory: Option<PathBuf>,

    #[arg(
        long = "title",
        value_name = "TITLE",
        help = "Set the initial terminal tab title"
    )]
    title: Option<String>,

    #[arg(
        long = "new-tab",
        visible_alias = "tab",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath
    )]
    new_tabs: Vec<PathBuf>,

    #[arg(
        long = "new-tab-title",
        visible_alias = "tab-title",
        value_name = "TITLE",
        help = "Set the title for a --new-tab by order"
    )]
    new_tab_titles: Vec<String>,

    #[arg(
        long = "new-tab-profile",
        visible_alias = "tab-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "list_profiles",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config"
        ],
        help = "Open an additional startup tab from a terminal.json profile by name"
    )]
    new_tab_profiles: Vec<String>,

    #[arg(
        long = "new-tab-profile-title",
        visible_alias = "tab-profile-title",
        value_name = "TITLE",
        conflicts_with_all = [
            "list_profiles",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config"
        ],
        help = "Set the title for a --new-tab-profile by order"
    )]
    new_tab_profile_titles: Vec<String>,

    #[arg(
        long = "new-tab-profile-split",
        visible_alias = "tab-profile-split",
        value_name = "DIRECTION",
        value_enum,
        conflicts_with_all = [
            "list_profiles",
            "set_default_profile",
            "clear_default_profile",
            "describe_profile",
            "copy_profile",
            "remove_profile",
            "rename_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "print_default_keymap",
            "init_config",
            "doctor",
            "no_startup_config"
        ],
        help = "Set the split direction for a --new-tab-profile by order"
    )]
    new_tab_profile_splits: Vec<TerminalStartupSplitDirection>,

    #[arg(
        long = "new-tab-command",
        visible_alias = "tab-command",
        value_name = "COMMAND",
        value_hint = ValueHint::CommandString,
        allow_hyphen_values = true
    )]
    new_tab_commands: Vec<String>,

    #[arg(
        long = "new-tab-command-directory",
        visible_alias = "tab-command-directory",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath,
        help = "Set the working directory for a --new-tab-command by order"
    )]
    new_tab_command_directories: Vec<PathBuf>,

    #[arg(
        long = "new-tab-command-title",
        visible_alias = "tab-command-title",
        value_name = "TITLE",
        help = "Set the title for a --new-tab-command by order"
    )]
    new_tab_command_titles: Vec<String>,

    #[arg(
        value_name = "COMMAND",
        value_hint = ValueHint::CommandWithArguments,
        last = true,
        num_args = 1..,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

#[derive(Clone, Debug)]
enum TerminalCliCommand {
    PrintPaths {
        path_options: TerminalPathOptions,
        format: TerminalPathsOutputFormat,
    },
    ListProfiles {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
        include_hidden: bool,
        format: TerminalListProfilesOutputFormat,
    },
    DescribeProfile {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
        profile: String,
        format: TerminalDescribeProfileOutputFormat,
    },
    DescribeStartup {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
        format: TerminalDescribeStartupOutputFormat,
    },
    SetDefaultProfile {
        path_options: TerminalPathOptions,
        profile: String,
        format: TerminalDefaultProfileUpdateOutputFormat,
    },
    ClearDefaultProfile {
        path_options: TerminalPathOptions,
        format: TerminalDefaultProfileUpdateOutputFormat,
    },
    CreateProfile {
        path_options: TerminalPathOptions,
        profile: String,
        metadata: TerminalStartupProfileCreationMetadata,
        format: TerminalStartupProfileCreationOutputFormat,
    },
    UpdateProfile {
        path_options: TerminalPathOptions,
        profile: String,
        update: TerminalStartupProfileMetadataUpdateRequest,
        format: TerminalStartupProfileUpdateOutputFormat,
    },
    UpdateProfileStartup {
        path_options: TerminalPathOptions,
        profile: String,
        update: TerminalStartupProfileStartupUpdateRequest,
        format: TerminalStartupProfileStartupUpdateOutputFormat,
    },
    UpdateStartup {
        path_options: TerminalPathOptions,
        update: TerminalStartupUpdateRequest,
        format: TerminalStartupUpdateOutputFormat,
    },
    UpdateStartupEnv {
        path_options: TerminalPathOptions,
        update: TerminalStartupEnvUpdateRequest,
        format: TerminalStartupEnvUpdateOutputFormat,
    },
    UpdateProfileEnv {
        path_options: TerminalPathOptions,
        profile: String,
        update: TerminalStartupProfileEnvUpdateRequest,
        format: TerminalStartupProfileEnvUpdateOutputFormat,
    },
    CopyProfile {
        path_options: TerminalPathOptions,
        source_profile: String,
        target_profile: String,
        format: TerminalStartupProfileCopyOutputFormat,
    },
    RemoveProfile {
        path_options: TerminalPathOptions,
        profile: String,
        format: TerminalStartupProfileRemovalOutputFormat,
    },
    SetProfileVisibility {
        path_options: TerminalPathOptions,
        profile: String,
        hidden: bool,
        format: TerminalStartupProfileVisibilityOutputFormat,
    },
    RenameProfile {
        path_options: TerminalPathOptions,
        old_profile: String,
        new_profile: String,
        format: TerminalStartupProfileRenameOutputFormat,
    },
    ValidateStartupConfig {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
        format: TerminalStartupConfigValidationOutputFormat,
    },
    PrintStartupLayout {
        launch_options: LaunchOptions,
        format: TerminalStartupLayoutOutputFormat,
    },
    PrintStartupConfigSchema {
        path_options: TerminalPathOptions,
    },
    PrintDefaultKeymap {
        path_options: TerminalPathOptions,
    },
    InitConfig {
        path_options: TerminalPathOptions,
        format: TerminalConfigInitializationOutputFormat,
    },
    Doctor {
        path_options: TerminalPathOptions,
        format: TerminalDoctorOutputFormat,
    },
    ValidateKeymap {
        path_options: TerminalPathOptions,
        format: TerminalKeymapValidationOutputFormat,
    },
    Launch(LaunchOptions),
}

#[derive(Clone, Debug)]
struct LaunchOptions {
    path_options: TerminalPathOptions,
    initial_tab: LaunchTab,
    additional_tabs: Vec<LaunchTab>,
    new_terminal_tab: LaunchTab,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalPathOptions {
    data_dir: PathBuf,
    config_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalPathReport {
    config_dir: PathBuf,
    data_dir: PathBuf,
    logs_dir: PathBuf,
    settings_file: PathBuf,
    startup_config_file: PathBuf,
    startup_config_schema_file: PathBuf,
    global_settings_file: PathBuf,
    keymap_file: PathBuf,
    default_keymap_reference_file: PathBuf,
    themes_dir: PathBuf,
    log_file: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchTab {
    working_directory: Option<PathBuf>,
    command: Option<LaunchCommand>,
    env: HashMap<String, String>,
    title: Option<String>,
    shell: Option<Shell>,
    split: Option<TerminalStartupSplitDirection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchCommand {
    program: String,
    args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupLayoutReport {
    startup_config_file: PathBuf,
    new_terminal_tab: TerminalStartupLayoutTabReport,
    tabs: Vec<TerminalStartupLayoutTabReport>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupLayoutTabReport {
    kind: TerminalStartupLayoutTabKind,
    placement: TerminalStartupLayoutPlacement,
    title: Option<String>,
    working_directory: Option<PathBuf>,
    command: Option<LaunchCommand>,
    shell: Option<Shell>,
    env_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalStartupLayoutTabKind {
    Shell,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalStartupLayoutPlacement {
    Tab,
    Split(TerminalStartupSplitDirection),
}

impl TerminalStartupLayoutTabKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Command => "command",
        }
    }
}

impl TerminalStartupLayoutPlacement {
    fn kind(self) -> &'static str {
        match self {
            Self::Tab => "tab",
            Self::Split(_) => "split",
        }
    }

    fn split_direction(self) -> Option<TerminalStartupSplitDirection> {
        match self {
            Self::Tab => None,
            Self::Split(direction) => Some(direction),
        }
    }

    fn display_label(self) -> String {
        match self {
            Self::Tab => "tab".into(),
            Self::Split(direction) => format!("split {}", direction.as_str()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupConfig {
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    shell: Option<TerminalStartupShellConfig>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    tabs: Vec<TerminalStartupTabConfig>,
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, TerminalStartupProfileConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupProfileConfig {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    shell: Option<TerminalStartupShellConfig>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    tabs: Vec<TerminalStartupTabConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupTabConfig {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    shell: Option<TerminalStartupShellConfig>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    split: Option<TerminalStartupSplitDirection>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
enum TerminalStartupShellConfig {
    Program(String),
    WithArguments(TerminalStartupShellWithArgumentsConfig),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupShellWithArgumentsConfig {
    program: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum TerminalStartupSplitDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileSummary {
    name: String,
    display_name: String,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    hidden: bool,
    is_default: bool,
    tab_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileListReport {
    startup_config_file: PathBuf,
    include_hidden: bool,
    total_count: usize,
    visible_count: usize,
    hidden_count: usize,
    profiles: Vec<TerminalStartupProfileSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileDescription {
    startup_config_file: PathBuf,
    profile: String,
    display_name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    hidden: bool,
    is_default: bool,
    working_directory: Option<PathBuf>,
    command: Option<String>,
    title: Option<String>,
    shell: Option<TerminalStartupShellConfig>,
    env_keys: Vec<String>,
    tabs: Vec<TerminalStartupProfileTabDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupDescription {
    startup_config_file: PathBuf,
    source: TerminalDoctorConfigSource,
    working_directory: Option<PathBuf>,
    command: Option<String>,
    title: Option<String>,
    shell: Option<TerminalStartupShellConfig>,
    env_keys: Vec<String>,
    tabs: Vec<TerminalStartupProfileTabDescription>,
    default_profile: Option<String>,
    profile_count: usize,
    visible_profile_count: usize,
    hidden_profile_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileTabDescription {
    profile: Option<String>,
    working_directory: Option<PathBuf>,
    command: Option<String>,
    title: Option<String>,
    shell: Option<TerminalStartupShellConfig>,
    env_keys: Vec<String>,
    split: Option<TerminalStartupSplitDirection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileMenuEntry {
    profile: String,
    label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupProfileCreationMetadata {
    display_name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupProfileMetadataUpdateRequest {
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    icon: Option<Option<String>>,
    color: Option<Option<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupProfileStartupUpdateRequest {
    working_directory: Option<Option<PathBuf>>,
    command: Option<Option<String>>,
    title: Option<Option<String>>,
    shell: Option<Option<TerminalStartupShellConfig>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupUpdateRequest {
    working_directory: Option<Option<PathBuf>>,
    command: Option<Option<String>>,
    title: Option<Option<String>>,
    shell: Option<Option<TerminalStartupShellConfig>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupProfileEnvUpdateRequest {
    set: Vec<(String, String)>,
    remove: Vec<String>,
    clear: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupEnvUpdateRequest {
    set: Vec<(String, String)>,
    remove: Vec<String>,
    clear: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalProfileSplitDirectionEntry {
    label: &'static str,
    direction: TerminalStartupSplitDirection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalDefaultProfileUpdate {
    path: PathBuf,
    previous_profile: Option<String>,
    default_profile: Option<String>,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileCreation {
    path: PathBuf,
    profile: String,
    display_name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    hidden: bool,
    changed: bool,
    total_profile_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileMetadataUpdate {
    path: PathBuf,
    profile: String,
    previous_display_name: Option<String>,
    display_name: Option<String>,
    previous_description: Option<String>,
    description: Option<String>,
    previous_icon: Option<String>,
    icon: Option<String>,
    previous_color: Option<String>,
    color: Option<String>,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileStartupUpdate {
    path: PathBuf,
    profile: String,
    previous_working_directory: Option<PathBuf>,
    working_directory: Option<PathBuf>,
    previous_command: Option<String>,
    command: Option<String>,
    previous_title: Option<String>,
    title: Option<String>,
    previous_shell: Option<TerminalStartupShellConfig>,
    shell: Option<TerminalStartupShellConfig>,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupUpdate {
    path: PathBuf,
    previous_working_directory: Option<PathBuf>,
    working_directory: Option<PathBuf>,
    previous_command: Option<String>,
    command: Option<String>,
    previous_title: Option<String>,
    title: Option<String>,
    previous_shell: Option<TerminalStartupShellConfig>,
    shell: Option<TerminalStartupShellConfig>,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileEnvUpdate {
    path: PathBuf,
    profile: String,
    previous_env_keys: Vec<String>,
    env_keys: Vec<String>,
    added_env_keys: Vec<String>,
    updated_env_keys: Vec<String>,
    removed_env_keys: Vec<String>,
    cleared: bool,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupEnvUpdate {
    path: PathBuf,
    previous_env_keys: Vec<String>,
    env_keys: Vec<String>,
    added_env_keys: Vec<String>,
    updated_env_keys: Vec<String>,
    removed_env_keys: Vec<String>,
    cleared: bool,
    changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileCopy {
    path: PathBuf,
    source_profile: String,
    profile: String,
    changed: bool,
    copied_tab_count: usize,
    total_profile_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileRemoval {
    path: PathBuf,
    profile: String,
    changed: bool,
    remaining_profile_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileRename {
    path: PathBuf,
    previous_profile: String,
    profile: String,
    changed: bool,
    updated_reference_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupProfileVisibilityUpdate {
    path: PathBuf,
    profile: String,
    previous_hidden: bool,
    hidden: bool,
    changed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupConfigValidation {
    layout_count: usize,
    tab_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalStartupConfigValidationReport {
    startup_config_file: PathBuf,
    validation: TerminalStartupConfigValidation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalConfigInitialization {
    files: Vec<TerminalConfigFileInitialization>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalConfigFileInitialization {
    label: &'static str,
    path: PathBuf,
    status: TerminalConfigFileInitializationStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalConfigFileInitializationStatus {
    Created,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalDoctorReport {
    directories: Vec<TerminalDoctorPathCheck>,
    config_files: Vec<TerminalDoctorPathCheck>,
    startup_config: TerminalDoctorStartupConfigCheck,
    keymap: TerminalDoctorKeymapCheck,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalDoctorPathCheck {
    label: &'static str,
    path: PathBuf,
    status: TerminalDoctorCheckStatus,
    message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalDoctorStartupConfigCheck {
    path: PathBuf,
    status: TerminalDoctorCheckStatus,
    source: Option<TerminalDoctorConfigSource>,
    validation: Option<TerminalStartupConfigValidation>,
    message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalDoctorKeymapCheck {
    path: PathBuf,
    status: TerminalDoctorCheckStatus,
    source: Option<TerminalUserKeymapSource>,
    validation: Option<TerminalKeymapValidation>,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalDoctorCheckStatus {
    Ok,
    Missing,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalDoctorConfigSource {
    File,
    Initial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalKeymapValidation {
    default_binding_count: usize,
    user_binding_count: usize,
    user_keymap_source: TerminalUserKeymapSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalKeymapValidationReport {
    keymap_file: PathBuf,
    validation: TerminalKeymapValidation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalUserKeymapSource {
    File,
    Initial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalDoctorPathKind {
    Directory,
    File,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalConfigInitializationOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalPathsOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalListProfilesOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalDescribeProfileOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalDescribeStartupOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalDefaultProfileUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileCreationOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileStartupUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupEnvUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileEnvUpdateOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileCopyOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileRemovalOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileRenameOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupProfileVisibilityOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupLayoutOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalStartupConfigValidationOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalKeymapValidationOutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TerminalDoctorOutputFormat {
    #[default]
    Text,
    Json,
}

impl TerminalCliCommand {
    fn from_cli_and_config_file(cli: Cli) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;
        let startup_config = if cli.print_paths
            || cli.no_startup_config
            || cli.set_default_profile.is_some()
            || cli.clear_default_profile
            || cli.create_profile.is_some()
            || cli.update_profile.is_some()
            || cli.update_profile_startup.is_some()
            || cli.update_startup
            || cli.update_startup_env
            || cli.update_profile_env.is_some()
            || !cli.copy_profile.is_empty()
            || cli.remove_profile.is_some()
            || !cli.rename_profile.is_empty()
            || cli.hide_profile.is_some()
            || cli.show_profile.is_some()
            || cli.validate_keymap
            || cli.print_startup_config_schema
            || cli.print_default_keymap
            || cli.init_config
            || cli.doctor
        {
            TerminalStartupConfig::default()
        } else {
            TerminalStartupConfig::load(&terminal_startup_config_file(&path_options.config_dir))?
        };

        Self::from_cli_parts(cli, startup_config, path_options)
    }

    #[cfg(test)]
    fn from_cli_and_startup_config(
        cli: Cli,
        startup_config: TerminalStartupConfig,
    ) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;

        Self::from_cli_parts(cli, startup_config, path_options)
    }

    fn from_cli_parts(
        cli: Cli,
        startup_config: TerminalStartupConfig,
        path_options: TerminalPathOptions,
    ) -> Result<Self> {
        if cli.print_paths {
            return Ok(Self::PrintPaths {
                path_options,
                format: cli.paths_format.unwrap_or_default(),
            });
        }

        if cli.list_profiles {
            return Ok(Self::ListProfiles {
                path_options,
                startup_config,
                include_hidden: cli.all_profiles,
                format: cli.list_profiles_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.describe_profile {
            return Ok(Self::DescribeProfile {
                path_options,
                startup_config,
                profile,
                format: cli.describe_profile_format.unwrap_or_default(),
            });
        }

        if cli.describe_startup {
            return Ok(Self::DescribeStartup {
                path_options,
                startup_config,
                format: cli.describe_startup_format.unwrap_or_default(),
            });
        }

        if cli.print_startup_layout {
            let format = cli.startup_layout_format.unwrap_or_default();
            return Ok(Self::PrintStartupLayout {
                launch_options: LaunchOptions::from_cli_parts(cli, startup_config, path_options)?,
                format,
            });
        }

        if cli.update_startup {
            let update = TerminalStartupUpdateRequest {
                working_directory: if cli.clear_startup_working_directory {
                    Some(None)
                } else {
                    cli.startup_working_directory.map(Some)
                },
                command: terminal_command_update_value(
                    cli.startup_command.as_deref(),
                    cli.clear_startup_command,
                    "--startup-command",
                )?,
                title: terminal_title_update_value(
                    cli.startup_title.as_deref(),
                    cli.clear_startup_title,
                ),
                shell: terminal_shell_update_value(
                    cli.startup_shell.as_deref(),
                    &cli.startup_shell_args,
                    cli.clear_startup_shell,
                    "--startup-shell",
                )?,
            };
            update.ensure_requested()?;
            return Ok(Self::UpdateStartup {
                path_options,
                update,
                format: cli.update_startup_format.unwrap_or_default(),
            });
        }

        if cli.update_startup_env {
            let update = TerminalStartupEnvUpdateRequest {
                set: cli
                    .startup_env
                    .iter()
                    .map(|assignment| parse_startup_env_assignment(assignment))
                    .collect::<Result<Vec<_>>>()?,
                remove: cli
                    .remove_startup_env
                    .iter()
                    .map(|key| normalize_startup_env_key(key))
                    .collect::<Result<Vec<_>>>()?,
                clear: cli.clear_startup_env,
            };
            update.ensure_requested()?;
            return Ok(Self::UpdateStartupEnv {
                path_options,
                update,
                format: cli.update_startup_env_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.set_default_profile {
            return Ok(Self::SetDefaultProfile {
                path_options,
                profile,
                format: cli.default_profile_format.unwrap_or_default(),
            });
        }

        if cli.clear_default_profile {
            return Ok(Self::ClearDefaultProfile {
                path_options,
                format: cli.default_profile_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.create_profile {
            return Ok(Self::CreateProfile {
                path_options,
                profile,
                metadata: TerminalStartupProfileCreationMetadata {
                    display_name: normalize_profile_text(cli.profile_display_name.as_deref()),
                    description: normalize_profile_text(cli.profile_description.as_deref()),
                    icon: normalize_profile_text(cli.profile_icon.as_deref()),
                    color: normalize_profile_text(cli.profile_color.as_deref()),
                    hidden: cli.create_profile_hidden,
                },
                format: cli.create_profile_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.update_profile {
            let update = TerminalStartupProfileMetadataUpdateRequest {
                display_name: profile_metadata_update_value(
                    cli.profile_display_name.as_deref(),
                    cli.clear_profile_display_name,
                ),
                description: profile_metadata_update_value(
                    cli.profile_description.as_deref(),
                    cli.clear_profile_description,
                ),
                icon: profile_metadata_update_value(
                    cli.profile_icon.as_deref(),
                    cli.clear_profile_icon,
                ),
                color: profile_metadata_update_value(
                    cli.profile_color.as_deref(),
                    cli.clear_profile_color,
                ),
            };
            update.ensure_requested()?;
            return Ok(Self::UpdateProfile {
                path_options,
                profile,
                update,
                format: cli.update_profile_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.update_profile_startup {
            let update = TerminalStartupProfileStartupUpdateRequest {
                working_directory: if cli.clear_profile_working_directory {
                    Some(None)
                } else {
                    cli.profile_working_directory.map(Some)
                },
                command: terminal_command_update_value(
                    cli.profile_command.as_deref(),
                    cli.clear_profile_command,
                    "--profile-command",
                )?,
                title: terminal_title_update_value(
                    cli.profile_title.as_deref(),
                    cli.clear_profile_title,
                ),
                shell: terminal_shell_update_value(
                    cli.profile_shell.as_deref(),
                    &cli.profile_shell_args,
                    cli.clear_profile_shell,
                    "--profile-shell",
                )?,
            };
            update.ensure_requested()?;
            return Ok(Self::UpdateProfileStartup {
                path_options,
                profile,
                update,
                format: cli.update_profile_startup_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.update_profile_env {
            let update = TerminalStartupProfileEnvUpdateRequest {
                set: cli
                    .profile_env
                    .iter()
                    .map(|assignment| parse_profile_env_assignment(assignment))
                    .collect::<Result<Vec<_>>>()?,
                remove: cli
                    .remove_profile_env
                    .iter()
                    .map(|key| normalize_profile_env_key(key))
                    .collect::<Result<Vec<_>>>()?,
                clear: cli.clear_profile_env,
            };
            update.ensure_requested()?;
            return Ok(Self::UpdateProfileEnv {
                path_options,
                profile,
                update,
                format: cli.update_profile_env_format.unwrap_or_default(),
            });
        }

        if !cli.copy_profile.is_empty() {
            let [source_profile, target_profile]: [String; 2] =
                cli.copy_profile.try_into().map_err(|profiles: Vec<_>| {
                    anyhow::anyhow!(
                        "--copy-profile requires exactly 2 values, got {}",
                        profiles.len()
                    )
                })?;
            return Ok(Self::CopyProfile {
                path_options,
                source_profile,
                target_profile,
                format: cli.copy_profile_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.remove_profile {
            return Ok(Self::RemoveProfile {
                path_options,
                profile,
                format: cli.remove_profile_format.unwrap_or_default(),
            });
        }

        if !cli.rename_profile.is_empty() {
            let [old_profile, new_profile]: [String; 2] =
                cli.rename_profile.try_into().map_err(|profiles: Vec<_>| {
                    anyhow::anyhow!(
                        "--rename-profile requires exactly 2 values, got {}",
                        profiles.len()
                    )
                })?;
            return Ok(Self::RenameProfile {
                path_options,
                old_profile,
                new_profile,
                format: cli.rename_profile_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.hide_profile {
            return Ok(Self::SetProfileVisibility {
                path_options,
                profile,
                hidden: true,
                format: cli.profile_visibility_format.unwrap_or_default(),
            });
        }

        if let Some(profile) = cli.show_profile {
            return Ok(Self::SetProfileVisibility {
                path_options,
                profile,
                hidden: false,
                format: cli.profile_visibility_format.unwrap_or_default(),
            });
        }

        if cli.validate_startup_config {
            return Ok(Self::ValidateStartupConfig {
                path_options,
                startup_config,
                format: cli.validate_startup_config_format.unwrap_or_default(),
            });
        }

        if cli.print_startup_config_schema {
            return Ok(Self::PrintStartupConfigSchema { path_options });
        }

        if cli.print_default_keymap {
            return Ok(Self::PrintDefaultKeymap { path_options });
        }

        if cli.init_config {
            return Ok(Self::InitConfig {
                path_options,
                format: cli.init_config_format.unwrap_or_default(),
            });
        }

        if cli.doctor {
            return Ok(Self::Doctor {
                path_options,
                format: cli.doctor_format.unwrap_or_default(),
            });
        }

        if cli.validate_keymap {
            return Ok(Self::ValidateKeymap {
                path_options,
                format: cli.validate_keymap_format.unwrap_or_default(),
            });
        }

        Ok(Self::Launch(LaunchOptions::from_cli_parts(
            cli,
            startup_config,
            path_options,
        )?))
    }

    fn path_options(&self) -> &TerminalPathOptions {
        match self {
            Self::PrintPaths { path_options, .. } => path_options,
            Self::ListProfiles { path_options, .. } => path_options,
            Self::DescribeProfile { path_options, .. } => path_options,
            Self::DescribeStartup { path_options, .. } => path_options,
            Self::SetDefaultProfile { path_options, .. } => path_options,
            Self::ClearDefaultProfile { path_options, .. } => path_options,
            Self::CreateProfile { path_options, .. } => path_options,
            Self::UpdateProfile { path_options, .. } => path_options,
            Self::UpdateProfileStartup { path_options, .. } => path_options,
            Self::UpdateStartup { path_options, .. } => path_options,
            Self::UpdateStartupEnv { path_options, .. } => path_options,
            Self::UpdateProfileEnv { path_options, .. } => path_options,
            Self::CopyProfile { path_options, .. } => path_options,
            Self::RemoveProfile { path_options, .. } => path_options,
            Self::SetProfileVisibility { path_options, .. } => path_options,
            Self::RenameProfile { path_options, .. } => path_options,
            Self::ValidateStartupConfig { path_options, .. } => path_options,
            Self::PrintStartupLayout { launch_options, .. } => &launch_options.path_options,
            Self::PrintStartupConfigSchema { path_options } => path_options,
            Self::PrintDefaultKeymap { path_options } => path_options,
            Self::InitConfig { path_options, .. } => path_options,
            Self::Doctor { path_options, .. } => path_options,
            Self::ValidateKeymap { path_options, .. } => path_options,
            Self::Launch(launch_options) => &launch_options.path_options,
        }
    }
}

impl LaunchOptions {
    #[cfg(test)]
    fn from_cli(cli: Cli) -> Result<Self> {
        Self::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
    }

    #[cfg(test)]
    fn from_cli_and_startup_config(
        cli: Cli,
        startup_config: TerminalStartupConfig,
    ) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;

        Self::from_cli_parts(cli, startup_config, path_options)
    }

    fn from_cli_parts(
        cli: Cli,
        startup_config: TerminalStartupConfig,
        path_options: TerminalPathOptions,
    ) -> Result<Self> {
        let command = LaunchCommand::from_args(cli.command);
        let working_directory = cli
            .working_directory
            .or(cli.directory)
            .map(|directory| resolve_working_directory(&directory))
            .transpose()?;
        let startup_config = if cli.no_startup_config {
            TerminalStartupConfig::default()
        } else {
            startup_config
        };
        let profile = if cli.no_startup_config {
            None
        } else {
            cli.profile.as_deref()
        };
        let inherited_env = startup_config
            .inherited_env(profile)
            .context("failed to resolve configured startup environment")?;
        let inherited_shell = startup_config
            .inherited_shell(profile)
            .context("failed to resolve configured startup shell")?;
        let mut new_terminal_tab = startup_config
            .initial_tab(profile)
            .context("failed to resolve configured new terminal tab")?;
        let mut initial_tab = new_terminal_tab.clone();
        if let Some(working_directory) = working_directory {
            initial_tab.working_directory = Some(working_directory.clone());
            new_terminal_tab.working_directory = Some(working_directory);
        }
        if let Some(command) = command {
            initial_tab.command = Some(command);
            initial_tab.env = inherited_env.clone();
            initial_tab.shell = None;
        }
        if cli.title.is_some() {
            initial_tab.title = normalize_terminal_title(cli.title.as_deref());
        }
        let mut additional_tabs = startup_config
            .additional_tabs(profile)
            .context("failed to resolve configured startup tabs")?;
        additional_tabs.extend(LaunchTab::additional_from_cli(
            &cli.new_tabs,
            &cli.new_tab_titles,
            &cli.new_tab_profiles,
            &cli.new_tab_profile_titles,
            &cli.new_tab_profile_splits,
            &cli.new_tab_commands,
            &cli.new_tab_command_directories,
            &cli.new_tab_command_titles,
            &inherited_env,
            inherited_shell.as_ref(),
            &startup_config,
        )?);

        Ok(Self {
            path_options,
            initial_tab,
            additional_tabs,
            new_terminal_tab,
        })
    }

    fn startup_working_directories(&self) -> Vec<PathBuf> {
        let mut directories = Vec::new();
        for tab in std::iter::once(&self.initial_tab)
            .chain(std::iter::once(&self.new_terminal_tab))
            .chain(self.additional_tabs.iter())
        {
            let Some(working_directory) = tab.working_directory.as_ref() else {
                continue;
            };
            if !directories.contains(working_directory) {
                directories.push(working_directory.clone());
            }
        }
        directories
    }

    fn runtime_new_window_options(&self) -> Self {
        let mut initial_tab = self.new_terminal_tab.clone();
        initial_tab.split = None;

        Self {
            path_options: self.path_options.clone(),
            initial_tab: initial_tab.clone(),
            additional_tabs: Vec::new(),
            new_terminal_tab: initial_tab,
        }
    }
}

impl TerminalPathOptions {
    fn from_cli(user_data_dir: Option<&Path>, config_dir: Option<&Path>) -> Result<Self> {
        let data_dir = user_data_dir
            .map(expand_tilde)
            .transpose()?
            .unwrap_or(default_terminal_data_dir()?);

        let config_dir = match config_dir {
            Some(config_dir) => expand_tilde(config_dir)?,
            None if user_data_dir.is_some() => data_dir.join("config"),
            None => default_terminal_config_dir()?,
        };

        Ok(Self {
            data_dir,
            config_dir,
        })
    }
}

impl LaunchTab {
    fn additional_from_cli(
        directories: &[PathBuf],
        directory_titles: &[String],
        profiles: &[String],
        profile_titles: &[String],
        profile_splits: &[TerminalStartupSplitDirection],
        commands: &[String],
        command_directories: &[PathBuf],
        command_titles: &[String],
        inherited_env: &HashMap<String, String>,
        inherited_shell: Option<&Shell>,
        startup_config: &TerminalStartupConfig,
    ) -> Result<Vec<Self>> {
        let mut tabs = Vec::with_capacity(directories.len() + profiles.len() + commands.len());

        if directory_titles.len() > directories.len() {
            bail!("startup tab title requires a matching --new-tab");
        }
        if profile_titles.len() > profiles.len() {
            bail!("startup profile tab title requires a matching --new-tab-profile");
        }
        if profile_splits.len() > profiles.len() {
            bail!("startup profile tab split requires a matching --new-tab-profile");
        }
        if command_directories.len() > commands.len() {
            bail!("startup command tab directory requires a matching --new-tab-command");
        }
        if command_titles.len() > commands.len() {
            bail!("startup command tab title requires a matching --new-tab-command");
        }

        for (directory_index, directory) in directories.iter().enumerate() {
            tabs.push(Self {
                working_directory: Some(resolve_working_directory(directory).with_context(
                    || format!("failed to resolve startup tab {}", tabs.len() + 2),
                )?),
                command: None,
                env: HashMap::default(),
                title: normalize_terminal_title(
                    directory_titles.get(directory_index).map(String::as_str),
                ),
                shell: inherited_shell.cloned(),
                split: None,
            });
        }

        for (profile_index, profile) in profiles.iter().enumerate() {
            let tab_number = tabs.len() + 2;
            let mut tab = startup_config
                .profile_launch_tab(profile, profile_splits.get(profile_index).copied())
                .with_context(|| format!("failed to resolve startup profile tab {tab_number}"))?;
            if profile_titles.get(profile_index).is_some() {
                tab.title =
                    normalize_terminal_title(profile_titles.get(profile_index).map(String::as_str));
            }
            tabs.push(tab);
        }

        for (command_index, command) in commands.iter().enumerate() {
            let tab_number = tabs.len() + 2;
            let working_directory = command_directories
                .get(command_index)
                .map(|directory| {
                    resolve_working_directory(directory).with_context(|| {
                        format!("failed to resolve startup tab {tab_number} working directory")
                    })
                })
                .transpose()?;

            tabs.push(Self {
                working_directory,
                command: Some(
                    LaunchCommand::from_command_line(command)
                        .with_context(|| format!("failed to parse startup tab {tab_number}"))?,
                ),
                env: inherited_env.clone(),
                title: normalize_terminal_title(
                    command_titles.get(command_index).map(String::as_str),
                ),
                shell: None,
                split: None,
            });
        }

        Ok(tabs)
    }

    fn from_config(
        working_directory: Option<&Path>,
        command: Option<&str>,
        inherited_env: &HashMap<String, String>,
        tab_env: &HashMap<String, String>,
        title: Option<&str>,
        inherited_shell: Option<&Shell>,
        tab_shell: Option<&TerminalStartupShellConfig>,
        split: Option<TerminalStartupSplitDirection>,
        label: impl std::fmt::Display,
    ) -> Result<Self> {
        let working_directory = working_directory
            .map(resolve_working_directory)
            .transpose()
            .with_context(|| format!("failed to resolve working directory for {label}"))?;
        let command = command
            .map(LaunchCommand::from_command_line)
            .transpose()
            .with_context(|| format!("failed to parse command for {label}"))?;
        if command.is_none() && !tab_env.is_empty() {
            bail!("environment variables require a command for {label}");
        }
        if command.is_some() && tab_shell.is_some() {
            bail!("shell selection requires a shell tab for {label}");
        }
        let env = if command.is_some() {
            let mut env = inherited_env.clone();
            env.extend(tab_env.clone());
            env
        } else {
            HashMap::default()
        };
        let shell = if command.is_some() {
            None
        } else if let Some(shell) = tab_shell {
            Some(
                shell
                    .to_shell()
                    .with_context(|| format!("failed to parse shell for {label}"))?,
            )
        } else {
            inherited_shell.cloned()
        };

        Ok(Self {
            working_directory,
            command,
            env,
            title: normalize_terminal_title(title),
            shell,
            split,
        })
    }
}

impl TerminalStartupSplitDirection {
    fn to_workspace_split_direction(self) -> workspace::SplitDirection {
        match self {
            Self::Up => workspace::SplitDirection::Up,
            Self::Down => workspace::SplitDirection::Down,
            Self::Left => workspace::SplitDirection::Left,
            Self::Right => workspace::SplitDirection::Right,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

impl TerminalStartupShellConfig {
    fn to_shell(&self) -> Result<Shell> {
        match self {
            TerminalStartupShellConfig::Program(program) => {
                let program = normalize_terminal_shell_program(program)?;
                Ok(Shell::Program(program))
            }
            TerminalStartupShellConfig::WithArguments(config) => {
                let program = normalize_terminal_shell_program(&config.program)?;
                if config.args.is_empty() {
                    Ok(Shell::Program(program))
                } else {
                    Ok(Shell::WithArguments {
                        program,
                        args: config.args.clone(),
                        title_override: None,
                    })
                }
            }
        }
    }
}

impl LaunchCommand {
    fn from_args(args: Vec<String>) -> Option<Self> {
        let mut args = args.into_iter();
        let program = args.next()?;
        Some(Self {
            program,
            args: args.collect(),
        })
    }

    fn from_command_line(command_line: &str) -> Result<Self> {
        let args = shlex::split(command_line)
            .with_context(|| format!("could not parse command line: {command_line:?}"))?;
        Self::from_args(args).with_context(|| "command line is empty")
    }

    fn into_spawn_task(
        self,
        cwd: Option<PathBuf>,
        env: HashMap<String, String>,
    ) -> SpawnInTerminal {
        let label = self.display_label();

        SpawnInTerminal {
            id: TaskId(format!("zed-terminal:{label}")),
            full_label: label.clone(),
            label: label.clone(),
            command: Some(self.program),
            args: self.args,
            command_label: label,
            cwd,
            env,
            use_new_terminal: true,
            allow_concurrent_runs: true,
            reveal: RevealStrategy::Always,
            reveal_target: RevealTarget::Center,
            hide: HideStrategy::Never,
            shell: Shell::System,
            show_summary: true,
            show_command: true,
            show_rerun: false,
            save: SaveStrategy::None,
            ..SpawnInTerminal::default()
        }
    }

    fn display_label(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(format_command_part)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl TerminalStartupConfig {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std_fs::read_to_string(path).with_context(|| {
            format!("failed to read terminal startup config {}", path.display())
        })?;
        settings::parse_json_with_comments(&content)
            .with_context(|| format!("failed to parse terminal startup config {}", path.display()))
    }

    fn validate(&self) -> Result<TerminalStartupConfigValidation> {
        if let Some(default_profile) = self.default_profile.as_deref() {
            self.validate_profile_reference("default_profile", default_profile)?;
        }

        let mut validation = self.validate_layout(&TerminalStartupLayout {
            working_directory: self.working_directory.as_deref(),
            command: self.command.as_deref(),
            title: self.title.as_deref(),
            shell: self.shell.as_ref(),
            env: &self.env,
            tabs: &self.tabs,
            label: "root startup layout".into(),
        })?;

        for (name, profile) in &self.profiles {
            if name.trim().is_empty() {
                bail!("startup profile name is empty");
            }

            let profile_validation = self
                .validate_layout(&TerminalStartupLayout {
                    working_directory: profile.working_directory.as_deref(),
                    command: profile.command.as_deref(),
                    title: profile.title.as_deref(),
                    shell: profile.shell.as_ref(),
                    env: &profile.env,
                    tabs: &profile.tabs,
                    label: format!("startup profile {name:?}"),
                })
                .with_context(|| format!("failed to validate startup profile {name:?}"))?;

            validation.layout_count += profile_validation.layout_count;
            validation.tab_count += profile_validation.tab_count;
        }

        Ok(validation)
    }

    fn validate_profile_reference(&self, field: &str, profile_name: &str) -> Result<()> {
        if profile_name.trim().is_empty() {
            bail!("{field} is empty");
        }

        if !self.profiles.contains_key(profile_name) {
            if self.profiles.is_empty() {
                bail!("{field} references missing startup profile: {profile_name}");
            } else {
                bail!(
                    "{field} references missing startup profile: {profile_name}. Available profiles: {}",
                    self.profile_names().join(", ")
                );
            }
        }

        Ok(())
    }

    fn validate_layout(
        &self,
        layout: &TerminalStartupLayout<'_>,
    ) -> Result<TerminalStartupConfigValidation> {
        let shell = layout
            .shell
            .map(TerminalStartupShellConfig::to_shell)
            .transpose()
            .with_context(|| format!("failed to resolve shell for {}", layout.label))?;

        LaunchTab::from_config(
            layout.working_directory,
            layout.command,
            layout.env,
            &HashMap::default(),
            layout.title,
            shell.as_ref(),
            None,
            None,
            format!("initial tab for {}", layout.label),
        )?;

        for (index, tab) in layout.tabs.iter().enumerate() {
            self.tab_from_config(
                tab,
                layout.env,
                shell.as_ref(),
                format!("tab {} for {}", index + 2, layout.label),
            )?;
        }

        Ok(TerminalStartupConfigValidation {
            layout_count: 1,
            tab_count: 1 + layout.tabs.len(),
        })
    }

    fn selected_layout(
        &self,
        requested_profile: Option<&str>,
    ) -> Result<TerminalStartupLayout<'_>> {
        let Some(profile_name) = requested_profile.or(self.default_profile.as_deref()) else {
            return Ok(TerminalStartupLayout {
                working_directory: self.working_directory.as_deref(),
                command: self.command.as_deref(),
                title: self.title.as_deref(),
                shell: self.shell.as_ref(),
                env: &self.env,
                tabs: &self.tabs,
                label: "root startup layout".into(),
            });
        };

        if profile_name.is_empty() {
            bail!("startup profile name is empty");
        }

        let profile = self.profiles.get(profile_name).with_context(|| {
            if self.profiles.is_empty() {
                format!("startup profile not found: {profile_name}")
            } else {
                format!(
                    "startup profile not found: {profile_name}. Available profiles: {}",
                    self.profile_names().join(", ")
                )
            }
        })?;

        Ok(TerminalStartupLayout {
            working_directory: profile.working_directory.as_deref(),
            command: profile.command.as_deref(),
            title: profile.title.as_deref(),
            shell: profile.shell.as_ref(),
            env: &profile.env,
            tabs: &profile.tabs,
            label: format!("startup profile {profile_name:?}"),
        })
    }

    fn initial_tab(&self, requested_profile: Option<&str>) -> Result<LaunchTab> {
        let layout = self.selected_layout(requested_profile)?;
        let shell = layout
            .shell
            .map(TerminalStartupShellConfig::to_shell)
            .transpose()?;
        LaunchTab::from_config(
            layout.working_directory,
            layout.command,
            layout.env,
            &HashMap::default(),
            layout.title,
            shell.as_ref(),
            None,
            None,
            format!("initial tab for {}", layout.label),
        )
    }

    fn inherited_env(&self, requested_profile: Option<&str>) -> Result<HashMap<String, String>> {
        Ok(self.selected_layout(requested_profile)?.env.clone())
    }

    fn inherited_shell(&self, requested_profile: Option<&str>) -> Result<Option<Shell>> {
        self.selected_layout(requested_profile)?
            .shell
            .map(TerminalStartupShellConfig::to_shell)
            .transpose()
    }

    fn additional_tabs(&self, requested_profile: Option<&str>) -> Result<Vec<LaunchTab>> {
        let layout = self.selected_layout(requested_profile)?;
        let shell = layout
            .shell
            .map(TerminalStartupShellConfig::to_shell)
            .transpose()?;
        layout
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                self.tab_from_config(
                    tab,
                    layout.env,
                    shell.as_ref(),
                    format!("tab {} for {}", index + 2, layout.label),
                )
            })
            .collect()
    }

    fn tab_from_config(
        &self,
        tab: &TerminalStartupTabConfig,
        inherited_env: &HashMap<String, String>,
        inherited_shell: Option<&Shell>,
        label: impl std::fmt::Display,
    ) -> Result<LaunchTab> {
        let label = label.to_string();
        if let Some(profile) = tab.profile.as_deref() {
            Self::validate_profile_tab_fields(tab, &label)?;
            let mut launch_tab = self
                .profile_launch_tab(profile, tab.split)
                .with_context(|| format!("failed to resolve profile for {label}"))?;
            if tab.title.is_some() {
                launch_tab.title = normalize_terminal_title(tab.title.as_deref());
            }
            Ok(launch_tab)
        } else {
            LaunchTab::from_config(
                tab.working_directory.as_deref(),
                tab.command.as_deref(),
                inherited_env,
                &tab.env,
                tab.title.as_deref(),
                inherited_shell,
                tab.shell.as_ref(),
                tab.split,
                label,
            )
        }
    }

    fn validate_profile_tab_fields(tab: &TerminalStartupTabConfig, label: &str) -> Result<()> {
        if tab.working_directory.is_some() {
            bail!("profile startup tab cannot include working_directory for {label}");
        }
        if tab.command.is_some() {
            bail!("profile startup tab cannot include command for {label}");
        }
        if tab.shell.is_some() {
            bail!("profile startup tab cannot include shell for {label}");
        }
        if !tab.env.is_empty() {
            bail!("profile startup tab cannot include env for {label}");
        }

        Ok(())
    }

    fn profile_summaries(&self, include_hidden: bool) -> Vec<TerminalStartupProfileSummary> {
        self.profiles
            .iter()
            .filter_map(|(name, profile)| {
                if profile.hidden && !include_hidden {
                    return None;
                }

                Some(TerminalStartupProfileSummary {
                    name: name.clone(),
                    display_name: normalize_profile_text(profile.display_name.as_deref())
                        .unwrap_or_else(|| name.clone()),
                    description: normalize_profile_text(profile.description.as_deref()),
                    icon: normalize_profile_text(profile.icon.as_deref()),
                    color: normalize_profile_text(profile.color.as_deref()),
                    hidden: profile.hidden,
                    is_default: self.default_profile.as_deref() == Some(name.as_str()),
                    tab_count: 1 + profile.tabs.len(),
                })
            })
            .collect()
    }

    fn profile_menu_entries(&self) -> Vec<TerminalStartupProfileMenuEntry> {
        self.profile_summaries(false)
            .into_iter()
            .map(|profile| TerminalStartupProfileMenuEntry {
                label: profile_menu_label(&profile),
                profile: profile.name,
            })
            .collect()
    }

    fn profile_initial_tab(&self, profile: &str) -> Result<LaunchTab> {
        self.profile_launch_tab(profile, None)
    }

    fn profile_launch_tab(
        &self,
        profile: &str,
        split: Option<TerminalStartupSplitDirection>,
    ) -> Result<LaunchTab> {
        let profile = profile.trim();
        if profile.is_empty() {
            bail!("startup profile name is empty");
        }

        let mut tab = self.initial_tab(Some(profile))?;
        tab.split = split;
        Ok(tab)
    }

    fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }
}

struct TerminalStartupLayout<'a> {
    working_directory: Option<&'a Path>,
    command: Option<&'a str>,
    title: Option<&'a str>,
    shell: Option<&'a TerminalStartupShellConfig>,
    env: &'a HashMap<String, String>,
    tabs: &'a [TerminalStartupTabConfig],
    label: String,
}

fn main() {
    let command = match TerminalCliCommand::from_cli_and_config_file(Cli::parse()) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("failed to run zed terminal: {error:#}");
            process::exit(2);
        }
    };

    match &command {
        TerminalCliCommand::Doctor {
            path_options,
            format,
        } => {
            run_terminal_doctor(path_options.clone(), *format);
            return;
        }
        TerminalCliCommand::PrintStartupLayout {
            launch_options,
            format,
        } => {
            if let Err(error) = print_startup_layout(launch_options, *format) {
                eprintln!("failed to print terminal startup layout: {error:#}");
                process::exit(2);
            }
            return;
        }
        _ => {}
    }

    if let Err(error) = install_terminal_paths(command.path_options()) {
        eprintln!("failed to run zed terminal: {error:#}");
        process::exit(2);
    }

    match command {
        TerminalCliCommand::PrintPaths { format, .. } => {
            if let Err(error) = print_terminal_paths(format) {
                eprintln!("failed to print terminal paths: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::ListProfiles {
            startup_config,
            include_hidden,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profiles(&startup_config, include_hidden, format) {
                eprintln!("failed to list terminal startup profiles: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::DescribeProfile {
            startup_config,
            profile,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_description(&startup_config, &profile, format)
            {
                eprintln!("failed to describe terminal startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::DescribeStartup {
            startup_config,
            format,
            ..
        } => {
            if let Err(error) = print_startup_description(&startup_config, format) {
                eprintln!("failed to describe terminal startup config: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::ValidateStartupConfig {
            startup_config,
            format,
            ..
        } => {
            if let Err(error) = print_startup_config_validation(&startup_config, format) {
                eprintln!("failed to validate terminal startup config: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::PrintStartupLayout { .. } => {
            unreachable!("startup layout printing is handled before path install")
        }
        TerminalCliCommand::SetDefaultProfile {
            profile, format, ..
        } => {
            if let Err(error) = print_default_profile_update(&profile, format) {
                eprintln!("failed to set default startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::ClearDefaultProfile { format, .. } => {
            if let Err(error) = print_clear_default_profile_update(format) {
                eprintln!("failed to clear default startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::CreateProfile {
            profile,
            metadata,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_creation(&profile, &metadata, format) {
                eprintln!("failed to create startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::UpdateProfile {
            profile,
            update,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_metadata_update(&profile, &update, format) {
                eprintln!("failed to update startup profile metadata: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::UpdateProfileStartup {
            profile,
            update,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_startup_update(&profile, &update, format) {
                eprintln!("failed to update startup profile startup fields: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::UpdateStartup { update, format, .. } => {
            if let Err(error) = print_startup_update(&update, format) {
                eprintln!("failed to update root startup fields: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::UpdateStartupEnv { update, format, .. } => {
            if let Err(error) = print_startup_env_update(&update, format) {
                eprintln!("failed to update root startup environment variables: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::UpdateProfileEnv {
            profile,
            update,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_env_update(&profile, &update, format) {
                eprintln!("failed to update startup profile environment variables: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::CopyProfile {
            source_profile,
            target_profile,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_copy(&source_profile, &target_profile, format)
            {
                eprintln!("failed to copy startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::RemoveProfile {
            profile, format, ..
        } => {
            if let Err(error) = print_startup_profile_removal(&profile, format) {
                eprintln!("failed to remove startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::SetProfileVisibility {
            profile,
            hidden,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_visibility_update(&profile, hidden, format) {
                eprintln!("failed to update startup profile visibility: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::RenameProfile {
            old_profile,
            new_profile,
            format,
            ..
        } => {
            if let Err(error) = print_startup_profile_rename(&old_profile, &new_profile, format) {
                eprintln!("failed to rename startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::PrintStartupConfigSchema { .. } => {
            if let Err(error) = print_startup_config_schema() {
                eprintln!("failed to print terminal startup config schema: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::PrintDefaultKeymap { .. } => {
            print_default_keymap();
        }
        TerminalCliCommand::InitConfig { format, .. } => {
            if let Err(error) = print_config_initialization(format) {
                eprintln!("failed to initialize terminal config files: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::Doctor { .. } => unreachable!("doctor is handled before path install"),
        TerminalCliCommand::ValidateKeymap { format, .. } => run_keymap_validation(format),
        TerminalCliCommand::Launch(launch_options) => launch_terminal(launch_options),
    }
}

fn run_terminal_doctor(path_options: TerminalPathOptions, format: TerminalDoctorOutputFormat) {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            let report = diagnose_terminal(&path_options, cx);
            let output = match format {
                TerminalDoctorOutputFormat::Text => Ok(format_doctor_report(&report)),
                TerminalDoctorOutputFormat::Json => format_doctor_report_json(&report),
            };
            match output {
                Ok(output) => print!("{output}"),
                Err(error) => {
                    eprintln!("failed to format terminal doctor report: {error:#}");
                    cx.quit();
                    process::exit(2);
                }
            }
            io::stdout()
                .flush()
                .expect("failed to flush terminal doctor output");
            if report.has_errors() {
                cx.quit();
                process::exit(2);
            }
            cx.quit();
        });
}

fn run_keymap_validation(format: TerminalKeymapValidationOutputFormat) {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            match keymap_validation_report(paths::keymap_file(), cx)
                .and_then(|report| format_keymap_validation_report(&report, format))
            {
                Ok(output) => {
                    print!("{output}");
                    io::stdout()
                        .flush()
                        .expect("failed to flush keymap validation output");
                }
                Err(error) => {
                    eprintln!("failed to validate terminal keymap: {error:#}");
                    io::stderr().flush().ok();
                    process::exit(2);
                }
            }
            cx.quit();
        });
}

fn format_keymap_validation_report(
    report: &TerminalKeymapValidationReport,
    format: TerminalKeymapValidationOutputFormat,
) -> Result<String> {
    match format {
        TerminalKeymapValidationOutputFormat::Text => Ok(format_keymap_validation(report)),
        TerminalKeymapValidationOutputFormat::Json => format_keymap_validation_json(report),
    }
}

fn keymap_validation_report(
    keymap_file: &Path,
    cx: &mut App,
) -> Result<TerminalKeymapValidationReport> {
    Ok(TerminalKeymapValidationReport {
        keymap_file: keymap_file.to_path_buf(),
        validation: validate_keymaps(keymap_file, cx)?,
    })
}

fn launch_terminal(launch_options: LaunchOptions) {
    init_terminal_logging();

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            if let Err(error) = init(launch_options.clone(), cx) {
                eprintln!("failed to start zed terminal: {error:#}");
                cx.quit();
            }
        });
}

fn install_terminal_paths(path_options: &TerminalPathOptions) -> Result<()> {
    paths::try_set_custom_data_dir_path(&path_options.data_dir).with_context(|| {
        format!(
            "failed to initialize data directory {}",
            path_options.data_dir.display()
        )
    })?;
    paths::try_set_custom_config_dir_path(&path_options.config_dir).with_context(|| {
        format!(
            "failed to initialize config directory {}",
            path_options.config_dir.display()
        )
    })?;

    Ok(())
}

fn init_terminal_logging() {
    zlog::init();
    env_logger::try_init().ok();

    if let Err(error) = std_fs::create_dir_all(paths::logs_dir()) {
        eprintln!("Could not create log directory: {error}... Defaulting to stderr");
        zlog::init_output_stderr();
        return;
    }

    if let Err(error) = zlog::init_output_file(terminal_log_file(), Some(terminal_old_log_file())) {
        eprintln!("Could not open log file: {error}... Defaulting to stderr");
        zlog::init_output_stderr();
    }
}

fn print_terminal_paths(format: TerminalPathsOutputFormat) -> Result<()> {
    let report = active_terminal_path_report();
    match format {
        TerminalPathsOutputFormat::Text => print!("{}", format_terminal_paths(&report)),
        TerminalPathsOutputFormat::Json => print!("{}", format_terminal_paths_json(&report)?),
    }
    Ok(())
}

fn active_terminal_path_report() -> TerminalPathReport {
    TerminalPathReport {
        config_dir: paths::config_dir().clone(),
        data_dir: paths::data_dir().clone(),
        logs_dir: paths::logs_dir().clone(),
        settings_file: paths::settings_file().clone(),
        startup_config_file: active_terminal_startup_config_file(),
        startup_config_schema_file: active_terminal_startup_config_schema_file(),
        global_settings_file: paths::global_settings_file().clone(),
        keymap_file: paths::keymap_file().clone(),
        default_keymap_reference_file: active_terminal_default_keymap_reference_file(),
        themes_dir: paths::themes_dir().clone(),
        log_file: terminal_log_file().clone(),
    }
}

fn print_startup_profiles(
    startup_config: &TerminalStartupConfig,
    include_hidden: bool,
    format: TerminalListProfilesOutputFormat,
) -> Result<()> {
    let startup_config_file = active_terminal_startup_config_file();
    match format {
        TerminalListProfilesOutputFormat::Text => print!("{}", {
            format_startup_profiles(startup_config, &startup_config_file, include_hidden)
        }),
        TerminalListProfilesOutputFormat::Json => {
            let report =
                startup_profile_list_report(startup_config, &startup_config_file, include_hidden);
            print!("{}", format_startup_profiles_json(&report)?)
        }
    }
    Ok(())
}

fn print_startup_profile_description(
    startup_config: &TerminalStartupConfig,
    profile: &str,
    format: TerminalDescribeProfileOutputFormat,
) -> Result<()> {
    let report = startup_profile_description_report(
        startup_config,
        &active_terminal_startup_config_file(),
        profile,
    )?;
    match format {
        TerminalDescribeProfileOutputFormat::Text => {
            print!("{}", format_startup_profile_description(&report))
        }
        TerminalDescribeProfileOutputFormat::Json => {
            print!("{}", format_startup_profile_description_json(&report)?)
        }
    }
    Ok(())
}

fn print_startup_description(
    startup_config: &TerminalStartupConfig,
    format: TerminalDescribeStartupOutputFormat,
) -> Result<()> {
    let report =
        startup_description_report(startup_config, &active_terminal_startup_config_file())?;
    match format {
        TerminalDescribeStartupOutputFormat::Text => {
            print!("{}", format_startup_description(&report))
        }
        TerminalDescribeStartupOutputFormat::Json => {
            print!("{}", format_startup_description_json(&report)?)
        }
    }
    Ok(())
}

fn print_startup_config_validation(
    startup_config: &TerminalStartupConfig,
    format: TerminalStartupConfigValidationOutputFormat,
) -> Result<()> {
    let report =
        startup_config_validation_report(startup_config, &active_terminal_startup_config_file())?;
    match format {
        TerminalStartupConfigValidationOutputFormat::Text => {
            print!("{}", format_startup_config_validation(&report))
        }
        TerminalStartupConfigValidationOutputFormat::Json => {
            print!("{}", format_startup_config_validation_json(&report)?)
        }
    }
    Ok(())
}

fn startup_config_validation_report(
    startup_config: &TerminalStartupConfig,
    startup_config_file: &Path,
) -> Result<TerminalStartupConfigValidationReport> {
    Ok(TerminalStartupConfigValidationReport {
        startup_config_file: startup_config_file.to_path_buf(),
        validation: startup_config.validate()?,
    })
}

fn print_startup_layout(
    launch_options: &LaunchOptions,
    format: TerminalStartupLayoutOutputFormat,
) -> Result<()> {
    let startup_config_file = terminal_startup_config_file(&launch_options.path_options.config_dir);
    match format {
        TerminalStartupLayoutOutputFormat::Text => {
            print!(
                "{}",
                format_startup_layout(launch_options, &startup_config_file)
            )
        }
        TerminalStartupLayoutOutputFormat::Json => {
            let report = startup_layout_report(launch_options, &startup_config_file);
            print!("{}", format_startup_layout_json(&report)?)
        }
    }
    Ok(())
}

fn print_startup_config_schema() -> Result<()> {
    print!("{}", format_startup_config_schema()?);
    Ok(())
}

fn print_default_keymap() {
    print!("{}", default_keymap_content());
}

fn print_config_initialization(format: TerminalConfigInitializationOutputFormat) -> Result<()> {
    let initialization = initialize_terminal_config_files()?;
    match format {
        TerminalConfigInitializationOutputFormat::Text => {
            print!("{}", format_config_initialization(&initialization))
        }
        TerminalConfigInitializationOutputFormat::Json => {
            print!("{}", format_config_initialization_json(&initialization)?)
        }
    }
    Ok(())
}

fn print_default_profile_update(
    profile: &str,
    format: TerminalDefaultProfileUpdateOutputFormat,
) -> Result<()> {
    let update = set_default_startup_profile(&active_terminal_startup_config_file(), profile)?;
    match format {
        TerminalDefaultProfileUpdateOutputFormat::Text => {
            print!("{}", format_default_profile_update(&update))
        }
        TerminalDefaultProfileUpdateOutputFormat::Json => {
            print!("{}", format_default_profile_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_clear_default_profile_update(
    format: TerminalDefaultProfileUpdateOutputFormat,
) -> Result<()> {
    let update = clear_default_startup_profile(&active_terminal_startup_config_file())?;
    match format {
        TerminalDefaultProfileUpdateOutputFormat::Text => {
            print!("{}", format_default_profile_update(&update))
        }
        TerminalDefaultProfileUpdateOutputFormat::Json => {
            print!("{}", format_default_profile_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_profile_creation(
    profile: &str,
    metadata: &TerminalStartupProfileCreationMetadata,
    format: TerminalStartupProfileCreationOutputFormat,
) -> Result<()> {
    let creation =
        create_startup_profile(&active_terminal_startup_config_file(), profile, metadata)?;
    match format {
        TerminalStartupProfileCreationOutputFormat::Text => {
            print!("{}", format_startup_profile_creation(&creation))
        }
        TerminalStartupProfileCreationOutputFormat::Json => {
            print!("{}", format_startup_profile_creation_json(&creation)?)
        }
    }
    Ok(())
}

fn print_startup_profile_metadata_update(
    profile: &str,
    update: &TerminalStartupProfileMetadataUpdateRequest,
    format: TerminalStartupProfileUpdateOutputFormat,
) -> Result<()> {
    let update =
        update_startup_profile_metadata(&active_terminal_startup_config_file(), profile, update)?;
    match format {
        TerminalStartupProfileUpdateOutputFormat::Text => {
            print!("{}", format_startup_profile_metadata_update(&update))
        }
        TerminalStartupProfileUpdateOutputFormat::Json => {
            print!("{}", format_startup_profile_metadata_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_profile_startup_update(
    profile: &str,
    update: &TerminalStartupProfileStartupUpdateRequest,
    format: TerminalStartupProfileStartupUpdateOutputFormat,
) -> Result<()> {
    let update =
        update_startup_profile_startup(&active_terminal_startup_config_file(), profile, update)?;
    match format {
        TerminalStartupProfileStartupUpdateOutputFormat::Text => {
            print!("{}", format_startup_profile_startup_update(&update))
        }
        TerminalStartupProfileStartupUpdateOutputFormat::Json => {
            print!("{}", format_startup_profile_startup_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_update(
    update: &TerminalStartupUpdateRequest,
    format: TerminalStartupUpdateOutputFormat,
) -> Result<()> {
    let update = update_root_startup(&active_terminal_startup_config_file(), update)?;
    match format {
        TerminalStartupUpdateOutputFormat::Text => print!("{}", format_startup_update(&update)),
        TerminalStartupUpdateOutputFormat::Json => {
            print!("{}", format_startup_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_env_update(
    update: &TerminalStartupEnvUpdateRequest,
    format: TerminalStartupEnvUpdateOutputFormat,
) -> Result<()> {
    let update = update_root_startup_env(&active_terminal_startup_config_file(), update)?;
    match format {
        TerminalStartupEnvUpdateOutputFormat::Text => {
            print!("{}", format_startup_env_update(&update))
        }
        TerminalStartupEnvUpdateOutputFormat::Json => {
            print!("{}", format_startup_env_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_profile_env_update(
    profile: &str,
    update: &TerminalStartupProfileEnvUpdateRequest,
    format: TerminalStartupProfileEnvUpdateOutputFormat,
) -> Result<()> {
    let update =
        update_startup_profile_env(&active_terminal_startup_config_file(), profile, update)?;
    match format {
        TerminalStartupProfileEnvUpdateOutputFormat::Text => {
            print!("{}", format_startup_profile_env_update(&update))
        }
        TerminalStartupProfileEnvUpdateOutputFormat::Json => {
            print!("{}", format_startup_profile_env_update_json(&update)?)
        }
    }
    Ok(())
}

fn print_startup_profile_copy(
    source_profile: &str,
    target_profile: &str,
    format: TerminalStartupProfileCopyOutputFormat,
) -> Result<()> {
    let copy = copy_startup_profile(
        &active_terminal_startup_config_file(),
        source_profile,
        target_profile,
    )?;
    match format {
        TerminalStartupProfileCopyOutputFormat::Text => {
            print!("{}", format_startup_profile_copy(&copy))
        }
        TerminalStartupProfileCopyOutputFormat::Json => {
            print!("{}", format_startup_profile_copy_json(&copy)?)
        }
    }
    Ok(())
}

fn print_startup_profile_removal(
    profile: &str,
    format: TerminalStartupProfileRemovalOutputFormat,
) -> Result<()> {
    let removal = remove_startup_profile(&active_terminal_startup_config_file(), profile)?;
    match format {
        TerminalStartupProfileRemovalOutputFormat::Text => {
            print!("{}", format_startup_profile_removal(&removal))
        }
        TerminalStartupProfileRemovalOutputFormat::Json => {
            print!("{}", format_startup_profile_removal_json(&removal)?)
        }
    }
    Ok(())
}

fn print_startup_profile_rename(
    old_profile: &str,
    new_profile: &str,
    format: TerminalStartupProfileRenameOutputFormat,
) -> Result<()> {
    let rename = rename_startup_profile(
        &active_terminal_startup_config_file(),
        old_profile,
        new_profile,
    )?;
    match format {
        TerminalStartupProfileRenameOutputFormat::Text => {
            print!("{}", format_startup_profile_rename(&rename))
        }
        TerminalStartupProfileRenameOutputFormat::Json => {
            print!("{}", format_startup_profile_rename_json(&rename)?)
        }
    }
    Ok(())
}

fn print_startup_profile_visibility_update(
    profile: &str,
    hidden: bool,
    format: TerminalStartupProfileVisibilityOutputFormat,
) -> Result<()> {
    let update =
        set_startup_profile_visibility(&active_terminal_startup_config_file(), profile, hidden)?;
    match format {
        TerminalStartupProfileVisibilityOutputFormat::Text => {
            print!("{}", format_startup_profile_visibility_update(&update))
        }
        TerminalStartupProfileVisibilityOutputFormat::Json => {
            print!(
                "{}",
                format_startup_profile_visibility_update_json(&update)?
            )
        }
    }
    Ok(())
}

fn initialize_terminal_config_files() -> Result<TerminalConfigInitialization> {
    initialize_terminal_config_files_at(active_terminal_config_file_paths())
}

fn active_terminal_config_file_paths() -> TerminalConfigFilePaths {
    TerminalConfigFilePaths::from_path_options(&TerminalPathOptions {
        data_dir: paths::data_dir().clone(),
        config_dir: paths::config_dir().clone(),
    })
}

struct TerminalConfigFilePaths {
    settings_file: PathBuf,
    global_settings_file: PathBuf,
    keymap_file: PathBuf,
    default_keymap_reference_file: PathBuf,
    startup_config_file: PathBuf,
    startup_config_schema_file: PathBuf,
}

impl TerminalConfigFilePaths {
    fn from_path_options(path_options: &TerminalPathOptions) -> Self {
        Self {
            settings_file: path_options.config_dir.join("settings.json"),
            global_settings_file: path_options.config_dir.join("global_settings.json"),
            keymap_file: path_options.config_dir.join("keymap.json"),
            default_keymap_reference_file: terminal_default_keymap_reference_file(
                &path_options.config_dir,
            ),
            startup_config_file: terminal_startup_config_file(&path_options.config_dir),
            startup_config_schema_file: terminal_startup_config_schema_file(
                &path_options.config_dir,
            ),
        }
    }
}

fn initialize_terminal_config_files_at(
    file_paths: TerminalConfigFilePaths,
) -> Result<TerminalConfigInitialization> {
    let startup_config_schema = format_startup_config_schema()?;
    Ok(TerminalConfigInitialization {
        files: vec![
            initialize_terminal_config_file(
                "settings_file",
                file_paths.settings_file,
                settings::initial_user_settings_content().as_ref(),
            )?,
            initialize_terminal_config_file(
                "global_settings_file",
                file_paths.global_settings_file,
                "{}\n",
            )?,
            initialize_terminal_config_file(
                "keymap_file",
                file_paths.keymap_file,
                settings::initial_keymap_content().as_ref(),
            )?,
            initialize_terminal_config_file(
                "default_keymap_reference_file",
                file_paths.default_keymap_reference_file,
                default_keymap_content(),
            )?,
            initialize_terminal_config_file(
                "startup_config_file",
                file_paths.startup_config_file,
                initial_terminal_startup_config_content(),
            )?,
            initialize_terminal_config_file(
                "startup_config_schema_file",
                file_paths.startup_config_schema_file,
                &startup_config_schema,
            )?,
        ],
    })
}

fn initialize_terminal_config_file(
    label: &'static str,
    path: PathBuf,
    content: &str,
) -> Result<TerminalConfigFileInitialization> {
    match std_fs::metadata(&path) {
        Ok(metadata) => {
            if metadata.is_file() {
                return Ok(TerminalConfigFileInitialization {
                    label,
                    path,
                    status: TerminalConfigFileInitializationStatus::Existing,
                });
            }
            bail!("{} exists but is not a file", path.display());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }

    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    match std_fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(content.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(TerminalConfigFileInitialization {
                label,
                path,
                status: TerminalConfigFileInitializationStatus::Created,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std_fs::metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if !metadata.is_file() {
                bail!("{} exists but is not a file", path.display());
            }
            Ok(TerminalConfigFileInitialization {
                label,
                path,
                status: TerminalConfigFileInitializationStatus::Existing,
            })
        }
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

impl TerminalStartupProfileCreationMetadata {
    fn normalized(&self) -> Self {
        Self {
            display_name: normalize_profile_text(self.display_name.as_deref()),
            description: normalize_profile_text(self.description.as_deref()),
            icon: normalize_profile_text(self.icon.as_deref()),
            color: normalize_profile_text(self.color.as_deref()),
            hidden: self.hidden,
        }
    }

    fn to_profile_config(&self) -> TerminalStartupProfileConfig {
        Self::normalized(self).into_profile_config()
    }

    fn into_profile_config(self) -> TerminalStartupProfileConfig {
        TerminalStartupProfileConfig {
            display_name: self.display_name,
            description: self.description,
            icon: self.icon,
            color: self.color,
            hidden: self.hidden,
            ..TerminalStartupProfileConfig::default()
        }
    }

    fn to_json_value(&self) -> serde_json::Value {
        let metadata = self.normalized();
        let mut object = serde_json::Map::new();
        if let Some(display_name) = metadata.display_name {
            object.insert(
                "display_name".into(),
                serde_json::Value::String(display_name),
            );
        }
        if let Some(description) = metadata.description {
            object.insert("description".into(), serde_json::Value::String(description));
        }
        if let Some(icon) = metadata.icon {
            object.insert("icon".into(), serde_json::Value::String(icon));
        }
        if let Some(color) = metadata.color {
            object.insert("color".into(), serde_json::Value::String(color));
        }
        if metadata.hidden {
            object.insert("hidden".into(), serde_json::Value::Bool(true));
        }
        serde_json::Value::Object(object)
    }
}

impl TerminalStartupProfileMetadataUpdateRequest {
    fn normalized(&self) -> Self {
        Self {
            display_name: normalized_profile_metadata_update_value(&self.display_name),
            description: normalized_profile_metadata_update_value(&self.description),
            icon: normalized_profile_metadata_update_value(&self.icon),
            color: normalized_profile_metadata_update_value(&self.color),
        }
    }

    fn ensure_requested(&self) -> Result<()> {
        if self.display_name.is_none()
            && self.description.is_none()
            && self.icon.is_none()
            && self.color.is_none()
        {
            bail!(
                "--update-profile requires at least one profile metadata flag: --profile-display-name, --profile-description, --profile-icon, --profile-color, --clear-profile-display-name, --clear-profile-description, --clear-profile-icon, or --clear-profile-color"
            );
        }
        Ok(())
    }
}

impl TerminalStartupProfileStartupUpdateRequest {
    fn normalized(&self) -> Self {
        Self {
            working_directory: self.working_directory.clone(),
            command: normalized_profile_metadata_update_value(&self.command),
            title: self
                .title
                .as_ref()
                .map(|title| normalize_terminal_title(title.as_deref())),
            shell: self.shell.clone(),
        }
    }

    fn ensure_requested(&self) -> Result<()> {
        if self.working_directory.is_none()
            && self.command.is_none()
            && self.title.is_none()
            && self.shell.is_none()
        {
            bail!(
                "--update-profile-startup requires at least one startup field flag: --profile-working-directory, --profile-command, --profile-title, --profile-shell, --clear-profile-working-directory, --clear-profile-command, --clear-profile-title, or --clear-profile-shell"
            );
        }
        Ok(())
    }
}

impl TerminalStartupUpdateRequest {
    fn normalized(&self) -> Self {
        Self {
            working_directory: self.working_directory.clone(),
            command: normalized_profile_metadata_update_value(&self.command),
            title: self
                .title
                .as_ref()
                .map(|title| normalize_terminal_title(title.as_deref())),
            shell: self.shell.clone(),
        }
    }

    fn ensure_requested(&self) -> Result<()> {
        if self.working_directory.is_none()
            && self.command.is_none()
            && self.title.is_none()
            && self.shell.is_none()
        {
            bail!(
                "--update-startup requires at least one startup field flag: --startup-working-directory, --startup-command, --startup-title, --startup-shell, --clear-startup-working-directory, --clear-startup-command, --clear-startup-title, or --clear-startup-shell"
            );
        }
        Ok(())
    }
}

impl TerminalStartupProfileEnvUpdateRequest {
    fn normalized(&self) -> Result<Self> {
        Ok(Self {
            set: self
                .set
                .iter()
                .map(|(key, value)| Ok((normalize_profile_env_key(key)?, value.clone())))
                .collect::<Result<Vec<_>>>()?,
            remove: self
                .remove
                .iter()
                .map(|key| normalize_profile_env_key(key))
                .collect::<Result<Vec<_>>>()?,
            clear: self.clear,
        })
    }

    fn ensure_requested(&self) -> Result<()> {
        if self.set.is_empty() && self.remove.is_empty() && !self.clear {
            bail!(
                "--update-profile-env requires at least one environment flag: --profile-env, --remove-profile-env, or --clear-profile-env"
            );
        }
        Ok(())
    }
}

impl TerminalStartupEnvUpdateRequest {
    fn normalized(&self) -> Result<Self> {
        Ok(Self {
            set: self
                .set
                .iter()
                .map(|(key, value)| Ok((normalize_startup_env_key(key)?, value.clone())))
                .collect::<Result<Vec<_>>>()?,
            remove: self
                .remove
                .iter()
                .map(|key| normalize_startup_env_key(key))
                .collect::<Result<Vec<_>>>()?,
            clear: self.clear,
        })
    }

    fn ensure_requested(&self) -> Result<()> {
        if self.set.is_empty() && self.remove.is_empty() && !self.clear {
            bail!(
                "--update-startup-env requires at least one environment flag: --startup-env, --remove-startup-env, or --clear-startup-env"
            );
        }
        Ok(())
    }
}

fn normalized_profile_metadata_update_value(
    value: &Option<Option<String>>,
) -> Option<Option<String>> {
    value
        .as_ref()
        .map(|value| normalize_profile_text(value.as_deref()))
}

fn profile_metadata_update_value(value: Option<&str>, clear: bool) -> Option<Option<String>> {
    if clear {
        Some(None)
    } else {
        value.map(|value| normalize_profile_text(Some(value)))
    }
}

fn terminal_command_update_value(
    value: Option<&str>,
    clear: bool,
    flag: &'static str,
) -> Result<Option<Option<String>>> {
    if clear {
        return Ok(Some(None));
    }

    let Some(value) = value else {
        return Ok(None);
    };
    let Some(command) = normalize_profile_text(Some(value)) else {
        return Ok(Some(None));
    };
    LaunchCommand::from_command_line(&command)
        .with_context(|| format!("failed to parse {flag}"))?;
    Ok(Some(Some(command)))
}

fn terminal_title_update_value(value: Option<&str>, clear: bool) -> Option<Option<String>> {
    if clear {
        Some(None)
    } else {
        value.map(|value| normalize_terminal_title(Some(value)))
    }
}

fn terminal_shell_update_value(
    program: Option<&str>,
    args: &[String],
    clear: bool,
    flag: &'static str,
) -> Result<Option<Option<TerminalStartupShellConfig>>> {
    if clear {
        return Ok(Some(None));
    }

    let Some(program) = program else {
        return Ok(None);
    };
    let program = normalize_terminal_shell_program(program)
        .with_context(|| format!("failed to parse {flag}"))?;
    let shell = if args.is_empty() {
        TerminalStartupShellConfig::Program(program)
    } else {
        TerminalStartupShellConfig::WithArguments(TerminalStartupShellWithArgumentsConfig {
            program,
            args: args.to_vec(),
        })
    };
    shell
        .to_shell()
        .with_context(|| format!("failed to parse {flag}"))?;
    Ok(Some(Some(shell)))
}

fn parse_profile_env_assignment(assignment: &str) -> Result<(String, String)> {
    let (key, value) = assignment
        .split_once('=')
        .with_context(|| "--profile-env requires KEY=VALUE")?;
    Ok((normalize_profile_env_key(key)?, value.into()))
}

fn parse_startup_env_assignment(assignment: &str) -> Result<(String, String)> {
    let (key, value) = assignment
        .split_once('=')
        .with_context(|| "--startup-env requires KEY=VALUE")?;
    Ok((normalize_startup_env_key(key)?, value.into()))
}

fn normalize_profile_env_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() {
        bail!("profile environment variable key is empty");
    }
    if key.contains('=') {
        bail!("profile environment variable key must not contain '='");
    }
    Ok(key.into())
}

fn normalize_startup_env_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() {
        bail!("startup environment variable key is empty");
    }
    if key.contains('=') {
        bail!("startup environment variable key must not contain '='");
    }
    Ok(key.into())
}

fn create_startup_profile(
    path: &Path,
    profile: &str,
    metadata: &TerminalStartupProfileCreationMetadata,
) -> Result<TerminalStartupProfileCreation> {
    let profile = normalize_startup_profile_name(profile)?;
    let metadata = metadata.normalized();
    let mut text = match std_fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            initial_terminal_startup_config_content().into()
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read terminal startup config {}", path.display())
            });
        }
    };

    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    if startup_config.profiles.contains_key(&profile) {
        bail!("startup profile already exists: {profile}");
    }

    let new_profile_config = metadata.to_profile_config();
    let mut updated_config = startup_config.clone();
    updated_config
        .profiles
        .insert(profile.clone(), new_profile_config);
    updated_config.validate().with_context(|| {
        format!(
            "refusing to create startup profile {profile:?} because it would make {} invalid",
            path.display()
        )
    })?;

    let new_value = metadata.to_json_value();
    let (range, replacement) = settings_json::replace_value_in_json_text(
        &text,
        &["profiles", profile.as_str()],
        settings_json::infer_json_indent_size(&text),
        Some(&new_value),
        None,
    );
    text.replace_range(range, &replacement);

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile creation produced unexpected content",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileCreation {
        path: path.to_path_buf(),
        profile,
        display_name: metadata.display_name,
        description: metadata.description,
        icon: metadata.icon,
        color: metadata.color,
        hidden: metadata.hidden,
        changed: true,
        total_profile_count: updated_config.profiles.len(),
    })
}

fn update_startup_profile_metadata(
    path: &Path,
    profile: &str,
    update: &TerminalStartupProfileMetadataUpdateRequest,
) -> Result<TerminalStartupProfileMetadataUpdate> {
    let update = update.normalized();
    update.ensure_requested()?;
    let profile = normalize_startup_profile_name(profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    let startup_profile = startup_config.profiles.get(&profile).with_context(|| {
        if startup_config.profiles.is_empty() {
            format!("startup profile not found: {profile}")
        } else {
            format!(
                "startup profile not found: {profile}. Available profiles: {}",
                startup_config.profile_names().join(", ")
            )
        }
    })?;

    let previous_display_name = startup_profile.display_name.clone();
    let previous_description = startup_profile.description.clone();
    let previous_icon = startup_profile.icon.clone();
    let previous_color = startup_profile.color.clone();

    let display_name = update
        .display_name
        .clone()
        .unwrap_or_else(|| previous_display_name.clone());
    let description = update
        .description
        .clone()
        .unwrap_or_else(|| previous_description.clone());
    let icon = update.icon.clone().unwrap_or_else(|| previous_icon.clone());
    let color = update
        .color
        .clone()
        .unwrap_or_else(|| previous_color.clone());

    if previous_display_name == display_name
        && previous_description == description
        && previous_icon == icon
        && previous_color == color
    {
        return Ok(TerminalStartupProfileMetadataUpdate {
            path: path.to_path_buf(),
            profile,
            previous_display_name,
            display_name,
            previous_description,
            description,
            previous_icon,
            icon,
            previous_color,
            color,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    let updated_profile = updated_config
        .profiles
        .get_mut(&profile)
        .expect("startup profile was already checked");
    updated_profile.display_name = display_name.clone();
    updated_profile.description = description.clone();
    updated_profile.icon = icon.clone();
    updated_profile.color = color.clone();
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update startup profile {profile:?} metadata because it would make {} invalid",
            path.display()
        )
    })?;

    if update.display_name.is_some() && previous_display_name != display_name {
        replace_startup_profile_metadata_field(&mut text, &profile, "display_name", &display_name);
    }
    if update.description.is_some() && previous_description != description {
        replace_startup_profile_metadata_field(&mut text, &profile, "description", &description);
    }
    if update.icon.is_some() && previous_icon != icon {
        replace_startup_profile_metadata_field(&mut text, &profile, "icon", &icon);
    }
    if update.color.is_some() && previous_color != color {
        replace_startup_profile_metadata_field(&mut text, &profile, "color", &color);
    }

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile metadata update produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileMetadataUpdate {
        path: path.to_path_buf(),
        profile,
        previous_display_name,
        display_name,
        previous_description,
        description,
        previous_icon,
        icon,
        previous_color,
        color,
        changed: true,
    })
}

fn update_startup_profile_startup(
    path: &Path,
    profile: &str,
    update: &TerminalStartupProfileStartupUpdateRequest,
) -> Result<TerminalStartupProfileStartupUpdate> {
    let update = update.normalized();
    update.ensure_requested()?;
    let profile = normalize_startup_profile_name(profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    let startup_profile = startup_config.profiles.get(&profile).with_context(|| {
        if startup_config.profiles.is_empty() {
            format!("startup profile not found: {profile}")
        } else {
            format!(
                "startup profile not found: {profile}. Available profiles: {}",
                startup_config.profile_names().join(", ")
            )
        }
    })?;

    let previous_working_directory = startup_profile.working_directory.clone();
    let previous_command = startup_profile.command.clone();
    let previous_title = startup_profile.title.clone();
    let previous_shell = startup_profile.shell.clone();

    let working_directory = update
        .working_directory
        .clone()
        .unwrap_or_else(|| previous_working_directory.clone());
    let mut command = update
        .command
        .clone()
        .unwrap_or_else(|| previous_command.clone());
    let title = update
        .title
        .clone()
        .unwrap_or_else(|| previous_title.clone());
    let mut shell = update
        .shell
        .clone()
        .unwrap_or_else(|| previous_shell.clone());

    if update.command.as_ref().is_some_and(Option::is_some) {
        shell = None;
    }
    if update.shell.as_ref().is_some_and(Option::is_some) {
        command = None;
    }

    if previous_working_directory == working_directory
        && previous_command == command
        && previous_title == title
        && previous_shell == shell
    {
        return Ok(TerminalStartupProfileStartupUpdate {
            path: path.to_path_buf(),
            profile,
            previous_working_directory,
            working_directory,
            previous_command,
            command,
            previous_title,
            title,
            previous_shell,
            shell,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    let updated_profile = updated_config
        .profiles
        .get_mut(&profile)
        .expect("startup profile was already checked");
    updated_profile.working_directory = working_directory.clone();
    updated_profile.command = command.clone();
    updated_profile.title = title.clone();
    updated_profile.shell = shell.clone();
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update startup profile {profile:?} startup fields because it would make {} invalid",
            path.display()
        )
    })?;

    if previous_working_directory != working_directory {
        replace_startup_profile_field(
            &mut text,
            &profile,
            "working_directory",
            working_directory
                .as_ref()
                .map(|path| path_to_json_value(path.as_path())),
        );
    }
    if previous_command != command {
        replace_startup_profile_field(
            &mut text,
            &profile,
            "command",
            command
                .as_ref()
                .map(|command| serde_json::Value::String(command.clone())),
        );
    }
    if previous_title != title {
        replace_startup_profile_field(
            &mut text,
            &profile,
            "title",
            title
                .as_ref()
                .map(|title| serde_json::Value::String(title.clone())),
        );
    }
    if previous_shell != shell {
        replace_startup_profile_field(
            &mut text,
            &profile,
            "shell",
            shell.as_ref().map(startup_shell_config_to_json_value),
        );
    }

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile startup update produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileStartupUpdate {
        path: path.to_path_buf(),
        profile,
        previous_working_directory,
        working_directory,
        previous_command,
        command,
        previous_title,
        title,
        previous_shell,
        shell,
        changed: true,
    })
}

fn update_root_startup(
    path: &Path,
    update: &TerminalStartupUpdateRequest,
) -> Result<TerminalStartupUpdate> {
    let update = update.normalized();
    update.ensure_requested()?;
    let mut created_from_initial = false;
    let mut text = match std_fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            created_from_initial = true;
            initial_terminal_startup_config_content().into()
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read terminal startup config {}", path.display())
            });
        }
    };
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;

    let previous_working_directory = startup_config.working_directory.clone();
    let previous_command = startup_config.command.clone();
    let previous_title = startup_config.title.clone();
    let previous_shell = startup_config.shell.clone();

    let working_directory = update
        .working_directory
        .clone()
        .unwrap_or_else(|| previous_working_directory.clone());
    let mut command = update
        .command
        .clone()
        .unwrap_or_else(|| previous_command.clone());
    let title = update
        .title
        .clone()
        .unwrap_or_else(|| previous_title.clone());
    let mut shell = update
        .shell
        .clone()
        .unwrap_or_else(|| previous_shell.clone());

    if update.command.as_ref().is_some_and(Option::is_some) {
        shell = None;
    }
    if update.shell.as_ref().is_some_and(Option::is_some) {
        command = None;
    }

    let fields_changed = previous_working_directory != working_directory
        || previous_command != command
        || previous_title != title
        || previous_shell != shell;
    if !fields_changed && !created_from_initial {
        return Ok(TerminalStartupUpdate {
            path: path.to_path_buf(),
            previous_working_directory,
            working_directory,
            previous_command,
            command,
            previous_title,
            title,
            previous_shell,
            shell,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    updated_config.working_directory = working_directory.clone();
    updated_config.command = command.clone();
    updated_config.title = title.clone();
    updated_config.shell = shell.clone();
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update root startup fields because it would make {} invalid",
            path.display()
        )
    })?;

    if previous_working_directory != working_directory {
        replace_startup_field(
            &mut text,
            "working_directory",
            working_directory
                .as_ref()
                .map(|path| path_to_json_value(path.as_path())),
        );
    }
    if previous_command != command {
        replace_startup_field(
            &mut text,
            "command",
            command
                .as_ref()
                .map(|command| serde_json::Value::String(command.clone())),
        );
    }
    if previous_title != title {
        replace_startup_field(
            &mut text,
            "title",
            title
                .as_ref()
                .map(|title| serde_json::Value::String(title.clone())),
        );
    }
    if previous_shell != shell {
        replace_startup_field(
            &mut text,
            "shell",
            shell.as_ref().map(startup_shell_config_to_json_value),
        );
    }

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because root startup update produced unexpected content",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupUpdate {
        path: path.to_path_buf(),
        previous_working_directory,
        working_directory,
        previous_command,
        command,
        previous_title,
        title,
        previous_shell,
        shell,
        changed: fields_changed || created_from_initial,
    })
}

fn update_root_startup_env(
    path: &Path,
    update: &TerminalStartupEnvUpdateRequest,
) -> Result<TerminalStartupEnvUpdate> {
    let update = update.normalized()?;
    update.ensure_requested()?;
    let mut created_from_initial = false;
    let mut text = match std_fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            created_from_initial = true;
            initial_terminal_startup_config_content().into()
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read terminal startup config {}", path.display())
            });
        }
    };
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;

    let previous_env = startup_config.env.clone();
    let mut env = if update.clear {
        HashMap::default()
    } else {
        previous_env.clone()
    };
    for key in &update.remove {
        env.remove(key);
    }
    for (key, value) in &update.set {
        env.insert(key.clone(), value.clone());
    }

    let previous_env_keys = sorted_env_keys(&previous_env);
    let env_keys = sorted_env_keys(&env);
    let previous_key_set = previous_env_keys.iter().cloned().collect::<BTreeSet<_>>();
    let env_key_set = env_keys.iter().cloned().collect::<BTreeSet<_>>();
    let added_env_keys = env_key_set
        .difference(&previous_key_set)
        .cloned()
        .collect::<Vec<_>>();
    let removed_env_keys = previous_key_set
        .difference(&env_key_set)
        .cloned()
        .collect::<Vec<_>>();
    let updated_env_keys = previous_key_set
        .intersection(&env_key_set)
        .filter(|key| previous_env.get(*key) != env.get(*key))
        .cloned()
        .collect::<Vec<_>>();
    let cleared = update.clear && !previous_env.is_empty();

    if previous_env == env && !created_from_initial {
        return Ok(TerminalStartupEnvUpdate {
            path: path.to_path_buf(),
            previous_env_keys,
            env_keys,
            added_env_keys,
            updated_env_keys,
            removed_env_keys,
            cleared,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    updated_config.env = env.clone();
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update root startup environment variables because it would make {} invalid",
            path.display()
        )
    })?;

    if previous_env != env {
        replace_startup_field(
            &mut text,
            "env",
            if env.is_empty() {
                None
            } else {
                Some(env_to_json_value(&env))
            },
        );
    }

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because root environment update produced unexpected content",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupEnvUpdate {
        path: path.to_path_buf(),
        previous_env_keys,
        env_keys,
        added_env_keys,
        updated_env_keys,
        removed_env_keys,
        cleared,
        changed: previous_env != env || created_from_initial,
    })
}

fn update_startup_profile_env(
    path: &Path,
    profile: &str,
    update: &TerminalStartupProfileEnvUpdateRequest,
) -> Result<TerminalStartupProfileEnvUpdate> {
    let update = update.normalized()?;
    update.ensure_requested()?;
    let profile = normalize_startup_profile_name(profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    let startup_profile = startup_config.profiles.get(&profile).with_context(|| {
        if startup_config.profiles.is_empty() {
            format!("startup profile not found: {profile}")
        } else {
            format!(
                "startup profile not found: {profile}. Available profiles: {}",
                startup_config.profile_names().join(", ")
            )
        }
    })?;

    let previous_env = startup_profile.env.clone();
    let mut env = if update.clear {
        HashMap::default()
    } else {
        previous_env.clone()
    };
    for key in &update.remove {
        env.remove(key);
    }
    for (key, value) in &update.set {
        env.insert(key.clone(), value.clone());
    }

    let previous_env_keys = sorted_env_keys(&previous_env);
    let env_keys = sorted_env_keys(&env);
    let previous_key_set = previous_env_keys.iter().cloned().collect::<BTreeSet<_>>();
    let env_key_set = env_keys.iter().cloned().collect::<BTreeSet<_>>();
    let added_env_keys = env_key_set
        .difference(&previous_key_set)
        .cloned()
        .collect::<Vec<_>>();
    let removed_env_keys = previous_key_set
        .difference(&env_key_set)
        .cloned()
        .collect::<Vec<_>>();
    let updated_env_keys = previous_key_set
        .intersection(&env_key_set)
        .filter(|key| previous_env.get(*key) != env.get(*key))
        .cloned()
        .collect::<Vec<_>>();
    let cleared = update.clear && !previous_env.is_empty();

    if previous_env == env {
        return Ok(TerminalStartupProfileEnvUpdate {
            path: path.to_path_buf(),
            profile,
            previous_env_keys,
            env_keys,
            added_env_keys,
            updated_env_keys,
            removed_env_keys,
            cleared,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    let updated_profile = updated_config
        .profiles
        .get_mut(&profile)
        .expect("startup profile was already checked");
    updated_profile.env = env.clone();
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update startup profile {profile:?} environment variables because it would make {} invalid",
            path.display()
        )
    })?;

    replace_startup_profile_field(
        &mut text,
        &profile,
        "env",
        if env.is_empty() {
            None
        } else {
            Some(env_to_json_value(&env))
        },
    );

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile environment update produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileEnvUpdate {
        path: path.to_path_buf(),
        profile,
        previous_env_keys,
        env_keys,
        added_env_keys,
        updated_env_keys,
        removed_env_keys,
        cleared,
        changed: true,
    })
}

fn copy_startup_profile(
    path: &Path,
    source_profile: &str,
    target_profile: &str,
) -> Result<TerminalStartupProfileCopy> {
    let source_profile = normalize_startup_profile_name(source_profile)?;
    let target_profile = normalize_startup_profile_name(target_profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;

    let mut copied_profile = startup_config
        .profiles
        .get(&source_profile)
        .with_context(|| {
            if startup_config.profiles.is_empty() {
                format!("startup profile not found: {source_profile}")
            } else {
                format!(
                    "startup profile not found: {source_profile}. Available profiles: {}",
                    startup_config.profile_names().join(", ")
                )
            }
        })?
        .clone();
    if startup_config.profiles.contains_key(&target_profile) {
        bail!("startup profile already exists: {target_profile}");
    }

    rename_startup_tab_profile_references(
        &mut copied_profile.tabs,
        &source_profile,
        &target_profile,
    );
    let copied_tab_count = 1 + copied_profile.tabs.len();

    let mut updated_config = startup_config.clone();
    updated_config
        .profiles
        .insert(target_profile.clone(), copied_profile.clone());
    updated_config.validate().with_context(|| {
        format!(
            "refusing to copy startup profile {source_profile:?} to {target_profile:?} because it would make {} invalid",
            path.display()
        )
    })?;

    let new_value = startup_profile_config_to_json_value(&copied_profile);
    let (range, replacement) = settings_json::replace_value_in_json_text(
        &text,
        &["profiles", target_profile.as_str()],
        settings_json::infer_json_indent_size(&text),
        Some(&new_value),
        None,
    );
    text.replace_range(range, &replacement);

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile copy produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileCopy {
        path: path.to_path_buf(),
        source_profile,
        profile: target_profile,
        changed: true,
        copied_tab_count,
        total_profile_count: updated_config.profiles.len(),
    })
}

fn startup_profile_config_to_json_value(
    profile: &TerminalStartupProfileConfig,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if let Some(display_name) = &profile.display_name {
        object.insert(
            "display_name".into(),
            serde_json::Value::String(display_name.clone()),
        );
    }
    if let Some(description) = &profile.description {
        object.insert(
            "description".into(),
            serde_json::Value::String(description.clone()),
        );
    }
    if let Some(icon) = &profile.icon {
        object.insert("icon".into(), serde_json::Value::String(icon.clone()));
    }
    if let Some(color) = &profile.color {
        object.insert("color".into(), serde_json::Value::String(color.clone()));
    }
    if profile.hidden {
        object.insert("hidden".into(), serde_json::Value::Bool(true));
    }
    if let Some(working_directory) = &profile.working_directory {
        object.insert(
            "working_directory".into(),
            path_to_json_value(working_directory),
        );
    }
    if let Some(command) = &profile.command {
        object.insert("command".into(), serde_json::Value::String(command.clone()));
    }
    if let Some(title) = &profile.title {
        object.insert("title".into(), serde_json::Value::String(title.clone()));
    }
    if let Some(shell) = &profile.shell {
        object.insert("shell".into(), startup_shell_config_to_json_value(shell));
    }
    if !profile.env.is_empty() {
        object.insert("env".into(), env_to_json_value(&profile.env));
    }
    if !profile.tabs.is_empty() {
        object.insert(
            "tabs".into(),
            serde_json::Value::Array(
                profile
                    .tabs
                    .iter()
                    .map(startup_tab_config_to_json_value)
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(object)
}

fn startup_tab_config_to_json_value(tab: &TerminalStartupTabConfig) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if let Some(profile) = &tab.profile {
        object.insert("profile".into(), serde_json::Value::String(profile.clone()));
    }
    if let Some(working_directory) = &tab.working_directory {
        object.insert(
            "working_directory".into(),
            path_to_json_value(working_directory),
        );
    }
    if let Some(command) = &tab.command {
        object.insert("command".into(), serde_json::Value::String(command.clone()));
    }
    if let Some(title) = &tab.title {
        object.insert("title".into(), serde_json::Value::String(title.clone()));
    }
    if let Some(shell) = &tab.shell {
        object.insert("shell".into(), startup_shell_config_to_json_value(shell));
    }
    if !tab.env.is_empty() {
        object.insert("env".into(), env_to_json_value(&tab.env));
    }
    if let Some(split) = tab.split {
        object.insert(
            "split".into(),
            serde_json::Value::String(split.as_str().into()),
        );
    }
    serde_json::Value::Object(object)
}

fn startup_shell_config_to_json_value(shell: &TerminalStartupShellConfig) -> serde_json::Value {
    match shell {
        TerminalStartupShellConfig::Program(program) => serde_json::Value::String(program.clone()),
        TerminalStartupShellConfig::WithArguments(config) => {
            let mut object = serde_json::Map::new();
            object.insert(
                "program".into(),
                serde_json::Value::String(config.program.clone()),
            );
            if !config.args.is_empty() {
                object.insert(
                    "args".into(),
                    serde_json::Value::Array(
                        config
                            .args
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }
            serde_json::Value::Object(object)
        }
    }
}

fn env_to_json_value(env: &HashMap<String, String>) -> serde_json::Value {
    let mut entries = env.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut object = serde_json::Map::new();
    for (key, value) in entries {
        object.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    serde_json::Value::Object(object)
}

fn sorted_env_keys(env: &HashMap<String, String>) -> Vec<String> {
    let mut keys = env.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn path_to_json_value(path: &Path) -> serde_json::Value {
    serde_json::Value::String(path.to_string_lossy().into_owned())
}

fn replace_startup_profile_metadata_field(
    text: &mut String,
    profile: &str,
    field: &str,
    value: &Option<String>,
) {
    let new_value = value
        .as_ref()
        .map(|value| serde_json::Value::String(value.clone()));
    let (range, replacement) = settings_json::replace_value_in_json_text(
        text,
        &["profiles", profile, field],
        settings_json::infer_json_indent_size(text),
        new_value.as_ref(),
        None,
    );
    text.replace_range(range, &replacement);
}

fn replace_startup_profile_field(
    text: &mut String,
    profile: &str,
    field: &str,
    value: Option<serde_json::Value>,
) {
    let (range, replacement) = settings_json::replace_value_in_json_text(
        text,
        &["profiles", profile, field],
        settings_json::infer_json_indent_size(text),
        value.as_ref(),
        None,
    );
    text.replace_range(range, &replacement);
}

fn replace_startup_field(text: &mut String, field: &str, value: Option<serde_json::Value>) {
    let (range, replacement) = settings_json::replace_value_in_json_text(
        text,
        &[field],
        settings_json::infer_json_indent_size(text),
        value.as_ref(),
        None,
    );
    text.replace_range(range, &replacement);
}

fn set_default_startup_profile(path: &Path, profile: &str) -> Result<TerminalDefaultProfileUpdate> {
    let profile = normalize_startup_profile_name(profile)?;
    update_default_startup_profile(path, Some(profile))
}

fn clear_default_startup_profile(path: &Path) -> Result<TerminalDefaultProfileUpdate> {
    update_default_startup_profile(path, None)
}

fn remove_startup_profile(path: &Path, profile: &str) -> Result<TerminalStartupProfileRemoval> {
    let profile = normalize_startup_profile_name(profile)?;
    let mut text = match std_fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TerminalStartupProfileRemoval {
                path: path.to_path_buf(),
                profile,
                changed: false,
                remaining_profile_count: 0,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read terminal startup config {}", path.display())
            });
        }
    };
    let mut startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;

    if startup_config.profiles.remove(&profile).is_none() {
        return Ok(TerminalStartupProfileRemoval {
            path: path.to_path_buf(),
            profile,
            changed: false,
            remaining_profile_count: startup_config.profiles.len(),
        });
    }

    startup_config.validate().with_context(|| {
        format!(
            "refusing to remove startup profile {profile:?} because it would make {} invalid",
            path.display()
        )
    })?;

    let indent_size = settings_json::infer_json_indent_size(&text);
    let (range, replacement) = settings_json::replace_value_in_json_text(
        &text,
        &["profiles", profile.as_str()],
        indent_size,
        None,
        None,
    );
    text.replace_range(range, &replacement);

    let updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileRemoval {
        path: path.to_path_buf(),
        profile,
        changed: true,
        remaining_profile_count: startup_config.profiles.len(),
    })
}

fn rename_startup_profile(
    path: &Path,
    old_profile: &str,
    new_profile: &str,
) -> Result<TerminalStartupProfileRename> {
    let old_profile = normalize_startup_profile_name(old_profile)?;
    let new_profile = normalize_startup_profile_name(new_profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;

    if old_profile == new_profile {
        startup_config.validate_profile_reference("startup profile", &old_profile)?;
        return Ok(TerminalStartupProfileRename {
            path: path.to_path_buf(),
            previous_profile: old_profile,
            profile: new_profile,
            changed: false,
            updated_reference_count: 0,
        });
    }

    let mut updated_config = startup_config.clone();
    let profile = updated_config
        .profiles
        .remove(&old_profile)
        .with_context(|| {
            if updated_config.profiles.is_empty() {
                format!("startup profile not found: {old_profile}")
            } else {
                format!(
                    "startup profile not found: {old_profile}. Available profiles: {}",
                    updated_config.profile_names().join(", ")
                )
            }
        })?;
    if updated_config.profiles.contains_key(&new_profile) {
        bail!("startup profile already exists: {new_profile}");
    }
    updated_config.profiles.insert(new_profile.clone(), profile);
    let updated_reference_count =
        rename_startup_profile_references(&mut updated_config, &old_profile, &new_profile);

    updated_config.validate().with_context(|| {
        format!(
            "refusing to rename startup profile {old_profile:?} to {new_profile:?} because it would make {} invalid",
            path.display()
        )
    })?;

    let (range, replacement) = settings_json::replace_key_in_json_text(
        &text,
        &["profiles", old_profile.as_str()],
        &new_profile,
    )
    .with_context(|| {
        format!(
            "failed to find startup profile {old_profile:?} in {}",
            path.display()
        )
    })?;
    text.replace_range(range, &replacement);

    let new_profile_value = serde_json::Value::String(new_profile.clone());
    if startup_config.default_profile.as_deref() == Some(old_profile.as_str()) {
        let (range, replacement) = settings_json::replace_value_in_json_text(
            &text,
            &["default_profile"],
            settings_json::infer_json_indent_size(&text),
            Some(&new_profile_value),
            None,
        );
        text.replace_range(range, &replacement);
    }

    for path in startup_profile_reference_paths(&startup_config, &old_profile, &new_profile) {
        let (range, replacement) = settings_json::replace_value_in_json_text(
            &text,
            &path,
            settings_json::infer_json_indent_size(&text),
            Some(&new_profile_value),
            None,
        );
        text.replace_range(range, &replacement);
    }

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile rename produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileRename {
        path: path.to_path_buf(),
        previous_profile: old_profile,
        profile: new_profile,
        changed: true,
        updated_reference_count,
    })
}

fn set_startup_profile_visibility(
    path: &Path,
    profile: &str,
    hidden: bool,
) -> Result<TerminalStartupProfileVisibilityUpdate> {
    let profile = normalize_startup_profile_name(profile)?;
    let mut text = std_fs::read_to_string(path)
        .with_context(|| format!("failed to read terminal startup config {}", path.display()))?;
    let startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    let startup_profile = startup_config.profiles.get(&profile).with_context(|| {
        if startup_config.profiles.is_empty() {
            format!("startup profile not found: {profile}")
        } else {
            format!(
                "startup profile not found: {profile}. Available profiles: {}",
                startup_config.profile_names().join(", ")
            )
        }
    })?;
    let previous_hidden = startup_profile.hidden;

    if previous_hidden == hidden {
        return Ok(TerminalStartupProfileVisibilityUpdate {
            path: path.to_path_buf(),
            profile,
            previous_hidden,
            hidden,
            changed: false,
        });
    }

    let mut updated_config = startup_config.clone();
    updated_config
        .profiles
        .get_mut(&profile)
        .expect("startup profile was already checked")
        .hidden = hidden;
    updated_config.validate().with_context(|| {
        format!(
            "refusing to update startup profile {profile:?} visibility because it would make {} invalid",
            path.display()
        )
    })?;

    let new_value = serde_json::Value::Bool(hidden);
    let (range, replacement) = settings_json::replace_value_in_json_text(
        &text,
        &["profiles", profile.as_str(), "hidden"],
        settings_json::infer_json_indent_size(&text),
        Some(&new_value),
        None,
    );
    text.replace_range(range, &replacement);

    let parsed_updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    parsed_updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;
    if parsed_updated_config != updated_config {
        bail!(
            "refusing to write terminal startup config {} because profile visibility update produced unexpected content",
            path.display()
        );
    }

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalStartupProfileVisibilityUpdate {
        path: path.to_path_buf(),
        profile,
        previous_hidden,
        hidden,
        changed: true,
    })
}

fn rename_startup_profile_references(
    config: &mut TerminalStartupConfig,
    old_profile: &str,
    new_profile: &str,
) -> usize {
    let mut count = 0;
    if config.default_profile.as_deref() == Some(old_profile) {
        config.default_profile = Some(new_profile.into());
        count += 1;
    }
    count += rename_startup_tab_profile_references(&mut config.tabs, old_profile, new_profile);
    for profile in config.profiles.values_mut() {
        count += rename_startup_tab_profile_references(&mut profile.tabs, old_profile, new_profile);
    }
    count
}

fn rename_startup_tab_profile_references(
    tabs: &mut [TerminalStartupTabConfig],
    old_profile: &str,
    new_profile: &str,
) -> usize {
    let mut count = 0;
    for tab in tabs {
        if tab.profile.as_deref() == Some(old_profile) {
            tab.profile = Some(new_profile.into());
            count += 1;
        }
    }
    count
}

fn startup_profile_reference_paths(
    config: &TerminalStartupConfig,
    old_profile: &str,
    new_profile: &str,
) -> Vec<Vec<String>> {
    let mut paths = Vec::new();
    for (index, tab) in config.tabs.iter().enumerate() {
        if tab.profile.as_deref() == Some(old_profile) {
            paths.push(vec!["tabs".into(), format!("#{index}"), "profile".into()]);
        }
    }
    for (profile_name, profile) in &config.profiles {
        let profile_key = if profile_name == old_profile {
            new_profile
        } else {
            profile_name.as_str()
        };
        for (index, tab) in profile.tabs.iter().enumerate() {
            if tab.profile.as_deref() == Some(old_profile) {
                paths.push(vec![
                    "profiles".into(),
                    profile_key.into(),
                    "tabs".into(),
                    format!("#{index}"),
                    "profile".into(),
                ]);
            }
        }
    }
    paths
}

fn update_default_startup_profile(
    path: &Path,
    default_profile: Option<String>,
) -> Result<TerminalDefaultProfileUpdate> {
    let mut text = match std_fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && default_profile.is_none() => {
            return Ok(TerminalDefaultProfileUpdate {
                path: path.to_path_buf(),
                previous_profile: None,
                default_profile: None,
                changed: false,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to read terminal startup config {}", path.display())
            });
        }
    };
    let mut startup_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| format!("failed to parse terminal startup config {}", path.display()))?;
    if let Some(default_profile) = default_profile.as_deref() {
        startup_config.validate_profile_reference("default_profile", default_profile)?;
    }

    let previous_profile = startup_config.default_profile.clone();
    startup_config.default_profile = default_profile.clone();
    startup_config.validate().with_context(|| {
        format!(
            "refusing to write invalid terminal startup config {}",
            path.display()
        )
    })?;

    let indent_size = settings_json::infer_json_indent_size(&text);
    let new_value = default_profile
        .clone()
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    let (range, replacement) = settings_json::replace_value_in_json_text(
        &text,
        &["default_profile"],
        indent_size,
        Some(&new_value),
        None,
    );
    text.replace_range(range, &replacement);

    let updated_config = settings::parse_json_with_comments::<TerminalStartupConfig>(&text)
        .with_context(|| {
            format!(
                "failed to parse updated terminal startup config {}",
                path.display()
            )
        })?;
    updated_config.validate().with_context(|| {
        format!(
            "refusing to write invalid updated terminal startup config {}",
            path.display()
        )
    })?;

    std_fs::write(path, text)
        .with_context(|| format!("failed to write terminal startup config {}", path.display()))?;

    Ok(TerminalDefaultProfileUpdate {
        path: path.to_path_buf(),
        changed: previous_profile != default_profile,
        previous_profile,
        default_profile,
    })
}

fn diagnose_terminal(path_options: &TerminalPathOptions, cx: &mut App) -> TerminalDoctorReport {
    let file_paths = TerminalConfigFilePaths::from_path_options(path_options);
    let startup_config_file = file_paths.startup_config_file.clone();
    let keymap_file = file_paths.keymap_file.clone();

    TerminalDoctorReport {
        directories: diagnose_terminal_directories(path_options),
        config_files: diagnose_terminal_config_files(file_paths),
        startup_config: diagnose_startup_config_file(startup_config_file),
        keymap: diagnose_keymap(keymap_file, cx),
    }
}

fn diagnose_terminal_directories(
    path_options: &TerminalPathOptions,
) -> Vec<TerminalDoctorPathCheck> {
    vec![
        diagnose_path(
            "data_dir",
            path_options.data_dir.clone(),
            TerminalDoctorPathKind::Directory,
        ),
        diagnose_path(
            "config_dir",
            path_options.config_dir.clone(),
            TerminalDoctorPathKind::Directory,
        ),
        diagnose_path(
            "logs_dir",
            path_options.data_dir.join("logs"),
            TerminalDoctorPathKind::Directory,
        ),
        diagnose_path(
            "themes_dir",
            path_options.config_dir.join("themes"),
            TerminalDoctorPathKind::Directory,
        ),
    ]
}

fn diagnose_terminal_config_files(
    file_paths: TerminalConfigFilePaths,
) -> Vec<TerminalDoctorPathCheck> {
    vec![
        diagnose_path(
            "settings_file",
            file_paths.settings_file,
            TerminalDoctorPathKind::File,
        ),
        diagnose_path(
            "global_settings_file",
            file_paths.global_settings_file,
            TerminalDoctorPathKind::File,
        ),
        diagnose_path(
            "keymap_file",
            file_paths.keymap_file,
            TerminalDoctorPathKind::File,
        ),
        diagnose_path(
            "default_keymap_reference_file",
            file_paths.default_keymap_reference_file,
            TerminalDoctorPathKind::File,
        ),
        diagnose_path(
            "startup_config_file",
            file_paths.startup_config_file,
            TerminalDoctorPathKind::File,
        ),
        diagnose_path(
            "startup_config_schema_file",
            file_paths.startup_config_schema_file,
            TerminalDoctorPathKind::File,
        ),
    ]
}

fn diagnose_path(
    label: &'static str,
    path: PathBuf,
    expected_kind: TerminalDoctorPathKind,
) -> TerminalDoctorPathCheck {
    match std_fs::metadata(&path) {
        Ok(metadata) if matches_expected_path_kind(&metadata, expected_kind) => {
            TerminalDoctorPathCheck {
                label,
                path,
                status: TerminalDoctorCheckStatus::Ok,
                message: None,
            }
        }
        Ok(_) => TerminalDoctorPathCheck {
            label,
            path,
            status: TerminalDoctorCheckStatus::Error,
            message: Some(match expected_kind {
                TerminalDoctorPathKind::Directory => "expected a directory".into(),
                TerminalDoctorPathKind::File => "expected a file".into(),
            }),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => TerminalDoctorPathCheck {
            label,
            path,
            status: TerminalDoctorCheckStatus::Missing,
            message: None,
        },
        Err(error) => TerminalDoctorPathCheck {
            label,
            path,
            status: TerminalDoctorCheckStatus::Error,
            message: Some(format!("failed to inspect path: {error}")),
        },
    }
}

fn matches_expected_path_kind(
    metadata: &std_fs::Metadata,
    expected_kind: TerminalDoctorPathKind,
) -> bool {
    match expected_kind {
        TerminalDoctorPathKind::Directory => metadata.is_dir(),
        TerminalDoctorPathKind::File => metadata.is_file(),
    }
}

fn diagnose_startup_config_file(path: PathBuf) -> TerminalDoctorStartupConfigCheck {
    match std_fs::metadata(&path) {
        Ok(metadata) if !metadata.is_file() => TerminalDoctorStartupConfigCheck {
            path,
            status: TerminalDoctorCheckStatus::Error,
            source: None,
            validation: None,
            message: Some("expected a file".into()),
        },
        Ok(_) => match TerminalStartupConfig::load(&path).and_then(|config| config.validate()) {
            Ok(validation) => TerminalDoctorStartupConfigCheck {
                path,
                status: TerminalDoctorCheckStatus::Ok,
                source: Some(TerminalDoctorConfigSource::File),
                validation: Some(validation),
                message: None,
            },
            Err(error) => TerminalDoctorStartupConfigCheck {
                path,
                status: TerminalDoctorCheckStatus::Error,
                source: Some(TerminalDoctorConfigSource::File),
                validation: None,
                message: Some(format!("{error:#}")),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let validation = TerminalStartupConfig::default()
                .validate()
                .expect("default startup config should validate");
            TerminalDoctorStartupConfigCheck {
                path,
                status: TerminalDoctorCheckStatus::Missing,
                source: Some(TerminalDoctorConfigSource::Initial),
                validation: Some(validation),
                message: None,
            }
        }
        Err(error) => TerminalDoctorStartupConfigCheck {
            path,
            status: TerminalDoctorCheckStatus::Error,
            source: None,
            validation: None,
            message: Some(format!("failed to inspect startup config: {error}")),
        },
    }
}

fn diagnose_keymap(path: PathBuf, cx: &mut App) -> TerminalDoctorKeymapCheck {
    let source = match std_fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(TerminalUserKeymapSource::File),
        Ok(_) => {
            return TerminalDoctorKeymapCheck {
                path,
                status: TerminalDoctorCheckStatus::Error,
                source: None,
                validation: None,
                message: Some("expected a file".into()),
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Some(TerminalUserKeymapSource::Initial)
        }
        Err(error) => {
            return TerminalDoctorKeymapCheck {
                path,
                status: TerminalDoctorCheckStatus::Error,
                source: None,
                validation: None,
                message: Some(format!("failed to inspect keymap: {error}")),
            };
        }
    };

    match validate_keymaps(&path, cx) {
        Ok(validation) => TerminalDoctorKeymapCheck {
            path,
            status: if validation.user_keymap_source == TerminalUserKeymapSource::Initial {
                TerminalDoctorCheckStatus::Missing
            } else {
                TerminalDoctorCheckStatus::Ok
            },
            source,
            validation: Some(validation),
            message: None,
        },
        Err(error) => TerminalDoctorKeymapCheck {
            path,
            status: TerminalDoctorCheckStatus::Error,
            source,
            validation: None,
            message: Some(format!("{error:#}")),
        },
    }
}

fn validate_keymaps(keymap_file: &Path, cx: &mut App) -> Result<TerminalKeymapValidation> {
    let default_binding_count =
        KeymapFile::load_asset(TERMINAL_KEYMAP_PATH, Some(KeybindSource::Default), cx)
            .context("failed to validate zed terminal default keymap")?
            .len();
    let (user_keymap_content, user_keymap_source) = read_user_keymap_content(keymap_file)?;
    let user_binding_count =
        load_keymap_content_for_validation("terminal keymap file", &user_keymap_content, cx)?;

    Ok(TerminalKeymapValidation {
        default_binding_count,
        user_binding_count,
        user_keymap_source,
    })
}

fn read_user_keymap_content(keymap_file: &Path) -> Result<(String, TerminalUserKeymapSource)> {
    match std_fs::read_to_string(keymap_file) {
        Ok(content) => Ok((content, TerminalUserKeymapSource::File)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            settings::initial_keymap_content().into_owned(),
            TerminalUserKeymapSource::Initial,
        )),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read terminal keymap {}", keymap_file.display())),
    }
}

fn load_keymap_content_for_validation(label: &str, content: &str, cx: &App) -> Result<usize> {
    match KeymapFile::load(content, cx) {
        KeymapFileLoadResult::Success { key_bindings } => Ok(key_bindings.len()),
        KeymapFileLoadResult::SomeFailedToLoad { error_message, .. } => {
            bail!("{label} has errors: {}", error_message.0)
        }
        KeymapFileLoadResult::JsonParseFailure { error } => {
            Err(error).with_context(|| format!("failed to parse {label}"))
        }
    }
}

fn format_startup_profiles(
    startup_config: &TerminalStartupConfig,
    startup_config_file: &Path,
    include_hidden: bool,
) -> String {
    let report = startup_profile_list_report(startup_config, startup_config_file, include_hidden);
    format_startup_profiles_report(&report)
}

fn startup_profile_list_report(
    startup_config: &TerminalStartupConfig,
    startup_config_file: &Path,
    include_hidden: bool,
) -> TerminalStartupProfileListReport {
    let profiles = startup_config.profile_summaries(include_hidden);
    let hidden_count = startup_config
        .profiles
        .values()
        .filter(|profile| profile.hidden)
        .count();
    let visible_count = startup_config.profiles.len() - hidden_count;

    TerminalStartupProfileListReport {
        startup_config_file: startup_config_file.to_path_buf(),
        include_hidden,
        total_count: startup_config.profiles.len(),
        visible_count,
        hidden_count,
        profiles,
    }
}

fn startup_profile_description_report(
    startup_config: &TerminalStartupConfig,
    startup_config_file: &Path,
    profile: &str,
) -> Result<TerminalStartupProfileDescription> {
    let profile = normalize_startup_profile_name(profile)?;
    if !startup_config_file.is_file() {
        bail!(
            "failed to read terminal startup config {}",
            startup_config_file.display()
        );
    }
    startup_config.validate().with_context(|| {
        format!(
            "failed to validate terminal startup config {}",
            startup_config_file.display()
        )
    })?;
    let startup_profile = startup_config.profiles.get(&profile).with_context(|| {
        if startup_config.profiles.is_empty() {
            format!("startup profile not found: {profile}")
        } else {
            format!(
                "startup profile not found: {profile}. Available profiles: {}",
                startup_config.profile_names().join(", ")
            )
        }
    })?;

    Ok(TerminalStartupProfileDescription {
        startup_config_file: startup_config_file.to_path_buf(),
        profile: profile.clone(),
        display_name: startup_profile.display_name.clone(),
        description: startup_profile.description.clone(),
        icon: startup_profile.icon.clone(),
        color: startup_profile.color.clone(),
        hidden: startup_profile.hidden,
        is_default: startup_config.default_profile.as_deref() == Some(profile.as_str()),
        working_directory: startup_profile.working_directory.clone(),
        command: startup_profile.command.clone(),
        title: startup_profile.title.clone(),
        shell: startup_profile.shell.clone(),
        env_keys: sorted_env_keys(&startup_profile.env),
        tabs: startup_profile
            .tabs
            .iter()
            .map(startup_profile_tab_description)
            .collect(),
    })
}

fn startup_description_report(
    startup_config: &TerminalStartupConfig,
    startup_config_file: &Path,
) -> Result<TerminalStartupDescription> {
    let source = match std_fs::metadata(startup_config_file) {
        Ok(metadata) if metadata.is_file() => TerminalDoctorConfigSource::File,
        Ok(_) => bail!(
            "failed to read terminal startup config {}: expected a file",
            startup_config_file.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            TerminalDoctorConfigSource::Initial
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect terminal startup config {}",
                    startup_config_file.display()
                )
            });
        }
    };

    startup_config.validate().with_context(|| {
        format!(
            "failed to validate terminal startup config {}",
            startup_config_file.display()
        )
    })?;

    let profile_count = startup_config.profiles.len();
    let hidden_profile_count = startup_config
        .profiles
        .values()
        .filter(|profile| profile.hidden)
        .count();
    let visible_profile_count = profile_count - hidden_profile_count;

    Ok(TerminalStartupDescription {
        startup_config_file: startup_config_file.to_path_buf(),
        source,
        working_directory: startup_config.working_directory.clone(),
        command: startup_config.command.clone(),
        title: startup_config.title.clone(),
        shell: startup_config.shell.clone(),
        env_keys: sorted_env_keys(&startup_config.env),
        tabs: startup_config
            .tabs
            .iter()
            .map(startup_profile_tab_description)
            .collect(),
        default_profile: startup_config.default_profile.clone(),
        profile_count,
        visible_profile_count,
        hidden_profile_count,
    })
}

fn startup_profile_tab_description(
    tab: &TerminalStartupTabConfig,
) -> TerminalStartupProfileTabDescription {
    TerminalStartupProfileTabDescription {
        profile: tab.profile.clone(),
        working_directory: tab.working_directory.clone(),
        command: tab.command.clone(),
        title: tab.title.clone(),
        shell: tab.shell.clone(),
        env_keys: sorted_env_keys(&tab.env),
        split: tab.split,
    }
}

fn format_startup_profiles_report(report: &TerminalStartupProfileListReport) -> String {
    let mut output = String::new();

    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");

    if report.profiles.is_empty() {
        if report.total_count == 0 {
            writeln!(&mut output, "No startup profiles configured.")
                .expect("writing to string should not fail");
        } else if report.include_hidden {
            writeln!(&mut output, "No startup profiles configured.")
                .expect("writing to string should not fail");
        } else {
            writeln!(
                &mut output,
                "No visible startup profiles configured. Use --all-profiles to include hidden profiles."
            )
            .expect("writing to string should not fail");
        }
        return output;
    }

    writeln!(
        &mut output,
        "profiles: {} visible, {} hidden",
        report.visible_count, report.hidden_count
    )
    .expect("writing to string should not fail");

    for profile in &report.profiles {
        let mut badges = Vec::new();
        if profile.is_default {
            badges.push("default");
        }
        if profile.hidden {
            badges.push("hidden");
        }
        let badges = if badges.is_empty() {
            String::new()
        } else {
            format!(" ({})", badges.join(", "))
        };

        writeln!(&mut output, "- {}{}", profile.name, badges)
            .expect("writing to string should not fail");
        writeln!(&mut output, "  display_name: {}", profile.display_name)
            .expect("writing to string should not fail");
        if let Some(description) = &profile.description {
            writeln!(&mut output, "  description: {description}")
                .expect("writing to string should not fail");
        }
        if let Some(icon) = &profile.icon {
            writeln!(&mut output, "  icon: {icon}").expect("writing to string should not fail");
        }
        if let Some(color) = &profile.color {
            writeln!(&mut output, "  color: {color}").expect("writing to string should not fail");
        }
        writeln!(&mut output, "  tabs: {}", profile.tab_count)
            .expect("writing to string should not fail");
    }

    output
}

fn format_startup_profiles_json(report: &TerminalStartupProfileListReport) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": report.startup_config_file.display().to_string(),
        "include_hidden": report.include_hidden,
        "total_count": report.total_count,
        "visible_count": report.visible_count,
        "hidden_count": report.hidden_count,
        "profiles": report
            .profiles
            .iter()
            .map(startup_profile_summary_json)
            .collect::<Vec<_>>(),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profiles as json")?;
    output.push('\n');
    Ok(output)
}

fn startup_profile_summary_json(profile: &TerminalStartupProfileSummary) -> serde_json::Value {
    serde_json::json!({
        "name": profile.name.as_str(),
        "display_name": profile.display_name.as_str(),
        "description": profile.description.as_deref(),
        "icon": profile.icon.as_deref(),
        "color": profile.color.as_deref(),
        "hidden": profile.hidden,
        "is_default": profile.is_default,
        "tab_count": profile.tab_count,
    })
}

fn format_startup_profile_description(report: &TerminalStartupProfileDescription) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", report.profile)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "display_name: {}",
        report.display_name.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "description: {}",
        report.description.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "icon: {}",
        report.icon.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "color: {}",
        report.color.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "hidden: {}", report.hidden).expect("writing to string should not fail");
    writeln!(&mut output, "is_default: {}", report.is_default)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "working_directory: {}",
        report
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "command: {}",
        report.command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "title: {}",
        report.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "shell: {}",
        format_startup_shell_config(report.shell.as_ref())
    )
    .expect("writing to string should not fail");
    format_env_key_list(&mut output, "", &report.env_keys);
    writeln!(&mut output, "tabs: {}", report.tabs.len())
        .expect("writing to string should not fail");
    for (index, tab) in report.tabs.iter().enumerate() {
        format_startup_profile_tab_description(&mut output, index + 1, tab);
    }
    output
}

fn format_startup_description(report: &TerminalStartupDescription) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "source: {}", report.source.as_str())
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "working_directory: {}",
        report
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "command: {}",
        report.command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "title: {}",
        report.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "shell: {}",
        format_startup_shell_config(report.shell.as_ref())
    )
    .expect("writing to string should not fail");
    format_env_key_list(&mut output, "", &report.env_keys);
    writeln!(&mut output, "tabs: {}", report.tabs.len())
        .expect("writing to string should not fail");
    for (index, tab) in report.tabs.iter().enumerate() {
        format_startup_profile_tab_description(&mut output, index + 1, tab);
    }
    writeln!(
        &mut output,
        "default_profile: {}",
        report.default_profile.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "profiles: {} visible, {} hidden",
        report.visible_profile_count, report.hidden_profile_count
    )
    .expect("writing to string should not fail");
    output
}

fn format_startup_profile_tab_description(
    output: &mut String,
    tab_number: usize,
    tab: &TerminalStartupProfileTabDescription,
) {
    writeln!(output, "- tab {tab_number}").expect("writing to string should not fail");
    writeln!(
        output,
        "  profile: {}",
        tab.profile.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "  working_directory: {}",
        tab.working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "  command: {}",
        tab.command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "  title: {}",
        tab.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "  shell: {}",
        format_startup_shell_config(tab.shell.as_ref())
    )
    .expect("writing to string should not fail");
    format_env_key_list(output, "  ", &tab.env_keys);
    writeln!(
        output,
        "  split: {}",
        tab.split
            .map(TerminalStartupSplitDirection::as_str)
            .unwrap_or("tab")
    )
    .expect("writing to string should not fail");
}

fn format_env_key_list(output: &mut String, prefix: &str, env_keys: &[String]) {
    format_env_key_list_with_label(output, prefix, "env", env_keys);
}

fn format_env_key_list_with_label(
    output: &mut String,
    prefix: &str,
    label: &str,
    env_keys: &[String],
) {
    writeln!(output, "{prefix}{label}: {} variables", env_keys.len())
        .expect("writing to string should not fail");
    for key in env_keys {
        writeln!(output, "{prefix}  - {key}").expect("writing to string should not fail");
    }
}

fn format_startup_profile_description_json(
    report: &TerminalStartupProfileDescription,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": report.startup_config_file.display().to_string(),
        "status": "ok",
        "profile": report.profile.as_str(),
        "display_name": report.display_name.as_deref(),
        "description": report.description.as_deref(),
        "icon": report.icon.as_deref(),
        "color": report.color.as_deref(),
        "hidden": report.hidden,
        "is_default": report.is_default,
        "working_directory": report
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "command": report.command.as_deref(),
        "title": report.title.as_deref(),
        "shell": startup_shell_config_description_json(report.shell.as_ref()),
        "env_count": report.env_keys.len(),
        "env_keys": &report.env_keys,
        "tab_count": report.tabs.len(),
        "tabs": report
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| startup_profile_tab_description_json(index + 1, tab))
            .collect::<Vec<_>>(),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile description as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_description_json(report: &TerminalStartupDescription) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": report.startup_config_file.display().to_string(),
        "status": "ok",
        "source": report.source.as_str(),
        "working_directory": report
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "command": report.command.as_deref(),
        "title": report.title.as_deref(),
        "shell": startup_shell_config_description_json(report.shell.as_ref()),
        "env_count": report.env_keys.len(),
        "env_keys": &report.env_keys,
        "tab_count": report.tabs.len(),
        "tabs": report
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| startup_profile_tab_description_json(index + 1, tab))
            .collect::<Vec<_>>(),
        "default_profile": report.default_profile.as_deref(),
        "profile_count": report.profile_count,
        "visible_profile_count": report.visible_profile_count,
        "hidden_profile_count": report.hidden_profile_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup description as json")?;
    output.push('\n');
    Ok(output)
}

fn startup_profile_tab_description_json(
    tab_number: usize,
    tab: &TerminalStartupProfileTabDescription,
) -> serde_json::Value {
    serde_json::json!({
        "tab": tab_number,
        "profile": tab.profile.as_deref(),
        "working_directory": tab
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "command": tab.command.as_deref(),
        "title": tab.title.as_deref(),
        "shell": startup_shell_config_description_json(tab.shell.as_ref()),
        "env_count": tab.env_keys.len(),
        "env_keys": &tab.env_keys,
        "split": tab.split.map(TerminalStartupSplitDirection::as_str),
    })
}

fn format_startup_shell_config(shell: Option<&TerminalStartupShellConfig>) -> String {
    match shell {
        None => "default".into(),
        Some(TerminalStartupShellConfig::Program(program)) => format_command_part(program),
        Some(TerminalStartupShellConfig::WithArguments(config)) => {
            std::iter::once(config.program.as_str())
                .chain(config.args.iter().map(String::as_str))
                .map(format_command_part)
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

fn startup_shell_config_description_json(
    shell: Option<&TerminalStartupShellConfig>,
) -> serde_json::Value {
    match shell {
        None => serde_json::json!({
            "kind": "default",
            "program": null,
            "args": [],
            "label": "default",
        }),
        Some(TerminalStartupShellConfig::Program(program)) => serde_json::json!({
            "kind": "program",
            "program": program.as_str(),
            "args": [],
            "label": format_startup_shell_config(shell),
        }),
        Some(TerminalStartupShellConfig::WithArguments(config)) => serde_json::json!({
            "kind": "with_arguments",
            "program": config.program.as_str(),
            "args": &config.args,
            "label": format_startup_shell_config(shell),
        }),
    }
}

fn format_startup_layout(launch_options: &LaunchOptions, startup_config_file: &Path) -> String {
    let report = startup_layout_report(launch_options, startup_config_file);
    format_startup_layout_report(&report)
}

fn startup_layout_report(
    launch_options: &LaunchOptions,
    startup_config_file: &Path,
) -> TerminalStartupLayoutReport {
    TerminalStartupLayoutReport {
        startup_config_file: startup_config_file.to_path_buf(),
        new_terminal_tab: startup_layout_tab_report(&launch_options.new_terminal_tab),
        tabs: std::iter::once(&launch_options.initial_tab)
            .chain(launch_options.additional_tabs.iter())
            .map(startup_layout_tab_report)
            .collect(),
    }
}

fn startup_layout_tab_report(tab: &LaunchTab) -> TerminalStartupLayoutTabReport {
    TerminalStartupLayoutTabReport {
        kind: if tab.command.is_some() {
            TerminalStartupLayoutTabKind::Command
        } else {
            TerminalStartupLayoutTabKind::Shell
        },
        placement: tab
            .split
            .map(TerminalStartupLayoutPlacement::Split)
            .unwrap_or(TerminalStartupLayoutPlacement::Tab),
        title: tab.title.clone(),
        working_directory: tab.working_directory.clone(),
        command: tab.command.clone(),
        shell: tab.shell.clone(),
        env_count: tab.env.len(),
    }
}

fn format_startup_layout_report(report: &TerminalStartupLayoutReport) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "tabs: {}", report.tabs.len())
        .expect("writing to string should not fail");
    writeln!(&mut output, "new_terminal_tab:").expect("writing to string should not fail");
    format_startup_layout_tab_body(&mut output, "  ", &report.new_terminal_tab);

    for (index, tab) in report.tabs.iter().enumerate() {
        format_startup_layout_tab(&mut output, index + 1, tab);
    }

    output
}

fn format_startup_layout_tab(
    output: &mut String,
    tab_number: usize,
    tab: &TerminalStartupLayoutTabReport,
) {
    writeln!(output, "- tab {tab_number}").expect("writing to string should not fail");
    format_startup_layout_tab_body(output, "  ", tab);
}

fn format_startup_layout_tab_body(
    output: &mut String,
    prefix: &str,
    tab: &TerminalStartupLayoutTabReport,
) {
    writeln!(output, "{prefix}kind: {}", tab.kind.as_str())
        .expect("writing to string should not fail");
    writeln!(
        output,
        "{prefix}placement: {}",
        tab.placement.display_label()
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "{prefix}title: {}",
        tab.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        output,
        "{prefix}working_directory: {}",
        tab.working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");

    if let Some(command) = &tab.command {
        writeln!(output, "{prefix}command: {}", command.display_label())
            .expect("writing to string should not fail");
    } else {
        writeln!(
            output,
            "{prefix}shell: {}",
            format_optional_shell(tab.shell.as_ref())
        )
        .expect("writing to string should not fail");
    }

    writeln!(output, "{prefix}env: {} variables", tab.env_count)
        .expect("writing to string should not fail");
}

fn format_startup_layout_json(report: &TerminalStartupLayoutReport) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": report.startup_config_file.display().to_string(),
        "status": "ok",
        "tab_count": report.tabs.len(),
        "new_terminal_tab": startup_layout_tab_json(&report.new_terminal_tab, None),
        "tabs": report
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| startup_layout_tab_json(tab, Some(index + 1)))
            .collect::<Vec<_>>(),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup layout as json")?;
    output.push('\n');
    Ok(output)
}

fn startup_layout_tab_json(
    tab: &TerminalStartupLayoutTabReport,
    tab_number: Option<usize>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "kind": tab.kind.as_str(),
        "placement": tab.placement.kind(),
        "split_direction": tab.placement.split_direction().map(TerminalStartupSplitDirection::as_str),
        "title": tab.title.as_deref(),
        "working_directory": tab
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "command": tab.command.as_ref().map(startup_layout_command_json),
        "shell": if tab.kind == TerminalStartupLayoutTabKind::Shell {
            Some(startup_layout_shell_json(tab.shell.as_ref()))
        } else {
            None
        },
        "env_count": tab.env_count,
    });
    if let Some(tab_number) = tab_number {
        value
            .as_object_mut()
            .expect("startup layout tab json should be an object")
            .insert("tab".into(), serde_json::json!(tab_number));
    }
    value
}

fn startup_layout_command_json(command: &LaunchCommand) -> serde_json::Value {
    serde_json::json!({
        "program": command.program.as_str(),
        "args": &command.args,
        "label": command.display_label(),
    })
}

fn startup_layout_shell_json(shell: Option<&Shell>) -> serde_json::Value {
    match shell {
        None => serde_json::json!({
            "kind": "default",
            "program": null,
            "args": [],
            "label": "default",
        }),
        Some(Shell::System) => serde_json::json!({
            "kind": "system",
            "program": null,
            "args": [],
            "label": "system",
        }),
        Some(shell @ Shell::Program(program)) => serde_json::json!({
            "kind": "program",
            "program": program.as_str(),
            "args": [],
            "label": format_shell(shell),
        }),
        Some(shell @ Shell::WithArguments { program, args, .. }) => serde_json::json!({
            "kind": "with_arguments",
            "program": program.as_str(),
            "args": args,
            "label": format_shell(shell),
        }),
    }
}

fn format_optional_shell(shell: Option<&Shell>) -> String {
    shell.map(format_shell).unwrap_or_else(|| "default".into())
}

fn format_shell(shell: &Shell) -> String {
    match shell {
        Shell::System => "system".into(),
        Shell::Program(program) => format_command_part(program),
        Shell::WithArguments { program, args, .. } => std::iter::once(program.as_str())
            .chain(args.iter().map(String::as_str))
            .map(format_command_part)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn format_startup_config_validation(report: &TerminalStartupConfigValidationReport) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "layouts: {}", report.validation.layout_count)
        .expect("writing to string should not fail");
    writeln!(&mut output, "tabs: {}", report.validation.tab_count)
        .expect("writing to string should not fail");
    output
}

fn format_startup_config_validation_json(
    report: &TerminalStartupConfigValidationReport,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": report.startup_config_file.display().to_string(),
        "status": "ok",
        "layout_count": report.validation.layout_count,
        "tab_count": report.validation.tab_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup config validation as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_config_schema() -> Result<String> {
    let schema = schemars::schema_for!(TerminalStartupConfig);
    let mut output = serde_json::to_string_pretty(&schema)
        .context("failed to serialize terminal startup config schema")?;
    output.push('\n');
    Ok(output)
}

fn write_startup_config_schema_file(path: &Path) -> Result<()> {
    let schema = format_startup_config_schema()?;
    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    std_fs::write(path, schema)
        .with_context(|| format!("failed to write startup config schema {}", path.display()))
}

fn default_keymap_content() -> &'static str {
    include_str!("../../../assets/keymaps/zed-terminal.json")
}

fn write_default_keymap_reference_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    std_fs::write(path, default_keymap_content()).with_context(|| {
        format!(
            "failed to write default keymap reference {}",
            path.display()
        )
    })
}

fn format_config_initialization(initialization: &TerminalConfigInitialization) -> String {
    let mut output = String::new();
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    for file in &initialization.files {
        writeln!(
            &mut output,
            "{}: {} {}",
            file.label,
            file.status.as_str(),
            file.path.display()
        )
        .expect("writing to string should not fail");
    }
    output
}

fn format_config_initialization_json(
    initialization: &TerminalConfigInitialization,
) -> Result<String> {
    let created_count = initialization
        .files
        .iter()
        .filter(|file| file.status == TerminalConfigFileInitializationStatus::Created)
        .count();
    let existing_count = initialization
        .files
        .iter()
        .filter(|file| file.status == TerminalConfigFileInitializationStatus::Existing)
        .count();
    let value = serde_json::json!({
        "status": "ok",
        "file_count": initialization.files.len(),
        "created_count": created_count,
        "existing_count": existing_count,
        "files": initialization
            .files
            .iter()
            .map(|file| serde_json::json!({
                "label": file.label,
                "path": file.path.display().to_string(),
                "status": file.status.as_str(),
            }))
            .collect::<Vec<_>>(),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal config initialization as json")?;
    output.push('\n');
    Ok(output)
}

fn format_terminal_paths(report: &TerminalPathReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "config_dir: {}", report.config_dir.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "data_dir: {}", report.data_dir.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "logs_dir: {}", report.logs_dir.display())
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "settings_file: {}",
        report.settings_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "startup_config_file: {}",
        report.startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "startup_config_schema_file: {}",
        report.startup_config_schema_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "global_settings_file: {}",
        report.global_settings_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "keymap_file: {}", report.keymap_file.display())
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "default_keymap_reference_file: {}",
        report.default_keymap_reference_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "themes_dir: {}", report.themes_dir.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "log_file: {}", report.log_file.display())
        .expect("writing to string should not fail");
    output
}

fn format_terminal_paths_json(report: &TerminalPathReport) -> Result<String> {
    let value = serde_json::json!({
        "config_dir": report.config_dir.display().to_string(),
        "data_dir": report.data_dir.display().to_string(),
        "logs_dir": report.logs_dir.display().to_string(),
        "settings_file": report.settings_file.display().to_string(),
        "startup_config_file": report.startup_config_file.display().to_string(),
        "startup_config_schema_file": report.startup_config_schema_file.display().to_string(),
        "global_settings_file": report.global_settings_file.display().to_string(),
        "keymap_file": report.keymap_file.display().to_string(),
        "default_keymap_reference_file": report.default_keymap_reference_file.display().to_string(),
        "themes_dir": report.themes_dir.display().to_string(),
        "log_file": report.log_file.display().to_string(),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal paths as json")?;
    output.push('\n');
    Ok(output)
}

fn format_default_profile_update(update: &TerminalDefaultProfileUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_default_profile: {}",
        update.previous_profile.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "default_profile: {}",
        update.default_profile.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_default_profile_update_json(update: &TerminalDefaultProfileUpdate) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "previous_default_profile": update.previous_profile.as_deref(),
        "default_profile": update.default_profile.as_deref(),
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal default profile update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_creation(creation: &TerminalStartupProfileCreation) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        creation.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", creation.profile)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "display_name: {}",
        creation.display_name.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "description: {}",
        creation.description.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "icon: {}",
        creation.icon.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "color: {}",
        creation.color.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "hidden: {}", creation.hidden)
        .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", creation.changed)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "total_profiles: {}",
        creation.total_profile_count
    )
    .expect("writing to string should not fail");
    output
}

fn format_startup_profile_creation_json(
    creation: &TerminalStartupProfileCreation,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": creation.path.display().to_string(),
        "status": "ok",
        "profile": creation.profile.as_str(),
        "display_name": creation.display_name.as_deref(),
        "description": creation.description.as_deref(),
        "icon": creation.icon.as_deref(),
        "color": creation.color.as_deref(),
        "hidden": creation.hidden,
        "changed": creation.changed,
        "total_profile_count": creation.total_profile_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile creation as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_metadata_update(update: &TerminalStartupProfileMetadataUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", update.profile)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_display_name: {}",
        update.previous_display_name.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "display_name: {}",
        update.display_name.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_description: {}",
        update.previous_description.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "description: {}",
        update.description.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_icon: {}",
        update.previous_icon.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "icon: {}",
        update.icon.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_color: {}",
        update.previous_color.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "color: {}",
        update.color.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_profile_metadata_update_json(
    update: &TerminalStartupProfileMetadataUpdate,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "profile": update.profile.as_str(),
        "previous_display_name": update.previous_display_name.as_deref(),
        "display_name": update.display_name.as_deref(),
        "previous_description": update.previous_description.as_deref(),
        "description": update.description.as_deref(),
        "previous_icon": update.previous_icon.as_deref(),
        "icon": update.icon.as_deref(),
        "previous_color": update.previous_color.as_deref(),
        "color": update.color.as_deref(),
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile metadata update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_startup_update(update: &TerminalStartupProfileStartupUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", update.profile)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_working_directory: {}",
        update
            .previous_working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "working_directory: {}",
        update
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_command: {}",
        update.previous_command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "command: {}",
        update.command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_title: {}",
        update.previous_title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "title: {}",
        update.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_shell: {}",
        format_startup_shell_config(update.previous_shell.as_ref())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "shell: {}",
        format_startup_shell_config(update.shell.as_ref())
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_profile_startup_update_json(
    update: &TerminalStartupProfileStartupUpdate,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "profile": update.profile.as_str(),
        "previous_working_directory": update
            .previous_working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "working_directory": update
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "previous_command": update.previous_command.as_deref(),
        "command": update.command.as_deref(),
        "previous_title": update.previous_title.as_deref(),
        "title": update.title.as_deref(),
        "previous_shell": startup_shell_config_description_json(update.previous_shell.as_ref()),
        "shell": startup_shell_config_description_json(update.shell.as_ref()),
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile startup update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_update(update: &TerminalStartupUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_working_directory: {}",
        update
            .previous_working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "working_directory: {}",
        update
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".into())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_command: {}",
        update.previous_command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "command: {}",
        update.command.as_deref().unwrap_or("none")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_title: {}",
        update.previous_title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "title: {}",
        update.title.as_deref().unwrap_or("dynamic")
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "previous_shell: {}",
        format_startup_shell_config(update.previous_shell.as_ref())
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "shell: {}",
        format_startup_shell_config(update.shell.as_ref())
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_update_json(update: &TerminalStartupUpdate) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "previous_working_directory": update
            .previous_working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "working_directory": update
            .working_directory
            .as_ref()
            .map(|path| path.display().to_string()),
        "previous_command": update.previous_command.as_deref(),
        "command": update.command.as_deref(),
        "previous_title": update.previous_title.as_deref(),
        "title": update.title.as_deref(),
        "previous_shell": startup_shell_config_description_json(update.previous_shell.as_ref()),
        "shell": startup_shell_config_description_json(update.shell.as_ref()),
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_env_update(update: &TerminalStartupEnvUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    format_env_key_list_with_label(&mut output, "", "previous_env", &update.previous_env_keys);
    format_env_key_list(&mut output, "", &update.env_keys);
    format_env_key_list_with_label(&mut output, "", "added_env_keys", &update.added_env_keys);
    format_env_key_list_with_label(
        &mut output,
        "",
        "updated_env_keys",
        &update.updated_env_keys,
    );
    format_env_key_list_with_label(
        &mut output,
        "",
        "removed_env_keys",
        &update.removed_env_keys,
    );
    writeln!(&mut output, "cleared: {}", update.cleared)
        .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_env_update_json(update: &TerminalStartupEnvUpdate) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "previous_env_count": update.previous_env_keys.len(),
        "previous_env_keys": &update.previous_env_keys,
        "env_count": update.env_keys.len(),
        "env_keys": &update.env_keys,
        "added_env_keys": &update.added_env_keys,
        "updated_env_keys": &update.updated_env_keys,
        "removed_env_keys": &update.removed_env_keys,
        "cleared": update.cleared,
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup environment update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_env_update(update: &TerminalStartupProfileEnvUpdate) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", update.profile)
        .expect("writing to string should not fail");
    format_env_key_list_with_label(&mut output, "", "previous_env", &update.previous_env_keys);
    format_env_key_list(&mut output, "", &update.env_keys);
    format_env_key_list_with_label(&mut output, "", "added_env_keys", &update.added_env_keys);
    format_env_key_list_with_label(
        &mut output,
        "",
        "updated_env_keys",
        &update.updated_env_keys,
    );
    format_env_key_list_with_label(
        &mut output,
        "",
        "removed_env_keys",
        &update.removed_env_keys,
    );
    writeln!(&mut output, "cleared: {}", update.cleared)
        .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_profile_env_update_json(
    update: &TerminalStartupProfileEnvUpdate,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "profile": update.profile.as_str(),
        "previous_env_count": update.previous_env_keys.len(),
        "previous_env_keys": &update.previous_env_keys,
        "env_count": update.env_keys.len(),
        "env_keys": &update.env_keys,
        "added_env_keys": &update.added_env_keys,
        "updated_env_keys": &update.updated_env_keys,
        "removed_env_keys": &update.removed_env_keys,
        "cleared": update.cleared,
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile environment update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_copy(copy: &TerminalStartupProfileCopy) -> String {
    let mut output = String::new();
    writeln!(&mut output, "startup_config_file: {}", copy.path.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "source_profile: {}", copy.source_profile)
        .expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", copy.profile).expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", copy.changed).expect("writing to string should not fail");
    writeln!(&mut output, "copied_tabs: {}", copy.copied_tab_count)
        .expect("writing to string should not fail");
    writeln!(&mut output, "total_profiles: {}", copy.total_profile_count)
        .expect("writing to string should not fail");
    output
}

fn format_startup_profile_copy_json(copy: &TerminalStartupProfileCopy) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": copy.path.display().to_string(),
        "status": "ok",
        "source_profile": copy.source_profile.as_str(),
        "profile": copy.profile.as_str(),
        "changed": copy.changed,
        "copied_tab_count": copy.copied_tab_count,
        "total_profile_count": copy.total_profile_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile copy as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_removal(removal: &TerminalStartupProfileRemoval) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        removal.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", removal.profile)
        .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", removal.changed)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "remaining_profiles: {}",
        removal.remaining_profile_count
    )
    .expect("writing to string should not fail");
    output
}

fn format_startup_profile_removal_json(removal: &TerminalStartupProfileRemoval) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": removal.path.display().to_string(),
        "status": "ok",
        "profile": removal.profile.as_str(),
        "changed": removal.changed,
        "remaining_profile_count": removal.remaining_profile_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile removal as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_rename(rename: &TerminalStartupProfileRename) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        rename.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "previous_profile: {}", rename.previous_profile)
        .expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", rename.profile)
        .expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", rename.changed)
        .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "updated_references: {}",
        rename.updated_reference_count
    )
    .expect("writing to string should not fail");
    output
}

fn format_startup_profile_rename_json(rename: &TerminalStartupProfileRename) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": rename.path.display().to_string(),
        "status": "ok",
        "previous_profile": rename.previous_profile.as_str(),
        "profile": rename.profile.as_str(),
        "changed": rename.changed,
        "updated_reference_count": rename.updated_reference_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile rename as json")?;
    output.push('\n');
    Ok(output)
}

fn format_startup_profile_visibility_update(
    update: &TerminalStartupProfileVisibilityUpdate,
) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        update.path.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "profile: {}", update.profile)
        .expect("writing to string should not fail");
    writeln!(&mut output, "previous_hidden: {}", update.previous_hidden)
        .expect("writing to string should not fail");
    writeln!(&mut output, "hidden: {}", update.hidden).expect("writing to string should not fail");
    writeln!(&mut output, "changed: {}", update.changed)
        .expect("writing to string should not fail");
    output
}

fn format_startup_profile_visibility_update_json(
    update: &TerminalStartupProfileVisibilityUpdate,
) -> Result<String> {
    let value = serde_json::json!({
        "startup_config_file": update.path.display().to_string(),
        "status": "ok",
        "profile": update.profile.as_str(),
        "previous_hidden": update.previous_hidden,
        "hidden": update.hidden,
        "changed": update.changed,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal startup profile visibility update as json")?;
    output.push('\n');
    Ok(output)
}

fn format_doctor_report(report: &TerminalDoctorReport) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "status: {}",
        if report.has_errors() { "error" } else { "ok" }
    )
    .expect("writing to string should not fail");

    writeln!(&mut output, "directories:").expect("writing to string should not fail");
    for directory in &report.directories {
        format_doctor_path_check(&mut output, directory);
    }

    writeln!(&mut output, "config_files:").expect("writing to string should not fail");
    for file in &report.config_files {
        format_doctor_path_check(&mut output, file);
    }

    writeln!(&mut output, "startup_config:").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "  startup_config_file: {} {}",
        report.startup_config.status.as_str(),
        report.startup_config.path.display()
    )
    .expect("writing to string should not fail");
    if let Some(source) = report.startup_config.source {
        writeln!(&mut output, "  source: {}", source.as_str())
            .expect("writing to string should not fail");
    }
    if let Some(validation) = &report.startup_config.validation {
        writeln!(&mut output, "  layouts: {}", validation.layout_count)
            .expect("writing to string should not fail");
        writeln!(&mut output, "  tabs: {}", validation.tab_count)
            .expect("writing to string should not fail");
    }
    if let Some(message) = &report.startup_config.message {
        writeln!(&mut output, "  message: {message}").expect("writing to string should not fail");
    }

    writeln!(&mut output, "keymap:").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "  keymap_file: {} {}",
        report.keymap.status.as_str(),
        report.keymap.path.display()
    )
    .expect("writing to string should not fail");
    if let Some(source) = report.keymap.source {
        writeln!(&mut output, "  source: {}", source.as_str())
            .expect("writing to string should not fail");
    }
    if let Some(validation) = &report.keymap.validation {
        writeln!(
            &mut output,
            "  default_bindings: {}",
            validation.default_binding_count
        )
        .expect("writing to string should not fail");
        writeln!(
            &mut output,
            "  user_bindings: {}",
            validation.user_binding_count
        )
        .expect("writing to string should not fail");
    }
    if let Some(message) = &report.keymap.message {
        writeln!(&mut output, "  message: {message}").expect("writing to string should not fail");
    }

    output
}

fn format_doctor_report_json(report: &TerminalDoctorReport) -> Result<String> {
    let value = serde_json::json!({
        "status": if report.has_errors() { "error" } else { "ok" },
        "directories": report
            .directories
            .iter()
            .map(doctor_path_check_json)
            .collect::<Vec<_>>(),
        "config_files": report
            .config_files
            .iter()
            .map(doctor_path_check_json)
            .collect::<Vec<_>>(),
        "startup_config": {
            "path": report.startup_config.path.display().to_string(),
            "status": report.startup_config.status.as_str(),
            "source": report
                .startup_config
                .source
                .map(TerminalDoctorConfigSource::as_str),
            "validation": report
                .startup_config
                .validation
                .as_ref()
                .map(|validation| serde_json::json!({
                    "layouts": validation.layout_count,
                    "tabs": validation.tab_count,
                })),
            "message": report.startup_config.message.as_deref(),
        },
        "keymap": {
            "path": report.keymap.path.display().to_string(),
            "status": report.keymap.status.as_str(),
            "source": report.keymap.source.map(TerminalUserKeymapSource::as_str),
            "validation": report
                .keymap
                .validation
                .as_ref()
                .map(|validation| serde_json::json!({
                    "default_bindings": validation.default_binding_count,
                    "user_bindings": validation.user_binding_count,
                    "user_keymap_source": validation.user_keymap_source.as_str(),
                })),
            "message": report.keymap.message.as_deref(),
        },
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal doctor report as json")?;
    output.push('\n');
    Ok(output)
}

fn doctor_path_check_json(check: &TerminalDoctorPathCheck) -> serde_json::Value {
    serde_json::json!({
        "label": check.label,
        "path": check.path.display().to_string(),
        "status": check.status.as_str(),
        "message": check.message.as_deref(),
    })
}

fn format_doctor_path_check(output: &mut String, check: &TerminalDoctorPathCheck) {
    writeln!(
        output,
        "  {}: {} {}",
        check.label,
        check.status.as_str(),
        check.path.display()
    )
    .expect("writing to string should not fail");
    if let Some(message) = &check.message {
        writeln!(output, "    message: {message}").expect("writing to string should not fail");
    }
}

impl TerminalDoctorReport {
    fn has_errors(&self) -> bool {
        self.directories
            .iter()
            .chain(self.config_files.iter())
            .any(TerminalDoctorPathCheck::has_error)
            || self.startup_config.status == TerminalDoctorCheckStatus::Error
            || self.keymap.status == TerminalDoctorCheckStatus::Error
    }
}

impl TerminalDoctorPathCheck {
    fn has_error(&self) -> bool {
        self.status == TerminalDoctorCheckStatus::Error
    }
}

impl TerminalDoctorCheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

impl TerminalDoctorConfigSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Initial => "initial",
        }
    }
}

impl TerminalConfigFileInitializationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Existing => "existing",
        }
    }
}

fn format_keymap_validation(report: &TerminalKeymapValidationReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "keymap_file: {}", report.keymap_file.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "default_bindings: {}",
        report.validation.default_binding_count
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "user_keymap_source: {}",
        report.validation.user_keymap_source.as_str()
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "user_bindings: {}",
        report.validation.user_binding_count
    )
    .expect("writing to string should not fail");
    output
}

fn format_keymap_validation_json(report: &TerminalKeymapValidationReport) -> Result<String> {
    let value = serde_json::json!({
        "keymap_file": report.keymap_file.display().to_string(),
        "status": "ok",
        "default_binding_count": report.validation.default_binding_count,
        "user_keymap_source": report.validation.user_keymap_source.as_str(),
        "user_binding_count": report.validation.user_binding_count,
    });
    let mut output = serde_json::to_string_pretty(&value)
        .context("failed to serialize terminal keymap validation as json")?;
    output.push('\n');
    Ok(output)
}

impl TerminalUserKeymapSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Initial => "initial",
        }
    }
}

fn profile_menu_label(profile: &TerminalStartupProfileSummary) -> String {
    let mut label = profile.display_name.clone();
    if profile.display_name != profile.name {
        write!(&mut label, " ({})", profile.name).expect("writing to string should not fail");
    }
    if profile.is_default {
        label.push_str(" - Default");
    }
    label
}

fn startup_profile_menu_entries() -> Vec<TerminalStartupProfileMenuEntry> {
    match TerminalStartupConfig::load(&active_terminal_startup_config_file()) {
        Ok(startup_config) => startup_config.profile_menu_entries(),
        Err(error) => {
            log::warn!("failed to load startup profile menu: {error:#}");
            Vec::new()
        }
    }
}

fn terminal_profile_split_direction_entries() -> &'static [TerminalProfileSplitDirectionEntry] {
    &[
        TerminalProfileSplitDirectionEntry {
            label: "Right",
            direction: TerminalStartupSplitDirection::Right,
        },
        TerminalProfileSplitDirectionEntry {
            label: "Down",
            direction: TerminalStartupSplitDirection::Down,
        },
        TerminalProfileSplitDirectionEntry {
            label: "Left",
            direction: TerminalStartupSplitDirection::Left,
        },
        TerminalProfileSplitDirectionEntry {
            label: "Up",
            direction: TerminalStartupSplitDirection::Up,
        },
    ]
}

fn launch_tab_for_profile(
    profile: &str,
    split: Option<TerminalStartupSplitDirection>,
) -> Result<LaunchTab> {
    let startup_config = TerminalStartupConfig::load(&active_terminal_startup_config_file())?;
    match split {
        Some(split) => startup_config.profile_launch_tab(profile, Some(split)),
        None => startup_config.profile_initial_tab(profile),
    }
    .with_context(|| format!("failed to resolve startup profile {profile:?}"))
}

fn terminal_log_file() -> &'static PathBuf {
    TERMINAL_LOG_FILE.get_or_init(|| paths::logs_dir().join(format!("{TERMINAL_APP_NAME}.log")))
}

fn terminal_old_log_file() -> &'static PathBuf {
    TERMINAL_OLD_LOG_FILE
        .get_or_init(|| paths::logs_dir().join(format!("{TERMINAL_APP_NAME}.log.old")))
}

fn terminal_startup_config_file(config_dir: &Path) -> PathBuf {
    config_dir.join(TERMINAL_STARTUP_CONFIG_FILE)
}

fn active_terminal_startup_config_file() -> PathBuf {
    terminal_startup_config_file(paths::config_dir())
}

fn terminal_startup_config_schema_file(config_dir: &Path) -> PathBuf {
    config_dir.join(TERMINAL_STARTUP_CONFIG_SCHEMA_FILE)
}

fn active_terminal_startup_config_schema_file() -> PathBuf {
    terminal_startup_config_schema_file(paths::config_dir())
}

fn terminal_default_keymap_reference_file(config_dir: &Path) -> PathBuf {
    config_dir.join(TERMINAL_DEFAULT_KEYMAP_REFERENCE_FILE)
}

fn active_terminal_default_keymap_reference_file() -> PathBuf {
    terminal_default_keymap_reference_file(paths::config_dir())
}

fn init(launch_options: LaunchOptions, cx: &mut App) -> Result<()> {
    component::init();
    menu::init();
    zed_actions::init();

    cx.on_action(|_: &zed_actions::Quit, cx| cx.quit());
    cx.on_action(open_settings_file);
    cx.on_action(open_startup_config_file);
    cx.on_action(open_startup_config_schema_file);
    cx.on_action(open_keymap_file);
    cx.on_action(open_default_keymap_reference_file);
    cx.on_action(open_config_directory);
    cx.on_action(open_data_directory);
    cx.on_action(open_log_file);
    cx.on_action(open_logs_directory);
    cx.on_action(open_themes_directory);
    cx.on_action(set_default_startup_profile_action);
    cx.on_action(clear_default_startup_profile_action);
    cx.on_action(|_: &zed_actions::OpenSettingsFile, cx| {
        cx.dispatch_action(&OpenSettingsFile);
    });
    cx.on_action(|_: &zed_actions::OpenSettings, cx| {
        cx.dispatch_action(&OpenSettingsFile);
    });
    cx.on_action(|_: &zed_actions::OpenKeymapFile, cx| {
        cx.dispatch_action(&OpenKeymapFile);
    });

    set_app_menus(cx);

    let version = release_channel::AppVersion::load(env!("CARGO_PKG_VERSION"), None, None);
    release_channel::init(version, cx);
    cx.set_global(db::AppDatabase::new());

    let http_client =
        ReqwestClient::user_agent("zed-terminal").context("failed to create HTTP client")?;
    cx.set_http_client(Arc::new(http_client));

    let fs: Arc<dyn fs::Fs> = Arc::new(RealFs::new(None, cx.background_executor().clone()));
    <dyn fs::Fs>::set_global(fs.clone(), cx);
    register_terminal_font_size_actions(fs.clone(), cx);

    ensure_config_files(&fs, cx)?;
    settings::init(cx);
    watch_settings_files(fs.clone(), cx);
    watch_startup_config_file(fs.clone(), cx);
    bind_keys(fs.clone(), cx)?;
    Assets
        .load_fonts(cx)
        .context("failed to load Zed embedded fonts")?;
    theme_settings::init(theme::LoadThemes::All(Box::new(Assets)), cx);
    load_user_themes_in_background(fs.clone(), cx);
    watch_themes(fs.clone(), cx);
    command_palette::init(cx);
    configure_terminal_command_palette(cx);
    apply_rendering_settings(cx);
    observe_settings_for_rendering(cx);

    let languages = Arc::new(LanguageRegistry::new(cx.background_executor().clone()));
    let client = Client::production(cx);
    client::init(&client, cx);
    Project::init(&client, cx);

    let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
    let session_id = uuid::Uuid::new_v4().to_string();
    let kvp = db::kvp::KeyValueStore::global(cx);
    let session = cx
        .foreground_executor()
        .block_on(Session::new(session_id, kvp));
    let session = cx.new(|cx| AppSession::new(session, cx));
    let node_runtime = NodeRuntime::unavailable();

    let app_state = Arc::new(AppState {
        languages,
        client,
        user_store,
        workspace_store,
        fs,
        build_window_options: build_window_options,
        node_runtime,
        session,
    });
    AppState::set_global(app_state.clone(), cx);

    workspace::init(app_state.clone(), cx);
    editor::init(cx);
    init_terminal_search(cx);
    terminal_view::init(cx);

    open_terminal_window(app_state, launch_options, cx)?;
    cx.activate(true);
    Ok(())
}

fn register_terminal_font_size_actions(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    cx.on_action({
        let fs = fs.clone();
        move |action: &zed_actions::IncreaseBufferFontSize, cx| {
            if action.persist {
                settings::update_settings_file(fs.clone(), cx, move |settings, cx| {
                    let buffer_font_size =
                        ThemeSettings::get_global(cx).buffer_font_size(cx) + px(1.0);
                    let _ = settings.theme.buffer_font_size.insert(
                        f32::from(theme_settings::clamp_font_size(buffer_font_size)).into(),
                    );
                });
            } else {
                theme_settings::increase_buffer_font_size(cx);
            }
        }
    });
    cx.on_action({
        let fs = fs.clone();
        move |action: &zed_actions::DecreaseBufferFontSize, cx| {
            if action.persist {
                settings::update_settings_file(fs.clone(), cx, move |settings, cx| {
                    let buffer_font_size =
                        ThemeSettings::get_global(cx).buffer_font_size(cx) - px(1.0);
                    let _ = settings.theme.buffer_font_size.insert(
                        f32::from(theme_settings::clamp_font_size(buffer_font_size)).into(),
                    );
                });
            } else {
                theme_settings::decrease_buffer_font_size(cx);
            }
        }
    });
    cx.on_action({
        let fs = fs.clone();
        move |action: &zed_actions::ResetBufferFontSize, cx| {
            if action.persist {
                settings::update_settings_file(fs.clone(), cx, move |settings, _| {
                    settings.theme.buffer_font_size = None;
                });
            } else {
                theme_settings::reset_buffer_font_size(cx);
            }
        }
    });
}

fn configure_terminal_command_palette(cx: &mut App) {
    command_palette_hooks::CommandPaletteFilter::update_global(cx, |filter, _| {
        apply_terminal_command_palette_filter(filter);
    });
    command_palette_hooks::GlobalCommandPaletteInterceptor::set(cx, |query, _, cx| {
        let query = query.to_string();
        let startup_config_file = active_terminal_startup_config_file();
        cx.background_spawn(async move {
            terminal_profile_command_palette_result(&query, &startup_config_file)
        })
    });
}

fn apply_terminal_command_palette_filter(filter: &mut command_palette_hooks::CommandPaletteFilter) {
    for namespace in terminal_command_palette_hidden_namespaces() {
        filter.hide_namespace(namespace);
    }
    filter.show_action_types(terminal_command_palette_visible_action_types().iter());
}

fn terminal_command_palette_hidden_namespaces() -> &'static [&'static str] {
    &[
        "agent",
        "agents",
        "agents_sidebar",
        "assistant",
        "buffer_search",
        "collab",
        "command_palette",
        "debug_panel",
        "dev",
        "diagnostics",
        "edit_prediction",
        "editor",
        "feedback",
        "file_finder",
        "git",
        "git_panel",
        "icon_theme_selector",
        "notebook",
        "outline",
        "outline_panel",
        "preview",
        "project_panel",
        "project_symbols",
        "projects",
        "remote_debug",
        "search",
        "settings_profile_selector",
        "pane",
        "task",
        "terminal",
        "theme",
        "theme_selector",
        "vim",
        "welcome",
        "workspace",
        "zed",
        "zed_terminal",
        "zed_predict_onboarding",
    ]
}

fn terminal_command_palette_visible_action_types() -> Vec<TypeId> {
    vec![
        TypeId::of::<ClearDefaultStartupProfile>(),
        TypeId::of::<CloseTerminalWindow>(),
        TypeId::of::<DuplicateTerminalTab>(),
        TypeId::of::<MinimizeTerminalWindow>(),
        TypeId::of::<NewTerminalWindow>(),
        TypeId::of::<NewTerminalTab>(),
        TypeId::of::<NewTerminalSplitWithProfile>(),
        TypeId::of::<NewTerminalTabWithProfile>(),
        TypeId::of::<OpenConfigDirectory>(),
        TypeId::of::<OpenDataDirectory>(),
        TypeId::of::<OpenDefaultKeymapReferenceFile>(),
        TypeId::of::<OpenLogFile>(),
        TypeId::of::<OpenLogsDirectory>(),
        TypeId::of::<OpenStartupConfigFile>(),
        TypeId::of::<OpenStartupConfigSchemaFile>(),
        TypeId::of::<OpenThemesDirectory>(),
        TypeId::of::<ResetPaneSizes>(),
        TypeId::of::<ResizePaneDown>(),
        TypeId::of::<ResizePaneLeft>(),
        TypeId::of::<ResizePaneRight>(),
        TypeId::of::<ResizePaneUp>(),
        TypeId::of::<SetDefaultStartupProfile>(),
        TypeId::of::<ToggleFullScreen>(),
        TypeId::of::<ZoomTerminalWindow>(),
        TypeId::of::<editor::actions::SelectAll>(),
        TypeId::of::<terminal::Clear>(),
        TypeId::of::<terminal::Copy>(),
        TypeId::of::<terminal::Paste>(),
        TypeId::of::<terminal::PasteText>(),
        TypeId::of::<terminal::ScrollLineDown>(),
        TypeId::of::<terminal::ScrollLineUp>(),
        TypeId::of::<terminal::ScrollPageDown>(),
        TypeId::of::<terminal::ScrollPageUp>(),
        TypeId::of::<terminal::ScrollToBottom>(),
        TypeId::of::<terminal::ScrollToTop>(),
        TypeId::of::<terminal::ShowCharacterPalette>(),
        TypeId::of::<terminal::ToggleViMode>(),
        TypeId::of::<terminal_view::RenameTerminal>(),
        TypeId::of::<terminal_view::RerunTask>(),
        TypeId::of::<workspace::ActivateNextPane>(),
        TypeId::of::<workspace::ActivatePaneDown>(),
        TypeId::of::<workspace::ActivatePaneLeft>(),
        TypeId::of::<workspace::ActivatePaneRight>(),
        TypeId::of::<workspace::ActivatePaneUp>(),
        TypeId::of::<workspace::ActivatePreviousPane>(),
        TypeId::of::<workspace::FocusCenterPane>(),
        TypeId::of::<workspace::ToggleZoom>(),
        TypeId::of::<workspace::pane::ActivateItem>(),
        TypeId::of::<workspace::pane::ActivateNextItem>(),
        TypeId::of::<workspace::pane::ActivatePreviousItem>(),
        TypeId::of::<workspace::pane::CloseActiveItem>(),
        TypeId::of::<workspace::pane::CloseAllItems>(),
        TypeId::of::<workspace::pane::CloseItemsToTheLeft>(),
        TypeId::of::<workspace::pane::CloseItemsToTheRight>(),
        TypeId::of::<workspace::pane::CloseOtherItems>(),
        TypeId::of::<workspace::pane::SplitDown>(),
        TypeId::of::<workspace::pane::SplitLeft>(),
        TypeId::of::<workspace::pane::SplitRight>(),
        TypeId::of::<workspace::pane::SplitUp>(),
        TypeId::of::<workspace::pane::SwapItemLeft>(),
        TypeId::of::<workspace::pane::SwapItemRight>(),
        TypeId::of::<zed_actions::buffer_search::Deploy>(),
        TypeId::of::<zed_actions::command_palette::Toggle>(),
        TypeId::of::<zed_actions::DecreaseBufferFontSize>(),
        TypeId::of::<zed_actions::IncreaseBufferFontSize>(),
        TypeId::of::<zed_actions::OpenKeymapFile>(),
        TypeId::of::<zed_actions::OpenSettings>(),
        TypeId::of::<zed_actions::OpenSettingsFile>(),
        TypeId::of::<zed_actions::Quit>(),
        TypeId::of::<zed_actions::ResetBufferFontSize>(),
    ]
}

fn terminal_profile_command_palette_result(
    query: &str,
    startup_config_file: &Path,
) -> command_palette_hooks::CommandInterceptResult {
    let profiles = match TerminalStartupConfig::load(startup_config_file) {
        Ok(startup_config) => startup_config.profile_summaries(false),
        Err(error) => {
            log::warn!("failed to load startup profile command palette entries: {error:#}");
            Vec::new()
        }
    };

    terminal_profile_command_palette_result_from_summaries(query, profiles)
}

fn terminal_profile_command_palette_result_from_summaries(
    query: &str,
    profiles: Vec<TerminalStartupProfileSummary>,
) -> command_palette_hooks::CommandInterceptResult {
    let query = command_palette::normalize_action_query(query);
    let results = terminal_profile_command_palette_items(&query, profiles);

    command_palette_hooks::CommandInterceptResult {
        results,
        exclusive: false,
    }
}

fn terminal_profile_command_palette_items(
    query: &str,
    profiles: Vec<TerminalStartupProfileSummary>,
) -> Vec<command_palette_hooks::CommandInterceptItem> {
    let mut items = Vec::new();

    for profile in profiles {
        let label = profile_menu_label(&profile);
        let profile_name = profile.name.clone();
        items.push(terminal_profile_command_palette_item(
            query,
            format!("New Tab With Profile: {label}"),
            NewTerminalTabWithProfile {
                profile: profile_name.clone(),
            }
            .boxed_clone(),
        ));
        for split_direction in terminal_profile_split_direction_entries() {
            items.push(terminal_profile_command_palette_item(
                query,
                format!("Split {} With Profile: {label}", split_direction.label),
                NewTerminalSplitWithProfile {
                    profile: profile_name.clone(),
                    direction: split_direction.direction,
                }
                .boxed_clone(),
            ));
        }
        items.push(terminal_profile_command_palette_item(
            query,
            format!("Set Default Profile: {label}"),
            SetDefaultStartupProfile {
                profile: profile_name,
            }
            .boxed_clone(),
        ));
    }

    items
        .into_iter()
        .flatten()
        .take(TERMINAL_PROFILE_COMMAND_PALETTE_MAX_RESULTS)
        .collect()
}

fn terminal_profile_command_palette_item(
    query: &str,
    string: String,
    action: Box<dyn Action>,
) -> Option<command_palette_hooks::CommandInterceptItem> {
    terminal_command_palette_match_positions(&string, query).map(|positions| {
        command_palette_hooks::CommandInterceptItem {
            action,
            string,
            positions,
        }
    })
}

fn terminal_command_palette_match_positions(string: &str, query: &str) -> Option<Vec<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return Some(Vec::new());
    }

    let mut positions = Vec::new();
    let mut string_chars = string.char_indices();

    for query_char in query.chars().flat_map(char::to_lowercase) {
        let mut matched = false;
        for (index, string_char) in string_chars.by_ref() {
            if string_char
                .to_lowercase()
                .any(|string_char| string_char == query_char)
            {
                positions.push(index);
                matched = true;
                break;
            }
        }
        if !matched {
            return None;
        }
    }

    Some(positions)
}

fn init_terminal_search(cx: &mut App) {
    search::buffer_search::init(cx);
    cx.set_global(terminal_search_callbacks());
}

fn terminal_search_callbacks() -> workspace::PaneSearchBarCallbacks {
    workspace::PaneSearchBarCallbacks {
        setup_search_bar: setup_terminal_search_bar,
        wrap_div_with_search_actions: search::buffer_search::register_pane_search_actions,
    }
}

fn setup_terminal_search_bar(
    languages: Option<Arc<LanguageRegistry>>,
    toolbar: &gpui::Entity<workspace::Toolbar>,
    window: &mut Window,
    cx: &mut App,
) {
    let search_bar = cx.new(|cx| search::BufferSearchBar::new(languages, window, cx));
    toolbar.update(cx, |toolbar, cx| {
        toolbar.add_item(search_bar, window, cx);
    });
}

fn observe_settings_for_rendering(cx: &mut App) {
    cx.observe_global::<settings::SettingsStore>(move |cx| {
        apply_rendering_settings(cx);
    })
    .detach();
}

fn apply_rendering_settings(cx: &mut App) {
    for &mut window in cx.windows().iter_mut() {
        let background_appearance = cx.theme().window_background_appearance();
        window
            .update(cx, |_, window, _| {
                window.set_background_appearance(background_appearance)
            })
            .ok();
    }

    cx.set_text_rendering_mode(
        match WorkspaceSettings::get_global(cx).text_rendering_mode {
            settings::TextRenderingMode::PlatformDefault => {
                gpui::TextRenderingMode::PlatformDefault
            }
            settings::TextRenderingMode::Subpixel => gpui::TextRenderingMode::Subpixel,
            settings::TextRenderingMode::Grayscale => gpui::TextRenderingMode::Grayscale,
        },
    );
}

fn bind_keys(fs: Arc<dyn fs::Fs>, cx: &mut App) -> Result<()> {
    reload_keymaps(Vec::new(), cx)?;

    let user_keymap_content = cx
        .foreground_executor()
        .block_on(KeymapFile::load_keymap_file(&fs))
        .context("failed to load terminal keymap file")?;
    load_user_keymap(&user_keymap_content, cx)?;

    let (mut user_keymap_rx, user_keymap_watcher) =
        settings::watch_config_file(cx.background_executor(), fs, paths::keymap_file().clone());
    cx.spawn(async move |cx| {
        let _user_keymap_watcher = user_keymap_watcher;
        while let Some(user_keymap_content) = user_keymap_rx.next().await {
            cx.update(|cx| {
                if let Err(error) = load_user_keymap(&user_keymap_content, cx) {
                    log::warn!("failed to reload terminal keymap: {error:#}");
                }
            });
        }
    })
    .detach();

    Ok(())
}

fn load_user_keymap(user_keymap_content: &str, cx: &mut App) -> Result<()> {
    match KeymapFile::load(user_keymap_content, cx) {
        KeymapFileLoadResult::Success { key_bindings } => reload_keymaps(key_bindings, cx)?,
        KeymapFileLoadResult::SomeFailedToLoad {
            key_bindings,
            error_message,
        } => {
            log::warn!("partially loaded terminal keymap: {}", error_message.0);
            if !key_bindings.is_empty() {
                reload_keymaps(key_bindings, cx)?;
            }
        }
        KeymapFileLoadResult::JsonParseFailure { error } => {
            log::warn!("failed to parse terminal keymap file: {error:#}");
        }
    }

    Ok(())
}

fn reload_keymaps(mut user_key_bindings: Vec<KeyBinding>, cx: &mut App) -> Result<()> {
    cx.clear_key_bindings();
    load_default_keymap(cx)?;

    for key_binding in &mut user_key_bindings {
        key_binding.set_meta(KeybindSource::User.meta());
    }
    cx.bind_keys(user_key_bindings);
    set_app_menus(cx);

    Ok(())
}

fn load_default_keymap(cx: &mut App) -> Result<()> {
    cx.bind_keys(
        KeymapFile::load_asset(TERMINAL_KEYMAP_PATH, Some(KeybindSource::Default), cx)
            .context("failed to load zed terminal default keymap")?,
    );

    Ok(())
}

fn shell_menu_items(profile_entries: Vec<TerminalStartupProfileMenuEntry>) -> Vec<MenuItem> {
    let mut shell_items = vec![
        MenuItem::action("New Tab", NewTerminalTab),
        MenuItem::action("Duplicate Tab", DuplicateTerminalTab),
    ];
    if !profile_entries.is_empty() {
        shell_items.push(MenuItem::submenu(Menu::new("New Tab With Profile").items(
            profile_entries.iter().cloned().map(|entry| {
                MenuItem::action(
                    entry.label,
                    NewTerminalTabWithProfile {
                        profile: entry.profile,
                    },
                )
            }),
        )));
        for split_direction in terminal_profile_split_direction_entries() {
            shell_items.push(MenuItem::submenu(
                Menu::new(format!("Split {} With Profile", split_direction.label)).items(
                    profile_entries.iter().cloned().map(move |entry| {
                        MenuItem::action(
                            entry.label,
                            NewTerminalSplitWithProfile {
                                profile: entry.profile,
                                direction: split_direction.direction,
                            },
                        )
                    }),
                ),
            ));
        }
        shell_items.push(MenuItem::submenu(Menu::new("Set Default Profile").items(
            profile_entries.into_iter().map(|entry| {
                MenuItem::action(
                    entry.label,
                    SetDefaultStartupProfile {
                        profile: entry.profile,
                    },
                )
            }),
        )));
    }
    shell_items.push(MenuItem::action(
        "Clear Default Profile",
        ClearDefaultStartupProfile,
    ));
    shell_items.extend([
        MenuItem::action(
            "Close Tab",
            workspace::CloseActiveItem {
                close_pinned: false,
                save_intent: None,
            },
        ),
        MenuItem::action(
            "Close Other Tabs",
            workspace::CloseOtherItems {
                close_pinned: false,
                save_intent: None,
            },
        ),
        MenuItem::action(
            "Close Tabs to the Right",
            workspace::CloseItemsToTheRight {
                close_pinned: false,
            },
        ),
        MenuItem::action(
            "Close Tabs to the Left",
            workspace::CloseItemsToTheLeft {
                close_pinned: false,
            },
        ),
        MenuItem::action(
            "Close All Tabs",
            workspace::CloseAllItems {
                close_pinned: false,
                save_intent: None,
            },
        ),
        MenuItem::separator(),
        MenuItem::action("Next Tab", workspace::ActivateNextItem::default()),
        MenuItem::action("Previous Tab", workspace::ActivatePreviousItem::default()),
        MenuItem::action("Move Tab Left", workspace::pane::SwapItemLeft),
        MenuItem::action("Move Tab Right", workspace::pane::SwapItemRight),
    ]);

    shell_items
}

fn terminal_menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem::action("Find", zed_actions::buffer_search::Deploy::find()),
        MenuItem::separator(),
        MenuItem::action(
            "Zoom In",
            zed_actions::IncreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            "Zoom Out",
            zed_actions::DecreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            "Reset Zoom",
            zed_actions::ResetBufferFontSize { persist: false },
        ),
        MenuItem::separator(),
        MenuItem::action("Copy", terminal::Copy),
        MenuItem::action("Paste", terminal::Paste),
        MenuItem::action("Paste Text", terminal::PasteText),
        MenuItem::action("Select All", editor::actions::SelectAll),
        MenuItem::separator(),
        MenuItem::action("Clear", terminal::Clear),
        MenuItem::action("Toggle Vi Mode", terminal::ToggleViMode),
        MenuItem::action("Show Character Palette", terminal::ShowCharacterPalette),
        MenuItem::separator(),
        MenuItem::action("Scroll Line Up", terminal::ScrollLineUp),
        MenuItem::action("Scroll Line Down", terminal::ScrollLineDown),
        MenuItem::action("Scroll Page Up", terminal::ScrollPageUp),
        MenuItem::action("Scroll Page Down", terminal::ScrollPageDown),
        MenuItem::action("Scroll To Top", terminal::ScrollToTop),
        MenuItem::action("Scroll To Bottom", terminal::ScrollToBottom),
        MenuItem::separator(),
        MenuItem::action("Rerun Task", terminal_view::RerunTask),
        MenuItem::action("Rename Terminal", terminal_view::RenameTerminal),
    ]
}

fn set_app_menus(cx: &mut App) {
    let shell_items = shell_menu_items(startup_profile_menu_entries());

    cx.set_menus(vec![
        Menu::new("Zed Terminal").items(app_menu_items()),
        Menu::new("Shell").items(shell_items),
        Menu::new("Terminal").items(terminal_menu_items()),
        Menu::new("Pane").items(vec![
            MenuItem::action("Split Right", workspace::SplitRight::default()),
            MenuItem::action("Split Down", workspace::SplitDown::default()),
            MenuItem::action("Split Left", workspace::SplitLeft::default()),
            MenuItem::action("Split Up", workspace::SplitUp::default()),
            MenuItem::separator(),
            MenuItem::action("Focus Left", workspace::ActivatePaneLeft),
            MenuItem::action("Focus Right", workspace::ActivatePaneRight),
            MenuItem::action("Focus Up", workspace::ActivatePaneUp),
            MenuItem::action("Focus Down", workspace::ActivatePaneDown),
            MenuItem::separator(),
            MenuItem::action("Next Pane", workspace::ActivateNextPane),
            MenuItem::action("Previous Pane", workspace::ActivatePreviousPane),
            MenuItem::action("Toggle Pane Zoom", workspace::ToggleZoom),
            MenuItem::separator(),
            MenuItem::action("Resize Pane Left", ResizePaneLeft),
            MenuItem::action("Resize Pane Right", ResizePaneRight),
            MenuItem::action("Resize Pane Up", ResizePaneUp),
            MenuItem::action("Resize Pane Down", ResizePaneDown),
            MenuItem::action("Reset Pane Sizes", ResetPaneSizes),
        ]),
        Menu::new("Window").items(window_menu_items()),
    ]);
}

fn app_menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem::action("Command Palette...", zed_actions::command_palette::Toggle),
        MenuItem::separator(),
        MenuItem::action("Open Settings File", zed_actions::OpenSettingsFile),
        MenuItem::action("Open Startup Config File", OpenStartupConfigFile),
        MenuItem::action(
            "Open Startup Config Schema File",
            OpenStartupConfigSchemaFile,
        ),
        MenuItem::action("Open Keymap File", zed_actions::OpenKeymapFile),
        MenuItem::action(
            "Open Default Keymap Reference File",
            OpenDefaultKeymapReferenceFile,
        ),
        MenuItem::action("Open Config Directory", OpenConfigDirectory),
        MenuItem::action("Open Data Directory", OpenDataDirectory),
        MenuItem::action("Open Log File", OpenLogFile),
        MenuItem::action("Open Logs Directory", OpenLogsDirectory),
        MenuItem::action("Open Themes Directory", OpenThemesDirectory),
        MenuItem::separator(),
        MenuItem::action("Quit", zed_actions::Quit),
    ]
}

fn window_menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem::action("New Window", NewTerminalWindow),
        MenuItem::action("Close Window", CloseTerminalWindow),
        MenuItem::separator(),
        MenuItem::action("Minimize", MinimizeTerminalWindow),
        MenuItem::action("Zoom", ZoomTerminalWindow),
        MenuItem::separator(),
        MenuItem::action("Toggle Full Screen", ToggleFullScreen),
    ]
}

fn build_window_options(_: Option<uuid::Uuid>, _: &mut App) -> WindowOptions {
    WindowOptions::default()
}

fn terminal_pane_resize_width(window: &mut Window, cx: &App) -> Pixels {
    let theme = ThemeSettings::get_global(cx);
    let font_id = window.text_system().resolve_font(&theme.buffer_font);
    window
        .text_system()
        .advance(font_id, theme.buffer_font_size(cx), 'm')
        .map(|width| width.width)
        .unwrap_or_else(|_| theme.buffer_font_size(cx))
}

fn terminal_pane_resize_height(cx: &App) -> Pixels {
    let theme = ThemeSettings::get_global(cx);
    theme.buffer_font_size(cx) * theme.buffer_line_height.value()
}

fn open_terminal_window(
    app_state: Arc<AppState>,
    launch_options: LaunchOptions,
    cx: &mut App,
) -> Result<()> {
    let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
    let bounds = Bounds::centered(None, window_size, cx);
    let startup_working_directories = launch_options.startup_working_directories();
    let new_terminal_window = launch_options.runtime_new_window_options();
    let new_terminal_tab = launch_options.new_terminal_tab.clone();
    let duplicate_terminal_tab_fallback = new_terminal_tab.clone();
    let initial_tab = launch_options.initial_tab;
    let additional_tabs = launch_options.additional_tabs;

    cx.open_window(
        WindowOptions {
            titlebar: Some(Default::default()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        move |window, cx| {
            theme_settings::setup_ui_font(window, cx);
            window.set_background_appearance(cx.theme().window_background_appearance());
            set_terminal_window_title(window, cx);

            let project = Project::local(
                app_state.client.clone(),
                app_state.node_runtime.clone(),
                app_state.user_store.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags {
                    init_worktree_trust: false,
                    watch_global_configs: true,
                },
                cx,
            );
            for working_directory in startup_working_directories.clone() {
                project.update(cx, |project, cx| {
                    project
                        .find_or_create_worktree(&working_directory, true, cx)
                        .detach_and_log_err(cx);
                });
            }

            let workspace = cx.new(|cx| {
                let mut workspace =
                    Workspace::new(None, project.clone(), app_state.clone(), window, cx);
                let new_terminal_window_app_state = app_state.clone();
                workspace.register_action({
                    let new_terminal_window = new_terminal_window.clone();
                    move |_, _: &NewTerminalWindow, _, cx| match open_terminal_window(
                        new_terminal_window_app_state.clone(),
                        new_terminal_window.clone(),
                        cx,
                    ) {
                        Ok(()) => cx.activate(true),
                        Err(error) => {
                            log::warn!("failed to open new terminal window: {error:#}");
                        }
                    }
                });
                workspace.register_action(move |workspace, _: &NewTerminalTab, window, cx| {
                    add_new_terminal_tab(workspace, window, cx, new_terminal_tab.clone())
                        .detach_and_log_err(cx);
                });
                workspace.register_action(
                    move |workspace, _: &DuplicateTerminalTab, window, cx| {
                        duplicate_terminal_tab(
                            workspace,
                            window,
                            cx,
                            duplicate_terminal_tab_fallback.clone(),
                        )
                        .detach_and_log_err(cx);
                    },
                );
                workspace.register_action(|_, _: &ToggleFullScreen, window, _| {
                    window.toggle_fullscreen();
                });
                workspace.register_action(|_, _: &MinimizeTerminalWindow, window, _| {
                    window.minimize_window();
                });
                workspace.register_action(|_, _: &ZoomTerminalWindow, window, _| {
                    window.zoom_window();
                });
                workspace.register_action(|_workspace, _: &CloseTerminalWindow, window, cx| {
                    cx.spawn_in(window, async move |workspace, cx| {
                        let should_close = workspace
                            .update_in(cx, |workspace, window, cx| {
                                workspace.prepare_to_close(
                                    workspace::CloseIntent::CloseWindow,
                                    window,
                                    cx,
                                )
                            })?
                            .await?;

                        if should_close {
                            cx.update(|window, _cx| {
                                window.remove_window();
                            })?;
                        }

                        anyhow::Ok(())
                    })
                    .detach_and_log_err(cx);
                });
                workspace.register_action(|workspace, _: &ResizePaneLeft, window, cx| {
                    let amount = terminal_pane_resize_width(window, cx);
                    workspace.resize_pane(Axis::Horizontal, -amount, window, cx);
                });
                workspace.register_action(|workspace, _: &ResizePaneRight, window, cx| {
                    let amount = terminal_pane_resize_width(window, cx);
                    workspace.resize_pane(Axis::Horizontal, amount, window, cx);
                });
                workspace.register_action(|workspace, _: &ResizePaneUp, window, cx| {
                    let amount = terminal_pane_resize_height(cx);
                    workspace.resize_pane(Axis::Vertical, amount, window, cx);
                });
                workspace.register_action(|workspace, _: &ResizePaneDown, window, cx| {
                    let amount = terminal_pane_resize_height(cx);
                    workspace.resize_pane(Axis::Vertical, -amount, window, cx);
                });
                workspace.register_action(|workspace, _: &ResetPaneSizes, _, cx| {
                    workspace.reset_pane_sizes(cx);
                });
                let profile_project = project.clone();
                workspace.register_action(
                    move |workspace, action: &NewTerminalTabWithProfile, window, cx| {
                        match launch_tab_for_profile(&action.profile, None) {
                            Ok(tab) => {
                                if let Some(working_directory) = tab.working_directory.clone() {
                                    profile_project.update(cx, |project, cx| {
                                        project
                                            .find_or_create_worktree(&working_directory, true, cx)
                                            .detach_and_log_err(cx);
                                    });
                                }
                                add_launch_tab(workspace, window, cx, tab).detach_and_log_err(cx);
                            }
                            Err(error) => {
                                log::warn!(
                                    "failed to create terminal tab for profile {:?}: {error:#}",
                                    action.profile
                                );
                            }
                        }
                    },
                );
                let profile_split_project = project.clone();
                workspace.register_action(
                    move |workspace, action: &NewTerminalSplitWithProfile, window, cx| {
                        match launch_tab_for_profile(&action.profile, Some(action.direction)) {
                            Ok(tab) => {
                                if let Some(working_directory) = tab.working_directory.clone() {
                                    profile_split_project.update(cx, |project, cx| {
                                        project
                                            .find_or_create_worktree(&working_directory, true, cx)
                                            .detach_and_log_err(cx);
                                    });
                                }
                                add_launch_tab(workspace, window, cx, tab).detach_and_log_err(cx);
                            }
                            Err(error) => {
                                log::warn!(
                                    "failed to split terminal for profile {:?}: {error:#}",
                                    action.profile
                                );
                            }
                        }
                    },
                );
                workspace
            });
            let window_handle = window.window_handle();
            cx.subscribe(
                &workspace,
                move |_, event: &WorkspaceEvent, cx| match event {
                    WorkspaceEvent::ActiveItemChanged
                    | WorkspaceEvent::ItemAdded { .. }
                    | WorkspaceEvent::PaneAdded(_) => {
                        window_handle
                            .update(cx, |_, window, cx| {
                                set_terminal_window_title(window, cx);
                                window.defer(cx, set_terminal_window_title);
                            })
                            .ok();
                    }
                    _ => {}
                },
            )
            .detach();

            let initial_tab = initial_tab.clone();
            let additional_tabs = additional_tabs.clone();
            workspace.update(cx, |_workspace, cx| {
                add_launch_tabs(window, cx, initial_tab, additional_tabs).detach_and_log_err(cx);
            });

            window.defer(cx, set_terminal_window_title);
            workspace
        },
    )
    .context("failed to open terminal window")?;

    Ok(())
}

fn add_new_terminal_tab(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    mut tab: LaunchTab,
) -> Task<Result<WeakEntity<Terminal>>> {
    if tab.working_directory.is_none() {
        tab.working_directory = default_working_directory(workspace, cx);
    }
    add_launch_tab(workspace, window, cx, tab)
}

fn duplicate_terminal_tab(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    fallback_tab: LaunchTab,
) -> Task<Result<WeakEntity<Terminal>>> {
    let active_terminal_view = workspace
        .active_pane()
        .read(cx)
        .active_item()
        .and_then(|item| item.downcast::<TerminalView>());

    let Some(active_terminal_view) = active_terminal_view else {
        return add_new_terminal_tab(workspace, window, cx, fallback_tab);
    };

    let (terminal, custom_title) = {
        let active_terminal_view = active_terminal_view.read(cx);
        (
            active_terminal_view.terminal().clone(),
            active_terminal_view.custom_title().map(str::to_owned),
        )
    };
    let working_directory = terminal
        .read(cx)
        .working_directory()
        .or_else(|| default_working_directory(workspace, cx));

    TerminalPanel::add_center_terminal_with_custom_title(
        workspace,
        window,
        cx,
        custom_title,
        move |project, cx| project.clone_terminal(&terminal, cx, working_directory),
    )
}

fn add_launch_tab(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    tab: LaunchTab,
) -> Task<Result<WeakEntity<Terminal>>> {
    let working_directory = tab.working_directory;
    let command = tab.command;
    let env = tab.env;
    let title = tab.title;
    let shell = tab.shell;
    let split = tab.split;
    let create_terminal = move |project: &mut Project, cx: &mut Context<Project>| {
        if let Some(command) = command {
            project.create_terminal_task(command.into_spawn_task(working_directory, env), cx)
        } else if let Some(shell) = shell {
            project.create_terminal_shell_with_shell(working_directory, shell, cx)
        } else {
            project.create_terminal_shell(working_directory, cx)
        }
    };

    if let Some(split) = split {
        TerminalPanel::split_center_terminal_with_custom_title(
            workspace,
            window,
            cx,
            split.to_workspace_split_direction(),
            title,
            create_terminal,
        )
    } else {
        TerminalPanel::add_center_terminal_with_custom_title(
            workspace,
            window,
            cx,
            title,
            create_terminal,
        )
    }
}

fn add_launch_tabs(
    window: &mut Window,
    cx: &mut Context<Workspace>,
    initial_tab: LaunchTab,
    additional_tabs: Vec<LaunchTab>,
) -> Task<Result<()>> {
    let tabs = std::iter::once(initial_tab)
        .chain(additional_tabs)
        .collect::<Vec<_>>();
    cx.spawn_in(window, async move |workspace, cx| {
        for tab in tabs {
            workspace
                .update_in(cx, |workspace, window, cx| {
                    add_launch_tab(workspace, window, cx, tab)
                })?
                .await?;
        }

        Ok(())
    })
}

fn default_terminal_config_dir() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        Ok(dirs::config_dir()
            .context("failed to determine RoamingAppData directory")?
            .join(TERMINAL_APP_NAME))
    } else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        Ok(
            if let Ok(flatpak_xdg_config) = env::var("FLATPAK_XDG_CONFIG_HOME") {
                PathBuf::from(flatpak_xdg_config)
            } else {
                dirs::config_dir().context("failed to determine XDG_CONFIG_HOME directory")?
            }
            .join(TERMINAL_APP_NAME_LOWERCASE),
        )
    } else {
        Ok(paths::home_dir()
            .join(".config")
            .join(TERMINAL_APP_NAME_LOWERCASE))
    }
}

fn default_terminal_data_dir() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(paths::home_dir()
            .join("Library/Application Support")
            .join(TERMINAL_APP_NAME))
    } else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        Ok(
            if let Ok(flatpak_xdg_data) = env::var("FLATPAK_XDG_DATA_HOME") {
                PathBuf::from(flatpak_xdg_data)
            } else {
                dirs::data_local_dir().context("failed to determine XDG_DATA_HOME directory")?
            }
            .join(TERMINAL_APP_NAME_LOWERCASE),
        )
    } else if cfg!(target_os = "windows") {
        Ok(dirs::data_local_dir()
            .context("failed to determine LocalAppData directory")?
            .join(TERMINAL_APP_NAME))
    } else {
        default_terminal_config_dir()
    }
}

fn resolve_working_directory(input: &Path) -> Result<PathBuf> {
    let expanded = expand_tilde(input)?;
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()
            .context("failed to read the current directory")?
            .join(expanded)
    };
    let canonical = dunce::canonicalize(&absolute).with_context(|| {
        format!(
            "failed to resolve working directory {}",
            input.to_string_lossy()
        )
    })?;

    if !canonical.is_dir() {
        bail!(
            "working directory is not a directory: {}",
            canonical.display()
        );
    }

    Ok(canonical)
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        shellexpand::tilde(&path.to_string_lossy()).into_owned(),
    ))
}

fn format_command_part(part: &str) -> String {
    if part.is_empty() || part.chars().any(char::is_whitespace) {
        format!("\"{}\"", part.replace('"', "\\\""))
    } else {
        part.to_string()
    }
}

fn normalize_terminal_title(title: Option<&str>) -> Option<String> {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
}

fn normalize_profile_text(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn normalize_startup_profile_name(profile: &str) -> Result<String> {
    let profile = profile.trim();
    if profile.is_empty() {
        bail!("startup profile name is empty");
    }
    Ok(profile.to_string())
}

fn normalize_terminal_shell_program(program: &str) -> Result<String> {
    let program = program.trim();
    if program.is_empty() {
        bail!("shell program is empty");
    }
    Ok(program.to_string())
}

fn set_terminal_window_title(window: &mut Window, cx: &mut App) {
    window.set_window_title(APP_TITLE);
    SystemWindowTabController::update_tab_title(
        cx,
        window.window_handle().window_id(),
        SharedString::from(APP_TITLE),
    );
}

fn watch_settings_files(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    settings::SettingsStore::update(cx, |store, cx| {
        store.watch_settings_files(fs, cx, |settings_file, result, _cx| {
            if let Err(error) = result.result() {
                log::warn!("failed to load {settings_file:?}: {error:?}");
            }
        });
    });
}

fn watch_startup_config_file(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    let (mut startup_config_rx, startup_config_watcher) = settings::watch_config_file(
        cx.background_executor(),
        fs,
        active_terminal_startup_config_file(),
    );
    cx.spawn(async move |cx| {
        let _startup_config_watcher = startup_config_watcher;
        while startup_config_rx.next().await.is_some() {
            cx.update(set_app_menus);
        }
    })
    .detach();
}

fn load_user_themes_in_background(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    cx.spawn({
        let fs = fs.clone();
        async move |cx| {
            let theme_registry = cx.update(|cx| ThemeRegistry::global(cx));
            let themes_dir = paths::themes_dir().as_ref();
            match fs
                .metadata(themes_dir)
                .await
                .ok()
                .flatten()
                .map(|metadata| metadata.is_dir)
            {
                Some(is_dir) => {
                    anyhow::ensure!(is_dir, "themes path {themes_dir:?} is not a directory");
                }
                None => {
                    fs.create_dir(themes_dir).await.with_context(|| {
                        format!("failed to create themes directory {themes_dir:?}")
                    })?;
                }
            }

            let mut theme_paths = fs
                .read_dir(themes_dir)
                .await
                .with_context(|| format!("failed to read themes from {themes_dir:?}"))?;

            while let Some(theme_path) = theme_paths.next().await {
                match theme_path {
                    Ok(theme_path) => {
                        if let Some(bytes) = fs.load_bytes(&theme_path).await.ok() {
                            load_user_theme(&theme_registry, &bytes).ok();
                        }
                    }
                    Err(error) => {
                        log::warn!("failed to read user theme path: {error:?}");
                    }
                }
            }

            cx.update(theme_settings::reload_theme);
            anyhow::Ok(())
        }
    })
    .detach_and_log_err(cx);
}

fn watch_themes(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let (mut events, _) = fs
            .watch(paths::themes_dir(), Duration::from_millis(100))
            .await;

        while let Some(paths) = events.next().await {
            for event in paths {
                if fs.metadata(&event.path).await.ok().flatten().is_some() {
                    let theme_registry = cx.update(|cx| ThemeRegistry::global(cx));
                    if let Some(bytes) = fs.load_bytes(&event.path).await.ok()
                        && load_user_theme(&theme_registry, &bytes).is_ok()
                    {
                        cx.update(theme_settings::reload_theme);
                    }
                }
            }
        }
    })
    .detach();
}

fn ensure_config_files(fs: &Arc<dyn fs::Fs>, cx: &App) -> Result<()> {
    initialize_terminal_config_files()?;

    // Prime the abstract filesystem for platforms that use a non-std backend.
    let settings_path = paths::settings_file();
    if let Err(error) = cx.foreground_executor().block_on(fs.load(settings_path)) {
        log::warn!("failed to prime settings file {settings_path:?}: {error:?}");
    }
    let keymap_path = paths::keymap_file();
    if let Err(error) = cx.foreground_executor().block_on(fs.load(keymap_path)) {
        log::warn!("failed to prime keymap file {keymap_path:?}: {error:?}");
    }
    Ok(())
}

fn open_settings_file(_: &OpenSettingsFile, cx: &mut App) {
    if !ensure_settings_file() {
        return;
    }
    cx.open_with_system(paths::settings_file());
}

fn open_startup_config_file(_: &OpenStartupConfigFile, cx: &mut App) {
    if !ensure_startup_config_file() {
        return;
    }
    cx.open_with_system(&active_terminal_startup_config_file());
}

fn open_startup_config_schema_file(_: &OpenStartupConfigSchemaFile, cx: &mut App) {
    let startup_config_schema_file = active_terminal_startup_config_schema_file();
    if let Err(error) = write_startup_config_schema_file(&startup_config_schema_file) {
        log::warn!("failed to write startup config schema file: {error:#}");
        return;
    }

    cx.open_with_system(&startup_config_schema_file);
}

fn open_keymap_file(_: &OpenKeymapFile, cx: &mut App) {
    if !ensure_keymap_file() {
        return;
    }
    cx.open_with_system(paths::keymap_file());
}

fn open_default_keymap_reference_file(_: &OpenDefaultKeymapReferenceFile, cx: &mut App) {
    let default_keymap_reference_file = active_terminal_default_keymap_reference_file();
    if let Err(error) = write_default_keymap_reference_file(&default_keymap_reference_file) {
        log::warn!("failed to write default keymap reference file: {error:#}");
        return;
    }

    cx.open_with_system(&default_keymap_reference_file);
}

fn open_config_directory(_: &OpenConfigDirectory, cx: &mut App) {
    open_directory(paths::config_dir(), "config", cx);
}

fn open_data_directory(_: &OpenDataDirectory, cx: &mut App) {
    open_directory(paths::data_dir(), "data", cx);
}

fn open_log_file(_: &OpenLogFile, cx: &mut App) {
    let log_file = terminal_log_file();
    if let Err(error) = ensure_log_file(log_file) {
        log::warn!("failed to ensure log file {log_file:?}: {error:#}");
        return;
    }

    cx.open_with_system(log_file);
}

fn open_logs_directory(_: &OpenLogsDirectory, cx: &mut App) {
    open_directory(paths::logs_dir(), "logs", cx);
}

fn open_themes_directory(_: &OpenThemesDirectory, cx: &mut App) {
    open_directory(paths::themes_dir(), "themes", cx);
}

fn set_default_startup_profile_action(action: &SetDefaultStartupProfile, cx: &mut App) {
    match set_default_startup_profile(&active_terminal_startup_config_file(), &action.profile) {
        Ok(update) => {
            log::info!(
                "set default startup profile to {:?} in {:?}",
                update.default_profile,
                update.path
            );
            set_app_menus(cx);
        }
        Err(error) => {
            log::warn!(
                "failed to set default startup profile {:?}: {error:#}",
                action.profile
            );
        }
    }
}

fn clear_default_startup_profile_action(_: &ClearDefaultStartupProfile, cx: &mut App) {
    match clear_default_startup_profile(&active_terminal_startup_config_file()) {
        Ok(update) => {
            log::info!("cleared default startup profile in {:?}", update.path);
            set_app_menus(cx);
        }
        Err(error) => {
            log::warn!("failed to clear default startup profile: {error:#}");
        }
    }
}

fn open_directory(path: &Path, label: &str, cx: &mut App) {
    if let Err(error) = std_fs::create_dir_all(path) {
        log::warn!("failed to create {label} directory {path:?}: {error:?}");
        return;
    }

    cx.open_with_system(path);
}

fn ensure_log_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }

    std_fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to create log file {}", path.display()))?;

    Ok(())
}

fn ensure_settings_file() -> bool {
    if let Err(error) = initialize_terminal_config_file(
        "settings_file",
        paths::settings_file().clone(),
        settings::initial_user_settings_content().as_ref(),
    ) {
        log::warn!("failed to ensure settings file: {error:?}");
        return false;
    }

    true
}

fn ensure_startup_config_file() -> bool {
    let startup_config_file = active_terminal_startup_config_file();
    if let Err(error) = initialize_terminal_config_file(
        "startup_config_file",
        startup_config_file,
        initial_terminal_startup_config_content(),
    ) {
        log::warn!("failed to ensure startup config file: {error:?}");
        return false;
    }

    true
}

fn ensure_keymap_file() -> bool {
    if let Err(error) = initialize_terminal_config_file(
        "keymap_file",
        paths::keymap_file().clone(),
        settings::initial_keymap_content().as_ref(),
    ) {
        log::warn!("failed to ensure keymap file: {error:?}");
        return false;
    }

    true
}

fn initial_terminal_startup_config_content() -> &'static str {
    r#"// Zed Terminal startup layout.
// Command strings use the same shell-like quoting rules as --new-tab-command.
// Environment variables apply to command-backed startup tabs only.
// tabs[].split may be "right", "down", "left", or "up" to open that tab as a startup split pane.
// tabs[].profile may reference a named profile and may be combined with title and split only.
// Profiles may include display_name, description, icon, color, and hidden metadata.
{
  "working_directory": null,
  "command": null,
  "title": null,
  "shell": null,
  "env": {},
  "tabs": [],
  "default_profile": null,
  "profiles": {}
}
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn temp_test_dir() -> PathBuf {
        let path = env::temp_dir().join(format!("zed-terminal-test-{}", uuid::Uuid::new_v4()));
        std_fs::create_dir_all(&path).expect("failed to create temp test directory");
        path
    }

    fn assert_initial_working_directory(options: &LaunchOptions, dir: &Path) {
        assert_eq!(
            options.initial_tab.working_directory.as_deref(),
            Some(dunce::canonicalize(dir).unwrap().as_path())
        );
    }

    fn assert_tab_working_directory(tab: &LaunchTab, dir: &Path) {
        assert_eq!(
            tab.working_directory.as_deref(),
            Some(dunce::canonicalize(dir).unwrap().as_path())
        );
    }

    fn test_env(entries: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env = HashMap::default();
        for (key, value) in entries {
            env.insert((*key).into(), (*value).into());
        }
        env
    }

    fn shell_with_args(program: &str, args: &[&str]) -> Shell {
        Shell::WithArguments {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            title_override: None,
        }
    }

    fn assert_cli_conflict(args: &[&str], reason: &str) {
        let error = Cli::try_parse_from(args.iter().copied()).expect_err(reason);
        assert!(error.to_string().contains("cannot be used with"));
    }

    fn assert_menu_action(menu_items: &[MenuItem], label: &str, action_name: &str) {
        let item = menu_items
            .iter()
            .find(|item| match item {
                MenuItem::Action { name, .. } => name.as_ref() == label,
                _ => false,
            })
            .unwrap_or_else(|| panic!("missing menu action {label:?}"));

        let MenuItem::Action { action, .. } = item else {
            panic!("menu item {label:?} should be an action");
        };
        assert_eq!(action.name(), action_name);
    }

    fn assert_profile_split_submenu_action(
        menu_items: &[MenuItem],
        submenu_label: &str,
        action_label: &str,
        expected_profile: &str,
        expected_direction: TerminalStartupSplitDirection,
    ) {
        let submenu = menu_items
            .iter()
            .find_map(|item| match item {
                MenuItem::Submenu(menu) if menu.name.as_ref() == submenu_label => Some(menu),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing submenu {submenu_label:?}"));
        let item = submenu
            .items
            .iter()
            .find(|item| match item {
                MenuItem::Action { name, .. } => name.as_ref() == action_label,
                _ => false,
            })
            .unwrap_or_else(|| {
                panic!("missing submenu action {action_label:?} in {submenu_label:?}")
            });

        let MenuItem::Action { action, .. } = item else {
            panic!("submenu item {action_label:?} should be an action");
        };
        let action = action
            .as_any()
            .downcast_ref::<NewTerminalSplitWithProfile>()
            .expect("expected profile split action");
        assert_eq!(action.profile, expected_profile);
        assert_eq!(action.direction, expected_direction);
    }

    #[test]
    fn parses_path_options() {
        let data_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--paths",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("failed to build cli command");
        let TerminalCliCommand::PrintPaths {
            path_options,
            format,
        } = command
        else {
            panic!("expected paths mode");
        };

        assert_eq!(format, TerminalPathsOutputFormat::Text);
        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(
            path_options.config_dir,
            path_options.data_dir.join("config")
        );
        std_fs::remove_dir_all(path_options.data_dir).ok();
    }

    #[test]
    fn parses_config_dir_override() {
        let data_dir = temp_test_dir();
        let config_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--config-dir",
            config_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.path_options.data_dir, data_dir);
        assert_eq!(options.path_options.config_dir, config_dir);
        std_fs::remove_dir_all(options.path_options.data_dir).ok();
        std_fs::remove_dir_all(options.path_options.config_dir).ok();
    }

    #[test]
    fn parses_terminal_keymap_asset() {
        settings::KeymapFile::parse(include_str!("../../../assets/keymaps/zed-terminal.json"))
            .expect("terminal keymap asset should parse");
    }

    #[test]
    fn terminal_keymap_includes_pane_workflow_bindings() {
        let keymap: gpui::private::serde_json::Value = settings::parse_json_with_comments(
            include_str!("../../../assets/keymaps/zed-terminal.json"),
        )
        .expect("terminal keymap asset should parse as json");

        assert_key_binding(
            &keymap,
            None,
            "ctrl-shift-n",
            "zed_terminal::NewTerminalWindow",
        );
        assert_key_binding(
            &keymap,
            None,
            "ctrl-shift-d",
            "zed_terminal::DuplicateTerminalTab",
        );
        assert_key_binding(&keymap, None, "f11", "zed_terminal::ToggleFullScreen");
        assert_key_binding(&keymap, None, "shift-escape", "workspace::ToggleZoom");
        for tab_number in 1..=8 {
            assert_key_binding_with_param(
                &keymap,
                None,
                &format!("ctrl-alt-{tab_number}"),
                "pane::ActivateItem",
                tab_number - 1,
            );
        }
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-left",
            "workspace::ActivatePaneLeft",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-right",
            "workspace::ActivatePaneRight",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-up",
            "workspace::ActivatePaneUp",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-down",
            "workspace::ActivatePaneDown",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-shift-left",
            "zed_terminal::ResizePaneLeft",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-shift-right",
            "zed_terminal::ResizePaneRight",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-shift-up",
            "zed_terminal::ResizePaneUp",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-shift-down",
            "zed_terminal::ResizePaneDown",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-enter",
            "zed_terminal::ToggleFullScreen",
        );
        assert_key_binding(
            &keymap,
            Some("os == windows && Terminal"),
            "alt-f4",
            "zed_terminal::CloseTerminalWindow",
        );
    }

    #[test]
    fn terminal_keymap_includes_terminal_search_binding() {
        let keymap: gpui::private::serde_json::Value = settings::parse_json_with_comments(
            include_str!("../../../assets/keymaps/zed-terminal.json"),
        )
        .expect("terminal keymap asset should parse as json");

        assert_key_binding(
            &keymap,
            Some("Terminal"),
            "ctrl-shift-f",
            "buffer_search::Deploy",
        );
    }

    #[test]
    fn terminal_keymap_includes_font_zoom_bindings() {
        let keymap: gpui::private::serde_json::Value = settings::parse_json_with_comments(
            include_str!("../../../assets/keymaps/zed-terminal.json"),
        )
        .expect("terminal keymap asset should parse as json");

        assert_key_binding_with_bool_param(
            &keymap,
            None,
            "ctrl-=",
            "zed::IncreaseBufferFontSize",
            "persist",
            false,
        );
        assert_key_binding_with_bool_param(
            &keymap,
            None,
            "ctrl-shift-=",
            "zed::IncreaseBufferFontSize",
            "persist",
            false,
        );
        assert_key_binding_with_bool_param(
            &keymap,
            None,
            "ctrl--",
            "zed::DecreaseBufferFontSize",
            "persist",
            false,
        );
        assert_key_binding_with_bool_param(
            &keymap,
            None,
            "ctrl-0",
            "zed::ResetBufferFontSize",
            "persist",
            false,
        );
    }

    #[test]
    fn terminal_keymap_includes_command_palette_bindings() {
        let keymap: gpui::private::serde_json::Value = settings::parse_json_with_comments(
            include_str!("../../../assets/keymaps/zed-terminal.json"),
        )
        .expect("terminal keymap asset should parse as json");

        assert_key_binding(&keymap, None, "ctrl-shift-p", "command_palette::Toggle");
        assert_key_binding(&keymap, None, "f1", "command_palette::Toggle");
    }

    #[test]
    fn terminal_command_palette_filter_keeps_terminal_product_actions() {
        let filter = terminal_command_palette_filter_for_test();

        assert_command_palette_action_visible(&filter, &CloseTerminalWindow);
        assert_command_palette_action_visible(&filter, &DuplicateTerminalTab);
        assert_command_palette_action_visible(&filter, &MinimizeTerminalWindow);
        assert_command_palette_action_visible(&filter, &NewTerminalWindow);
        assert_command_palette_action_visible(&filter, &NewTerminalTab);
        assert_command_palette_action_visible(
            &filter,
            &NewTerminalTabWithProfile {
                profile: "work".into(),
            },
        );
        assert_command_palette_action_visible(
            &filter,
            &NewTerminalSplitWithProfile {
                profile: "work".into(),
                direction: TerminalStartupSplitDirection::Right,
            },
        );
        assert_command_palette_action_visible(
            &filter,
            &SetDefaultStartupProfile {
                profile: "work".into(),
            },
        );
        assert_command_palette_action_visible(&filter, &ToggleFullScreen);
        assert_command_palette_action_visible(&filter, &ZoomTerminalWindow);
        assert_command_palette_action_visible(&filter, &zed_actions::command_palette::Toggle);
        assert_command_palette_action_visible(&filter, &zed_actions::OpenSettingsFile);
        assert_command_palette_action_visible(&filter, &OpenConfigDirectory);
        assert_command_palette_action_visible(&filter, &OpenDataDirectory);
        assert_command_palette_action_visible(&filter, &OpenLogFile);
        assert_command_palette_action_visible(&filter, &OpenLogsDirectory);
        assert_command_palette_action_visible(&filter, &OpenStartupConfigFile);
        assert_command_palette_action_visible(&filter, &OpenStartupConfigSchemaFile);
        assert_command_palette_action_visible(&filter, &OpenThemesDirectory);
        assert_command_palette_action_visible(&filter, &OpenDefaultKeymapReferenceFile);
        assert_command_palette_action_visible(&filter, &terminal::Copy);
        assert_command_palette_action_visible(&filter, &terminal::Paste);
        assert_command_palette_action_visible(&filter, &terminal::Clear);
        assert_command_palette_action_visible(&filter, &terminal_view::RenameTerminal);
        assert_command_palette_action_visible(&filter, &editor::actions::SelectAll);
        assert_command_palette_action_visible(&filter, &zed_actions::buffer_search::Deploy::find());
        assert_command_palette_action_visible(
            &filter,
            &zed_actions::IncreaseBufferFontSize { persist: false },
        );
        assert_command_palette_action_visible(&filter, &workspace::ActivatePaneLeft);
        assert_command_palette_action_visible(&filter, &workspace::SplitRight::default());
        assert_command_palette_action_visible(&filter, &workspace::CloseActiveItem::default());
    }

    #[test]
    fn terminal_command_palette_filter_hides_non_terminal_product_actions() {
        let filter = terminal_command_palette_filter_for_test();

        assert_command_palette_action_hidden(&filter, &editor::actions::ToggleComments::default());
        assert_command_palette_action_hidden(&filter, &workspace::NewFile);
        assert_command_palette_action_hidden(&filter, &workspace::ToggleFileFinder::default());
        assert_command_palette_action_hidden(&filter, &workspace::ToggleBottomDock);
        assert_command_palette_action_hidden(&filter, &zed_actions::Extensions::default());
        assert_command_palette_action_hidden(&filter, &zed_actions::agent::Chat);
        assert_command_palette_action_hidden(&filter, &zed_actions::theme::ToggleMode);
        assert_command_palette_action_hidden(&filter, &terminal::SearchTest);
        assert_command_palette_action_hidden(&filter, &terminal::ScrollHalfPageUp);
    }

    #[test]
    fn terminal_command_palette_filter_hides_broad_mixed_namespaces() {
        let hidden_namespaces = terminal_command_palette_hidden_namespaces()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        for namespace in [
            "buffer_search",
            "command_palette",
            "editor",
            "pane",
            "terminal",
            "workspace",
            "zed",
            "zed_terminal",
        ] {
            assert!(
                hidden_namespaces.contains(namespace),
                "terminal command palette should hide broad namespace {namespace:?}"
            );
        }
    }

    #[test]
    fn terminal_profile_command_palette_exposes_visible_profile_actions() {
        let result = terminal_profile_command_palette_result_from_summaries(
            "work",
            vec![TerminalStartupProfileSummary {
                name: "work".into(),
                display_name: "Work Shell".into(),
                description: Some("Project shell".into()),
                icon: Some("terminal".into()),
                color: Some("#0f766e".into()),
                hidden: false,
                is_default: true,
                tab_count: 2,
            }],
        );

        assert!(!result.exclusive);
        assert_eq!(
            result
                .results
                .iter()
                .map(|item| item.string.as_str())
                .collect::<Vec<_>>(),
            vec![
                "New Tab With Profile: Work Shell (work) - Default",
                "Split Right With Profile: Work Shell (work) - Default",
                "Split Down With Profile: Work Shell (work) - Default",
                "Split Left With Profile: Work Shell (work) - Default",
                "Split Up With Profile: Work Shell (work) - Default",
                "Set Default Profile: Work Shell (work) - Default",
            ]
        );

        assert_profile_tab_action(&result.results[0], "work");
        assert_profile_split_action(
            &result.results[1],
            "work",
            TerminalStartupSplitDirection::Right,
        );
        assert_profile_split_action(
            &result.results[2],
            "work",
            TerminalStartupSplitDirection::Down,
        );
        assert_profile_split_action(
            &result.results[3],
            "work",
            TerminalStartupSplitDirection::Left,
        );
        assert_profile_split_action(
            &result.results[4],
            "work",
            TerminalStartupSplitDirection::Up,
        );
        assert_set_default_profile_action(&result.results[5], "work");
        assert!(result.results.iter().all(|item| !item.positions.is_empty()));
    }

    #[test]
    fn terminal_profile_command_palette_uses_visible_startup_profiles() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "hidden".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Hidden Shell".into()),
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Work Shell".into()),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let result = terminal_profile_command_palette_result_from_summaries(
            "",
            config.profile_summaries(false),
        );

        assert_eq!(result.results.len(), 6);
        assert!(
            result
                .results
                .iter()
                .all(|item| item.string.contains("Work Shell"))
        );
        assert!(
            result
                .results
                .iter()
                .all(|item| !item.string.contains("Hidden Shell"))
        );
    }

    #[test]
    fn terminal_profile_command_palette_filters_by_query() {
        let profiles = vec![
            TerminalStartupProfileSummary {
                name: "build".into(),
                display_name: "Build Shell".into(),
                description: None,
                icon: None,
                color: None,
                hidden: false,
                is_default: false,
                tab_count: 1,
            },
            TerminalStartupProfileSummary {
                name: "logs".into(),
                display_name: "Log Tail".into(),
                description: None,
                icon: None,
                color: None,
                hidden: false,
                is_default: false,
                tab_count: 1,
            },
        ];

        let result = terminal_profile_command_palette_result_from_summaries("log", profiles);

        assert_eq!(result.results.len(), 6);
        assert!(
            result
                .results
                .iter()
                .all(|item| item.string.contains("Log Tail"))
        );
    }

    #[test]
    fn terminal_profile_split_directions_cover_full_pane_model() {
        assert_eq!(
            terminal_profile_split_direction_entries()
                .iter()
                .map(|entry| (entry.label, entry.direction))
                .collect::<Vec<_>>(),
            vec![
                ("Right", TerminalStartupSplitDirection::Right),
                ("Down", TerminalStartupSplitDirection::Down),
                ("Left", TerminalStartupSplitDirection::Left),
                ("Up", TerminalStartupSplitDirection::Up),
            ]
        );
    }

    #[test]
    fn terminal_command_palette_match_positions_are_subsequence_matches() {
        assert_eq!(
            terminal_command_palette_match_positions("New Tab With Profile: Work", "ntwp")
                .expect("query should match"),
            vec![0, 4, 8, 13]
        );
        assert!(terminal_command_palette_match_positions("New Tab", "zzz").is_none());
        assert_eq!(
            terminal_command_palette_match_positions("New Tab", "   ")
                .expect("blank query matches"),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn terminal_keymap_actions_are_registered() {
        let keymap: gpui::private::serde_json::Value = settings::parse_json_with_comments(
            include_str!("../../../assets/keymaps/zed-terminal.json"),
        )
        .expect("terminal keymap asset should parse as json");
        let mut keymap_action_names = BTreeSet::new();
        collect_action_names(&keymap, &mut keymap_action_names);

        let registered_action_names = gpui::generate_list_of_all_registered_actions()
            .map(|action| action.name)
            .collect::<BTreeSet<_>>();
        let missing_action_names = keymap_action_names
            .iter()
            .filter(|action_name| !registered_action_names.contains(action_name.as_str()))
            .cloned()
            .collect::<Vec<_>>();

        assert!(
            missing_action_names.is_empty(),
            "terminal keymap references unregistered actions: {missing_action_names:?}"
        );
    }

    fn assert_profile_tab_action(
        item: &command_palette_hooks::CommandInterceptItem,
        expected_profile: &str,
    ) {
        let action = item
            .action
            .as_any()
            .downcast_ref::<NewTerminalTabWithProfile>()
            .expect("expected profile tab action");
        assert_eq!(action.profile, expected_profile);
    }

    fn assert_profile_split_action(
        item: &command_palette_hooks::CommandInterceptItem,
        expected_profile: &str,
        expected_direction: TerminalStartupSplitDirection,
    ) {
        let action = item
            .action
            .as_any()
            .downcast_ref::<NewTerminalSplitWithProfile>()
            .expect("expected profile split action");
        assert_eq!(action.profile, expected_profile);
        assert_eq!(action.direction, expected_direction);
    }

    fn assert_set_default_profile_action(
        item: &command_palette_hooks::CommandInterceptItem,
        expected_profile: &str,
    ) {
        let action = item
            .action
            .as_any()
            .downcast_ref::<SetDefaultStartupProfile>()
            .expect("expected set default profile action");
        assert_eq!(action.profile, expected_profile);
    }

    fn terminal_command_palette_filter_for_test() -> command_palette_hooks::CommandPaletteFilter {
        let mut filter = command_palette_hooks::CommandPaletteFilter::default();
        apply_terminal_command_palette_filter(&mut filter);
        filter
    }

    fn assert_command_palette_action_visible(
        filter: &command_palette_hooks::CommandPaletteFilter,
        action: &dyn Action,
    ) {
        assert!(
            !filter.is_hidden(action),
            "{} should be visible in zed terminal command palette",
            action.name()
        );
    }

    fn assert_command_palette_action_hidden(
        filter: &command_palette_hooks::CommandPaletteFilter,
        action: &dyn Action,
    ) {
        assert!(
            filter.is_hidden(action),
            "{} should be hidden from zed terminal command palette",
            action.name()
        );
    }

    fn assert_key_binding(
        keymap: &gpui::private::serde_json::Value,
        context: Option<&str>,
        keystroke: &str,
        expected_action: &str,
    ) {
        let action = key_binding(keymap, context, keystroke)
            .as_str()
            .unwrap_or_else(|| {
                panic!("key binding {keystroke:?} in context {context:?} should be a string action")
            });

        assert_eq!(action, expected_action);
    }

    fn assert_key_binding_with_param(
        keymap: &gpui::private::serde_json::Value,
        context: Option<&str>,
        keystroke: &str,
        expected_action: &str,
        expected_param: usize,
    ) {
        let binding = key_binding(keymap, context, keystroke)
            .as_array()
            .unwrap_or_else(|| {
                panic!("key binding {keystroke:?} in context {context:?} should be an array action")
            });
        let action = binding
            .first()
            .and_then(gpui::private::serde_json::Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "key binding {keystroke:?} in context {context:?} should include an action name"
                )
            });
        let param = binding
            .get(1)
            .and_then(gpui::private::serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("key binding {keystroke:?} in context {context:?} should include a numeric parameter"));

        assert_eq!(action, expected_action);
        assert_eq!(param, expected_param as u64);
    }

    fn assert_key_binding_with_bool_param(
        keymap: &gpui::private::serde_json::Value,
        context: Option<&str>,
        keystroke: &str,
        expected_action: &str,
        param_name: &str,
        expected_param: bool,
    ) {
        let binding = key_binding(keymap, context, keystroke)
            .as_array()
            .unwrap_or_else(|| {
                panic!("key binding {keystroke:?} in context {context:?} should be an array action")
            });
        let action = binding
            .first()
            .and_then(gpui::private::serde_json::Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "key binding {keystroke:?} in context {context:?} should include an action name"
                )
            });
        let param = binding
            .get(1)
            .and_then(|param| param.get(param_name))
            .and_then(gpui::private::serde_json::Value::as_bool)
            .unwrap_or_else(|| panic!("key binding {keystroke:?} in context {context:?} should include a boolean {param_name:?} parameter"));

        assert_eq!(action, expected_action);
        assert_eq!(param, expected_param);
    }

    fn key_binding<'a>(
        keymap: &'a gpui::private::serde_json::Value,
        context: Option<&str>,
        keystroke: &str,
    ) -> &'a gpui::private::serde_json::Value {
        let entries = keymap
            .as_array()
            .expect("terminal keymap should be a JSON array");
        let entry = entries
            .iter()
            .find(|entry| {
                entry
                    .get("context")
                    .and_then(gpui::private::serde_json::Value::as_str)
                    == context
            })
            .unwrap_or_else(|| panic!("missing keymap context {context:?}"));
        entry
            .get("bindings")
            .and_then(|bindings| bindings.get(keystroke))
            .unwrap_or_else(|| panic!("missing key binding {keystroke:?} in context {context:?}"))
    }

    fn collect_action_names(
        value: &gpui::private::serde_json::Value,
        action_names: &mut BTreeSet<String>,
    ) {
        match value {
            gpui::private::serde_json::Value::String(value) => {
                if value.contains("::") {
                    action_names.insert(value.clone());
                }
            }
            gpui::private::serde_json::Value::Array(values) => {
                for value in values {
                    collect_action_names(value, action_names);
                }
            }
            gpui::private::serde_json::Value::Object(entries) => {
                for value in entries.values() {
                    collect_action_names(value, action_names);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn parses_initial_terminal_startup_config_content() {
        let config: TerminalStartupConfig =
            settings::parse_json_with_comments(initial_terminal_startup_config_content())
                .expect("initial terminal startup config should parse");

        assert_eq!(config, TerminalStartupConfig::default());
    }

    #[test]
    fn parses_startup_profile_metadata() {
        let config: TerminalStartupConfig = settings::parse_json_with_comments(
            r##"{
                "default_profile": "work",
                "profiles": {
                    "hidden": {
                        "display_name": "   ",
                        "hidden": true
                    },
                    "work": {
                        "display_name": " Work Shell ",
                        "description": " Project startup shell ",
                        "icon": " terminal ",
                        "color": " #0f766e ",
                        "tabs": [{ "title": "Logs" }]
                    }
                }
            }"##,
        )
        .expect("profile metadata should parse");

        assert_eq!(
            config.profile_summaries(false),
            vec![TerminalStartupProfileSummary {
                name: "work".into(),
                display_name: "Work Shell".into(),
                description: Some("Project startup shell".into()),
                icon: Some("terminal".into()),
                color: Some("#0f766e".into()),
                hidden: false,
                is_default: true,
                tab_count: 2,
            }]
        );
        assert_eq!(
            config
                .profile_summaries(true)
                .into_iter()
                .map(|profile| (profile.name, profile.display_name, profile.hidden))
                .collect::<Vec<_>>(),
            vec![
                ("hidden".into(), "hidden".into(), true),
                ("work".into(), "Work Shell".into(), false),
            ]
        );
    }

    #[test]
    fn profile_menu_entries_use_visible_profiles() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Secret".into()),
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Work Shell".into()),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        assert_eq!(
            config.profile_menu_entries(),
            vec![TerminalStartupProfileMenuEntry {
                profile: "work".into(),
                label: "Work Shell (work) - Default".into(),
            }]
        );
    }

    #[test]
    fn shell_menu_exposes_duplicate_tab_action() {
        let items = shell_menu_items(Vec::new());

        assert_menu_action(
            &items,
            "Duplicate Tab",
            "zed_terminal::DuplicateTerminalTab",
        );
    }

    #[test]
    fn shell_menu_exposes_bulk_tab_close_actions() {
        let items = shell_menu_items(Vec::new());

        assert_menu_action(&items, "Close Tab", "pane::CloseActiveItem");
        assert_menu_action(&items, "Close Other Tabs", "pane::CloseOtherItems");
        assert_menu_action(
            &items,
            "Close Tabs to the Right",
            "pane::CloseItemsToTheRight",
        );
        assert_menu_action(
            &items,
            "Close Tabs to the Left",
            "pane::CloseItemsToTheLeft",
        );
        assert_menu_action(&items, "Close All Tabs", "pane::CloseAllItems");
    }

    #[test]
    fn shell_menu_exposes_tab_reorder_actions() {
        let items = shell_menu_items(Vec::new());

        assert_menu_action(&items, "Move Tab Left", "pane::SwapItemLeft");
        assert_menu_action(&items, "Move Tab Right", "pane::SwapItemRight");
    }

    #[test]
    fn shell_menu_exposes_all_profile_split_directions() {
        let items = shell_menu_items(vec![TerminalStartupProfileMenuEntry {
            profile: "work".into(),
            label: "Work Shell (work)".into(),
        }]);

        assert_profile_split_submenu_action(
            &items,
            "Split Right With Profile",
            "Work Shell (work)",
            "work",
            TerminalStartupSplitDirection::Right,
        );
        assert_profile_split_submenu_action(
            &items,
            "Split Down With Profile",
            "Work Shell (work)",
            "work",
            TerminalStartupSplitDirection::Down,
        );
        assert_profile_split_submenu_action(
            &items,
            "Split Left With Profile",
            "Work Shell (work)",
            "work",
            TerminalStartupSplitDirection::Left,
        );
        assert_profile_split_submenu_action(
            &items,
            "Split Up With Profile",
            "Work Shell (work)",
            "work",
            TerminalStartupSplitDirection::Up,
        );
    }

    #[test]
    fn terminal_menu_exposes_find_action() {
        let items = terminal_menu_items();

        assert_menu_action(&items, "Find", "buffer_search::Deploy");
    }

    #[test]
    fn terminal_menu_exposes_font_zoom_actions() {
        let items = terminal_menu_items();

        assert_menu_action(&items, "Zoom In", "zed::IncreaseBufferFontSize");
        assert_menu_action(&items, "Zoom Out", "zed::DecreaseBufferFontSize");
        assert_menu_action(&items, "Reset Zoom", "zed::ResetBufferFontSize");
    }

    #[test]
    fn app_menu_exposes_command_palette_action() {
        let items = app_menu_items();

        assert_menu_action(&items, "Command Palette...", "command_palette::Toggle");
        assert_menu_action(
            &items,
            "Open Startup Config Schema File",
            "zed_terminal::OpenStartupConfigSchemaFile",
        );
        assert_menu_action(
            &items,
            "Open Default Keymap Reference File",
            "zed_terminal::OpenDefaultKeymapReferenceFile",
        );
        assert_menu_action(
            &items,
            "Open Config Directory",
            "zed_terminal::OpenConfigDirectory",
        );
        assert_menu_action(
            &items,
            "Open Data Directory",
            "zed_terminal::OpenDataDirectory",
        );
        assert_menu_action(&items, "Open Log File", "zed_terminal::OpenLogFile");
        assert_menu_action(
            &items,
            "Open Logs Directory",
            "zed_terminal::OpenLogsDirectory",
        );
        assert_menu_action(
            &items,
            "Open Themes Directory",
            "zed_terminal::OpenThemesDirectory",
        );
    }

    #[test]
    fn window_menu_exposes_window_lifecycle_actions() {
        let items = window_menu_items();

        assert_menu_action(&items, "New Window", "zed_terminal::NewTerminalWindow");
        assert_menu_action(&items, "Close Window", "zed_terminal::CloseTerminalWindow");
        assert_menu_action(&items, "Minimize", "zed_terminal::MinimizeTerminalWindow");
        assert_menu_action(&items, "Zoom", "zed_terminal::ZoomTerminalWindow");
        assert_menu_action(
            &items,
            "Toggle Full Screen",
            "zed_terminal::ToggleFullScreen",
        );
    }

    #[test]
    fn terminal_search_callbacks_use_zed_buffer_search() {
        let callbacks = terminal_search_callbacks();

        assert!(std::ptr::fn_addr_eq(
            callbacks.setup_search_bar,
            setup_terminal_search_bar
                as fn(
                    Option<Arc<LanguageRegistry>>,
                    &gpui::Entity<workspace::Toolbar>,
                    &mut Window,
                    &mut App,
                )
        ));
        assert!(std::ptr::fn_addr_eq(
            callbacks.wrap_div_with_search_actions,
            search::buffer_search::register_pane_search_actions
                as fn(gpui::Div, gpui::Entity<workspace::Pane>) -> gpui::Div
        ));
    }

    #[test]
    fn profile_initial_tab_resolves_selected_profile_only() {
        let initial_dir = temp_test_dir();
        let extra_tab_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(initial_dir.clone()),
                command: Some("cmd /C work".into()),
                title: Some("Work".into()),
                env: test_env(&[("ZED_TERMINAL_PROFILE", "work")]),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(extra_tab_dir.clone()),
                    command: Some("cmd /C logs".into()),
                    title: Some("Logs".into()),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let tab = config
            .profile_initial_tab("work")
            .expect("profile initial tab should resolve");

        assert_tab_working_directory(&tab, &initial_dir);
        assert_ne!(
            tab.working_directory.as_deref(),
            Some(dunce::canonicalize(&extra_tab_dir).unwrap().as_path())
        );
        assert_eq!(
            tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "work".into()],
            })
        );
        assert_eq!(tab.env, test_env(&[("ZED_TERMINAL_PROFILE", "work")]));
        assert_eq!(tab.title.as_deref(), Some("Work"));
        assert_eq!(tab.shell, None);

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(extra_tab_dir).ok();
    }

    #[test]
    fn profile_initial_tab_allows_hidden_profiles_for_direct_actions() {
        let secret_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                hidden: true,
                working_directory: Some(secret_dir.clone()),
                title: Some("Secret".into()),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        assert_eq!(config.profile_menu_entries(), Vec::new());

        let tab = config
            .profile_initial_tab("secret")
            .expect("hidden profiles should still be directly invokable");

        assert_tab_working_directory(&tab, &secret_dir);
        assert_eq!(tab.title.as_deref(), Some("Secret"));

        std_fs::remove_dir_all(secret_dir).ok();
    }

    #[test]
    fn profile_launch_tab_applies_runtime_split_without_replaying_profile_tabs() {
        let initial_dir = temp_test_dir();
        let extra_tab_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(initial_dir.clone()),
                title: Some("Work".into()),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(extra_tab_dir.clone()),
                    title: Some("Logs".into()),
                    split: Some(TerminalStartupSplitDirection::Down),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let tab = config
            .profile_launch_tab(" work ", Some(TerminalStartupSplitDirection::Right))
            .expect("profile split tab should resolve");

        assert_tab_working_directory(&tab, &initial_dir);
        assert_ne!(
            tab.working_directory.as_deref(),
            Some(dunce::canonicalize(&extra_tab_dir).unwrap().as_path())
        );
        assert_eq!(tab.title.as_deref(), Some("Work"));
        assert_eq!(tab.split, Some(TerminalStartupSplitDirection::Right));

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(extra_tab_dir).ok();
    }

    #[test]
    fn parses_startup_profile_tabs_from_config() {
        let profile_dir = temp_test_dir();
        let profile_extra_dir = temp_test_dir();
        let config: TerminalStartupConfig = settings::parse_json_with_comments(&format!(
            r#"{{
                "tabs": [
                    {{ "profile": "work", "title": "Profile Tab", "split": "right" }}
                ],
                "profiles": {{
                    "work": {{
                        "working_directory": "{}",
                        "command": "cmd /C work",
                        "title": "Work",
                        "env": {{ "ZED_TERMINAL_PROFILE": "work" }},
                        "tabs": [
                            {{ "working_directory": "{}", "title": "Logs" }}
                        ]
                    }}
                }}
            }}"#,
            profile_dir.display().to_string().replace('\\', "\\\\"),
            profile_extra_dir
                .display()
                .to_string()
                .replace('\\', "\\\\"),
        ))
        .expect("startup profile tab config should parse");

        assert_eq!(config.tabs[0].profile.as_deref(), Some("work"));
        assert_eq!(
            config.tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );
        assert_eq!(
            config
                .validate()
                .expect("startup config should validate profile tabs"),
            TerminalStartupConfigValidation {
                layout_count: 2,
                tab_count: 4,
            }
        );

        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 1);
        let tab = &options.additional_tabs[0];
        assert_tab_working_directory(tab, &profile_dir);
        assert_ne!(
            tab.working_directory.as_deref(),
            Some(dunce::canonicalize(&profile_extra_dir).unwrap().as_path())
        );
        assert_eq!(
            tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "work".into()],
            })
        );
        assert_eq!(tab.env, test_env(&[("ZED_TERMINAL_PROFILE", "work")]));
        assert_eq!(tab.title.as_deref(), Some("Profile Tab"));
        assert_eq!(tab.split, Some(TerminalStartupSplitDirection::Right));

        let output = format_startup_layout(&options, Path::new("terminal.json"));
        assert!(output.contains("- tab 2"));
        assert!(output.contains("  placement: split right"));
        assert!(output.contains("  title: Profile Tab"));
        assert!(output.contains("  command: cmd /C work"));

        std_fs::remove_dir_all(profile_dir).ok();
        std_fs::remove_dir_all(profile_extra_dir).ok();
    }

    #[test]
    fn rejects_mixed_profile_startup_tab_fields() {
        let tab_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());

        for (tab, expected_field) in [
            (
                TerminalStartupTabConfig {
                    profile: Some("work".into()),
                    working_directory: Some(tab_dir.clone()),
                    ..TerminalStartupTabConfig::default()
                },
                "working_directory",
            ),
            (
                TerminalStartupTabConfig {
                    profile: Some("work".into()),
                    command: Some("cmd /C mixed".into()),
                    ..TerminalStartupTabConfig::default()
                },
                "command",
            ),
            (
                TerminalStartupTabConfig {
                    profile: Some("work".into()),
                    shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
                    ..TerminalStartupTabConfig::default()
                },
                "shell",
            ),
            (
                TerminalStartupTabConfig {
                    profile: Some("work".into()),
                    env: test_env(&[("MIXED", "1")]),
                    ..TerminalStartupTabConfig::default()
                },
                "env",
            ),
        ] {
            let config = TerminalStartupConfig {
                tabs: vec![tab],
                profiles: profiles.clone(),
                ..TerminalStartupConfig::default()
            };

            let error = config
                .validate()
                .expect_err("mixed profile startup tab fields should be rejected");
            let message = format!("{error:#}");
            assert!(message.contains("profile startup tab cannot include"));
            assert!(message.contains(expected_field));
            assert!(message.contains("tab 2 for root startup layout"));
        }

        std_fs::remove_dir_all(tab_dir).ok();
    }

    #[test]
    fn rejects_missing_startup_profile_tab_reference() {
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        let config = TerminalStartupConfig {
            tabs: vec![TerminalStartupTabConfig {
                profile: Some("missing".into()),
                ..TerminalStartupTabConfig::default()
            }],
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = config
            .validate()
            .expect_err("missing profile tab reference should fail validation");
        let message = format!("{error:#}");

        assert!(message.contains("failed to resolve profile for tab 2 for root startup layout"));
        assert!(message.contains("startup profile not found: missing"));
        assert!(message.contains("Available profiles: work"));
    }

    #[test]
    fn parses_new_terminal_tab_with_profile_action_input() {
        let action = <NewTerminalTabWithProfile as Action>::build(
            gpui::private::serde_json::json!({ "profile": "work" }),
        )
        .expect("profile action input should parse");
        let action = action
            .as_any()
            .downcast_ref::<NewTerminalTabWithProfile>()
            .expect("action type should match");

        assert_eq!(
            action,
            &NewTerminalTabWithProfile {
                profile: "work".into()
            }
        );

        let error = <NewTerminalTabWithProfile as Action>::build(
            gpui::private::serde_json::json!({ "profile": "work", "extra": true }),
        )
        .expect_err("unknown profile action fields should be rejected");

        assert!(format!("{error:#}").contains("unknown field"));
    }

    #[test]
    fn parses_new_terminal_split_with_profile_action_input() {
        let action =
            <NewTerminalSplitWithProfile as Action>::build(gpui::private::serde_json::json!({
                "profile": "work",
                "direction": "right"
            }))
            .expect("profile split action input should parse");
        let action = action
            .as_any()
            .downcast_ref::<NewTerminalSplitWithProfile>()
            .expect("action type should match");

        assert_eq!(
            action,
            &NewTerminalSplitWithProfile {
                profile: "work".into(),
                direction: TerminalStartupSplitDirection::Right,
            }
        );

        let error =
            <NewTerminalSplitWithProfile as Action>::build(gpui::private::serde_json::json!({
                "profile": "work",
                "direction": "diagonal"
            }))
            .expect_err("unknown profile split directions should be rejected");
        assert!(format!("{error:#}").contains("unknown variant"));

        let error =
            <NewTerminalSplitWithProfile as Action>::build(gpui::private::serde_json::json!({
                "profile": "work",
                "direction": "right",
                "extra": true
            }))
            .expect_err("unknown profile split action fields should be rejected");
        assert!(format!("{error:#}").contains("unknown field"));
    }

    #[test]
    fn parses_set_default_startup_profile_action_input() {
        let action = <SetDefaultStartupProfile as Action>::build(
            gpui::private::serde_json::json!({ "profile": "work" }),
        )
        .expect("set default profile action input should parse");
        let action = action
            .as_any()
            .downcast_ref::<SetDefaultStartupProfile>()
            .expect("action type should match");

        assert_eq!(
            action,
            &SetDefaultStartupProfile {
                profile: "work".into()
            }
        );

        let error = <SetDefaultStartupProfile as Action>::build(
            gpui::private::serde_json::json!({ "profile": "work", "extra": true }),
        )
        .expect_err("unknown set default profile action fields should be rejected");

        assert!(format!("{error:#}").contains("unknown field"));
    }

    #[test]
    fn parses_clear_default_startup_profile_action_input() {
        let action =
            <ClearDefaultStartupProfile as Action>::build(gpui::private::serde_json::json!({}))
                .expect("clear default profile action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<ClearDefaultStartupProfile>()
                .is_some()
        );
    }

    #[test]
    fn parses_reset_pane_sizes_action_input() {
        let action = <ResetPaneSizes as Action>::build(gpui::private::serde_json::json!({}))
            .expect("reset pane sizes action input should parse");

        assert!(action.as_any().downcast_ref::<ResetPaneSizes>().is_some());
    }

    #[test]
    fn parses_toggle_full_screen_action_input() {
        let action = <ToggleFullScreen as Action>::build(gpui::private::serde_json::json!({}))
            .expect("toggle full screen action input should parse");

        assert!(action.as_any().downcast_ref::<ToggleFullScreen>().is_some());
    }

    #[test]
    fn parses_new_terminal_window_action_input() {
        let action = <NewTerminalWindow as Action>::build(gpui::private::serde_json::json!({}))
            .expect("new terminal window action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<NewTerminalWindow>()
                .is_some()
        );
    }

    #[test]
    fn parses_close_terminal_window_action_input() {
        let action = <CloseTerminalWindow as Action>::build(gpui::private::serde_json::json!({}))
            .expect("close terminal window action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<CloseTerminalWindow>()
                .is_some()
        );
    }

    #[test]
    fn parses_minimize_terminal_window_action_input() {
        let action =
            <MinimizeTerminalWindow as Action>::build(gpui::private::serde_json::json!({}))
                .expect("minimize terminal window action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<MinimizeTerminalWindow>()
                .is_some()
        );
    }

    #[test]
    fn parses_zoom_terminal_window_action_input() {
        let action = <ZoomTerminalWindow as Action>::build(gpui::private::serde_json::json!({}))
            .expect("zoom terminal window action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<ZoomTerminalWindow>()
                .is_some()
        );
    }

    #[test]
    fn parses_open_startup_config_schema_file_action_input() {
        let action =
            <OpenStartupConfigSchemaFile as Action>::build(gpui::private::serde_json::json!({}))
                .expect("open startup config schema file action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<OpenStartupConfigSchemaFile>()
                .is_some()
        );
    }

    #[test]
    fn parses_open_default_keymap_reference_file_action_input() {
        let action =
            <OpenDefaultKeymapReferenceFile as Action>::build(gpui::private::serde_json::json!({}))
                .expect("open default keymap reference file action input should parse");

        assert!(
            action
                .as_any()
                .downcast_ref::<OpenDefaultKeymapReferenceFile>()
                .is_some()
        );
    }

    #[test]
    fn parses_open_log_file_action_input() {
        let action = <OpenLogFile as Action>::build(gpui::private::serde_json::json!({}))
            .expect("open log file action input should parse");

        assert!(action.as_any().downcast_ref::<OpenLogFile>().is_some());
    }

    #[test]
    fn parses_support_directory_action_inputs() {
        let action = <OpenConfigDirectory as Action>::build(gpui::private::serde_json::json!({}))
            .expect("open config directory action input should parse");
        assert!(
            action
                .as_any()
                .downcast_ref::<OpenConfigDirectory>()
                .is_some()
        );

        let action = <OpenDataDirectory as Action>::build(gpui::private::serde_json::json!({}))
            .expect("open data directory action input should parse");
        assert!(
            action
                .as_any()
                .downcast_ref::<OpenDataDirectory>()
                .is_some()
        );

        let action = <OpenLogsDirectory as Action>::build(gpui::private::serde_json::json!({}))
            .expect("open logs directory action input should parse");
        assert!(
            action
                .as_any()
                .downcast_ref::<OpenLogsDirectory>()
                .is_some()
        );

        let action = <OpenThemesDirectory as Action>::build(gpui::private::serde_json::json!({}))
            .expect("open themes directory action input should parse");
        assert!(
            action
                .as_any()
                .downcast_ref::<OpenThemesDirectory>()
                .is_some()
        );
    }

    #[test]
    fn formats_startup_profile_list() {
        let config = sample_startup_profile_list_config();

        let visible = format_startup_profiles(&config, Path::new("terminal.json"), false);

        assert!(visible.contains("startup_config_file: terminal.json"));
        assert!(visible.contains("profiles: 1 visible, 1 hidden"));
        assert!(visible.contains("- work (default)"));
        assert!(visible.contains("  display_name: Work Shell"));
        assert!(visible.contains("  description: Project startup shell"));
        assert!(visible.contains("  icon: terminal"));
        assert!(visible.contains("  color: #0f766e"));
        assert!(visible.contains("  tabs: 2"));
        assert!(!visible.contains("- secret"));

        let all = format_startup_profiles(&config, Path::new("terminal.json"), true);

        assert!(all.contains("- secret (hidden)"));
        assert!(all.contains("  display_name: Secret"));
    }

    #[test]
    fn formats_startup_profile_list_json() {
        let config = sample_startup_profile_list_config();
        let report = startup_profile_list_report(&config, Path::new("terminal.json"), true);
        let output =
            format_startup_profiles_json(&report).expect("profile list json should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile list json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["include_hidden"], true);
        assert_eq!(json["total_count"], 2);
        assert_eq!(json["visible_count"], 1);
        assert_eq!(json["hidden_count"], 1);
        assert_eq!(json["profiles"][0]["name"], "secret");
        assert_eq!(json["profiles"][0]["display_name"], "Secret");
        assert_eq!(json["profiles"][0]["hidden"], true);
        assert_eq!(json["profiles"][0]["is_default"], false);
        assert_eq!(json["profiles"][0]["tab_count"], 1);
        assert_eq!(json["profiles"][1]["name"], "work");
        assert_eq!(json["profiles"][1]["display_name"], "Work Shell");
        assert_eq!(json["profiles"][1]["description"], "Project startup shell");
        assert_eq!(json["profiles"][1]["icon"], "terminal");
        assert_eq!(json["profiles"][1]["color"], "#0f766e");
        assert_eq!(json["profiles"][1]["hidden"], false);
        assert_eq!(json["profiles"][1]["is_default"], true);
        assert_eq!(json["profiles"][1]["tab_count"], 2);
        assert!(output.ends_with('\n'));
    }

    fn sample_startup_profile_list_config() -> TerminalStartupConfig {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Secret".into()),
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Work Shell".into()),
                description: Some("Project startup shell".into()),
                icon: Some("terminal".into()),
                color: Some("#0f766e".into()),
                tabs: vec![TerminalStartupTabConfig::default()],
                ..TerminalStartupProfileConfig::default()
            },
        );
        TerminalStartupConfig {
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        }
    }

    #[test]
    fn formats_startup_profile_description() {
        let report = TerminalStartupProfileDescription {
            startup_config_file: PathBuf::from("terminal.json"),
            profile: "work".into(),
            display_name: Some("Work Shell".into()),
            description: Some("Project startup shell".into()),
            icon: Some("terminal".into()),
            color: Some("#0f766e".into()),
            hidden: true,
            is_default: true,
            working_directory: Some(PathBuf::from(".")),
            command: Some("cmd /C echo work".into()),
            title: Some("Work".into()),
            shell: None,
            env_keys: vec!["API_KEY".into(), "ZED_MODE".into()],
            tabs: vec![TerminalStartupProfileTabDescription {
                profile: Some("admin".into()),
                working_directory: None,
                command: None,
                title: Some("Admin".into()),
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                env_keys: Vec::new(),
                split: Some(TerminalStartupSplitDirection::Right),
            }],
        };

        let output = format_startup_profile_description(&report);

        assert!(output.contains("startup_config_file: terminal.json"));
        assert!(output.contains("status: ok"));
        assert!(output.contains("profile: work"));
        assert!(output.contains("display_name: Work Shell"));
        assert!(output.contains("description: Project startup shell"));
        assert!(output.contains("icon: terminal"));
        assert!(output.contains("color: #0f766e"));
        assert!(output.contains("hidden: true"));
        assert!(output.contains("is_default: true"));
        assert!(output.contains("working_directory: ."));
        assert!(output.contains("command: cmd /C echo work"));
        assert!(output.contains("title: Work"));
        assert!(output.contains("shell: default"));
        assert!(output.contains("env: 2 variables"));
        assert!(output.contains("  - API_KEY"));
        assert!(output.contains("  - ZED_MODE"));
        assert!(output.contains("tabs: 1"));
        assert!(output.contains("- tab 1"));
        assert!(output.contains("  profile: admin"));
        assert!(output.contains("  title: Admin"));
        assert!(output.contains("  shell: pwsh.exe -NoLogo"));
        assert!(output.contains("  split: right"));
    }

    #[test]
    fn formats_startup_profile_description_json() {
        let report = TerminalStartupProfileDescription {
            startup_config_file: PathBuf::from("terminal.json"),
            profile: "work".into(),
            display_name: Some("Work Shell".into()),
            description: Some("Project startup shell".into()),
            icon: Some("terminal".into()),
            color: Some("#0f766e".into()),
            hidden: false,
            is_default: true,
            working_directory: Some(PathBuf::from(".")),
            command: Some("cmd /C echo work".into()),
            title: Some("Work".into()),
            shell: None,
            env_keys: vec!["API_KEY".into(), "ZED_MODE".into()],
            tabs: vec![TerminalStartupProfileTabDescription {
                profile: None,
                working_directory: Some(PathBuf::from("logs")),
                command: Some("pwsh -NoLogo".into()),
                title: Some("Logs".into()),
                shell: None,
                env_keys: vec!["LOG_TOKEN".into()],
                split: Some(TerminalStartupSplitDirection::Down),
            }],
        };

        let output = format_startup_profile_description_json(&report)
            .expect("profile description json should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile description json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["display_name"], "Work Shell");
        assert_eq!(json["description"], "Project startup shell");
        assert_eq!(json["icon"], "terminal");
        assert_eq!(json["color"], "#0f766e");
        assert_eq!(json["hidden"], false);
        assert_eq!(json["is_default"], true);
        assert_eq!(json["working_directory"], ".");
        assert_eq!(json["command"], "cmd /C echo work");
        assert_eq!(json["title"], "Work");
        assert_eq!(json["shell"]["kind"], "default");
        assert_eq!(json["env_count"], 2);
        assert_eq!(json["env_keys"][0], "API_KEY");
        assert_eq!(json["env_keys"][1], "ZED_MODE");
        assert_eq!(json["tab_count"], 1);
        assert_eq!(json["tabs"][0]["tab"], 1);
        assert_eq!(json["tabs"][0]["profile"], serde_json::Value::Null);
        assert_eq!(json["tabs"][0]["working_directory"], "logs");
        assert_eq!(json["tabs"][0]["command"], "pwsh -NoLogo");
        assert_eq!(json["tabs"][0]["title"], "Logs");
        assert_eq!(json["tabs"][0]["shell"]["kind"], "default");
        assert_eq!(json["tabs"][0]["env_count"], 1);
        assert_eq!(json["tabs"][0]["env_keys"][0], "LOG_TOKEN");
        assert_eq!(json["tabs"][0]["split"], "down");
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_description() {
        let report = TerminalStartupDescription {
            startup_config_file: PathBuf::from("terminal.json"),
            source: TerminalDoctorConfigSource::File,
            working_directory: Some(PathBuf::from(".")),
            command: Some("cmd /C echo root".into()),
            title: Some("Root".into()),
            shell: None,
            env_keys: vec!["API_KEY".into(), "ZED_MODE".into()],
            tabs: vec![TerminalStartupProfileTabDescription {
                profile: Some("admin".into()),
                working_directory: None,
                command: None,
                title: Some("Admin".into()),
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                env_keys: vec!["ADMIN_TOKEN".into()],
                split: Some(TerminalStartupSplitDirection::Right),
            }],
            default_profile: Some("work".into()),
            profile_count: 3,
            visible_profile_count: 2,
            hidden_profile_count: 1,
        };

        let output = format_startup_description(&report);

        assert!(output.contains("startup_config_file: terminal.json"));
        assert!(output.contains("status: ok"));
        assert!(output.contains("source: file"));
        assert!(output.contains("working_directory: ."));
        assert!(output.contains("command: cmd /C echo root"));
        assert!(output.contains("title: Root"));
        assert!(output.contains("shell: default"));
        assert!(output.contains("env: 2 variables"));
        assert!(output.contains("  - API_KEY"));
        assert!(output.contains("  - ZED_MODE"));
        assert!(output.contains("tabs: 1"));
        assert!(output.contains("- tab 1"));
        assert!(output.contains("  profile: admin"));
        assert!(output.contains("  title: Admin"));
        assert!(output.contains("  shell: pwsh.exe -NoLogo"));
        assert!(output.contains("  env: 1 variables"));
        assert!(output.contains("    - ADMIN_TOKEN"));
        assert!(output.contains("  split: right"));
        assert!(output.contains("default_profile: work"));
        assert!(output.contains("profiles: 2 visible, 1 hidden"));
    }

    #[test]
    fn formats_startup_description_json_without_env_values() {
        let report = TerminalStartupDescription {
            startup_config_file: PathBuf::from("terminal.json"),
            source: TerminalDoctorConfigSource::File,
            working_directory: Some(PathBuf::from(".")),
            command: Some("cmd /C echo root".into()),
            title: Some("Root".into()),
            shell: None,
            env_keys: vec!["API_KEY".into(), "ZED_MODE".into()],
            tabs: vec![TerminalStartupProfileTabDescription {
                profile: None,
                working_directory: Some(PathBuf::from("logs")),
                command: Some("pwsh -NoLogo".into()),
                title: Some("Logs".into()),
                shell: None,
                env_keys: vec!["LOG_TOKEN".into()],
                split: Some(TerminalStartupSplitDirection::Down),
            }],
            default_profile: Some("work".into()),
            profile_count: 3,
            visible_profile_count: 2,
            hidden_profile_count: 1,
        };

        let output = format_startup_description_json(&report)
            .expect("startup description json should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("startup description json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["source"], "file");
        assert_eq!(json["working_directory"], ".");
        assert_eq!(json["command"], "cmd /C echo root");
        assert_eq!(json["title"], "Root");
        assert_eq!(json["shell"]["kind"], "default");
        assert_eq!(json["env_count"], 2);
        assert_eq!(json["env_keys"][0], "API_KEY");
        assert_eq!(json["env_keys"][1], "ZED_MODE");
        assert_eq!(json["tab_count"], 1);
        assert_eq!(json["tabs"][0]["tab"], 1);
        assert_eq!(json["tabs"][0]["profile"], serde_json::Value::Null);
        assert_eq!(json["tabs"][0]["working_directory"], "logs");
        assert_eq!(json["tabs"][0]["command"], "pwsh -NoLogo");
        assert_eq!(json["tabs"][0]["title"], "Logs");
        assert_eq!(json["tabs"][0]["shell"]["kind"], "default");
        assert_eq!(json["tabs"][0]["env_count"], 1);
        assert_eq!(json["tabs"][0]["env_keys"][0], "LOG_TOKEN");
        assert_eq!(json["tabs"][0]["split"], "down");
        assert_eq!(json["default_profile"], "work");
        assert_eq!(json["profile_count"], 3);
        assert_eq!(json["visible_profile_count"], 2);
        assert_eq!(json["hidden_profile_count"], 1);
        assert!(!output.contains("SECRET_VALUE"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn startup_description_reports_root_config_without_env_values() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let root_working_dir = root_dir.join("root");
        let tab_dir = root_dir.join("logs");
        std_fs::create_dir_all(&root_working_dir).expect("failed to create root dir");
        std_fs::create_dir_all(&tab_dir).expect("failed to create tab dir");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            working_directory: Some(root_working_dir.clone()),
            command: Some("cmd /C echo root".into()),
            title: Some("Root".into()),
            env: test_env(&[("ZED_MODE", "production"), ("API_KEY", "SECRET_VALUE")]),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(tab_dir.clone()),
                command: Some("pwsh -NoLogo".into()),
                title: Some("Logs".into()),
                env: test_env(&[("LOG_TOKEN", "TOKEN_VALUE")]),
                split: Some(TerminalStartupSplitDirection::Right),
                ..TerminalStartupTabConfig::default()
            }],
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        let report = startup_description_report(&config, &startup_config_file)
            .expect("startup description should report");

        assert_eq!(report.source, TerminalDoctorConfigSource::File);
        assert_eq!(report.working_directory, Some(root_working_dir));
        assert_eq!(report.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(report.title.as_deref(), Some("Root"));
        assert_eq!(report.env_keys, vec!["API_KEY", "ZED_MODE"]);
        assert_eq!(report.tabs.len(), 1);
        assert_eq!(report.tabs[0].env_keys, vec!["LOG_TOKEN"]);
        assert_eq!(report.default_profile.as_deref(), Some("work"));
        assert_eq!(report.profile_count, 2);
        assert_eq!(report.visible_profile_count, 1);
        assert_eq!(report.hidden_profile_count, 1);

        let text = format_startup_description(&report);
        assert!(text.contains("  - API_KEY"));
        assert!(text.contains("  - ZED_MODE"));
        assert!(text.contains("  - LOG_TOKEN"));
        assert!(!text.contains("SECRET_VALUE"));
        assert!(!text.contains("TOKEN_VALUE"));

        let json = format_startup_description_json(&report)
            .expect("startup description json should format");
        assert!(json.contains("API_KEY"));
        assert!(json.contains("LOG_TOKEN"));
        assert!(!json.contains("SECRET_VALUE"));
        assert!(!json.contains("TOKEN_VALUE"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_description_reports_initial_source_for_missing_config_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let report =
            startup_description_report(&TerminalStartupConfig::default(), &startup_config_file)
                .expect("default startup description should report");

        assert_eq!(report.source, TerminalDoctorConfigSource::Initial);
        assert_eq!(report.working_directory, None);
        assert_eq!(report.command, None);
        assert_eq!(report.title, None);
        assert_eq!(report.env_keys, Vec::<String>::new());
        assert!(report.tabs.is_empty());
        assert_eq!(report.default_profile, None);
        assert_eq!(report.profile_count, 0);
        assert_eq!(report.visible_profile_count, 0);
        assert_eq!(report.hidden_profile_count, 0);
        assert!(
            !startup_config_file.exists(),
            "describing a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_description_rejects_invalid_config() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            ..TerminalStartupConfig::default()
        };

        let error = startup_description_report(&config, &startup_config_file)
            .expect_err("invalid startup config should be rejected before reporting");
        let message = format!("{error:#}");

        assert!(message.contains("failed to validate terminal startup config"));
        assert!(message.contains("default_profile references missing startup profile: missing"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_profile_description_reports_full_profile_without_env_values() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let profile_dir = root_dir.join("work");
        let tab_dir = root_dir.join("logs");
        std_fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        std_fs::create_dir_all(&tab_dir).expect("failed to create tab dir");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Work Shell".into()),
                description: Some("Project startup shell".into()),
                icon: Some("terminal".into()),
                color: Some("#0f766e".into()),
                working_directory: Some(profile_dir.clone()),
                command: Some("cmd /C echo work".into()),
                title: Some("Work".into()),
                env: test_env(&[("ZED_MODE", "production"), ("API_KEY", "SECRET_VALUE")]),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(tab_dir.clone()),
                    command: Some("pwsh -NoLogo".into()),
                    title: Some("Logs".into()),
                    env: test_env(&[("LOG_TOKEN", "TOKEN_VALUE")]),
                    split: Some(TerminalStartupSplitDirection::Right),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        let report = startup_profile_description_report(&config, &startup_config_file, " work ")
            .expect("profile description should report");

        assert_eq!(report.profile, "work");
        assert_eq!(report.display_name.as_deref(), Some("Work Shell"));
        assert!(report.is_default);
        assert_eq!(report.env_keys, vec!["API_KEY", "ZED_MODE"]);
        assert_eq!(report.tabs.len(), 1);
        assert_eq!(report.tabs[0].env_keys, vec!["LOG_TOKEN"]);
        assert_eq!(
            report.tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );

        let text = format_startup_profile_description(&report);
        assert!(text.contains("  - API_KEY"));
        assert!(text.contains("  - ZED_MODE"));
        assert!(text.contains("  - LOG_TOKEN"));
        assert!(!text.contains("SECRET_VALUE"));
        assert!(!text.contains("TOKEN_VALUE"));

        let json = format_startup_profile_description_json(&report)
            .expect("profile description json should format");
        assert!(json.contains("API_KEY"));
        assert!(json.contains("LOG_TOKEN"));
        assert!(!json.contains("SECRET_VALUE"));
        assert!(!json.contains("TOKEN_VALUE"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_profile_description_allows_hidden_profiles() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Secret".into()),
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let report = startup_profile_description_report(&config, &startup_config_file, "secret")
            .expect("hidden profile should be described by explicit name");

        assert_eq!(report.profile, "secret");
        assert!(report.hidden);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_profile_description_rejects_missing_profile() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = startup_profile_description_report(&config, &startup_config_file, "missing")
            .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: missing"));
        assert!(message.contains("Available profiles: work"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_profile_description_rejects_blank_profile() {
        let error = startup_profile_description_report(
            &TerminalStartupConfig::default(),
            Path::new("terminal.json"),
            "  ",
        )
        .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
    }

    #[test]
    fn startup_profile_description_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = startup_profile_description_report(&config, &startup_config_file, "work")
            .expect_err("missing startup config file should be rejected");

        assert!(format!("{error:#}").contains("failed to read terminal startup config"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn startup_profile_description_rejects_invalid_config() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, "{}").expect("failed to write startup config");
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = startup_profile_description_report(&config, &startup_config_file, "work")
            .expect_err("invalid startup config should be rejected before reporting");
        let message = format!("{error:#}");

        assert!(message.contains("failed to validate terminal startup config"));
        assert!(message.contains("default_profile references missing startup profile: missing"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn formats_resolved_startup_layout_without_env_values() {
        let initial_dir = temp_test_dir();
        let command_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(initial_dir.clone()),
                title: Some("Configured".into()),
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                env: test_env(&[
                    ("ZED_TERMINAL_PROFILE", "work"),
                    ("ZED_TERMINAL_SECRET", "do-not-print"),
                ]),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "--title",
            "CLI",
            "--new-tab-command",
            "cmd /C \"echo build\"",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
            "--new-tab-command-title",
            "Build",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        let output = format_startup_layout(&options, Path::new("terminal.json"));

        assert!(output.contains("startup_config_file: terminal.json"));
        assert!(output.contains("status: ok"));
        assert!(output.contains("tabs: 2"));
        assert!(output.contains("new_terminal_tab:"));
        assert!(output.contains("  title: Configured"));
        assert!(output.contains("  shell: pwsh.exe -NoLogo"));
        assert!(output.contains("- tab 1"));
        assert!(output.contains("  kind: shell"));
        assert!(output.contains("  placement: tab"));
        assert!(output.contains("  title: CLI"));
        assert!(output.contains(&format!(
            "  working_directory: {}",
            dunce::canonicalize(&initial_dir).unwrap().display()
        )));
        assert!(output.contains("  shell: pwsh.exe -NoLogo"));
        assert!(output.contains("- tab 2"));
        assert!(output.contains("  kind: command"));
        assert!(output.contains("  placement: tab"));
        assert!(output.contains("  title: Build"));
        assert!(output.contains(&format!(
            "  working_directory: {}",
            dunce::canonicalize(&command_dir).unwrap().display()
        )));
        assert!(output.contains("  command: cmd /C \"echo build\""));
        assert!(output.contains("  env: 2 variables"));
        assert!(!output.contains("ZED_TERMINAL_SECRET"));
        assert!(!output.contains("do-not-print"));

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn formats_resolved_startup_layout_json_without_env_values() {
        let initial_dir = temp_test_dir();
        let command_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(initial_dir.clone()),
                title: Some("Configured".into()),
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                env: test_env(&[
                    ("ZED_TERMINAL_PROFILE", "work"),
                    ("ZED_TERMINAL_SECRET", "do-not-print"),
                ]),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "--title",
            "CLI",
            "--new-tab-command",
            "cmd /C \"echo build\"",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
            "--new-tab-command-title",
            "Build",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");
        let report = startup_layout_report(&options, Path::new("terminal.json"));

        let output = format_startup_layout_json(&report).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("startup layout json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["tab_count"], 2);
        assert_eq!(json["new_terminal_tab"]["kind"], "shell");
        assert_eq!(json["new_terminal_tab"]["placement"], "tab");
        assert_eq!(
            json["new_terminal_tab"]["split_direction"],
            serde_json::Value::Null
        );
        assert_eq!(json["new_terminal_tab"]["title"], "Configured");
        assert_eq!(json["new_terminal_tab"]["shell"]["kind"], "with_arguments");
        assert_eq!(json["new_terminal_tab"]["shell"]["program"], "pwsh.exe");
        assert_eq!(json["new_terminal_tab"]["shell"]["args"][0], "-NoLogo");
        assert_eq!(json["tabs"][0]["tab"], 1);
        assert_eq!(json["tabs"][0]["kind"], "shell");
        assert_eq!(json["tabs"][0]["placement"], "tab");
        assert_eq!(json["tabs"][0]["title"], "CLI");
        assert_eq!(
            json["tabs"][0]["working_directory"],
            dunce::canonicalize(&initial_dir)
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(json["tabs"][0]["shell"]["label"], "pwsh.exe -NoLogo");
        assert_eq!(json["tabs"][1]["tab"], 2);
        assert_eq!(json["tabs"][1]["kind"], "command");
        assert_eq!(json["tabs"][1]["placement"], "tab");
        assert_eq!(json["tabs"][1]["title"], "Build");
        assert_eq!(
            json["tabs"][1]["working_directory"],
            dunce::canonicalize(&command_dir)
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(json["tabs"][1]["command"]["program"], "cmd");
        assert_eq!(json["tabs"][1]["command"]["args"][0], "/C");
        assert_eq!(json["tabs"][1]["command"]["args"][1], "echo build");
        assert_eq!(json["tabs"][1]["command"]["label"], "cmd /C \"echo build\"");
        assert_eq!(json["tabs"][1]["shell"], serde_json::Value::Null);
        assert_eq!(json["tabs"][1]["env_count"], 2);
        assert!(output.ends_with('\n'));
        assert!(!output.contains("ZED_TERMINAL_SECRET"));
        assert!(!output.contains("do-not-print"));

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn parses_startup_split_tabs_from_config() {
        let config: TerminalStartupConfig = settings::parse_json_with_comments(
            r#"{
  "tabs": [
    { "title": "Right", "split": "right" },
    { "title": "Down", "split": "down" },
    { "title": "Plain" }
  ]
}"#,
        )
        .expect("startup config should parse split tabs");

        assert_eq!(
            config.tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );
        assert_eq!(
            config.tabs[1].split,
            Some(TerminalStartupSplitDirection::Down)
        );
        assert_eq!(config.tabs[2].split, None);
    }

    #[test]
    fn resolves_configured_startup_split_tabs() {
        let right_dir = temp_test_dir();
        let down_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            tabs: vec![
                TerminalStartupTabConfig {
                    working_directory: Some(right_dir.clone()),
                    title: Some("Right".into()),
                    split: Some(TerminalStartupSplitDirection::Right),
                    ..TerminalStartupTabConfig::default()
                },
                TerminalStartupTabConfig {
                    working_directory: Some(down_dir.clone()),
                    command: Some("cmd /C split".into()),
                    title: Some("Down".into()),
                    split: Some(TerminalStartupSplitDirection::Down),
                    ..TerminalStartupTabConfig::default()
                },
            ],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.split, None);
        assert_eq!(options.additional_tabs.len(), 2);
        assert_tab_working_directory(&options.additional_tabs[0], &right_dir);
        assert_eq!(
            options.additional_tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );
        assert_eq!(options.additional_tabs[0].title.as_deref(), Some("Right"));
        assert_tab_working_directory(&options.additional_tabs[1], &down_dir);
        assert_eq!(
            options.additional_tabs[1].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "split".into()],
            })
        );
        assert_eq!(
            options.additional_tabs[1].split,
            Some(TerminalStartupSplitDirection::Down)
        );

        let output = format_startup_layout(&options, Path::new("terminal.json"));
        assert!(output.contains("- tab 1"));
        assert!(output.contains("  placement: tab"));
        assert!(output.contains("- tab 2"));
        assert!(output.contains("  placement: split right"));
        assert!(output.contains("- tab 3"));
        assert!(output.contains("  placement: split down"));

        std_fs::remove_dir_all(right_dir).ok();
        std_fs::remove_dir_all(down_dir).ok();
    }

    #[test]
    fn cli_appended_tabs_do_not_inherit_configured_startup_split() {
        let configured_dir = temp_test_dir();
        let cli_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(configured_dir.clone()),
                split: Some(TerminalStartupSplitDirection::Right),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            cli_dir.to_str().unwrap(),
            "--new-tab-command",
            "cmd /C cli",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 3);
        assert_eq!(
            options.additional_tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );
        assert_eq!(options.additional_tabs[1].split, None);
        assert_eq!(options.additional_tabs[2].split, None);

        std_fs::remove_dir_all(configured_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
    }

    #[test]
    fn validates_startup_config_layouts() {
        let root_dir = temp_test_dir();
        let root_tab_dir = temp_test_dir();
        let profile_dir = temp_test_dir();
        let profile_tab_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(profile_dir.clone()),
                shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(profile_tab_dir.clone()),
                    command: Some("cmd /C profile-tab".into()),
                    env: test_env(&[("ZED_TERMINAL_PROFILE", "work")]),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            working_directory: Some(root_dir.clone()),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(root_tab_dir.clone()),
                ..TerminalStartupTabConfig::default()
            }],
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        assert_eq!(
            config
                .validate()
                .expect("startup config should validate successfully"),
            TerminalStartupConfigValidation {
                layout_count: 2,
                tab_count: 4,
            }
        );

        std_fs::remove_dir_all(root_dir).ok();
        std_fs::remove_dir_all(root_tab_dir).ok();
        std_fs::remove_dir_all(profile_dir).ok();
        std_fs::remove_dir_all(profile_tab_dir).ok();
    }

    #[test]
    fn validate_startup_config_rejects_missing_default_profile() {
        let mut profiles = BTreeMap::new();
        profiles.insert("work".into(), TerminalStartupProfileConfig::default());
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = config
            .validate()
            .expect_err("missing default profile should fail validation");
        let message = format!("{error:#}");

        assert!(message.contains("default_profile references missing startup profile: missing"));
        assert!(message.contains("Available profiles: work"));
    }

    #[test]
    fn validate_startup_config_checks_hidden_profiles() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "secret".into(),
            TerminalStartupProfileConfig {
                hidden: true,
                tabs: vec![TerminalStartupTabConfig {
                    env: test_env(&[("SECRET", "1")]),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };

        let error = config
            .validate()
            .expect_err("hidden profile tab errors should fail validation");
        let message = format!("{error:#}");

        assert!(message.contains("failed to validate startup profile \"secret\""));
        assert!(message.contains("environment variables require a command"));
        assert!(message.contains("tab 2 for startup profile \"secret\""));
    }

    #[test]
    fn formats_startup_config_validation() {
        let report = TerminalStartupConfigValidationReport {
            startup_config_file: PathBuf::from("terminal.json"),
            validation: TerminalStartupConfigValidation {
                layout_count: 2,
                tab_count: 4,
            },
        };

        let output = format_startup_config_validation(&report);

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nlayouts: 2\ntabs: 4\n"
        );
    }

    #[test]
    fn formats_startup_config_validation_json() {
        let report = TerminalStartupConfigValidationReport {
            startup_config_file: PathBuf::from("terminal.json"),
            validation: TerminalStartupConfigValidation {
                layout_count: 2,
                tab_count: 4,
            },
        };

        let output =
            format_startup_config_validation_json(&report).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("startup config validation json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["layout_count"], 2);
        assert_eq!(json["tab_count"], 4);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_config_schema() {
        let schema = format_startup_config_schema().expect("schema should format");
        let schema: gpui::private::serde_json::Value =
            serde_json::from_str(&schema).expect("schema should parse as json");

        let properties = schema
            .get("properties")
            .and_then(gpui::private::serde_json::Value::as_object)
            .expect("schema should contain root properties");

        for property in [
            "working_directory",
            "command",
            "title",
            "shell",
            "env",
            "tabs",
            "default_profile",
            "profiles",
        ] {
            assert!(
                properties.contains_key(property),
                "schema should include {property}: {schema:#}"
            );
        }

        let tab_properties = schema
            .get("$defs")
            .and_then(|defs| defs.get("TerminalStartupTabConfig"))
            .and_then(|tab_config| tab_config.get("properties"))
            .and_then(gpui::private::serde_json::Value::as_object)
            .expect("schema should contain tab item properties");
        assert!(
            tab_properties.contains_key("profile"),
            "schema should include tabs[].profile: {schema:#}"
        );
        assert!(
            tab_properties.contains_key("split"),
            "schema should include tabs[].split: {schema:#}"
        );
    }

    #[test]
    fn writes_startup_config_schema_file_by_refreshing_existing_content() {
        let root_dir = temp_test_dir();
        let schema_file = root_dir.join("config").join("terminal.schema.json");
        std_fs::create_dir_all(schema_file.parent().unwrap()).expect("failed to create config dir");
        std_fs::write(&schema_file, "{ stale schema }\n").expect("failed to write stale schema");

        write_startup_config_schema_file(&schema_file).expect("schema file should write");

        let schema_text = std_fs::read_to_string(&schema_file).expect("failed to read schema file");
        let schema: gpui::private::serde_json::Value =
            serde_json::from_str(&schema_text).expect("schema file should parse as json");
        assert!(
            schema
                .get("properties")
                .and_then(gpui::private::serde_json::Value::as_object)
                .is_some(),
            "schema file should contain root properties: {schema:#}"
        );
        assert_ne!(schema_text, "{ stale schema }\n");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn default_keymap_content_matches_bundled_keymap_asset() {
        assert_eq!(
            default_keymap_content(),
            include_str!("../../../assets/keymaps/zed-terminal.json")
        );
        settings::KeymapFile::parse(default_keymap_content())
            .expect("default keymap reference should parse as a keymap");
    }

    #[test]
    fn writes_default_keymap_reference_file_without_touching_user_keymap() {
        let root_dir = temp_test_dir();
        let config_dir = root_dir.join("config");
        let keymap_file = config_dir.join("keymap.json");
        let reference_file = config_dir.join("default-keymap.json");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(&keymap_file, "custom keymap\n").expect("failed to write user keymap");
        std_fs::write(&reference_file, "{ stale reference }\n")
            .expect("failed to write stale reference");

        write_default_keymap_reference_file(&reference_file)
            .expect("default keymap reference should write");

        assert_eq!(
            std_fs::read_to_string(&keymap_file).expect("failed to read user keymap"),
            "custom keymap\n"
        );
        assert_eq!(
            std_fs::read_to_string(&reference_file).expect("failed to read reference keymap"),
            default_keymap_content()
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn formats_config_initialization() {
        let initialization = TerminalConfigInitialization {
            files: vec![
                TerminalConfigFileInitialization {
                    label: "settings_file",
                    path: PathBuf::from("settings.json"),
                    status: TerminalConfigFileInitializationStatus::Created,
                },
                TerminalConfigFileInitialization {
                    label: "keymap_file",
                    path: PathBuf::from("keymap.json"),
                    status: TerminalConfigFileInitializationStatus::Existing,
                },
            ],
        };

        let output = format_config_initialization(&initialization);

        assert_eq!(
            output,
            "status: ok\nsettings_file: created settings.json\nkeymap_file: existing keymap.json\n"
        );
    }

    #[test]
    fn formats_config_initialization_json() {
        let initialization = TerminalConfigInitialization {
            files: vec![
                TerminalConfigFileInitialization {
                    label: "settings_file",
                    path: PathBuf::from("settings.json"),
                    status: TerminalConfigFileInitializationStatus::Created,
                },
                TerminalConfigFileInitialization {
                    label: "keymap_file",
                    path: PathBuf::from("keymap.json"),
                    status: TerminalConfigFileInitializationStatus::Existing,
                },
            ],
        };

        let output =
            format_config_initialization_json(&initialization).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("config initialization json should parse");

        assert_eq!(json["status"], "ok");
        assert_eq!(json["file_count"], 2);
        assert_eq!(json["created_count"], 1);
        assert_eq!(json["existing_count"], 1);
        assert_eq!(json["files"][0]["label"], "settings_file");
        assert_eq!(json["files"][0]["path"], "settings.json");
        assert_eq!(json["files"][0]["status"], "created");
        assert_eq!(json["files"][1]["label"], "keymap_file");
        assert_eq!(json["files"][1]["path"], "keymap.json");
        assert_eq!(json["files"][1]["status"], "existing");
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_terminal_paths() {
        let output = format_terminal_paths(&sample_path_report());

        assert_eq!(
            output,
            concat!(
                "config_dir: config\n",
                "data_dir: data\n",
                "logs_dir: logs\n",
                "settings_file: settings.json\n",
                "startup_config_file: terminal.json\n",
                "startup_config_schema_file: terminal.schema.json\n",
                "global_settings_file: global-settings.json\n",
                "keymap_file: keymap.json\n",
                "default_keymap_reference_file: default-keymap.json\n",
                "themes_dir: themes\n",
                "log_file: zed-terminal.log\n",
            )
        );
    }

    #[test]
    fn formats_terminal_paths_json() {
        let output =
            format_terminal_paths_json(&sample_path_report()).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("paths json should parse");

        assert_eq!(json["config_dir"], "config");
        assert_eq!(json["data_dir"], "data");
        assert_eq!(json["logs_dir"], "logs");
        assert_eq!(json["settings_file"], "settings.json");
        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["startup_config_schema_file"], "terminal.schema.json");
        assert_eq!(json["global_settings_file"], "global-settings.json");
        assert_eq!(json["keymap_file"], "keymap.json");
        assert_eq!(json["default_keymap_reference_file"], "default-keymap.json");
        assert_eq!(json["themes_dir"], "themes");
        assert_eq!(json["log_file"], "zed-terminal.log");
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_default_profile_update() {
        let output = format_default_profile_update(&TerminalDefaultProfileUpdate {
            path: PathBuf::from("terminal.json"),
            previous_profile: Some("old".into()),
            default_profile: Some("work".into()),
            changed: true,
        });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nprevious_default_profile: old\ndefault_profile: work\nchanged: true\n"
        );

        let first_update = format_default_profile_update(&TerminalDefaultProfileUpdate {
            path: PathBuf::from("terminal.json"),
            previous_profile: None,
            default_profile: Some("work".into()),
            changed: true,
        });

        assert!(first_update.contains("previous_default_profile: none\n"));

        let clear_update = format_default_profile_update(&TerminalDefaultProfileUpdate {
            path: PathBuf::from("terminal.json"),
            previous_profile: Some("work".into()),
            default_profile: None,
            changed: true,
        });

        assert!(clear_update.contains("previous_default_profile: work\n"));
        assert!(clear_update.contains("default_profile: none\n"));
    }

    #[test]
    fn formats_default_profile_update_json() {
        let output = format_default_profile_update_json(&TerminalDefaultProfileUpdate {
            path: PathBuf::from("terminal.json"),
            previous_profile: Some("old".into()),
            default_profile: Some("work".into()),
            changed: true,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("default profile update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["previous_default_profile"], "old");
        assert_eq!(json["default_profile"], "work");
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));

        let clear_output = format_default_profile_update_json(&TerminalDefaultProfileUpdate {
            path: PathBuf::from("terminal.json"),
            previous_profile: Some("work".into()),
            default_profile: None,
            changed: true,
        })
        .expect("clear json output should format");
        let clear_json: serde_json::Value =
            serde_json::from_str(&clear_output).expect("clear json should parse");

        assert_eq!(clear_json["previous_default_profile"], "work");
        assert_eq!(clear_json["default_profile"], serde_json::Value::Null);
    }

    #[test]
    fn formats_startup_profile_creation() {
        let output = format_startup_profile_creation(&TerminalStartupProfileCreation {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            display_name: Some("Work".into()),
            description: Some("Project shell".into()),
            icon: Some("terminal".into()),
            color: Some("#0f766e".into()),
            hidden: true,
            changed: true,
            total_profile_count: 2,
        });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nprofile: work\ndisplay_name: Work\ndescription: Project shell\nicon: terminal\ncolor: #0f766e\nhidden: true\nchanged: true\ntotal_profiles: 2\n"
        );

        let minimal = format_startup_profile_creation(&TerminalStartupProfileCreation {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            display_name: None,
            description: None,
            icon: None,
            color: None,
            hidden: false,
            changed: true,
            total_profile_count: 1,
        });

        assert!(minimal.contains("display_name: none\n"));
        assert!(minimal.contains("description: none\n"));
        assert!(minimal.contains("icon: none\n"));
        assert!(minimal.contains("color: none\n"));
        assert!(minimal.contains("hidden: false\n"));
    }

    #[test]
    fn formats_startup_profile_creation_json() {
        let output = format_startup_profile_creation_json(&TerminalStartupProfileCreation {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            display_name: Some("Work".into()),
            description: Some("Project shell".into()),
            icon: Some("terminal".into()),
            color: Some("#0f766e".into()),
            hidden: true,
            changed: true,
            total_profile_count: 2,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile creation json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["display_name"], "Work");
        assert_eq!(json["description"], "Project shell");
        assert_eq!(json["icon"], "terminal");
        assert_eq!(json["color"], "#0f766e");
        assert_eq!(json["hidden"], true);
        assert_eq!(json["changed"], true);
        assert_eq!(json["total_profile_count"], 2);
        assert!(output.ends_with('\n'));

        let minimal_output =
            format_startup_profile_creation_json(&TerminalStartupProfileCreation {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                display_name: None,
                description: None,
                icon: None,
                color: None,
                hidden: false,
                changed: true,
                total_profile_count: 1,
            })
            .expect("minimal json output should format");
        let minimal_json: serde_json::Value = serde_json::from_str(&minimal_output)
            .expect("minimal profile creation json should parse");

        assert_eq!(minimal_json["display_name"], serde_json::Value::Null);
        assert_eq!(minimal_json["description"], serde_json::Value::Null);
        assert_eq!(minimal_json["icon"], serde_json::Value::Null);
        assert_eq!(minimal_json["color"], serde_json::Value::Null);
        assert_eq!(minimal_json["hidden"], false);
    }

    #[test]
    fn formats_startup_profile_metadata_update() {
        let output =
            format_startup_profile_metadata_update(&TerminalStartupProfileMetadataUpdate {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                previous_display_name: Some("Old Work".into()),
                display_name: Some("Work".into()),
                previous_description: None,
                description: Some("Project shell".into()),
                previous_icon: Some("old-terminal".into()),
                icon: Some("terminal".into()),
                previous_color: Some("#123456".into()),
                color: None,
                changed: true,
            });

        assert_eq!(
            output,
            concat!(
                "startup_config_file: terminal.json\n",
                "status: ok\n",
                "profile: work\n",
                "previous_display_name: Old Work\n",
                "display_name: Work\n",
                "previous_description: none\n",
                "description: Project shell\n",
                "previous_icon: old-terminal\n",
                "icon: terminal\n",
                "previous_color: #123456\n",
                "color: none\n",
                "changed: true\n",
            )
        );
    }

    #[test]
    fn formats_startup_profile_metadata_update_json() {
        let output =
            format_startup_profile_metadata_update_json(&TerminalStartupProfileMetadataUpdate {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                previous_display_name: Some("Old Work".into()),
                display_name: Some("Work".into()),
                previous_description: None,
                description: Some("Project shell".into()),
                previous_icon: Some("old-terminal".into()),
                icon: Some("terminal".into()),
                previous_color: Some("#123456".into()),
                color: None,
                changed: true,
            })
            .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile metadata update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["previous_display_name"], "Old Work");
        assert_eq!(json["display_name"], "Work");
        assert_eq!(json["previous_description"], serde_json::Value::Null);
        assert_eq!(json["description"], "Project shell");
        assert_eq!(json["previous_icon"], "old-terminal");
        assert_eq!(json["icon"], "terminal");
        assert_eq!(json["previous_color"], "#123456");
        assert_eq!(json["color"], serde_json::Value::Null);
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_profile_startup_update() {
        let output = format_startup_profile_startup_update(&TerminalStartupProfileStartupUpdate {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            previous_working_directory: Some(PathBuf::from("old")),
            working_directory: Some(PathBuf::from("new")),
            previous_command: None,
            command: Some("cmd /C echo work".into()),
            previous_title: Some("Old".into()),
            title: Some("Work".into()),
            previous_shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
            shell: None,
            changed: true,
        });

        assert_eq!(
            output,
            concat!(
                "startup_config_file: terminal.json\n",
                "status: ok\n",
                "profile: work\n",
                "previous_working_directory: old\n",
                "working_directory: new\n",
                "previous_command: none\n",
                "command: cmd /C echo work\n",
                "previous_title: Old\n",
                "title: Work\n",
                "previous_shell: pwsh.exe\n",
                "shell: default\n",
                "changed: true\n",
            )
        );
    }

    #[test]
    fn formats_startup_profile_startup_update_json() {
        let output =
            format_startup_profile_startup_update_json(&TerminalStartupProfileStartupUpdate {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                previous_working_directory: Some(PathBuf::from("old")),
                working_directory: Some(PathBuf::from("new")),
                previous_command: Some("cmd /C echo old".into()),
                command: None,
                previous_title: None,
                title: Some("Work".into()),
                previous_shell: None,
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                changed: true,
            })
            .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile startup update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["previous_working_directory"], "old");
        assert_eq!(json["working_directory"], "new");
        assert_eq!(json["previous_command"], "cmd /C echo old");
        assert_eq!(json["command"], serde_json::Value::Null);
        assert_eq!(json["previous_title"], serde_json::Value::Null);
        assert_eq!(json["title"], "Work");
        assert_eq!(json["previous_shell"]["kind"], "default");
        assert_eq!(json["shell"]["kind"], "with_arguments");
        assert_eq!(json["shell"]["program"], "pwsh.exe");
        assert_eq!(json["shell"]["args"], serde_json::json!(["-NoLogo"]));
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_update() {
        let output = format_startup_update(&TerminalStartupUpdate {
            path: PathBuf::from("terminal.json"),
            previous_working_directory: Some(PathBuf::from("old")),
            working_directory: Some(PathBuf::from("new")),
            previous_command: None,
            command: Some("cmd /C echo root".into()),
            previous_title: Some("Old".into()),
            title: Some("Root".into()),
            previous_shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
            shell: None,
            changed: true,
        });

        assert_eq!(
            output,
            concat!(
                "startup_config_file: terminal.json\n",
                "status: ok\n",
                "previous_working_directory: old\n",
                "working_directory: new\n",
                "previous_command: none\n",
                "command: cmd /C echo root\n",
                "previous_title: Old\n",
                "title: Root\n",
                "previous_shell: pwsh.exe\n",
                "shell: default\n",
                "changed: true\n",
            )
        );
    }

    #[test]
    fn formats_startup_update_json() {
        let output = format_startup_update_json(&TerminalStartupUpdate {
            path: PathBuf::from("terminal.json"),
            previous_working_directory: Some(PathBuf::from("old")),
            working_directory: Some(PathBuf::from("new")),
            previous_command: Some("cmd /C echo old".into()),
            command: None,
            previous_title: None,
            title: Some("Root".into()),
            previous_shell: None,
            shell: Some(TerminalStartupShellConfig::WithArguments(
                TerminalStartupShellWithArgumentsConfig {
                    program: "pwsh.exe".into(),
                    args: vec!["-NoLogo".into()],
                },
            )),
            changed: true,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("root startup update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["previous_working_directory"], "old");
        assert_eq!(json["working_directory"], "new");
        assert_eq!(json["previous_command"], "cmd /C echo old");
        assert_eq!(json["command"], serde_json::Value::Null);
        assert_eq!(json["previous_title"], serde_json::Value::Null);
        assert_eq!(json["title"], "Root");
        assert_eq!(json["previous_shell"]["kind"], "default");
        assert_eq!(json["shell"]["kind"], "with_arguments");
        assert_eq!(json["shell"]["program"], "pwsh.exe");
        assert_eq!(json["shell"]["args"], serde_json::json!(["-NoLogo"]));
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_env_update() {
        let output = format_startup_env_update(&TerminalStartupEnvUpdate {
            path: PathBuf::from("terminal.json"),
            previous_env_keys: vec!["API_KEY".into(), "MODE".into()],
            env_keys: vec!["MODE".into(), "TOKEN".into()],
            added_env_keys: vec!["TOKEN".into()],
            updated_env_keys: vec!["MODE".into()],
            removed_env_keys: vec!["API_KEY".into()],
            cleared: false,
            changed: true,
        });

        assert_eq!(
            output,
            concat!(
                "startup_config_file: terminal.json\n",
                "status: ok\n",
                "previous_env: 2 variables\n",
                "  - API_KEY\n",
                "  - MODE\n",
                "env: 2 variables\n",
                "  - MODE\n",
                "  - TOKEN\n",
                "added_env_keys: 1 variables\n",
                "  - TOKEN\n",
                "updated_env_keys: 1 variables\n",
                "  - MODE\n",
                "removed_env_keys: 1 variables\n",
                "  - API_KEY\n",
                "cleared: false\n",
                "changed: true\n",
            )
        );
        assert!(!output.contains("secret"));
    }

    #[test]
    fn formats_startup_env_update_json_without_values() {
        let output = format_startup_env_update_json(&TerminalStartupEnvUpdate {
            path: PathBuf::from("terminal.json"),
            previous_env_keys: vec!["API_KEY".into(), "MODE".into()],
            env_keys: vec!["MODE".into(), "TOKEN".into()],
            added_env_keys: vec!["TOKEN".into()],
            updated_env_keys: vec!["MODE".into()],
            removed_env_keys: vec!["API_KEY".into()],
            cleared: true,
            changed: true,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("root env update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["previous_env_count"], 2);
        assert_eq!(
            json["previous_env_keys"],
            serde_json::json!(["API_KEY", "MODE"])
        );
        assert_eq!(json["env_count"], 2);
        assert_eq!(json["env_keys"], serde_json::json!(["MODE", "TOKEN"]));
        assert_eq!(json["added_env_keys"], serde_json::json!(["TOKEN"]));
        assert_eq!(json["updated_env_keys"], serde_json::json!(["MODE"]));
        assert_eq!(json["removed_env_keys"], serde_json::json!(["API_KEY"]));
        assert_eq!(json["cleared"], true);
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));
        assert!(!output.contains("secret"));
    }

    #[test]
    fn formats_startup_profile_env_update() {
        let output = format_startup_profile_env_update(&TerminalStartupProfileEnvUpdate {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            previous_env_keys: vec!["API_KEY".into(), "MODE".into()],
            env_keys: vec!["MODE".into(), "TOKEN".into()],
            added_env_keys: vec!["TOKEN".into()],
            updated_env_keys: vec!["MODE".into()],
            removed_env_keys: vec!["API_KEY".into()],
            cleared: false,
            changed: true,
        });

        assert_eq!(
            output,
            concat!(
                "startup_config_file: terminal.json\n",
                "status: ok\n",
                "profile: work\n",
                "previous_env: 2 variables\n",
                "  - API_KEY\n",
                "  - MODE\n",
                "env: 2 variables\n",
                "  - MODE\n",
                "  - TOKEN\n",
                "added_env_keys: 1 variables\n",
                "  - TOKEN\n",
                "updated_env_keys: 1 variables\n",
                "  - MODE\n",
                "removed_env_keys: 1 variables\n",
                "  - API_KEY\n",
                "cleared: false\n",
                "changed: true\n",
            )
        );
        assert!(!output.contains("secret"));
    }

    #[test]
    fn formats_startup_profile_env_update_json_without_values() {
        let output = format_startup_profile_env_update_json(&TerminalStartupProfileEnvUpdate {
            path: PathBuf::from("terminal.json"),
            profile: "work".into(),
            previous_env_keys: vec!["API_KEY".into(), "MODE".into()],
            env_keys: vec!["MODE".into(), "TOKEN".into()],
            added_env_keys: vec!["TOKEN".into()],
            updated_env_keys: vec!["MODE".into()],
            removed_env_keys: vec!["API_KEY".into()],
            cleared: true,
            changed: true,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile env update json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["previous_env_count"], 2);
        assert_eq!(
            json["previous_env_keys"],
            serde_json::json!(["API_KEY", "MODE"])
        );
        assert_eq!(json["env_count"], 2);
        assert_eq!(json["env_keys"], serde_json::json!(["MODE", "TOKEN"]));
        assert_eq!(json["added_env_keys"], serde_json::json!(["TOKEN"]));
        assert_eq!(json["updated_env_keys"], serde_json::json!(["MODE"]));
        assert_eq!(json["removed_env_keys"], serde_json::json!(["API_KEY"]));
        assert_eq!(json["cleared"], true);
        assert_eq!(json["changed"], true);
        assert!(output.ends_with('\n'));
        assert!(!output.contains("secret"));
    }

    #[test]
    fn formats_startup_profile_copy() {
        let output = format_startup_profile_copy(&TerminalStartupProfileCopy {
            path: PathBuf::from("terminal.json"),
            source_profile: "old".into(),
            profile: "new".into(),
            changed: true,
            copied_tab_count: 3,
            total_profile_count: 4,
        });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nsource_profile: old\nprofile: new\nchanged: true\ncopied_tabs: 3\ntotal_profiles: 4\n"
        );
    }

    #[test]
    fn formats_startup_profile_copy_json() {
        let output = format_startup_profile_copy_json(&TerminalStartupProfileCopy {
            path: PathBuf::from("terminal.json"),
            source_profile: "old".into(),
            profile: "new".into(),
            changed: true,
            copied_tab_count: 3,
            total_profile_count: 4,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile copy json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["source_profile"], "old");
        assert_eq!(json["profile"], "new");
        assert_eq!(json["changed"], true);
        assert_eq!(json["copied_tab_count"], 3);
        assert_eq!(json["total_profile_count"], 4);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_profile_removal() {
        let output = format_startup_profile_removal(&TerminalStartupProfileRemoval {
            path: PathBuf::from("terminal.json"),
            profile: "old".into(),
            changed: true,
            remaining_profile_count: 2,
        });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nprofile: old\nchanged: true\nremaining_profiles: 2\n"
        );
    }

    #[test]
    fn formats_startup_profile_removal_json() {
        let output = format_startup_profile_removal_json(&TerminalStartupProfileRemoval {
            path: PathBuf::from("terminal.json"),
            profile: "old".into(),
            changed: false,
            remaining_profile_count: 1,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile removal json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "old");
        assert_eq!(json["changed"], false);
        assert_eq!(json["remaining_profile_count"], 1);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_profile_rename() {
        let output = format_startup_profile_rename(&TerminalStartupProfileRename {
            path: PathBuf::from("terminal.json"),
            previous_profile: "old".into(),
            profile: "new".into(),
            changed: true,
            updated_reference_count: 3,
        });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nprevious_profile: old\nprofile: new\nchanged: true\nupdated_references: 3\n"
        );
    }

    #[test]
    fn formats_startup_profile_rename_json() {
        let output = format_startup_profile_rename_json(&TerminalStartupProfileRename {
            path: PathBuf::from("terminal.json"),
            previous_profile: "old".into(),
            profile: "new".into(),
            changed: false,
            updated_reference_count: 0,
        })
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile rename json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["previous_profile"], "old");
        assert_eq!(json["profile"], "new");
        assert_eq!(json["changed"], false);
        assert_eq!(json["updated_reference_count"], 0);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_startup_profile_visibility_update() {
        let output =
            format_startup_profile_visibility_update(&TerminalStartupProfileVisibilityUpdate {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                previous_hidden: false,
                hidden: true,
                changed: true,
            });

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nprofile: work\nprevious_hidden: false\nhidden: true\nchanged: true\n"
        );
    }

    #[test]
    fn formats_startup_profile_visibility_update_json() {
        let output = format_startup_profile_visibility_update_json(
            &TerminalStartupProfileVisibilityUpdate {
                path: PathBuf::from("terminal.json"),
                profile: "work".into(),
                previous_hidden: true,
                hidden: false,
                changed: false,
            },
        )
        .expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("profile visibility json should parse");

        assert_eq!(json["startup_config_file"], "terminal.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["profile"], "work");
        assert_eq!(json["previous_hidden"], true);
        assert_eq!(json["hidden"], false);
        assert_eq!(json["changed"], false);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_doctor_report() {
        let output = format_doctor_report(&sample_doctor_report());

        assert_eq!(
            output,
            concat!(
                "status: error\n",
                "directories:\n",
                "  data_dir: ok data\n",
                "  logs_dir: missing logs\n",
                "config_files:\n",
                "  settings_file: error settings.json\n",
                "    message: expected a file\n",
                "startup_config:\n",
                "  startup_config_file: ok terminal.json\n",
                "  source: file\n",
                "  layouts: 2\n",
                "  tabs: 4\n",
                "keymap:\n",
                "  keymap_file: missing keymap.json\n",
                "  source: initial\n",
                "  default_bindings: 31\n",
                "  user_bindings: 0\n",
            )
        );
    }

    #[test]
    fn formats_doctor_report_json() {
        let output =
            format_doctor_report_json(&sample_doctor_report()).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("doctor json should parse");

        assert_eq!(json["status"], "error");
        assert_eq!(json["directories"][0]["label"], "data_dir");
        assert_eq!(json["directories"][0]["status"], "ok");
        assert_eq!(json["config_files"][0]["status"], "error");
        assert_eq!(json["config_files"][0]["message"], "expected a file");
        assert_eq!(json["startup_config"]["source"], "file");
        assert_eq!(json["startup_config"]["validation"]["layouts"], 2);
        assert_eq!(json["startup_config"]["validation"]["tabs"], 4);
        assert_eq!(json["keymap"]["status"], "missing");
        assert_eq!(json["keymap"]["source"], "initial");
        assert_eq!(json["keymap"]["validation"]["default_bindings"], 31);
        assert_eq!(json["keymap"]["validation"]["user_bindings"], 0);
        assert_eq!(
            json["keymap"]["validation"]["user_keymap_source"],
            "initial"
        );
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn doctor_report_treats_missing_as_non_fatal() {
        let report = TerminalDoctorReport {
            directories: vec![TerminalDoctorPathCheck {
                label: "data_dir",
                path: PathBuf::from("data"),
                status: TerminalDoctorCheckStatus::Missing,
                message: None,
            }],
            config_files: vec![TerminalDoctorPathCheck {
                label: "settings_file",
                path: PathBuf::from("settings.json"),
                status: TerminalDoctorCheckStatus::Missing,
                message: None,
            }],
            startup_config: TerminalDoctorStartupConfigCheck {
                path: PathBuf::from("terminal.json"),
                status: TerminalDoctorCheckStatus::Missing,
                source: Some(TerminalDoctorConfigSource::Initial),
                validation: Some(TerminalStartupConfigValidation {
                    layout_count: 1,
                    tab_count: 1,
                }),
                message: None,
            },
            keymap: TerminalDoctorKeymapCheck {
                path: PathBuf::from("keymap.json"),
                status: TerminalDoctorCheckStatus::Missing,
                source: Some(TerminalUserKeymapSource::Initial),
                validation: Some(TerminalKeymapValidation {
                    default_binding_count: 31,
                    user_binding_count: 0,
                    user_keymap_source: TerminalUserKeymapSource::Initial,
                }),
                message: None,
            },
        };

        assert!(!report.has_errors());
        assert!(format_doctor_report(&report).starts_with("status: ok\n"));
    }

    fn sample_path_report() -> TerminalPathReport {
        TerminalPathReport {
            config_dir: PathBuf::from("config"),
            data_dir: PathBuf::from("data"),
            logs_dir: PathBuf::from("logs"),
            settings_file: PathBuf::from("settings.json"),
            startup_config_file: PathBuf::from("terminal.json"),
            startup_config_schema_file: PathBuf::from("terminal.schema.json"),
            global_settings_file: PathBuf::from("global-settings.json"),
            keymap_file: PathBuf::from("keymap.json"),
            default_keymap_reference_file: PathBuf::from("default-keymap.json"),
            themes_dir: PathBuf::from("themes"),
            log_file: PathBuf::from("zed-terminal.log"),
        }
    }

    fn sample_doctor_report() -> TerminalDoctorReport {
        TerminalDoctorReport {
            directories: vec![
                TerminalDoctorPathCheck {
                    label: "data_dir",
                    path: PathBuf::from("data"),
                    status: TerminalDoctorCheckStatus::Ok,
                    message: None,
                },
                TerminalDoctorPathCheck {
                    label: "logs_dir",
                    path: PathBuf::from("logs"),
                    status: TerminalDoctorCheckStatus::Missing,
                    message: None,
                },
            ],
            config_files: vec![TerminalDoctorPathCheck {
                label: "settings_file",
                path: PathBuf::from("settings.json"),
                status: TerminalDoctorCheckStatus::Error,
                message: Some("expected a file".into()),
            }],
            startup_config: TerminalDoctorStartupConfigCheck {
                path: PathBuf::from("terminal.json"),
                status: TerminalDoctorCheckStatus::Ok,
                source: Some(TerminalDoctorConfigSource::File),
                validation: Some(TerminalStartupConfigValidation {
                    layout_count: 2,
                    tab_count: 4,
                }),
                message: None,
            },
            keymap: TerminalDoctorKeymapCheck {
                path: PathBuf::from("keymap.json"),
                status: TerminalDoctorCheckStatus::Missing,
                source: Some(TerminalUserKeymapSource::Initial),
                validation: Some(TerminalKeymapValidation {
                    default_binding_count: 31,
                    user_binding_count: 0,
                    user_keymap_source: TerminalUserKeymapSource::Initial,
                }),
                message: None,
            },
        }
    }

    #[test]
    fn diagnose_path_reports_kind_mismatches() {
        let root_dir = temp_test_dir();
        let file_path = root_dir.join("settings.json");
        let directory_path = root_dir.join("config");
        std_fs::write(&file_path, "{}\n").expect("failed to write test file");
        std_fs::create_dir_all(&directory_path).expect("failed to create test directory");

        assert_eq!(
            diagnose_path(
                "settings_file",
                file_path.clone(),
                TerminalDoctorPathKind::File
            )
            .status,
            TerminalDoctorCheckStatus::Ok
        );
        let file_as_directory =
            diagnose_path("data_dir", file_path, TerminalDoctorPathKind::Directory);
        assert_eq!(file_as_directory.status, TerminalDoctorCheckStatus::Error);
        assert_eq!(
            file_as_directory.message.as_deref(),
            Some("expected a directory")
        );

        let directory_as_file = diagnose_path(
            "settings_file",
            directory_path,
            TerminalDoctorPathKind::File,
        );
        assert_eq!(directory_as_file.status, TerminalDoctorCheckStatus::Error);
        assert_eq!(
            directory_as_file.message.as_deref(),
            Some("expected a file")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn diagnose_terminal_directories_does_not_create_missing_paths() {
        let root_dir = env::temp_dir().join(format!(
            "zed-terminal-doctor-read-only-{}",
            uuid::Uuid::new_v4()
        ));
        let path_options = TerminalPathOptions {
            data_dir: root_dir.join("data"),
            config_dir: root_dir.join("config"),
        };
        assert!(!path_options.data_dir.exists());
        assert!(!path_options.config_dir.exists());

        let directories = diagnose_terminal_directories(&path_options);

        assert_eq!(
            directories
                .iter()
                .map(|check| (check.label, check.status))
                .collect::<Vec<_>>(),
            vec![
                ("data_dir", TerminalDoctorCheckStatus::Missing),
                ("config_dir", TerminalDoctorCheckStatus::Missing),
                ("logs_dir", TerminalDoctorCheckStatus::Missing),
                ("themes_dir", TerminalDoctorCheckStatus::Missing),
            ]
        );
        assert!(!path_options.data_dir.exists());
        assert!(!path_options.config_dir.exists());
        assert!(!root_dir.exists());
    }

    #[test]
    fn diagnose_terminal_config_files_reports_support_reference_files() {
        let root_dir = temp_test_dir();
        let config_dir = root_dir.join("config");
        let path_options = TerminalPathOptions {
            data_dir: root_dir.join("data"),
            config_dir: config_dir.clone(),
        };

        let checks = diagnose_terminal_config_files(TerminalConfigFilePaths::from_path_options(
            &path_options,
        ));

        assert!(
            checks.iter().any(|check| {
                check.label == "startup_config_schema_file"
                    && check.path == terminal_startup_config_schema_file(&config_dir)
                    && check.status == TerminalDoctorCheckStatus::Missing
            }),
            "doctor config checks should include startup config schema file: {checks:#?}"
        );
        assert!(
            checks.iter().any(|check| {
                check.label == "default_keymap_reference_file"
                    && check.path == terminal_default_keymap_reference_file(&config_dir)
                    && check.status == TerminalDoctorCheckStatus::Missing
            }),
            "doctor config checks should include default keymap reference file: {checks:#?}"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn diagnose_startup_config_file_uses_initial_config_when_missing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let check = diagnose_startup_config_file(startup_config_file);

        assert_eq!(check.status, TerminalDoctorCheckStatus::Missing);
        assert_eq!(check.source, Some(TerminalDoctorConfigSource::Initial));
        assert_eq!(
            check.validation,
            Some(TerminalStartupConfigValidation {
                layout_count: 1,
                tab_count: 1,
            })
        );
        assert_eq!(check.message, None);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn diagnose_startup_config_file_reports_invalid_config() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, "{ broken terminal config")
            .expect("failed to write broken startup config");

        let check = diagnose_startup_config_file(startup_config_file);

        assert_eq!(check.status, TerminalDoctorCheckStatus::Error);
        assert_eq!(check.source, Some(TerminalDoctorConfigSource::File));
        assert_eq!(check.validation, None);
        assert!(
            check
                .message
                .as_deref()
                .is_some_and(|message| message.contains("failed to parse terminal startup config"))
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn initializes_missing_config_files_without_overwriting_existing_files() {
        let root_dir = temp_test_dir();
        let config_dir = root_dir.join("config");
        let settings_file = config_dir.join("settings.json");
        let global_settings_file = config_dir.join("global_settings.json");
        let keymap_file = config_dir.join("keymap.json");
        let default_keymap_reference_file = config_dir.join("default-keymap.json");
        let startup_config_file = config_dir.join("terminal.json");
        let startup_config_schema_file = config_dir.join("terminal.schema.json");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(&keymap_file, "custom keymap\n").expect("failed to write keymap");

        let initialization = initialize_terminal_config_files_at(TerminalConfigFilePaths {
            settings_file: settings_file.clone(),
            global_settings_file: global_settings_file.clone(),
            keymap_file: keymap_file.clone(),
            default_keymap_reference_file: default_keymap_reference_file.clone(),
            startup_config_file: startup_config_file.clone(),
            startup_config_schema_file: startup_config_schema_file.clone(),
        })
        .expect("config files should initialize");

        assert_eq!(
            initialization
                .files
                .iter()
                .map(|file| (file.label, file.status))
                .collect::<Vec<_>>(),
            vec![
                (
                    "settings_file",
                    TerminalConfigFileInitializationStatus::Created
                ),
                (
                    "global_settings_file",
                    TerminalConfigFileInitializationStatus::Created
                ),
                (
                    "keymap_file",
                    TerminalConfigFileInitializationStatus::Existing
                ),
                (
                    "default_keymap_reference_file",
                    TerminalConfigFileInitializationStatus::Created
                ),
                (
                    "startup_config_file",
                    TerminalConfigFileInitializationStatus::Created
                ),
                (
                    "startup_config_schema_file",
                    TerminalConfigFileInitializationStatus::Created
                ),
            ]
        );
        assert!(settings_file.exists());
        assert!(global_settings_file.exists());
        assert_eq!(
            std_fs::read_to_string(&keymap_file).expect("failed to read keymap"),
            "custom keymap\n"
        );
        assert_eq!(
            std_fs::read_to_string(&default_keymap_reference_file)
                .expect("failed to read default keymap reference"),
            default_keymap_content()
        );
        let startup_config: TerminalStartupConfig = settings::parse_json_with_comments(
            &std_fs::read_to_string(&startup_config_file).expect("failed to read startup config"),
        )
        .expect("startup config should parse");
        assert_eq!(startup_config, TerminalStartupConfig::default());
        let startup_config_schema: gpui::private::serde_json::Value = serde_json::from_str(
            &std_fs::read_to_string(&startup_config_schema_file)
                .expect("failed to read startup config schema"),
        )
        .expect("startup config schema should parse as json");
        assert!(
            startup_config_schema.get("properties").is_some(),
            "startup config schema should include root properties"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn ensure_log_file_creates_parent_and_preserves_existing_content() {
        let root_dir = temp_test_dir();
        let log_file = root_dir.join("logs").join("Zed Terminal.log");

        ensure_log_file(&log_file).expect("missing log file should be created");
        assert!(log_file.is_file());

        std_fs::write(&log_file, "existing log\n").expect("failed to write existing log");
        ensure_log_file(&log_file).expect("existing log file should still open");

        assert_eq!(
            std_fs::read_to_string(&log_file).expect("failed to read log file"),
            "existing log\n"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn initialize_config_file_rejects_directory_targets() {
        let root_dir = temp_test_dir();
        let directory_target = root_dir.join("settings.json");
        std_fs::create_dir_all(&directory_target).expect("failed to create directory target");

        let error = initialize_terminal_config_file("settings_file", directory_target, "{}\n")
            .expect_err("directory target should fail");
        assert!(format!("{error:#}").contains("exists but is not a file"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_default_startup_profile_updates_jsonc_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "default_profile": null,
  // keep profile comment
  "profiles": {
    "secret": {
      "hidden": true
    },
    "work": {
      "display_name": "Work"
    }
  },
  "tabs": []
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_default_startup_profile(&startup_config_file, " work ")
            .expect("default profile should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.previous_profile, None);
        assert_eq!(update.default_profile.as_deref(), Some("work"));
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile comment"));
        assert!(content.contains(r#""default_profile": "work""#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile.as_deref(), Some("work"));
        assert!(updated_config.profiles["secret"].hidden);
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_default_startup_profile_inserts_missing_default_profile_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_default_startup_profile(&startup_config_file, "work")
            .expect("default profile should be inserted");

        assert_eq!(update.previous_profile, None);
        assert_eq!(update.default_profile.as_deref(), Some("work"));
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""default_profile": "work""#));
        assert!(content.contains(r#""profiles""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile.as_deref(), Some("work"));
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_default_startup_profile_reports_unchanged_default() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "default_profile": "work",
  "profiles": {
    "work": {}
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_default_startup_profile(&startup_config_file, "work")
            .expect("existing default profile should update successfully");

        assert_eq!(update.previous_profile.as_deref(), Some("work"));
        assert_eq!(update.default_profile.as_deref(), Some("work"));
        assert!(!update.changed);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_default_startup_profile_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "default_profile": "work",
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = set_default_startup_profile(&startup_config_file, "missing")
            .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("default_profile references missing startup profile: missing"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_default_startup_profile_rejects_blank_profile() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, r#"{ "profiles": { "work": {} } }"#)
            .expect("failed to write startup config");

        let error = set_default_startup_profile(&startup_config_file, "  ")
            .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn clear_default_startup_profile_updates_jsonc_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "title": "Root",
  "default_profile": "work",
  // keep profile comment
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = clear_default_startup_profile(&startup_config_file)
            .expect("default profile should clear");

        assert_eq!(update.previous_profile.as_deref(), Some("work"));
        assert_eq!(update.default_profile, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile comment"));
        assert!(content.contains(r#""default_profile": null"#));
        assert!(content.contains(r#""title": "Root""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile, None);
        assert_eq!(updated_config.title.as_deref(), Some("Root"));
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn clear_default_startup_profile_reports_unchanged_when_absent() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "default_profile": null,
  "profiles": {
    "work": {}
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = clear_default_startup_profile(&startup_config_file)
            .expect("missing default profile should still clear successfully");

        assert_eq!(update.previous_profile, None);
        assert_eq!(update.default_profile, None);
        assert!(!update.changed);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn clear_default_startup_profile_reports_unchanged_when_file_is_missing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let update = clear_default_startup_profile(&startup_config_file)
            .expect("missing startup config should be a no-op when clearing");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.previous_profile, None);
        assert_eq!(update.default_profile, None);
        assert!(!update.changed);
        assert!(
            !update.path.exists(),
            "clearing a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn clear_default_startup_profile_repairs_missing_default_profile_reference() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "default_profile": "missing",
  "profiles": {
    "work": {}
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = clear_default_startup_profile(&startup_config_file)
            .expect("broken default profile reference should be repairable");

        assert_eq!(update.previous_profile.as_deref(), Some("missing"));
        assert_eq!(update.default_profile, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile, None);
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_initializes_missing_config_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("config").join("terminal.json");
        let creation = create_startup_profile(
            &startup_config_file,
            " work ",
            &TerminalStartupProfileCreationMetadata {
                display_name: Some(" Work Shell ".into()),
                description: Some(" Project shell ".into()),
                icon: Some(" terminal ".into()),
                color: Some(" #0f766e ".into()),
                hidden: true,
            },
        )
        .expect("startup profile should be created in missing config");

        assert_eq!(creation.path, startup_config_file);
        assert_eq!(creation.profile, "work");
        assert_eq!(creation.display_name.as_deref(), Some("Work Shell"));
        assert_eq!(creation.description.as_deref(), Some("Project shell"));
        assert_eq!(creation.icon.as_deref(), Some("terminal"));
        assert_eq!(creation.color.as_deref(), Some("#0f766e"));
        assert!(creation.hidden);
        assert!(creation.changed);
        assert_eq!(creation.total_profile_count, 1);

        let content =
            std_fs::read_to_string(&creation.path).expect("failed to read created startup config");
        assert!(content.contains("// Zed Terminal startup layout."));
        assert!(content.contains(r#""profiles""#));
        assert!(content.contains(r#""work""#));
        assert!(content.contains(r#""display_name": "Work Shell""#));
        assert!(content.contains(r#""description": "Project shell""#));
        assert!(content.contains(r#""icon": "terminal""#));
        assert!(content.contains("\"color\": \"#0f766e\""));
        assert!(content.contains(r#""hidden": true"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("created config should parse");
        updated_config
            .validate()
            .expect("created config should validate");
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work Shell")
        );
        assert_eq!(
            updated_config.profiles["work"].description.as_deref(),
            Some("Project shell")
        );
        assert_eq!(
            updated_config.profiles["work"].icon.as_deref(),
            Some("terminal")
        );
        assert_eq!(
            updated_config.profiles["work"].color.as_deref(),
            Some("#0f766e")
        );
        assert!(updated_config.profiles["work"].hidden);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_preserves_jsonc_comments_and_existing_profiles() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep profile map comment
  "profiles": {
    // keep old profile comment
    "old": {
      "display_name": "Old"
    }
  },
  "tabs": []
}
"#,
        )
        .expect("failed to write startup config");

        let creation = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata {
                display_name: Some("Work".into()),
                description: None,
                icon: None,
                color: None,
                hidden: false,
            },
        )
        .expect("startup profile should be created");

        assert_eq!(creation.profile, "work");
        assert_eq!(creation.display_name.as_deref(), Some("Work"));
        assert!(!creation.hidden);
        assert_eq!(creation.total_profile_count, 2);

        let content =
            std_fs::read_to_string(&creation.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile map comment"));
        assert!(content.contains("// keep old profile comment"));
        assert!(content.contains(r#""old""#));
        assert!(content.contains(r#""work""#));
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(!content.contains(r#""hidden": false"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        assert_eq!(updated_config.profiles.len(), 2);
        assert_eq!(
            updated_config.profiles["old"].display_name.as_deref(),
            Some("Old")
        );
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );
        assert!(!updated_config.profiles["work"].hidden);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_inserts_missing_profiles_map() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "title": "Root"
}
"#,
        )
        .expect("failed to write startup config");

        let creation = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata::default(),
        )
        .expect("startup profile should be created when profiles map is missing");

        assert_eq!(creation.profile, "work");
        assert_eq!(creation.total_profile_count, 1);

        let content =
            std_fs::read_to_string(&creation.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains(r#""profiles""#));
        assert!(content.contains(r#""work""#));
        assert!(content.contains(r#""title": "Root""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.title.as_deref(), Some("Root"));
        assert!(updated_config.profiles.contains_key("work"));
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_repairs_missing_default_profile_reference() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "default_profile": "work",
  "profiles": {}
}
"#,
        )
        .expect("failed to write startup config");

        let creation = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata::default(),
        )
        .expect("creating the referenced profile should repair default_profile");

        assert_eq!(creation.profile, "work");
        assert_eq!(creation.total_profile_count, 1);

        let content =
            std_fs::read_to_string(&creation.path).expect("failed to read updated startup config");
        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile.as_deref(), Some("work"));
        assert!(updated_config.profiles.contains_key("work"));
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_rejects_unrepaired_invalid_config_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "default_profile": "missing",
  "profiles": {}
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata::default(),
        )
        .expect_err("unrepaired invalid startup config should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("refusing to create startup profile"));
        assert!(message.contains("default_profile references missing startup profile: missing"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected creation"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_normalizes_blank_metadata() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(&startup_config_file, r#"{ "profiles": {} }"#)
            .expect("failed to write startup config");

        let creation = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata {
                display_name: Some("  ".into()),
                description: Some("\t".into()),
                icon: Some(" ".into()),
                color: Some(" ".into()),
                hidden: false,
            },
        )
        .expect("startup profile should be created");

        assert_eq!(creation.display_name, None);
        assert_eq!(creation.description, None);
        assert_eq!(creation.icon, None);
        assert_eq!(creation.color, None);
        assert!(!creation.hidden);

        let content =
            std_fs::read_to_string(&creation.path).expect("failed to read updated startup config");
        assert!(!content.contains("display_name"));
        assert!(!content.contains("description"));
        assert!(!content.contains("icon"));
        assert!(!content.contains("color"));
        assert!(!content.contains("hidden"));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(
            updated_config.profiles["work"],
            TerminalStartupProfileConfig::default()
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_rejects_duplicate_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = create_startup_profile(
            &startup_config_file,
            "work",
            &TerminalStartupProfileCreationMetadata {
                display_name: Some("Other".into()),
                ..TerminalStartupProfileCreationMetadata::default()
            },
        )
        .expect_err("duplicate profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile already exists: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected creation"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn create_startup_profile_rejects_blank_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": {} }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = create_startup_profile(
            &startup_config_file,
            "  ",
            &TerminalStartupProfileCreationMetadata::default(),
        )
        .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected creation"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_updates_jsonc_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep root title
  "title": "Root",
  "profiles": {
    // keep work profile comment
    "work": {
      "display_name": "Old Work",
      "description": "Old shell",
      "hidden": true,
      "working_directory": ".",
      "command": "cmd /C echo work",
      "tabs": [
        {
          "title": "Logs"
        }
      ]
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_metadata(
            &startup_config_file,
            " work ",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some(" Work Shell ".into())),
                description: Some(Some(" Project shell ".into())),
                icon: Some(Some(" terminal ".into())),
                color: Some(Some(" #0f766e ".into())),
            },
        )
        .expect("profile metadata should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.profile, "work");
        assert_eq!(update.previous_display_name.as_deref(), Some("Old Work"));
        assert_eq!(update.display_name.as_deref(), Some("Work Shell"));
        assert_eq!(update.previous_description.as_deref(), Some("Old shell"));
        assert_eq!(update.description.as_deref(), Some("Project shell"));
        assert_eq!(update.previous_icon, None);
        assert_eq!(update.icon.as_deref(), Some("terminal"));
        assert_eq!(update.previous_color, None);
        assert_eq!(update.color.as_deref(), Some("#0f766e"));
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep root title"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains(r#""display_name": "Work Shell""#));
        assert!(content.contains(r#""description": "Project shell""#));
        assert!(content.contains(r#""icon": "terminal""#));
        assert!(content.contains("\"color\": \"#0f766e\""));
        assert!(content.contains(r#""hidden": true"#));
        assert!(content.contains(r#""working_directory": ".""#));
        assert!(content.contains(r#""command": "cmd /C echo work""#));
        assert!(content.contains(r#""title": "Logs""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        let profile = &updated_config.profiles["work"];
        assert_eq!(profile.display_name.as_deref(), Some("Work Shell"));
        assert_eq!(profile.description.as_deref(), Some("Project shell"));
        assert_eq!(profile.icon.as_deref(), Some("terminal"));
        assert_eq!(profile.color.as_deref(), Some("#0f766e"));
        assert!(profile.hidden);
        assert_eq!(profile.working_directory, Some(PathBuf::from(".")));
        assert_eq!(profile.command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(profile.tabs[0].title.as_deref(), Some("Logs"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_clears_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r##"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "description": "Project shell",
      "icon": "terminal",
      "color": "#0f766e",
      "hidden": true
    }
  }
}
"##,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_metadata(
            &startup_config_file,
            "work",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(None),
                description: Some(None),
                icon: Some(None),
                color: Some(None),
            },
        )
        .expect("profile metadata should clear");

        assert_eq!(update.previous_display_name.as_deref(), Some("Work"));
        assert_eq!(update.display_name, None);
        assert_eq!(
            update.previous_description.as_deref(),
            Some("Project shell")
        );
        assert_eq!(update.description, None);
        assert_eq!(update.previous_icon.as_deref(), Some("terminal"));
        assert_eq!(update.icon, None);
        assert_eq!(update.previous_color.as_deref(), Some("#0f766e"));
        assert_eq!(update.color, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains("display_name"));
        assert!(!content.contains("description"));
        assert!(!content.contains("icon"));
        assert!(!content.contains("color"));
        assert!(content.contains(r#""hidden": true"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.profiles["work"].display_name, None);
        assert_eq!(updated_config.profiles["work"].description, None);
        assert_eq!(updated_config.profiles["work"].icon, None);
        assert_eq!(updated_config.profiles["work"].color, None);
        assert!(updated_config.profiles["work"].hidden);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_normalizes_blank_values_to_clears() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r##"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "description": "Project shell",
      "icon": "terminal",
      "color": "#0f766e"
    }
  }
}
"##,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_metadata(
            &startup_config_file,
            "work",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("  ".into())),
                description: Some(Some("\t".into())),
                icon: Some(Some(" ".into())),
                color: Some(Some(" ".into())),
            },
        )
        .expect("blank profile metadata should clear fields");

        assert!(update.changed);
        assert_eq!(update.display_name, None);
        assert_eq!(update.description, None);
        assert_eq!(update.icon, None);
        assert_eq!(update.color, None);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains("display_name"));
        assert!(!content.contains("description"));
        assert!(!content.contains("icon"));
        assert!(!content.contains("color"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_reports_unchanged_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "description": "Project shell"
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = update_startup_profile_metadata(
            &startup_config_file,
            "work",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Work".into())),
                description: Some(Some("Project shell".into())),
                icon: Some(None),
                color: Some(None),
            },
        )
        .expect("matching metadata should be unchanged");

        assert_eq!(update.display_name.as_deref(), Some("Work"));
        assert_eq!(update.description.as_deref(), Some("Project shell"));
        assert_eq!(update.icon, None);
        assert_eq!(update.color, None);
        assert!(!update.changed);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_metadata(
            &startup_config_file,
            "old",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Old".into())),
                ..TerminalStartupProfileMetadataUpdateRequest::default()
            },
        )
        .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_rejects_blank_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_metadata(
            &startup_config_file,
            "  ",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Work".into())),
                ..TerminalStartupProfileMetadataUpdateRequest::default()
            },
        )
        .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = update_startup_profile_metadata(
            &startup_config_file,
            "work",
            &TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Work".into())),
                ..TerminalStartupProfileMetadataUpdateRequest::default()
            },
        )
        .expect_err("missing startup config should be rejected when updating metadata");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "updating metadata in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_metadata_rejects_empty_update_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_metadata(
            &startup_config_file,
            "work",
            &TerminalStartupProfileMetadataUpdateRequest::default(),
        )
        .expect_err("empty metadata update should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("--update-profile requires at least one profile metadata flag"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_updates_jsonc_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let work_dir = root_dir.join("work");
        std_fs::create_dir_all(&work_dir).expect("failed to create work dir");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "profiles": {
    // keep work profile comment
    "work": {
      "display_name": "Work",
      "working_directory": ".",
      "title": "Old Work",
      "shell": "pwsh.exe",
      "tabs": [
        { "title": "Logs" }
      ]
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_startup(
            &startup_config_file,
            " work ",
            &TerminalStartupProfileStartupUpdateRequest {
                working_directory: Some(Some(work_dir.clone())),
                command: Some(Some("cmd /C echo work".into())),
                title: Some(Some(" Work Shell ".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect("profile startup fields should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.profile, "work");
        assert_eq!(update.previous_working_directory, Some(PathBuf::from(".")));
        assert_eq!(
            update.working_directory.as_deref(),
            Some(work_dir.as_path())
        );
        assert_eq!(update.previous_command, None);
        assert_eq!(update.command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(update.previous_title.as_deref(), Some("Old Work"));
        assert_eq!(update.title.as_deref(), Some("Work Shell"));
        assert_eq!(
            update.previous_shell,
            Some(TerminalStartupShellConfig::Program("pwsh.exe".into()))
        );
        assert_eq!(update.shell, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(content.contains(&format!(
            r#""working_directory": "{}""#,
            work_dir.to_string_lossy().replace('\\', "\\\\")
        )));
        assert!(content.contains(r#""command": "cmd /C echo work""#));
        assert!(content.contains(r#""title": "Work Shell""#));
        assert!(!content.contains(r#""shell":"#));
        assert!(content.contains(r#""tabs""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        let profile = &updated_config.profiles["work"];
        assert_eq!(profile.display_name.as_deref(), Some("Work"));
        assert_eq!(
            profile.working_directory.as_deref(),
            Some(work_dir.as_path())
        );
        assert_eq!(profile.command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(profile.title.as_deref(), Some("Work Shell"));
        assert_eq!(profile.shell, None);
        assert_eq!(profile.tabs[0].title.as_deref(), Some("Logs"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_sets_shell_and_clears_command() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {
      "command": "cmd /C echo work",
      "title": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest {
                shell: Some(Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                ))),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect("profile shell should update");

        assert_eq!(update.previous_command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(update.command, None);
        assert_eq!(
            update.shell,
            Some(TerminalStartupShellConfig::WithArguments(
                TerminalStartupShellWithArgumentsConfig {
                    program: "pwsh.exe".into(),
                    args: vec!["-NoLogo".into()],
                },
            ))
        );
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains(r#""command""#));
        assert!(content.contains(r#""shell": {"#));
        assert!(content.contains(r#""program": "pwsh.exe""#));
        assert!(content.contains(r#""args": ["#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        let profile = &updated_config.profiles["work"];
        assert_eq!(profile.command, None);
        assert_eq!(profile.title.as_deref(), Some("Work"));
        assert_eq!(profile.shell, update.shell);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_clears_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "working_directory": ".",
      "command": "cmd /C echo work",
      "title": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest {
                working_directory: Some(None),
                command: Some(None),
                title: Some(None),
                shell: Some(None),
            },
        )
        .expect("profile startup fields should clear");

        assert_eq!(update.working_directory, None);
        assert_eq!(update.command, None);
        assert_eq!(update.title, None);
        assert_eq!(update.shell, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(!content.contains(r#""working_directory""#));
        assert!(!content.contains(r#""command""#));
        assert!(!content.contains(r#""title""#));
        assert!(!content.contains(r#""shell""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );
        assert_eq!(updated_config.profiles["work"].working_directory, None);
        assert_eq!(updated_config.profiles["work"].command, None);
        assert_eq!(updated_config.profiles["work"].title, None);
        assert_eq!(updated_config.profiles["work"].shell, None);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_reports_unchanged_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "command": "cmd /C echo work",
      "title": "Work"
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest {
                command: Some(Some("cmd /C echo work".into())),
                title: Some(Some("Work".into())),
                shell: Some(None),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect("matching profile startup fields should be unchanged");

        assert!(!update.changed);
        assert_eq!(update.command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(update.title.as_deref(), Some("Work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_startup(
            &startup_config_file,
            "old",
            &TerminalStartupProfileStartupUpdateRequest {
                title: Some(Some("Old".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_rejects_blank_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_startup(
            &startup_config_file,
            "  ",
            &TerminalStartupProfileStartupUpdateRequest {
                title: Some(Some("Work".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest {
                title: Some(Some("Work".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect_err("missing startup config should be rejected when updating startup fields");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "updating startup fields in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_rejects_empty_update_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest::default(),
        )
        .expect_err("empty startup update should be rejected");
        let message = format!("{error:#}");

        assert!(
            message.contains("--update-profile-startup requires at least one startup field flag")
        );
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_startup_rejects_invalid_result_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "command": "cmd /C echo work",
      "env": {
        "MODE": "test"
      }
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_startup(
            &startup_config_file,
            "work",
            &TerminalStartupProfileStartupUpdateRequest {
                command: Some(Some("\"unterminated".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            },
        )
        .expect_err("invalid command should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("refusing to update startup profile"));
        assert!(message.contains("failed to parse command"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_updates_jsonc_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let work_dir = root_dir.join("work");
        std_fs::create_dir_all(&work_dir).expect("failed to create work dir");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep root comment
  "working_directory": ".",
  "title": "Old Root",
  "shell": "pwsh.exe",
  "env": {},
  "tabs": [
    { "title": "Logs" }
  ],
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                working_directory: Some(Some(work_dir.clone())),
                command: Some(Some("cmd /C echo root".into())),
                title: Some(Some(" Root Shell ".into())),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect("root startup fields should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.previous_working_directory, Some(PathBuf::from(".")));
        assert_eq!(
            update.working_directory.as_deref(),
            Some(work_dir.as_path())
        );
        assert_eq!(update.previous_command, None);
        assert_eq!(update.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(update.previous_title.as_deref(), Some("Old Root"));
        assert_eq!(update.title.as_deref(), Some("Root Shell"));
        assert_eq!(
            update.previous_shell,
            Some(TerminalStartupShellConfig::Program("pwsh.exe".into()))
        );
        assert_eq!(update.shell, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep root comment"));
        assert!(content.contains(&format!(
            r#""working_directory": "{}""#,
            work_dir.to_string_lossy().replace('\\', "\\\\")
        )));
        assert!(content.contains(r#""command": "cmd /C echo root""#));
        assert!(content.contains(r#""title": "Root Shell""#));
        assert!(!content.contains(r#""shell":"#));
        assert!(content.contains(r#""tabs""#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        assert_eq!(
            updated_config.working_directory.as_deref(),
            Some(work_dir.as_path())
        );
        assert_eq!(updated_config.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(updated_config.title.as_deref(), Some("Root Shell"));
        assert_eq!(updated_config.shell, None);
        assert_eq!(updated_config.tabs[0].title.as_deref(), Some("Logs"));
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_sets_shell_and_clears_command() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "command": "cmd /C echo root",
  "title": "Root"
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                shell: Some(Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                ))),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect("root shell should update");

        assert_eq!(update.previous_command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(update.command, None);
        assert_eq!(
            update.shell,
            Some(TerminalStartupShellConfig::WithArguments(
                TerminalStartupShellWithArgumentsConfig {
                    program: "pwsh.exe".into(),
                    args: vec!["-NoLogo".into()],
                },
            ))
        );
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains(r#""command""#));
        assert!(content.contains(r#""shell": {"#));
        assert!(content.contains(r#""program": "pwsh.exe""#));
        assert!(content.contains(r#""args": ["#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        assert_eq!(updated_config.command, None);
        assert_eq!(updated_config.title.as_deref(), Some("Root"));
        assert_eq!(updated_config.shell, update.shell);

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_clears_fields() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "working_directory": ".",
  "command": "cmd /C echo root",
  "title": "Root",
  "shell": "pwsh.exe",
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                working_directory: Some(None),
                command: Some(None),
                title: Some(None),
                shell: Some(None),
            },
        )
        .expect("root startup fields should clear");

        assert_eq!(update.working_directory, None);
        assert_eq!(update.command, None);
        assert_eq!(update.title, None);
        assert_eq!(update.shell, None);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains(r#""working_directory""#));
        assert!(!content.contains(r#""command""#));
        assert!(!content.contains(r#""title""#));
        assert!(!content.contains(r#""shell""#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.working_directory, None);
        assert_eq!(updated_config.command, None);
        assert_eq!(updated_config.title, None);
        assert_eq!(updated_config.shell, None);
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_reports_unchanged_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "command": "cmd /C echo root",
  "title": "Root"
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                command: Some(Some("cmd /C echo root".into())),
                title: Some(Some("Root".into())),
                shell: Some(None),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect("matching root startup fields should be unchanged");

        assert!(!update.changed);
        assert_eq!(update.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(update.title.as_deref(), Some("Root"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_initializes_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("config").join("terminal.json");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                command: Some(Some("cmd /C echo root".into())),
                title: Some(Some("Root".into())),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect("missing root startup config should initialize and update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.previous_command, None);
        assert_eq!(update.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(update.previous_title, None);
        assert_eq!(update.title.as_deref(), Some("Root"));
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read created startup config");
        assert!(content.contains("// Zed Terminal startup layout."));
        assert!(content.contains(r#""command": "cmd /C echo root""#));
        assert!(content.contains(r#""title": "Root""#));
        assert!(content.contains(r#""profiles": {}"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("created config should parse");
        updated_config
            .validate()
            .expect("created config should validate");
        assert_eq!(updated_config.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(updated_config.title.as_deref(), Some("Root"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_rejects_empty_update_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "title": "Root" }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest::default(),
        )
        .expect_err("empty startup update should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("--update-startup requires at least one startup field flag"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_rejects_invalid_result_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "command": "cmd /C echo root",
  "env": {
    "MODE": "test"
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                command: Some(Some("\"unterminated".into())),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect_err("invalid command should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("refusing to update root startup fields"));
        assert!(message.contains("failed to parse command"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_normalizes_blank_command_and_title_to_clears() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "command": "cmd /C echo root",
  "title": "Root"
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup(
            &startup_config_file,
            &TerminalStartupUpdateRequest {
                command: Some(Some("   ".into())),
                title: Some(Some("\t".into())),
                ..TerminalStartupUpdateRequest::default()
            },
        )
        .expect("blank command and title should clear fields");

        assert!(update.changed);
        assert_eq!(update.command, None);
        assert_eq!(update.title, None);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(!content.contains(r#""command""#));
        assert!(!content.contains(r#""title""#));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_updates_jsonc_fields_without_reporting_values() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep root comment
  "command": "cmd /C echo root",
  "env": {
    "API_KEY": "old-secret",
    "MODE": "old",
    "REMOVE_ME": "gone"
  },
  "tabs": [
    { "title": "Logs" }
  ],
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                set: vec![
                    (" MODE ".into(), "new-secret".into()),
                    ("TOKEN".into(), "first-secret".into()),
                    ("TOKEN".into(), "final-secret".into()),
                ],
                remove: vec![" REMOVE_ME ".into()],
                clear: false,
            },
        )
        .expect("root environment variables should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(
            update.previous_env_keys,
            vec!["API_KEY", "MODE", "REMOVE_ME"]
        );
        assert_eq!(update.env_keys, vec!["API_KEY", "MODE", "TOKEN"]);
        assert_eq!(update.added_env_keys, vec!["TOKEN"]);
        assert_eq!(update.updated_env_keys, vec!["MODE"]);
        assert_eq!(update.removed_env_keys, vec!["REMOVE_ME"]);
        assert!(!update.cleared);
        assert!(update.changed);

        let text_output = format_startup_env_update(&update);
        let json_output =
            format_startup_env_update_json(&update).expect("json output should format");
        for output in [&text_output, &json_output] {
            assert!(!output.contains("old-secret"));
            assert!(!output.contains("new-secret"));
            assert!(!output.contains("first-secret"));
            assert!(!output.contains("final-secret"));
            assert!(!output.contains("gone"));
        }

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep root comment"));
        assert!(content.contains(r#""command": "cmd /C echo root""#));
        assert!(content.contains(r#""API_KEY": "old-secret""#));
        assert!(content.contains(r#""MODE": "new-secret""#));
        assert!(content.contains(r#""TOKEN": "final-secret""#));
        assert!(!content.contains("REMOVE_ME"));
        assert!(content.contains(r#""tabs""#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        assert_eq!(updated_config.command.as_deref(), Some("cmd /C echo root"));
        assert_eq!(updated_config.env["API_KEY"], "old-secret");
        assert_eq!(updated_config.env["MODE"], "new-secret");
        assert_eq!(updated_config.env["TOKEN"], "final-secret");
        assert!(!updated_config.env.contains_key("REMOVE_ME"));
        assert_eq!(updated_config.tabs[0].title.as_deref(), Some("Logs"));
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_clear_removes_json_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "command": "cmd /C echo root",
  "env": {
    "API_KEY": "secret",
    "MODE": "test"
  },
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                clear: true,
                ..TerminalStartupEnvUpdateRequest::default()
            },
        )
        .expect("root environment variables should clear");

        assert_eq!(update.previous_env_keys, vec!["API_KEY", "MODE"]);
        assert!(update.env_keys.is_empty());
        assert!(update.added_env_keys.is_empty());
        assert!(update.updated_env_keys.is_empty());
        assert_eq!(update.removed_env_keys, vec!["API_KEY", "MODE"]);
        assert!(update.cleared);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""command": "cmd /C echo root""#));
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(!content.contains(r#""env""#));
        assert!(!content.contains("secret"));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert!(updated_config.env.is_empty());
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_reports_unchanged_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "command": "cmd /C echo root",
  "env": {
    "MODE": "test"
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                remove: vec!["MISSING".into()],
                clear: false,
            },
        )
        .expect("matching environment update should be unchanged");

        assert_eq!(update.previous_env_keys, vec!["MODE"]);
        assert_eq!(update.env_keys, vec!["MODE"]);
        assert!(update.added_env_keys.is_empty());
        assert!(update.updated_env_keys.is_empty());
        assert!(update.removed_env_keys.is_empty());
        assert!(!update.cleared);
        assert!(!update.changed);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_initializes_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("config").join("terminal.json");

        let update = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                set: vec![
                    ("MODE".into(), "test".into()),
                    ("TOKEN".into(), "secret".into()),
                ],
                ..TerminalStartupEnvUpdateRequest::default()
            },
        )
        .expect("missing root startup config should initialize and update env");

        assert_eq!(update.path, startup_config_file);
        assert!(update.previous_env_keys.is_empty());
        assert_eq!(update.env_keys, vec!["MODE", "TOKEN"]);
        assert_eq!(update.added_env_keys, vec!["MODE", "TOKEN"]);
        assert!(update.updated_env_keys.is_empty());
        assert!(update.removed_env_keys.is_empty());
        assert!(!update.cleared);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read created startup config");
        assert!(content.contains("// Zed Terminal startup layout."));
        assert!(content.contains(r#""MODE": "test""#));
        assert!(content.contains(r#""TOKEN": "secret""#));
        assert!(content.contains(r#""profiles": {}"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("created config should parse");
        updated_config
            .validate()
            .expect("created config should validate");
        assert_eq!(updated_config.env["MODE"], "test");
        assert_eq!(updated_config.env["TOKEN"], "secret");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_clear_initializes_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("config").join("terminal.json");

        let update = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                clear: true,
                ..TerminalStartupEnvUpdateRequest::default()
            },
        )
        .expect("missing root startup config should initialize when clearing env");

        assert!(update.previous_env_keys.is_empty());
        assert!(update.env_keys.is_empty());
        assert!(!update.cleared);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read created startup config");
        assert!(content.contains("// Zed Terminal startup layout."));
        assert!(content.contains(r#""env": {}"#));
        assert!(content.contains(r#""profiles": {}"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("created config should parse");
        assert!(updated_config.env.is_empty());

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_root_startup_env_rejects_invalid_requests_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "command": "cmd /C echo root",
  "env": {
    "MODE": "test"
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest::default(),
        )
        .expect_err("empty environment update should be rejected");
        assert!(
            format!("{error:#}")
                .contains("--update-startup-env requires at least one environment flag")
        );

        let error = update_root_startup_env(
            &startup_config_file,
            &TerminalStartupEnvUpdateRequest {
                set: vec![("  ".into(), "secret".into())],
                ..TerminalStartupEnvUpdateRequest::default()
            },
        )
        .expect_err("blank environment key should be rejected");
        assert!(format!("{error:#}").contains("startup environment variable key is empty"));

        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_env_updates_jsonc_fields_without_reporting_values() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "profiles": {
    // keep work profile comment
    "work": {
      "display_name": "Work",
      "env": {
        "API_KEY": "old-secret",
        "MODE": "old",
        "REMOVE_ME": "gone"
      },
      "tabs": [
        { "title": "Logs" }
      ]
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_env(
            &startup_config_file,
            " work ",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![
                    (" MODE ".into(), "new-secret".into()),
                    ("TOKEN".into(), "first-secret".into()),
                    ("TOKEN".into(), "final-secret".into()),
                ],
                remove: vec![" REMOVE_ME ".into()],
                clear: false,
            },
        )
        .expect("profile environment variables should update");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.profile, "work");
        assert_eq!(
            update.previous_env_keys,
            vec!["API_KEY", "MODE", "REMOVE_ME"]
        );
        assert_eq!(update.env_keys, vec!["API_KEY", "MODE", "TOKEN"]);
        assert_eq!(update.added_env_keys, vec!["TOKEN"]);
        assert_eq!(update.updated_env_keys, vec!["MODE"]);
        assert_eq!(update.removed_env_keys, vec!["REMOVE_ME"]);
        assert!(!update.cleared);
        assert!(update.changed);

        let text_output = format_startup_profile_env_update(&update);
        let json_output =
            format_startup_profile_env_update_json(&update).expect("json output should format");
        for output in [&text_output, &json_output] {
            assert!(!output.contains("old-secret"));
            assert!(!output.contains("new-secret"));
            assert!(!output.contains("first-secret"));
            assert!(!output.contains("final-secret"));
            assert!(!output.contains("gone"));
        }

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(content.contains(r#""API_KEY": "old-secret""#));
        assert!(content.contains(r#""MODE": "new-secret""#));
        assert!(content.contains(r#""TOKEN": "final-secret""#));
        assert!(!content.contains("REMOVE_ME"));
        assert!(content.contains(r#""tabs""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        let profile = &updated_config.profiles["work"];
        assert_eq!(profile.display_name.as_deref(), Some("Work"));
        assert_eq!(profile.env["API_KEY"], "old-secret");
        assert_eq!(profile.env["MODE"], "new-secret");
        assert_eq!(profile.env["TOKEN"], "final-secret");
        assert!(!profile.env.contains_key("REMOVE_ME"));
        assert_eq!(profile.tabs[0].title.as_deref(), Some("Logs"));

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_env_clear_removes_json_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "env": {
        "API_KEY": "secret",
        "MODE": "test"
      }
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = update_startup_profile_env(
            &startup_config_file,
            "work",
            &TerminalStartupProfileEnvUpdateRequest {
                clear: true,
                ..TerminalStartupProfileEnvUpdateRequest::default()
            },
        )
        .expect("profile environment variables should clear");

        assert_eq!(update.previous_env_keys, vec!["API_KEY", "MODE"]);
        assert!(update.env_keys.is_empty());
        assert!(update.added_env_keys.is_empty());
        assert!(update.updated_env_keys.is_empty());
        assert_eq!(update.removed_env_keys, vec!["API_KEY", "MODE"]);
        assert!(update.cleared);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(!content.contains(r#""env""#));
        assert!(!content.contains("secret"));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert!(updated_config.profiles["work"].env.is_empty());

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_env_reports_unchanged_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "env": {
        "MODE": "test"
      }
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = update_startup_profile_env(
            &startup_config_file,
            "work",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                remove: vec!["MISSING".into()],
                clear: false,
            },
        )
        .expect("matching environment update should be unchanged");

        assert_eq!(update.previous_env_keys, vec!["MODE"]);
        assert_eq!(update.env_keys, vec!["MODE"]);
        assert!(update.added_env_keys.is_empty());
        assert!(update.updated_env_keys.is_empty());
        assert!(update.removed_env_keys.is_empty());
        assert!(!update.cleared);
        assert!(!update.changed);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_env_rejects_invalid_requests_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "env": {
        "MODE": "test"
      }
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = update_startup_profile_env(
            &startup_config_file,
            "old",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                ..TerminalStartupProfileEnvUpdateRequest::default()
            },
        )
        .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");
        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));

        let error = update_startup_profile_env(
            &startup_config_file,
            "  ",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                ..TerminalStartupProfileEnvUpdateRequest::default()
            },
        )
        .expect_err("blank profile should be rejected");
        assert!(format!("{error:#}").contains("startup profile name is empty"));

        let error = update_startup_profile_env(
            &startup_config_file,
            "work",
            &TerminalStartupProfileEnvUpdateRequest::default(),
        )
        .expect_err("empty environment update should be rejected");
        assert!(
            format!("{error:#}")
                .contains("--update-profile-env requires at least one environment flag")
        );

        let error = update_startup_profile_env(
            &startup_config_file,
            "work",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![("  ".into(), "secret".into())],
                ..TerminalStartupProfileEnvUpdateRequest::default()
            },
        )
        .expect_err("blank environment key should be rejected");
        assert!(format!("{error:#}").contains("profile environment variable key is empty"));

        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn update_startup_profile_env_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = update_startup_profile_env(
            &startup_config_file,
            "work",
            &TerminalStartupProfileEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                ..TerminalStartupProfileEnvUpdateRequest::default()
            },
        )
        .expect_err("missing startup config should be rejected when updating environment");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "updating environment in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn parse_profile_env_assignment_rejects_missing_separator() {
        let error = parse_profile_env_assignment("MODE")
            .expect_err("profile env assignment without separator should be rejected");

        assert!(format!("{error:#}").contains("--profile-env requires KEY=VALUE"));
        assert_eq!(
            parse_profile_env_assignment("EMPTY=").expect("empty values should be allowed"),
            ("EMPTY".into(), "".into())
        );
    }

    #[test]
    fn copy_startup_profile_copies_jsonc_profile_config() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r##"// keep leading comment
{
  "default_profile": "work",
  "tabs": [
    { "title": "Root Work", "profile": "work" },
    { "title": "Root Ops", "profile": "ops" }
  ],
  // keep profile map comment
  "profiles": {
    // keep work profile comment
    "work": {
      "display_name": "Work",
      "description": "Project shell",
      "icon": "terminal",
      "color": "#0f766e",
      "hidden": true,
      "working_directory": ".",
      "command": "cmd /C echo work",
      "title": "Work Shell",
      "env": {
        "B": "2",
        "A": "1"
      },
      "tabs": [
        { "title": "Nested Self", "profile": "work", "split": "right" },
        { "title": "Diagnostics", "command": "cmd /C echo diagnostics", "env": { "TAB": "1" }, "split": "down" },
        { "title": "PowerShell", "shell": { "program": "pwsh.exe", "args": ["-NoLogo"] } }
      ]
    },
    // keep ops profile comment
    "ops": {
      "display_name": "Ops",
      "tabs": [
        { "title": "Nested Work", "profile": "work" }
      ]
    }
  }
}
"##,
        )
        .expect("failed to write startup config");

        let copy = copy_startup_profile(&startup_config_file, " work ", " admin ")
            .expect("startup profile should be copied");

        assert_eq!(copy.path, startup_config_file);
        assert_eq!(copy.source_profile, "work");
        assert_eq!(copy.profile, "admin");
        assert!(copy.changed);
        assert_eq!(copy.copied_tab_count, 4);
        assert_eq!(copy.total_profile_count, 3);

        let content =
            std_fs::read_to_string(&copy.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile map comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains("// keep ops profile comment"));
        assert!(content.contains(r#""admin": {"#));
        assert!(content.contains(r#""profile": "admin""#));
        assert!(content.contains(r#""default_profile": "work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        updated_config
            .validate()
            .expect("updated config should validate");
        assert_eq!(updated_config.default_profile.as_deref(), Some("work"));
        assert_eq!(updated_config.tabs[0].profile.as_deref(), Some("work"));
        assert_eq!(updated_config.tabs[1].profile.as_deref(), Some("ops"));
        assert!(updated_config.profiles.contains_key("work"));
        assert!(updated_config.profiles.contains_key("admin"));
        assert!(updated_config.profiles.contains_key("ops"));

        let source = &updated_config.profiles["work"];
        let copied = &updated_config.profiles["admin"];
        assert_eq!(copied.display_name.as_deref(), Some("Work"));
        assert_eq!(copied.description.as_deref(), Some("Project shell"));
        assert_eq!(copied.icon.as_deref(), Some("terminal"));
        assert_eq!(copied.color.as_deref(), Some("#0f766e"));
        assert!(copied.hidden);
        assert_eq!(copied.working_directory, Some(PathBuf::from(".")));
        assert_eq!(copied.command.as_deref(), Some("cmd /C echo work"));
        assert_eq!(copied.title.as_deref(), Some("Work Shell"));
        assert_eq!(copied.env, test_env(&[("A", "1"), ("B", "2")]));
        assert_eq!(copied.tabs.len(), source.tabs.len());
        assert_eq!(copied.tabs[0].title.as_deref(), Some("Nested Self"));
        assert_eq!(copied.tabs[0].profile.as_deref(), Some("admin"));
        assert_eq!(
            copied.tabs[0].split,
            Some(TerminalStartupSplitDirection::Right)
        );
        assert_eq!(
            copied.tabs[1].command.as_deref(),
            Some("cmd /C echo diagnostics")
        );
        assert_eq!(copied.tabs[1].env, test_env(&[("TAB", "1")]));
        assert_eq!(
            copied.tabs[1].split,
            Some(TerminalStartupSplitDirection::Down)
        );
        assert_eq!(copied.tabs[2].title.as_deref(), Some("PowerShell"));
        assert_eq!(copied.tabs[2].shell, source.tabs[2].shell);
        assert_eq!(
            updated_config.profiles["ops"].tabs[0].profile.as_deref(),
            Some("work")
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn copy_startup_profile_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = copy_startup_profile(&startup_config_file, "old", "new")
            .expect_err("missing source profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected copy"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn copy_startup_profile_rejects_existing_target_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "old": {},
    "new": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = copy_startup_profile(&startup_config_file, "old", "new")
            .expect_err("existing target profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile already exists: new"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected copy"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn copy_startup_profile_rejects_blank_profiles_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "old": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = copy_startup_profile(&startup_config_file, "  ", "new")
            .expect_err("blank source profile should be rejected");
        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected copy"),
            original
        );

        let error = copy_startup_profile(&startup_config_file, "old", "  ")
            .expect_err("blank target profile should be rejected");
        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected copy"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn copy_startup_profile_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = copy_startup_profile(&startup_config_file, "old", "new")
            .expect_err("missing startup config should be rejected when copying a profile");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "copying in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_updates_jsonc_profiles_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep profile map comment
  "profiles": {
    // keep old profile comment
    "old": {
      "display_name": "Old"
    },
    // keep work profile comment
    "work": {
      "display_name": "Work"
    }
  },
  "tabs": []
}
"#,
        )
        .expect("failed to write startup config");

        let removal = remove_startup_profile(&startup_config_file, " old ")
            .expect("startup profile should be removed");

        assert_eq!(removal.path, startup_config_file);
        assert_eq!(removal.profile, "old");
        assert!(removal.changed);
        assert_eq!(removal.remaining_profile_count, 1);

        let content =
            std_fs::read_to_string(&removal.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile map comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(!content.contains(r#""old""#));
        assert!(content.contains(r#""work""#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.profiles.len(), 1);
        assert!(!updated_config.profiles.contains_key("old"));
        assert_eq!(
            updated_config.profiles["work"].display_name.as_deref(),
            Some("Work")
        );
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_reports_unchanged_when_absent() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "display_name": "Work"
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let removal = remove_startup_profile(&startup_config_file, "old")
            .expect("missing profile should be an unchanged no-op");

        assert_eq!(removal.path, startup_config_file);
        assert_eq!(removal.profile, "old");
        assert!(!removal.changed);
        assert_eq!(removal.remaining_profile_count, 1);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op removal"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_reports_unchanged_when_file_is_missing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let removal = remove_startup_profile(&startup_config_file, "old")
            .expect("missing startup config should be a no-op when removing a profile");

        assert_eq!(removal.path, startup_config_file);
        assert_eq!(removal.profile, "old");
        assert!(!removal.changed);
        assert_eq!(removal.remaining_profile_count, 0);
        assert!(
            !removal.path.exists(),
            "removing from a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_rejects_default_profile_reference_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "default_profile": "work",
  "profiles": {
    "old": {},
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = remove_startup_profile(&startup_config_file, "work")
            .expect_err("default profile reference should block removal");
        let message = format!("{error:#}");

        assert!(message.contains("refusing to remove startup profile"));
        assert!(message.contains("default_profile references missing startup profile: work"));
        assert!(message.contains("Available profiles: old"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected removal"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_rejects_startup_tab_reference_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "tabs": [
    { "profile": "work" }
  ],
  "profiles": {
    "old": {},
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = remove_startup_profile(&startup_config_file, "work")
            .expect_err("startup tab reference should block removal");
        let message = format!("{error:#}");

        assert!(message.contains("refusing to remove startup profile"));
        assert!(message.contains("failed to resolve profile for tab 2 for root startup layout"));
        assert!(message.contains("startup profile not found: work"));
        assert!(message.contains("Available profiles: old"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected removal"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn remove_startup_profile_rejects_blank_profile() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = remove_startup_profile(&startup_config_file, "  ")
            .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected removal"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_updates_jsonc_key_and_references() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  "default_profile": "old",
  "tabs": [
    { "title": "Root Old", "profile": "old" },
    { "title": "Root Work", "profile": "work" }
  ],
  // keep profile map comment
  "profiles": {
    // keep old profile comment
    "old": {
      // keep inner comment
      "display_name": "Old",
      "tabs": [
        { "title": "Nested Self", "profile": "old" }
      ]
    },
    // keep work profile comment
    "work": {
      "display_name": "Work",
      "tabs": [
        { "title": "Nested Old", "profile": "old" }
      ]
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let rename = rename_startup_profile(&startup_config_file, " old ", " new ")
            .expect("startup profile should be renamed");

        assert_eq!(rename.path, startup_config_file);
        assert_eq!(rename.previous_profile, "old");
        assert_eq!(rename.profile, "new");
        assert!(rename.changed);
        assert_eq!(rename.updated_reference_count, 4);

        let content =
            std_fs::read_to_string(&rename.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile map comment"));
        assert!(content.contains("// keep old profile comment"));
        assert!(content.contains("// keep inner comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains(r#""default_profile": "new""#));
        assert!(content.contains(r#""new": {"#));
        assert!(!content.contains(r#""profile": "old""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert_eq!(updated_config.default_profile.as_deref(), Some("new"));
        assert!(updated_config.profiles.contains_key("new"));
        assert!(!updated_config.profiles.contains_key("old"));
        assert_eq!(
            updated_config.profiles["new"].display_name.as_deref(),
            Some("Old")
        );
        assert_eq!(updated_config.tabs[0].profile.as_deref(), Some("new"));
        assert_eq!(updated_config.tabs[1].profile.as_deref(), Some("work"));
        assert_eq!(
            updated_config.profiles["new"].tabs[0].profile.as_deref(),
            Some("new")
        );
        assert_eq!(
            updated_config.profiles["work"].tabs[0].profile.as_deref(),
            Some("new")
        );
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_reports_unchanged_for_same_profile() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "old": {
      "display_name": "Old"
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let rename = rename_startup_profile(&startup_config_file, " old ", "old")
            .expect("same existing startup profile rename should be unchanged");

        assert_eq!(rename.path, startup_config_file);
        assert_eq!(rename.previous_profile, "old");
        assert_eq!(rename.profile, "old");
        assert!(!rename.changed);
        assert_eq!(rename.updated_reference_count, 0);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op rename"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = rename_startup_profile(&startup_config_file, "old", "new")
            .expect_err("missing source profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected rename"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_rejects_existing_target_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "old": {},
    "new": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = rename_startup_profile(&startup_config_file, "old", "new")
            .expect_err("existing target profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile already exists: new"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected rename"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_rejects_blank_profiles_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "old": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = rename_startup_profile(&startup_config_file, "  ", "new")
            .expect_err("blank source profile should be rejected");
        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected rename"),
            original
        );

        let error = rename_startup_profile(&startup_config_file, "old", "  ")
            .expect_err("blank target profile should be rejected");
        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected rename"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn rename_startup_profile_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = rename_startup_profile(&startup_config_file, "old", "new")
            .expect_err("missing startup config should be rejected when renaming a profile");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "renaming in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_inserts_hidden_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"// keep leading comment
{
  // keep profile map comment
  "profiles": {
    // keep work profile comment
    "work": {
      "display_name": "Work",
      "description": "Project shell"
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_startup_profile_visibility(&startup_config_file, " work ", true)
            .expect("startup profile should be hidden");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.profile, "work");
        assert!(!update.previous_hidden);
        assert!(update.hidden);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains("// keep leading comment"));
        assert!(content.contains("// keep profile map comment"));
        assert!(content.contains("// keep work profile comment"));
        assert!(content.contains(r#""hidden": true"#));
        assert!(content.contains(r#""display_name": "Work""#));
        assert!(content.contains(r#""description": "Project shell""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert!(updated_config.profiles["work"].hidden);
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_inserts_hidden_field_into_empty_profile() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {}
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_startup_profile_visibility(&startup_config_file, "work", true)
            .expect("startup profile should be hidden");

        assert!(!update.previous_hidden);
        assert!(update.hidden);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""hidden": true"#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert!(updated_config.profiles["work"].hidden);
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_replaces_hidden_field() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        std_fs::write(
            &startup_config_file,
            r#"{
  "profiles": {
    "work": {
      "display_name": "Work",
      "hidden": true
    }
  }
}
"#,
        )
        .expect("failed to write startup config");

        let update = set_startup_profile_visibility(&startup_config_file, "work", false)
            .expect("startup profile should be shown");

        assert_eq!(update.previous_hidden, true);
        assert!(!update.hidden);
        assert!(update.changed);

        let content =
            std_fs::read_to_string(&update.path).expect("failed to read updated startup config");
        assert!(content.contains(r#""hidden": false"#));
        assert!(content.contains(r#""display_name": "Work""#));

        let updated_config: TerminalStartupConfig =
            settings::parse_json_with_comments(&content).expect("updated config should parse");
        assert!(!updated_config.profiles["work"].hidden);
        updated_config
            .validate()
            .expect("updated config should validate");

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_reports_unchanged_when_already_matching() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {
      "hidden": true
    }
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let update = set_startup_profile_visibility(&startup_config_file, "work", true)
            .expect("matching startup profile visibility should be unchanged");

        assert_eq!(update.path, startup_config_file);
        assert_eq!(update.profile, "work");
        assert!(update.previous_hidden);
        assert!(update.hidden);
        assert!(!update.changed);
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after no-op visibility update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_rejects_missing_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{
  "profiles": {
    "work": {}
  }
}
"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = set_startup_profile_visibility(&startup_config_file, "old", true)
            .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: old"));
        assert!(message.contains("Available profiles: work"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected visibility update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_rejects_blank_profile_without_writing() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");
        let original = r#"{ "profiles": { "work": {} } }"#;
        std_fs::write(&startup_config_file, original).expect("failed to write startup config");

        let error = set_startup_profile_visibility(&startup_config_file, "  ", true)
            .expect_err("blank profile should be rejected");

        assert!(format!("{error:#}").contains("startup profile name is empty"));
        assert_eq!(
            std_fs::read_to_string(&startup_config_file)
                .expect("failed to read startup config after rejected visibility update"),
            original
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn set_startup_profile_visibility_rejects_missing_file() {
        let root_dir = temp_test_dir();
        let startup_config_file = root_dir.join("terminal.json");

        let error = set_startup_profile_visibility(&startup_config_file, "work", true)
            .expect_err("missing startup config should be rejected when updating visibility");
        let message = format!("{error:#}");

        assert!(message.contains("failed to read terminal startup config"));
        assert!(
            !startup_config_file.exists(),
            "updating visibility in a missing startup config should not create terminal.json"
        );

        std_fs::remove_dir_all(root_dir).ok();
    }

    #[test]
    fn formats_keymap_validation() {
        let report = TerminalKeymapValidationReport {
            keymap_file: PathBuf::from("keymap.json"),
            validation: TerminalKeymapValidation {
                default_binding_count: 31,
                user_binding_count: 2,
                user_keymap_source: TerminalUserKeymapSource::File,
            },
        };

        let output = format_keymap_validation(&report);

        assert_eq!(
            output,
            "keymap_file: keymap.json\nstatus: ok\ndefault_bindings: 31\nuser_keymap_source: file\nuser_bindings: 2\n"
        );
    }

    #[test]
    fn formats_keymap_validation_json() {
        let report = TerminalKeymapValidationReport {
            keymap_file: PathBuf::from("keymap.json"),
            validation: TerminalKeymapValidation {
                default_binding_count: 31,
                user_binding_count: 2,
                user_keymap_source: TerminalUserKeymapSource::File,
            },
        };

        let output = format_keymap_validation_json(&report).expect("json output should format");
        let json: serde_json::Value =
            serde_json::from_str(&output).expect("keymap validation json should parse");

        assert_eq!(json["keymap_file"], "keymap.json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["default_binding_count"], 31);
        assert_eq!(json["user_keymap_source"], "file");
        assert_eq!(json["user_binding_count"], 2);
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn formats_initial_keymap_validation_source() {
        assert_eq!(TerminalUserKeymapSource::Initial.as_str(), "initial");
    }

    #[test]
    fn validate_startup_config_mode_does_not_launch() {
        let cli = Cli::try_parse_from(["zed-terminal", "--validate-startup-config"])
            .expect("failed to parse cli args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("failed to build cli command");

        let TerminalCliCommand::ValidateStartupConfig {
            startup_config,
            format,
            ..
        } = command
        else {
            panic!("expected startup config validation mode");
        };

        assert_eq!(format, TerminalStartupConfigValidationOutputFormat::Text);
        assert_eq!(
            startup_config
                .validate()
                .expect("default startup config should validate"),
            TerminalStartupConfigValidation {
                layout_count: 1,
                tab_count: 1,
            }
        );
    }

    #[test]
    fn print_startup_layout_mode_resolves_launch_options_without_launching() {
        let tab_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-layout",
            "--title",
            "Preview",
            "--new-tab",
            tab_dir.to_str().unwrap(),
            "--new-tab-title",
            "Logs",
        ])
        .expect("failed to parse cli args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("failed to build cli command");

        let TerminalCliCommand::PrintStartupLayout {
            launch_options: options,
            format,
        } = command
        else {
            panic!("expected startup layout printing mode");
        };

        assert_eq!(format, TerminalStartupLayoutOutputFormat::Text);
        assert_eq!(options.initial_tab.title.as_deref(), Some("Preview"));
        assert_eq!(options.additional_tabs.len(), 1);
        assert_tab_working_directory(&options.additional_tabs[0], &tab_dir);
        assert_eq!(options.additional_tabs[0].title.as_deref(), Some("Logs"));

        std_fs::remove_dir_all(tab_dir).ok();
    }

    #[test]
    fn print_startup_layout_with_no_startup_config_does_not_load_startup_config_file() {
        let data_dir = env::temp_dir().join(format!(
            "zed-terminal-layout-preview-read-only-{}",
            uuid::Uuid::new_v4()
        ));
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--no-startup-config",
            "--print-startup-layout",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("layout preview should not load terminal.json when startup config is disabled");

        let TerminalCliCommand::PrintStartupLayout {
            launch_options: options,
            format,
        } = command
        else {
            panic!("expected startup layout printing mode");
        };

        assert_eq!(format, TerminalStartupLayoutOutputFormat::Text);
        assert_eq!(options.path_options.data_dir, data_dir);
        assert_eq!(options.path_options.config_dir, config_dir);
        assert_eq!(options.initial_tab.working_directory, None);
        assert_eq!(options.initial_tab.command, None);
        assert_eq!(options.initial_tab.env, HashMap::default());
        assert_eq!(options.initial_tab.title, None);
        assert_eq!(options.initial_tab.shell, None);
        assert!(options.additional_tabs.is_empty());

        std_fs::remove_dir_all(options.path_options.data_dir).ok();
    }

    #[test]
    fn print_startup_config_schema_mode_does_not_load_startup_config_file() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--print-startup-config-schema",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("schema printing should not load terminal.json");

        assert!(matches!(
            command,
            TerminalCliCommand::PrintStartupConfigSchema { .. }
        ));

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn print_default_keymap_mode_does_not_load_startup_config_file() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--print-default-keymap",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("default keymap printing should not load terminal.json");

        assert!(matches!(
            command,
            TerminalCliCommand::PrintDefaultKeymap { .. }
        ));

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn init_config_mode_does_not_load_startup_config_file() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--init-config",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("config initialization should not load terminal.json");

        let TerminalCliCommand::InitConfig { format, .. } = command else {
            panic!("expected config initialization mode");
        };
        assert_eq!(format, TerminalConfigInitializationOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn doctor_mode_does_not_load_startup_config_file_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--doctor",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("doctor mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::Doctor { format, .. } = command else {
            panic!("expected doctor mode");
        };
        assert_eq!(format, TerminalDoctorOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn doctor_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from(["zed-terminal", "--doctor", "--doctor-format", "json"])
            .expect("failed to parse doctor json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("doctor json mode should resolve");

        let TerminalCliCommand::Doctor { format, .. } = command else {
            panic!("expected doctor mode");
        };
        assert_eq!(format, TerminalDoctorOutputFormat::Json);

        let cli = Cli::try_parse_from(["zed-terminal", "--doctor", "--format", "json"])
            .expect("failed to parse doctor format alias");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("doctor json alias should resolve");

        let TerminalCliCommand::Doctor { format, .. } = command else {
            panic!("expected doctor mode");
        };
        assert_eq!(format, TerminalDoctorOutputFormat::Json);
    }

    #[test]
    fn paths_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from(["zed-terminal", "--paths", "--paths-format", "json"])
            .expect("failed to parse paths json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("paths json mode should resolve");

        let TerminalCliCommand::PrintPaths { format, .. } = command else {
            panic!("expected paths mode");
        };
        assert_eq!(format, TerminalPathsOutputFormat::Json);
    }

    #[test]
    fn init_config_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--init-config",
            "--init-config-format",
            "json",
        ])
        .expect("failed to parse config initialization json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("config initialization json mode should resolve");

        let TerminalCliCommand::InitConfig { format, .. } = command else {
            panic!("expected config initialization mode");
        };
        assert_eq!(format, TerminalConfigInitializationOutputFormat::Json);
    }

    #[test]
    fn list_profiles_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "--list-profiles-format",
            "json",
        ])
        .expect("failed to parse list profiles json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("profile list json mode should resolve");

        let TerminalCliCommand::ListProfiles { format, .. } = command else {
            panic!("expected profile listing mode");
        };
        assert_eq!(format, TerminalListProfilesOutputFormat::Json);
    }

    #[test]
    fn describe_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            " work ",
            "--describe-profile-format",
            "json",
        ])
        .expect("failed to parse describe profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("profile description json mode should resolve");

        let TerminalCliCommand::DescribeProfile {
            profile, format, ..
        } = command
        else {
            panic!("expected profile description mode");
        };
        assert_eq!(profile, " work ");
        assert_eq!(format, TerminalDescribeProfileOutputFormat::Json);
    }

    #[test]
    fn describe_startup_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--describe-startup",
            "--describe-startup-format",
            "json",
        ])
        .expect("failed to parse describe startup json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("startup description json mode should resolve");

        let TerminalCliCommand::DescribeStartup { format, .. } = command else {
            panic!("expected startup description mode");
        };
        assert_eq!(format, TerminalDescribeStartupOutputFormat::Json);
    }

    #[test]
    fn startup_layout_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-layout",
            "--startup-layout-format",
            "json",
        ])
        .expect("failed to parse startup layout json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("startup layout json mode should resolve");

        let TerminalCliCommand::PrintStartupLayout { format, .. } = command else {
            panic!("expected startup layout printing mode");
        };
        assert_eq!(format, TerminalStartupLayoutOutputFormat::Json);
    }

    #[test]
    fn validate_startup_config_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "--validate-startup-config-format",
            "json",
        ])
        .expect("failed to parse startup config validation json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("startup config validation json mode should resolve");

        let TerminalCliCommand::ValidateStartupConfig { format, .. } = command else {
            panic!("expected startup config validation mode");
        };
        assert_eq!(format, TerminalStartupConfigValidationOutputFormat::Json);
    }

    #[test]
    fn validate_keymap_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--validate-keymap",
            "--validate-keymap-format",
            "json",
        ])
        .expect("failed to parse keymap validation json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("keymap validation json mode should resolve");

        let TerminalCliCommand::ValidateKeymap { format, .. } = command else {
            panic!("expected keymap validation mode");
        };
        assert_eq!(format, TerminalKeymapValidationOutputFormat::Json);
    }

    #[test]
    fn set_default_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--set-default-profile",
            "work",
            "--default-profile-format",
            "json",
        ])
        .expect("failed to parse set default profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("set default profile json mode should resolve");

        let TerminalCliCommand::SetDefaultProfile {
            profile, format, ..
        } = command
        else {
            panic!("expected set default profile mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(format, TerminalDefaultProfileUpdateOutputFormat::Json);
    }

    #[test]
    fn clear_default_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--clear-default-profile",
            "--default-profile-format",
            "json",
        ])
        .expect("failed to parse clear default profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("clear default profile json mode should resolve");

        let TerminalCliCommand::ClearDefaultProfile { format, .. } = command else {
            panic!("expected clear default profile mode");
        };
        assert_eq!(format, TerminalDefaultProfileUpdateOutputFormat::Json);
    }

    #[test]
    fn create_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "--profile-display-name",
            " Work Shell ",
            "--profile-description",
            " Project shell ",
            "--profile-icon",
            " terminal ",
            "--profile-color",
            " #0f766e ",
            "--create-profile-hidden",
            "--create-profile-format",
            "json",
        ])
        .expect("failed to parse create profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("create profile json mode should resolve");

        let TerminalCliCommand::CreateProfile {
            profile,
            metadata,
            format,
            ..
        } = command
        else {
            panic!("expected create profile mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            metadata,
            TerminalStartupProfileCreationMetadata {
                display_name: Some("Work Shell".into()),
                description: Some("Project shell".into()),
                icon: Some("terminal".into()),
                color: Some("#0f766e".into()),
                hidden: true,
            }
        );
        assert_eq!(format, TerminalStartupProfileCreationOutputFormat::Json);
    }

    #[test]
    fn update_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--profile-display-name",
            " Work Shell ",
            "--profile-description",
            " Project shell ",
            "--profile-icon",
            " terminal ",
            "--profile-color",
            " #0f766e ",
            "--update-profile-format",
            "json",
        ])
        .expect("failed to parse update profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile json mode should resolve");

        let TerminalCliCommand::UpdateProfile {
            profile,
            update,
            format,
            ..
        } = command
        else {
            panic!("expected update profile mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Work Shell".into())),
                description: Some(Some("Project shell".into())),
                icon: Some(Some("terminal".into())),
                color: Some(Some("#0f766e".into())),
            }
        );
        assert_eq!(format, TerminalStartupProfileUpdateOutputFormat::Json);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--clear-profile-display-name",
            "--clear-profile-description",
            "--clear-profile-icon",
            "--clear-profile-color",
        ])
        .expect("failed to parse update profile clear args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile clear mode should resolve");

        let TerminalCliCommand::UpdateProfile {
            profile,
            update,
            format,
            ..
        } = command
        else {
            panic!("expected update profile mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(None),
                description: Some(None),
                icon: Some(None),
                color: Some(None),
            }
        );
        assert_eq!(format, TerminalStartupProfileUpdateOutputFormat::Text);
    }

    #[test]
    fn update_profile_startup_format_json_is_carried_through_cli_resolution() {
        let work_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-working-directory",
            work_dir.to_str().unwrap(),
            "--profile-command",
            "cmd /C echo work",
            "--profile-title",
            " Work Shell ",
            "--update-profile-startup-format",
            "json",
        ])
        .expect("failed to parse update profile startup json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile startup json mode should resolve");

        let TerminalCliCommand::UpdateProfileStartup {
            profile,
            update,
            format,
            ..
        } = command
        else {
            panic!("expected update profile startup mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileStartupUpdateRequest {
                working_directory: Some(Some(work_dir.clone())),
                command: Some(Some("cmd /C echo work".into())),
                title: Some(Some("Work Shell".into())),
                shell: None,
            }
        );
        assert_eq!(
            format,
            TerminalStartupProfileStartupUpdateOutputFormat::Json
        );

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-shell",
            "pwsh.exe",
            "--profile-shell-arg",
            "-NoLogo",
            "--profile-shell-arg",
            "-NoProfile",
        ])
        .expect("failed to parse update profile startup shell args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile startup shell mode should resolve");

        let TerminalCliCommand::UpdateProfileStartup {
            profile,
            update,
            format,
            ..
        } = command
        else {
            panic!("expected update profile startup shell mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            update.shell,
            Some(Some(TerminalStartupShellConfig::WithArguments(
                TerminalStartupShellWithArgumentsConfig {
                    program: "pwsh.exe".into(),
                    args: vec!["-NoLogo".into(), "-NoProfile".into()],
                },
            )))
        );
        assert_eq!(
            format,
            TerminalStartupProfileStartupUpdateOutputFormat::Text
        );

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--clear-profile-working-directory",
            "--clear-profile-command",
            "--clear-profile-title",
            "--clear-profile-shell",
        ])
        .expect("failed to parse update profile startup clear args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile startup clear mode should resolve");

        let TerminalCliCommand::UpdateProfileStartup { update, .. } = command else {
            panic!("expected update profile startup clear mode");
        };
        assert_eq!(
            update,
            TerminalStartupProfileStartupUpdateRequest {
                working_directory: Some(None),
                command: Some(None),
                title: Some(None),
                shell: Some(None),
            }
        );

        std_fs::remove_dir_all(work_dir).ok();
    }

    #[test]
    fn update_startup_format_json_is_carried_through_cli_resolution() {
        let work_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--startup-working-directory",
            work_dir.to_str().unwrap(),
            "--startup-command",
            "cmd /C echo root",
            "--startup-title",
            " Root Shell ",
            "--update-startup-format",
            "json",
        ])
        .expect("failed to parse update startup json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update startup json mode should resolve");

        let TerminalCliCommand::UpdateStartup { update, format, .. } = command else {
            panic!("expected update startup mode");
        };
        assert_eq!(
            update,
            TerminalStartupUpdateRequest {
                working_directory: Some(Some(work_dir.clone())),
                command: Some(Some("cmd /C echo root".into())),
                title: Some(Some("Root Shell".into())),
                shell: None,
            }
        );
        assert_eq!(format, TerminalStartupUpdateOutputFormat::Json);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--startup-shell",
            "pwsh.exe",
            "--startup-shell-arg",
            "-NoLogo",
            "--startup-shell-arg",
            "-NoProfile",
        ])
        .expect("failed to parse update startup shell args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update startup shell mode should resolve");

        let TerminalCliCommand::UpdateStartup { update, format, .. } = command else {
            panic!("expected update startup shell mode");
        };
        assert_eq!(
            update.shell,
            Some(Some(TerminalStartupShellConfig::WithArguments(
                TerminalStartupShellWithArgumentsConfig {
                    program: "pwsh.exe".into(),
                    args: vec!["-NoLogo".into(), "-NoProfile".into()],
                },
            )))
        );
        assert_eq!(format, TerminalStartupUpdateOutputFormat::Text);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--clear-startup-working-directory",
            "--clear-startup-command",
            "--clear-startup-title",
            "--clear-startup-shell",
        ])
        .expect("failed to parse update startup clear args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update startup clear mode should resolve");

        let TerminalCliCommand::UpdateStartup { update, .. } = command else {
            panic!("expected update startup clear mode");
        };
        assert_eq!(
            update,
            TerminalStartupUpdateRequest {
                working_directory: Some(None),
                command: Some(None),
                title: Some(None),
                shell: Some(None),
            }
        );

        std_fs::remove_dir_all(work_dir).ok();
    }

    #[test]
    fn update_startup_env_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            " MODE=dev ",
            "--startup-env",
            "TOKEN=secret",
            "--remove-startup-env",
            " OLD_TOKEN ",
            "--update-startup-env-format",
            "json",
        ])
        .expect("failed to parse update startup env json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update startup env json mode should resolve");

        let TerminalCliCommand::UpdateStartupEnv { update, format, .. } = command else {
            panic!("expected update startup env mode");
        };
        assert_eq!(
            update,
            TerminalStartupEnvUpdateRequest {
                set: vec![
                    ("MODE".into(), "dev ".into()),
                    ("TOKEN".into(), "secret".into())
                ],
                remove: vec!["OLD_TOKEN".into()],
                clear: false,
            }
        );
        assert_eq!(format, TerminalStartupEnvUpdateOutputFormat::Json);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--clear-startup-env",
        ])
        .expect("failed to parse update startup env clear args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update startup env clear mode should resolve");

        let TerminalCliCommand::UpdateStartupEnv { update, format, .. } = command else {
            panic!("expected update startup env clear mode");
        };
        assert_eq!(
            update,
            TerminalStartupEnvUpdateRequest {
                clear: true,
                ..TerminalStartupEnvUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupEnvUpdateOutputFormat::Text);
    }

    #[test]
    fn update_profile_env_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            " MODE=dev ",
            "--profile-env",
            "TOKEN=secret",
            "--remove-profile-env",
            " OLD_TOKEN ",
            "--update-profile-env-format",
            "json",
        ])
        .expect("failed to parse update profile env json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile env json mode should resolve");

        let TerminalCliCommand::UpdateProfileEnv {
            profile,
            update,
            format,
            ..
        } = command
        else {
            panic!("expected update profile env mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileEnvUpdateRequest {
                set: vec![
                    ("MODE".into(), "dev ".into()),
                    ("TOKEN".into(), "secret".into())
                ],
                remove: vec!["OLD_TOKEN".into()],
                clear: false,
            }
        );
        assert_eq!(format, TerminalStartupProfileEnvUpdateOutputFormat::Json);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--clear-profile-env",
        ])
        .expect("failed to parse update profile env clear args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("update profile env clear mode should resolve");

        let TerminalCliCommand::UpdateProfileEnv { update, format, .. } = command else {
            panic!("expected update profile env clear mode");
        };
        assert_eq!(
            update,
            TerminalStartupProfileEnvUpdateRequest {
                clear: true,
                ..TerminalStartupProfileEnvUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupProfileEnvUpdateOutputFormat::Text);
    }

    #[test]
    fn copy_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--copy-profile-format",
            "json",
        ])
        .expect("failed to parse copy profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("copy profile json mode should resolve");

        let TerminalCliCommand::CopyProfile {
            source_profile,
            target_profile,
            format,
            ..
        } = command
        else {
            panic!("expected copy profile mode");
        };
        assert_eq!(source_profile, "old");
        assert_eq!(target_profile, "new");
        assert_eq!(format, TerminalStartupProfileCopyOutputFormat::Json);

        let cli = Cli::try_parse_from(["zed-terminal", "--duplicate-profile", "old", "copy"])
            .expect("failed to parse duplicate profile alias");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("duplicate profile alias should resolve");

        let TerminalCliCommand::CopyProfile {
            source_profile,
            target_profile,
            format,
            ..
        } = command
        else {
            panic!("expected copy profile mode from alias");
        };
        assert_eq!(source_profile, "old");
        assert_eq!(target_profile, "copy");
        assert_eq!(format, TerminalStartupProfileCopyOutputFormat::Text);
    }

    #[test]
    fn remove_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--remove-profile",
            "work",
            "--remove-profile-format",
            "json",
        ])
        .expect("failed to parse remove profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("remove profile json mode should resolve");

        let TerminalCliCommand::RemoveProfile {
            profile, format, ..
        } = command
        else {
            panic!("expected remove profile mode");
        };
        assert_eq!(profile, "work");
        assert_eq!(format, TerminalStartupProfileRemovalOutputFormat::Json);
    }

    #[test]
    fn rename_profile_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "new",
            "--rename-profile-format",
            "json",
        ])
        .expect("failed to parse rename profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("rename profile json mode should resolve");

        let TerminalCliCommand::RenameProfile {
            old_profile,
            new_profile,
            format,
            ..
        } = command
        else {
            panic!("expected rename profile mode");
        };
        assert_eq!(old_profile, "old");
        assert_eq!(new_profile, "new");
        assert_eq!(format, TerminalStartupProfileRenameOutputFormat::Json);
    }

    #[test]
    fn profile_visibility_format_json_is_carried_through_cli_resolution() {
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--hide-profile",
            "work",
            "--profile-visibility-format",
            "json",
        ])
        .expect("failed to parse hide profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("hide profile json mode should resolve");

        let TerminalCliCommand::SetProfileVisibility {
            profile,
            hidden,
            format,
            ..
        } = command
        else {
            panic!("expected profile visibility mode");
        };
        assert_eq!(profile, "work");
        assert!(hidden);
        assert_eq!(format, TerminalStartupProfileVisibilityOutputFormat::Json);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--show-profile",
            "work",
            "--profile-visibility-format",
            "json",
        ])
        .expect("failed to parse show profile json args");
        let command =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect("show profile json mode should resolve");

        let TerminalCliCommand::SetProfileVisibility {
            profile,
            hidden,
            format,
            ..
        } = command
        else {
            panic!("expected profile visibility mode");
        };
        assert_eq!(profile, "work");
        assert!(!hidden);
        assert_eq!(format, TerminalStartupProfileVisibilityOutputFormat::Json);
    }

    #[test]
    fn validate_keymap_mode_does_not_resolve_startup_layout() {
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--validate-keymap"])
            .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_startup_config(cli, config)
            .expect("keymap validation should not resolve startup layout");

        let TerminalCliCommand::ValidateKeymap { format, .. } = command else {
            panic!("expected keymap validation mode");
        };
        assert_eq!(format, TerminalKeymapValidationOutputFormat::Text);
    }

    #[test]
    fn set_default_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--set-default-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("set-default-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::SetDefaultProfile {
            path_options,
            profile,
            format,
        } = command
        else {
            panic!("expected set default profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(format, TerminalDefaultProfileUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn clear_default_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--clear-default-profile",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli).expect(
            "clear-default-profile mode should not load terminal.json during cli resolution",
        );

        let TerminalCliCommand::ClearDefaultProfile {
            path_options,
            format,
        } = command
        else {
            panic!("expected clear default profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(format, TerminalDefaultProfileUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn create_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--create-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("create-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::CreateProfile {
            path_options,
            profile,
            metadata,
            format,
        } = command
        else {
            panic!("expected create profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(metadata, TerminalStartupProfileCreationMetadata::default());
        assert_eq!(format, TerminalStartupProfileCreationOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn update_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--update-profile",
            "work",
            "--profile-display-name",
            "Work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("update-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::UpdateProfile {
            path_options,
            profile,
            update,
            format,
        } = command
        else {
            panic!("expected update profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileMetadataUpdateRequest {
                display_name: Some(Some("Work".into())),
                ..TerminalStartupProfileMetadataUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupProfileUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn update_profile_startup_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--update-profile-startup",
            "work",
            "--profile-title",
            "Work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli).expect(
            "update-profile-startup mode should not load terminal.json during cli resolution",
        );

        let TerminalCliCommand::UpdateProfileStartup {
            path_options,
            profile,
            update,
            format,
        } = command
        else {
            panic!("expected update profile startup mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileStartupUpdateRequest {
                title: Some(Some("Work".into())),
                ..TerminalStartupProfileStartupUpdateRequest::default()
            }
        );
        assert_eq!(
            format,
            TerminalStartupProfileStartupUpdateOutputFormat::Text
        );

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn update_startup_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--update-startup",
            "--startup-title",
            "Root",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("update-startup mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::UpdateStartup {
            path_options,
            update,
            format,
        } = command
        else {
            panic!("expected update startup mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(
            update,
            TerminalStartupUpdateRequest {
                title: Some(Some("Root".into())),
                ..TerminalStartupUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn update_startup_env_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("update-startup-env mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::UpdateStartupEnv {
            path_options,
            update,
            format,
        } = command
        else {
            panic!("expected update startup env mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(
            update,
            TerminalStartupEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                ..TerminalStartupEnvUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupEnvUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn update_profile_env_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("update-profile-env mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::UpdateProfileEnv {
            path_options,
            profile,
            update,
            format,
        } = command
        else {
            panic!("expected update profile env mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(
            update,
            TerminalStartupProfileEnvUpdateRequest {
                set: vec![("MODE".into(), "test".into())],
                ..TerminalStartupProfileEnvUpdateRequest::default()
            }
        );
        assert_eq!(format, TerminalStartupProfileEnvUpdateOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn copy_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--copy-profile",
            "old",
            "new",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("copy-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::CopyProfile {
            path_options,
            source_profile,
            target_profile,
            format,
        } = command
        else {
            panic!("expected copy profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(source_profile, "old");
        assert_eq!(target_profile, "new");
        assert_eq!(format, TerminalStartupProfileCopyOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn remove_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--remove-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("remove-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::RemoveProfile {
            path_options,
            profile,
            format,
        } = command
        else {
            panic!("expected remove profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(format, TerminalStartupProfileRemovalOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn rename_profile_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--rename-profile",
            "old",
            "new",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("rename-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::RenameProfile {
            path_options,
            old_profile,
            new_profile,
            format,
        } = command
        else {
            panic!("expected rename profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(old_profile, "old");
        assert_eq!(new_profile, "new");
        assert_eq!(format, TerminalStartupProfileRenameOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn profile_visibility_mode_does_not_load_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            "{ broken terminal config",
        )
        .expect("failed to write broken startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--hide-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("hide-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::SetProfileVisibility {
            path_options,
            profile,
            hidden,
            format,
        } = command
        else {
            panic!("expected profile visibility mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert!(hidden);
        assert_eq!(format, TerminalStartupProfileVisibilityOutputFormat::Text);

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--show-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("show-profile mode should not load terminal.json during cli resolution");

        let TerminalCliCommand::SetProfileVisibility {
            path_options,
            profile,
            hidden,
            format,
        } = command
        else {
            panic!("expected profile visibility mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert!(!hidden);
        assert_eq!(format, TerminalStartupProfileVisibilityOutputFormat::Text);

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn describe_profile_loads_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        let startup_config_file = terminal_startup_config_file(&config_dir);
        std_fs::write(
            &startup_config_file,
            r#"{ "profiles": { "work": { "display_name": "Work Shell" } } }"#,
        )
        .expect("failed to write startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--describe-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("describe-profile mode should load terminal.json during cli resolution");

        let TerminalCliCommand::DescribeProfile {
            path_options,
            startup_config,
            profile,
            format,
        } = command
        else {
            panic!("expected profile description mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");
        assert_eq!(format, TerminalDescribeProfileOutputFormat::Text);
        assert_eq!(
            startup_config
                .profiles
                .get("work")
                .and_then(|profile| profile.display_name.as_deref()),
            Some("Work Shell")
        );

        std_fs::write(&startup_config_file, "{ broken terminal config")
            .expect("failed to write broken startup config");
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--describe-profile",
            "work",
        ])
        .expect("failed to parse cli args");
        let error = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect_err("describe-profile mode should reject broken terminal.json");

        assert!(format!("{error:#}").contains("failed to parse terminal startup config"));

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn describe_startup_loads_startup_config_during_cli_resolution() {
        let data_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        let startup_config_file = terminal_startup_config_file(&config_dir);
        std_fs::write(
            &startup_config_file,
            r#"{ "title": "Root", "env": { "TOKEN": "secret" } }"#,
        )
        .expect("failed to write startup config");

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--describe-startup",
        ])
        .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect("describe-startup mode should load terminal.json during cli resolution");

        let TerminalCliCommand::DescribeStartup {
            path_options,
            startup_config,
            format,
        } = command
        else {
            panic!("expected startup description mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(format, TerminalDescribeStartupOutputFormat::Text);
        assert_eq!(startup_config.title.as_deref(), Some("Root"));
        assert_eq!(
            startup_config.env.get("TOKEN").map(String::as_str),
            Some("secret")
        );

        std_fs::write(&startup_config_file, "{ broken terminal config")
            .expect("failed to write broken startup config");
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
            "--describe-startup",
        ])
        .expect("failed to parse cli args");
        let error = TerminalCliCommand::from_cli_and_config_file(cli)
            .expect_err("describe-startup mode should reject broken terminal.json");

        assert!(format!("{error:#}").contains("failed to parse terminal startup config"));

        std_fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn set_default_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--set-default-profile",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with default profile updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--set-default-profile",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with default profile updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--set-default-profile",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with default profile updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--set-default-profile",
            "work",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with default profile updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--paths", "--set-default-profile", "work"])
                .expect_err("path inspection should conflict with default profile updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clear_default_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--clear-default-profile",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with default profile clearing");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--clear-default-profile",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with default profile clearing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--clear-default-profile",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with default profile clearing");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--clear-default-profile", "--all-profiles"])
                .expect_err("hidden profile listing should conflict with default profile clearing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--clear-default-profile"])
            .expect_err("path inspection should conflict with default profile clearing");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn create_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--create-profile", "work", "--paths"])
            .expect_err("path inspection should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--create-profile", "work", "--all-profiles"])
                .expect_err("hidden profile listing should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "--remove-profile",
            "old",
        ])
        .expect_err("profile creation should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--create-profile-format", "json"])
            .expect_err("create profile format should require create profile mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--profile-display-name", "Work"])
            .expect_err("profile display name should require create profile mode");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn update_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-profile", "work", "--paths"])
            .expect_err("path inspection should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--update-profile", "work", "--all-profiles"])
                .expect_err("hidden profile listing should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-profile-format", "json"])
            .expect_err("update profile format should require update profile mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--clear-profile-display-name"])
            .expect_err("profile metadata clears should require update profile mode");
        assert!(error.to_string().contains("required"));

        let cli = Cli::try_parse_from(["zed-terminal", "--update-profile", "work"])
            .expect("update profile mode without metadata should parse");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("update profile mode should require at least one metadata field");
        assert!(
            format!("{error:#}")
                .contains("--update-profile requires at least one profile metadata flag")
        );

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--profile-display-name",
            "Work",
            "--clear-profile-display-name",
        ])
        .expect_err("setting and clearing the same profile field should conflict");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--create-profile",
            "work",
            "--update-profile",
            "work",
            "--profile-display-name",
            "Work",
        ])
        .expect_err("profile creation should conflict with profile metadata updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn update_profile_startup_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--paths",
        ])
        .expect_err("path inspection should conflict with profile startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with profile startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--update-profile-startup-format", "json"])
                .expect_err(
                    "update profile startup format should require update profile startup mode",
                );
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--clear-profile-command"])
            .expect_err("profile startup clears should require update profile startup mode");
        assert!(error.to_string().contains("required"));

        let cli = Cli::try_parse_from(["zed-terminal", "--update-profile-startup", "work"])
            .expect("update profile startup mode without fields should parse");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err(
                    "update profile startup mode should require at least one startup field",
                );
        assert!(
            format!("{error:#}")
                .contains("--update-profile-startup requires at least one startup field flag")
        );

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-command",
            "cmd /C echo work",
            "--clear-profile-command",
        ])
        .expect_err("setting and clearing the same profile command should conflict");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-command",
            "cmd /C echo work",
            "--profile-shell",
            "pwsh.exe",
        ])
        .expect_err("profile command and shell should conflict");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile",
            "work",
            "--profile-display-name",
            "Work",
            "--update-profile-startup",
            "work",
            "--profile-title",
            "Work",
        ])
        .expect_err("profile metadata updates should conflict with startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn update_startup_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from(["zed-terminal", "--update-startup", "--profile", "admin"])
            .expect_err("profile selection should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-startup", "--paths"])
            .expect_err("path inspection should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-startup", "--all-profiles"])
            .expect_err("hidden profile listing should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-startup-format", "json"])
            .expect_err("update startup format should require update startup mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--clear-startup-command"])
            .expect_err("root startup clears should require update startup mode");
        assert!(error.to_string().contains("required"));

        let cli = Cli::try_parse_from(["zed-terminal", "--update-startup"])
            .expect("update startup mode without fields should parse");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("update startup mode should require at least one startup field");
        assert!(
            format!("{error:#}")
                .contains("--update-startup requires at least one startup field flag")
        );

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--startup-command",
            "cmd /C echo root",
            "--clear-startup-command",
        ])
        .expect_err("setting and clearing the same root command should conflict");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--startup-command",
            "cmd /C echo root",
            "--startup-shell",
            "pwsh.exe",
        ])
        .expect_err("root command and shell should conflict");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-title",
            "Work",
            "--update-startup",
            "--startup-title",
            "Root",
        ])
        .expect_err("profile startup updates should conflict with root startup updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn update_startup_env_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
            "--paths",
        ])
        .expect_err("path inspection should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-startup-env-format", "json"])
            .expect_err("update startup env format should require update startup env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--startup-env", "MODE=test"])
            .expect_err("startup env set should require update startup env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--remove-startup-env", "MODE"])
            .expect_err("startup env removal should require update startup env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--clear-startup-env"])
            .expect_err("startup env clearing should require update startup env mode");
        assert!(error.to_string().contains("required"));

        let cli = Cli::try_parse_from(["zed-terminal", "--update-startup-env"])
            .expect("update startup env mode without operations should parse");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err(
                    "update startup env mode should require at least one environment operation",
                );
        assert!(
            format!("{error:#}")
                .contains("--update-startup-env requires at least one environment flag")
        );

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            "MODE",
        ])
        .expect("startup env assignment without separator should parse as raw cli value");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("startup env assignment without separator should be rejected");
        assert!(format!("{error:#}").contains("--startup-env requires KEY=VALUE"));

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup-env",
            "--startup-env",
            " =secret",
        ])
        .expect("startup env assignment with blank key should parse as raw cli value");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("blank startup env key should be rejected");
        assert!(format!("{error:#}").contains("startup environment variable key is empty"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-startup",
            "--startup-title",
            "Root",
            "--update-startup-env",
            "--startup-env",
            "MODE=test",
        ])
        .expect_err("root startup field updates should conflict with environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "--update-startup-env",
            "--startup-env",
            "ROOT_MODE=test",
        ])
        .expect_err("profile environment updates should conflict with root environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn update_profile_env_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "--paths",
        ])
        .expect_err("path inspection should conflict with profile environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with profile environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--update-profile-env-format", "json"])
            .expect_err("update profile env format should require update profile env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--profile-env", "MODE=test"])
            .expect_err("profile env set should require update profile env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--remove-profile-env", "MODE"])
            .expect_err("profile env removal should require update profile env mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--clear-profile-env"])
            .expect_err("profile env clearing should require update profile env mode");
        assert!(error.to_string().contains("required"));

        let cli = Cli::try_parse_from(["zed-terminal", "--update-profile-env", "work"])
            .expect("update profile env mode without operations should parse");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err(
                    "update profile env mode should require at least one environment operation",
                );
        assert!(
            format!("{error:#}")
                .contains("--update-profile-env requires at least one environment flag")
        );

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE",
        ])
        .expect("profile env assignment without separator should parse as raw cli value");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("profile env assignment without separator should be rejected");
        assert!(format!("{error:#}").contains("--profile-env requires KEY=VALUE"));

        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-env",
            "work",
            "--profile-env",
            " =secret",
        ])
        .expect("profile env assignment with blank key should parse as raw cli value");
        let error =
            TerminalCliCommand::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
                .expect_err("blank profile env key should be rejected");
        assert!(format!("{error:#}").contains("profile environment variable key is empty"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--update-profile-startup",
            "work",
            "--profile-title",
            "Work",
            "--update-profile-env",
            "work",
            "--profile-env",
            "MODE=test",
        ])
        .expect_err("profile startup updates should conflict with environment updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn copy_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile copy");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile copy");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile copy");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--copy-profile", "old", "new", "--paths"])
                .expect_err("path inspection should conflict with profile copy");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with profile copy");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--copy-profile-format", "json"])
            .expect_err("copy profile format should require copy profile mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "--copy-profile-format",
            "json",
        ])
        .expect_err("copy profile mode should require source and target profile names");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--create-profile",
            "work",
        ])
        .expect_err("profile copy should conflict with profile creation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--rename-profile",
            "old",
            "other",
        ])
        .expect_err("profile copy should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--copy-profile",
            "old",
            "new",
            "--hide-profile",
            "old",
        ])
        .expect_err("profile copy should conflict with profile visibility updates");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remove_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--remove-profile",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--remove-profile",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--remove-profile",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--remove-profile", "work", "--paths"])
            .expect_err("path inspection should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--remove-profile", "work", "--all-profiles"])
                .expect_err("hidden profile listing should conflict with profile removal");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--remove-profile-format", "json"])
            .expect_err("remove profile format should require remove profile mode");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rename_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "new",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "new",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "new",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--rename-profile", "old", "new", "--paths"])
                .expect_err("path inspection should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "new",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with profile rename");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--rename-profile-format", "json"])
            .expect_err("rename profile format should require rename profile mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--rename-profile",
            "old",
            "--rename-profile-format",
            "json",
        ])
        .expect_err("rename profile mode should require old and new profile names");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn profile_visibility_rejects_startup_only_arguments() {
        for mode in ["--hide-profile", "--show-profile"] {
            let error = Cli::try_parse_from(["zed-terminal", mode, "work", "--profile", "admin"])
                .expect_err("profile selection should conflict with profile visibility update");
            assert!(error.to_string().contains("cannot be used with"));

            let dir = temp_test_dir();
            let error =
                Cli::try_parse_from(["zed-terminal", mode, "work", "-d", dir.to_str().unwrap()])
                    .expect_err("startup directory should conflict with profile visibility update");
            assert!(error.to_string().contains("cannot be used with"));

            let error = Cli::try_parse_from([
                "zed-terminal",
                mode,
                "work",
                "--new-tab-command",
                "cmd /C echo tab",
            ])
            .expect_err("startup tab command should conflict with profile visibility update");
            assert!(error.to_string().contains("cannot be used with"));

            let error = Cli::try_parse_from(["zed-terminal", mode, "work", "--paths"])
                .expect_err("path inspection should conflict with profile visibility update");
            assert!(error.to_string().contains("cannot be used with"));

            let error = Cli::try_parse_from(["zed-terminal", mode, "work", "--all-profiles"])
                .expect_err(
                    "hidden profile listing should conflict with profile visibility update",
                );
            assert!(error.to_string().contains("cannot be used with"));

            std_fs::remove_dir_all(dir).ok();
        }

        let error = Cli::try_parse_from(["zed-terminal", "--profile-visibility-format", "json"])
            .expect_err("profile visibility format should require a visibility command");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--hide-profile",
            "work",
            "--show-profile",
            "work",
        ])
        .expect_err("profile visibility commands should conflict with each other");
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn init_config_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from(["zed-terminal", "--init-config", "--profile", "work"])
            .expect_err("profile selection should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error =
            Cli::try_parse_from(["zed-terminal", "--init-config", "-d", dir.to_str().unwrap()])
                .expect_err("startup directory should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--init-config",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--init-config",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--init-config", "--", "cmd"])
            .expect_err("startup command should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--init-config"])
            .expect_err("path inspection should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "--all-profiles",
            "--init-config",
        ])
        .expect_err("hidden profile listing should conflict with config initialization");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn doctor_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from(["zed-terminal", "--doctor", "--profile", "work"])
            .expect_err("profile selection should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from(["zed-terminal", "--doctor", "-d", dir.to_str().unwrap()])
            .expect_err("startup directory should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--doctor",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--doctor",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--doctor", "--", "cmd"])
            .expect_err("startup command should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--doctor"])
            .expect_err("path inspection should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--list-profiles", "--doctor"])
            .expect_err("profile listing should conflict with doctor");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--doctor-format", "json"])
            .expect_err("doctor format should require doctor mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths-format", "json"])
            .expect_err("paths format should require paths mode");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--default-profile-format", "json"])
            .expect_err("default profile format should require a default profile command");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn validate_startup_config_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "--profile",
            "work",
        ])
        .expect_err("profile selection should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--validate-startup-config", "--", "cmd"])
            .expect_err("startup command should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--validate-startup-config"])
            .expect_err("path inspection should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-startup-config",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with config validation");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn print_startup_config_schema_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "--profile",
            "work",
        ])
        .expect_err("profile selection should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--print-startup-config-schema", "--", "cmd"])
                .expect_err("startup command should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--paths", "--print-startup-config-schema"])
                .expect_err("path inspection should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with schema printing");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn validate_keymap_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from(["zed-terminal", "--validate-keymap", "--profile", "work"])
            .expect_err("profile selection should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-keymap",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-keymap",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--validate-keymap",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--validate-keymap", "--", "cmd"])
            .expect_err("startup command should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--validate-keymap"])
            .expect_err("path inspection should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--validate-keymap", "--all-profiles"])
            .expect_err("hidden profile listing should conflict with keymap validation");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn print_default_keymap_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-default-keymap",
            "--profile",
            "work",
        ])
        .expect_err("profile selection should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-default-keymap",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-default-keymap",
            "--new-tab-profile",
            "work",
        ])
        .expect_err("startup profile tab should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-default-keymap",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-default-keymap",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--print-default-keymap", "--", "cmd"])
            .expect_err("startup command should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--print-default-keymap"])
            .expect_err("path inspection should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--print-startup-config-schema",
            "--print-default-keymap",
        ])
        .expect_err("startup schema printing should conflict with default keymap printing");
        assert!(error.to_string().contains("cannot be used with"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn non_launch_modes_reject_startup_title_arguments() {
        for mode in [
            "--init-config",
            "--doctor",
            "--validate-startup-config",
            "--print-startup-config-schema",
            "--print-default-keymap",
            "--validate-keymap",
            "--list-profiles",
            "--describe-profile",
            "--describe-startup",
            "--set-default-profile",
            "--clear-default-profile",
            "--create-profile",
            "--update-profile",
            "--update-startup",
            "--update-startup-env",
            "--update-profile-startup",
            "--update-profile-env",
            "--copy-profile",
            "--remove-profile",
            "--rename-profile",
            "--hide-profile",
            "--show-profile",
        ] {
            let mode_args = match mode {
                "--set-default-profile"
                | "--describe-profile"
                | "--create-profile"
                | "--update-profile"
                | "--update-profile-startup"
                | "--update-profile-env"
                | "--remove-profile"
                | "--hide-profile"
                | "--show-profile" => vec!["zed-terminal", mode, "work"],
                "--copy-profile" | "--rename-profile" => vec!["zed-terminal", mode, "old", "new"],
                _ => vec!["zed-terminal", mode],
            };

            let args = if matches!(
                mode,
                "--set-default-profile"
                    | "--describe-profile"
                    | "--create-profile"
                    | "--update-profile"
                    | "--update-profile-startup"
                    | "--update-profile-env"
                    | "--remove-profile"
                    | "--hide-profile"
                    | "--show-profile"
            ) {
                vec!["zed-terminal", mode, "work", "--title", "Production"]
            } else if matches!(mode, "--copy-profile" | "--rename-profile") {
                vec!["zed-terminal", mode, "old", "new", "--title", "Production"]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--title", "Production"]);
                args
            };
            assert_cli_conflict(&args, "initial title should conflict with non-launch modes");

            let args = if matches!(
                mode,
                "--set-default-profile"
                    | "--describe-profile"
                    | "--create-profile"
                    | "--update-profile"
                    | "--update-profile-startup"
                    | "--update-profile-env"
                    | "--remove-profile"
                    | "--hide-profile"
                    | "--show-profile"
            ) {
                vec!["zed-terminal", mode, "work", "--new-tab-title", "Logs"]
            } else if matches!(mode, "--copy-profile" | "--rename-profile") {
                vec![
                    "zed-terminal",
                    mode,
                    "old",
                    "new",
                    "--new-tab-title",
                    "Logs",
                ]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--new-tab-title", "Logs"]);
                args
            };
            assert_cli_conflict(
                &args,
                "startup tab title should conflict with non-launch modes",
            );

            let args = if matches!(
                mode,
                "--set-default-profile"
                    | "--describe-profile"
                    | "--create-profile"
                    | "--update-profile"
                    | "--update-profile-startup"
                    | "--update-profile-env"
                    | "--remove-profile"
                    | "--hide-profile"
                    | "--show-profile"
            ) {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
                    "--new-tab-profile-title",
                    "Work",
                ]
            } else if matches!(mode, "--copy-profile" | "--rename-profile") {
                vec![
                    "zed-terminal",
                    mode,
                    "old",
                    "new",
                    "--new-tab-profile-title",
                    "Work",
                ]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--new-tab-profile-title", "Work"]);
                args
            };
            assert_cli_conflict(
                &args,
                "startup profile tab title should conflict with non-launch modes",
            );

            let args = if matches!(
                mode,
                "--set-default-profile"
                    | "--describe-profile"
                    | "--create-profile"
                    | "--update-profile"
                    | "--remove-profile"
                    | "--hide-profile"
                    | "--show-profile"
            ) {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
                    "--new-tab-profile-split",
                    "right",
                ]
            } else if matches!(mode, "--copy-profile" | "--rename-profile") {
                vec![
                    "zed-terminal",
                    mode,
                    "old",
                    "new",
                    "--new-tab-profile-split",
                    "right",
                ]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--new-tab-profile-split", "right"]);
                args
            };
            assert_cli_conflict(
                &args,
                "startup profile tab split should conflict with non-launch modes",
            );

            let args = if matches!(
                mode,
                "--set-default-profile"
                    | "--describe-profile"
                    | "--create-profile"
                    | "--update-profile"
                    | "--remove-profile"
                    | "--hide-profile"
                    | "--show-profile"
            ) {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
                    "--new-tab-command-title",
                    "Build",
                ]
            } else if matches!(mode, "--copy-profile" | "--rename-profile") {
                vec![
                    "zed-terminal",
                    mode,
                    "old",
                    "new",
                    "--new-tab-command-title",
                    "Build",
                ]
            } else {
                let mut args = mode_args;
                args.extend(["--new-tab-command-title", "Build"]);
                args
            };
            assert_cli_conflict(
                &args,
                "startup command tab title should conflict with non-launch modes",
            );
        }
    }

    #[test]
    fn list_profiles_mode_does_not_resolve_startup_layout() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                display_name: Some("Work".into()),
                hidden: true,
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--list-profiles", "--all-profiles"])
            .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_startup_config(cli, config)
            .expect("profile listing should not resolve startup layout");

        let TerminalCliCommand::ListProfiles {
            startup_config,
            include_hidden,
            ..
        } = command
        else {
            panic!("expected profile listing mode");
        };

        assert!(include_hidden);
        assert_eq!(
            startup_config.profile_summaries(include_hidden),
            vec![TerminalStartupProfileSummary {
                name: "work".into(),
                display_name: "Work".into(),
                description: None,
                icon: None,
                color: None,
                hidden: true,
                is_default: false,
                tab_count: 1,
            }]
        );
    }

    #[test]
    fn list_profiles_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from(["zed-terminal", "--list-profiles", "--profile", "work"])
            .expect_err("profile selection should conflict with profile listing");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile listing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile listing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with profile listing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--list-profiles", "--", "cmd"])
            .expect_err("startup command should conflict with profile listing");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--list-profiles-format", "json"])
            .expect_err("profile list format should require profile listing");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--startup-layout-format", "json"])
            .expect_err("startup layout format should require startup layout printing");
        assert!(error.to_string().contains("required"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--validate-startup-config-format", "json"])
                .expect_err(
                    "startup config validation format should require startup config validation",
                );
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--validate-keymap-format", "json"])
            .expect_err("keymap validation format should require keymap validation");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--init-config-format", "json"])
            .expect_err("config initialization format should require config initialization");
        assert!(error.to_string().contains("required"));

        let error = Cli::try_parse_from(["zed-terminal", "--default-profile-format", "json"])
            .expect_err("default profile format should require a default profile command");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn describe_profile_rejects_startup_only_arguments() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--profile",
            "admin",
        ])
        .expect_err("profile selection should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--describe-profile", "work", "--", "cmd"])
                .expect_err("startup command should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--describe-profile", "work"])
            .expect_err("path inspection should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--list-profiles",
            "--describe-profile",
            "work",
        ])
        .expect_err("profile listing should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--all-profiles",
        ])
        .expect_err("hidden profile listing should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--create-profile",
            "admin",
        ])
        .expect_err("profile creation should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-profile",
            "work",
            "--copy-profile",
            "work",
            "admin",
        ])
        .expect_err("profile copy should conflict with profile description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--describe-profile-format", "json"])
            .expect_err("profile description format should require profile description mode");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn describe_startup_rejects_startup_only_arguments() {
        let error =
            Cli::try_parse_from(["zed-terminal", "--describe-startup", "--profile", "work"])
                .expect_err("profile selection should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let dir = temp_test_dir();
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-startup",
            "-d",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup directory should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-startup",
            "--new-tab-command",
            "cmd /C echo tab",
        ])
        .expect_err("startup tab command should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-startup",
            "--new-tab-command-directory",
            dir.to_str().unwrap(),
        ])
        .expect_err("startup tab command directory should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--describe-startup", "--", "cmd"])
            .expect_err("startup command should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--paths", "--describe-startup"])
            .expect_err("path inspection should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--list-profiles", "--describe-startup"])
            .expect_err("profile listing should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from([
            "zed-terminal",
            "--describe-startup",
            "--create-profile",
            "admin",
        ])
        .expect_err("profile creation should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error =
            Cli::try_parse_from(["zed-terminal", "--describe-startup", "--no-startup-config"])
                .expect_err("startup config disabling should conflict with startup description");
        assert!(error.to_string().contains("cannot be used with"));

        let error = Cli::try_parse_from(["zed-terminal", "--describe-startup-format", "json"])
            .expect_err("startup description format should require startup description mode");
        assert!(error.to_string().contains("required"));

        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn applies_configured_startup_tabs() {
        let initial_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(initial_dir.clone()),
            command: Some("cmd /C \"echo configured\"".into()),
            title: Some("Configured".into()),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(second_dir.clone()),
                command: Some("pwsh -NoLogo".into()),
                title: Some("PowerShell".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_initial_working_directory(&options, &initial_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo configured".into()],
            })
        );
        assert_eq!(options.initial_tab.title.as_deref(), Some("Configured"));
        assert_tab_working_directory(&options.new_terminal_tab, &initial_dir);
        assert_eq!(
            options.new_terminal_tab.command,
            options.initial_tab.command
        );
        assert_eq!(options.new_terminal_tab.title, options.initial_tab.title);
        assert_eq!(options.additional_tabs.len(), 1);
        assert_tab_working_directory(&options.additional_tabs[0], &second_dir);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );
        assert_eq!(
            options.additional_tabs[0].title.as_deref(),
            Some("PowerShell")
        );

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(second_dir).ok();
    }

    #[test]
    fn cli_overrides_configured_initial_startup_tab() {
        let configured_dir = temp_test_dir();
        let cli_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(configured_dir.clone()),
            command: Some("cmd /C configured".into()),
            title: Some("Configured".into()),
            tabs: Vec::new(),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "-d",
            cli_dir.to_str().unwrap(),
            "--",
            "pwsh",
            "-NoLogo",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_initial_working_directory(&options, &cli_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );
        assert_eq!(options.initial_tab.title.as_deref(), Some("Configured"));
        assert_tab_working_directory(&options.new_terminal_tab, &cli_dir);
        assert_eq!(
            options.new_terminal_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "configured".into()],
            })
        );
        assert_eq!(
            options.new_terminal_tab.title.as_deref(),
            Some("Configured")
        );
        assert!(options.additional_tabs.is_empty());

        std_fs::remove_dir_all(configured_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
    }

    #[test]
    fn cli_startup_title_and_command_do_not_replace_new_terminal_tab_template() {
        let config = TerminalStartupConfig {
            command: Some("cmd /C configured".into()),
            title: Some("Configured".into()),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--title", "CLI", "--", "pwsh", "-NoLogo"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.title.as_deref(), Some("CLI"));
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );
        assert_eq!(
            options.new_terminal_tab.title.as_deref(),
            Some("Configured")
        );
        assert_eq!(
            options.new_terminal_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "configured".into()],
            })
        );
    }

    #[test]
    fn cli_overrides_configured_initial_startup_tab_title() {
        let config = TerminalStartupConfig {
            title: Some("Configured".into()),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--title", "CLI"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.title.as_deref(), Some("CLI"));
        assert_eq!(
            options.new_terminal_tab.title.as_deref(),
            Some("Configured")
        );
    }

    #[test]
    fn cli_appends_tabs_after_configured_startup_tabs() {
        let configured_dir = temp_test_dir();
        let cli_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: None,
            command: None,
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(configured_dir.clone()),
                command: None,
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            cli_dir.to_str().unwrap(),
            "--new-tab-command",
            "cmd /C \"echo appended\"",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 3);
        assert_tab_working_directory(&options.additional_tabs[0], &configured_dir);
        assert_eq!(options.additional_tabs[0].command, None);
        assert_tab_working_directory(&options.additional_tabs[1], &cli_dir);
        assert_eq!(options.additional_tabs[1].command, None);
        assert_eq!(options.additional_tabs[2].working_directory, None);
        assert_eq!(
            options.additional_tabs[2].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo appended".into()],
            })
        );

        std_fs::remove_dir_all(configured_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
    }

    #[test]
    fn applies_startup_titles_to_shell_tabs() {
        let config = TerminalStartupConfig {
            title: Some("Shell".into()),
            tabs: vec![TerminalStartupTabConfig {
                title: Some("Logs".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.command, None);
        assert_eq!(options.initial_tab.title.as_deref(), Some("Shell"));
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].command, None);
        assert_eq!(options.additional_tabs[0].title.as_deref(), Some("Logs"));
    }

    #[test]
    fn applies_root_shell_to_shell_tabs() {
        let cli_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
            tabs: vec![TerminalStartupTabConfig {
                title: Some("Inherited".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            cli_dir.to_str().unwrap(),
            "--new-tab-command",
            "cmd /C cli-tab",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.new_terminal_tab.shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(options.new_terminal_tab.command, None);
        assert_eq!(options.initial_tab.command, None);
        assert_eq!(
            options.initial_tab.shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(options.additional_tabs.len(), 3);
        assert_eq!(
            options.additional_tabs[0].shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(
            options.additional_tabs[1].shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(
            options.additional_tabs[2].command.as_ref().unwrap().program,
            "cmd"
        );
        assert_eq!(options.additional_tabs[2].shell, None);

        std_fs::remove_dir_all(cli_dir).ok();
    }

    #[test]
    fn tab_shell_overrides_inherited_shell() {
        let config = TerminalStartupConfig {
            shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
            tabs: vec![TerminalStartupTabConfig {
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "cmd.exe".into(),
                        args: vec!["/K".into()],
                    },
                )),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.initial_tab.shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(
            options.additional_tabs[0].shell,
            Some(shell_with_args("cmd.exe", &["/K"]))
        );
    }

    #[test]
    fn profile_initial_tab_is_selected_for_shell_tabs_and_new_terminal_tabs() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                title: Some("Work".into()),
                shell: Some(TerminalStartupShellConfig::WithArguments(
                    TerminalStartupShellWithArgumentsConfig {
                        program: "pwsh.exe".into(),
                        args: vec!["-NoLogo".into()],
                    },
                )),
                tabs: vec![TerminalStartupTabConfig::default()],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--profile", "work"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.new_terminal_tab.shell,
            Some(shell_with_args("pwsh.exe", &["-NoLogo"]))
        );
        assert_eq!(options.new_terminal_tab.title.as_deref(), Some("Work"));
        assert_eq!(options.new_terminal_tab.command, None);
        assert_eq!(
            options.initial_tab.shell,
            Some(shell_with_args("pwsh.exe", &["-NoLogo"]))
        );
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(
            options.additional_tabs[0].shell,
            Some(shell_with_args("pwsh.exe", &["-NoLogo"]))
        );
    }

    #[test]
    fn cli_startup_command_does_not_inherit_profile_shell() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "--",
            "cmd",
            "/C",
            "cli",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.new_terminal_tab.shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
        assert_eq!(options.new_terminal_tab.command, None);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "cli".into()],
            })
        );
        assert_eq!(options.initial_tab.shell, None);
    }

    #[test]
    fn rejects_empty_startup_shell_program() {
        let config = TerminalStartupConfig {
            shell: Some(TerminalStartupShellConfig::Program("   ".into())),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");

        let error = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect_err("empty startup shell should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("failed to resolve configured startup shell"));
        assert!(message.contains("shell program is empty"));
    }

    #[test]
    fn rejects_tab_shell_on_command_tab() {
        let config = TerminalStartupConfig {
            tabs: vec![TerminalStartupTabConfig {
                command: Some("cmd /C tab".into()),
                shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");

        let error = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect_err("tab shell on command tab should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("failed to resolve configured startup tabs"));
        assert!(message.contains("shell selection requires a shell tab"));
    }

    #[test]
    fn no_startup_config_ignores_configured_startup_tabs() {
        let configured_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(configured_dir.clone()),
            command: Some("cmd /C configured".into()),
            title: Some("Configured".into()),
            shell: Some(TerminalStartupShellConfig::Program("pwsh.exe".into())),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(configured_dir.clone()),
                command: Some("cmd /C tab".into()),
                title: Some("Tab".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--no-startup-config"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.working_directory, None);
        assert_eq!(options.initial_tab.command, None);
        assert_eq!(options.initial_tab.title, None);
        assert_eq!(options.initial_tab.shell, None);
        assert_eq!(options.new_terminal_tab.working_directory, None);
        assert_eq!(options.new_terminal_tab.command, None);
        assert_eq!(options.new_terminal_tab.title, None);
        assert_eq!(options.new_terminal_tab.shell, None);
        assert!(options.additional_tabs.is_empty());

        std_fs::remove_dir_all(configured_dir).ok();
    }

    #[test]
    fn rejects_profile_with_no_startup_config() {
        let error =
            Cli::try_parse_from(["zed-terminal", "--profile", "work", "--no-startup-config"])
                .expect_err("profile and no-startup-config should conflict");

        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn loads_startup_config_from_config_file() {
        let data_dir = temp_test_dir();
        let configured_dir = temp_test_dir();
        let config_dir = data_dir.join("config");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(
            terminal_startup_config_file(&config_dir),
            format!(
                r#"{{
                    // JSON-with-comments should match Zed settings ergonomics.
                    "working_directory": "{}",
                    "command": "cmd /C configured"
                }}"#,
                configured_dir.display().to_string().replace('\\', "\\\\")
            ),
        )
        .expect("failed to write startup config");
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--user-data-dir",
            data_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let command =
            TerminalCliCommand::from_cli_and_config_file(cli).expect("failed to build cli command");
        let TerminalCliCommand::Launch(options) = command else {
            panic!("expected launch mode");
        };

        assert_initial_working_directory(&options, &configured_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "configured".into()],
            })
        );

        std_fs::remove_dir_all(data_dir).ok();
        std_fs::remove_dir_all(configured_dir).ok();
    }

    #[test]
    fn applies_root_env_to_configured_startup_command() {
        let env = test_env(&[("ZED_TERMINAL_ROOT", "1"), ("COMMON", "root")]);
        let config = TerminalStartupConfig {
            command: Some("cmd /C configured".into()),
            env: env.clone(),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "configured".into()],
            })
        );
        assert_eq!(options.initial_tab.env, env);
        assert!(options.additional_tabs.is_empty());
    }

    #[test]
    fn normalizes_blank_startup_titles() {
        let config = TerminalStartupConfig {
            title: Some("   ".into()),
            command: Some("cmd /C configured".into()),
            tabs: vec![TerminalStartupTabConfig {
                title: Some("\t".into()),
                command: Some("cmd /C tab".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.title, None);
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].title, None);
    }

    #[test]
    fn profile_env_is_inherited_by_profile_command_tabs() {
        let profile_env = test_env(&[("ZED_TERMINAL_PROFILE", "work"), ("COMMON", "profile")]);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                command: Some("cmd /C profile".into()),
                title: Some("Work".into()),
                env: profile_env.clone(),
                tabs: vec![TerminalStartupTabConfig {
                    command: Some("pwsh -NoLogo".into()),
                    title: Some("Profile Tab".into()),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.env, profile_env);
        assert_eq!(options.initial_tab.title.as_deref(), Some("Work"));
        assert_eq!(options.new_terminal_tab.env, options.initial_tab.env);
        assert_eq!(
            options.new_terminal_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "profile".into()],
            })
        );
        assert_eq!(options.new_terminal_tab.title.as_deref(), Some("Work"));
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].env, options.initial_tab.env);
        assert_eq!(
            options.additional_tabs[0].title.as_deref(),
            Some("Profile Tab")
        );
    }

    #[test]
    fn tab_env_overrides_inherited_env() {
        let config = TerminalStartupConfig {
            env: test_env(&[("COMMON", "root"), ("ROOT_ONLY", "yes")]),
            tabs: vec![TerminalStartupTabConfig {
                command: Some("cmd /C tab".into()),
                env: test_env(&[("COMMON", "tab"), ("TAB_ONLY", "yes")]),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.env, HashMap::default());
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(
            options.additional_tabs[0].env,
            test_env(&[("COMMON", "tab"), ("ROOT_ONLY", "yes"), ("TAB_ONLY", "yes"),])
        );
    }

    #[test]
    fn cli_startup_command_inherits_selected_profile_env() {
        let profile_env = test_env(&[("ZED_TERMINAL_PROFILE", "work")]);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                env: profile_env.clone(),
                title: Some("Work".into()),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "--",
            "cmd",
            "/C",
            "cli",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "cli".into()],
            })
        );
        assert_eq!(options.initial_tab.env, profile_env);
        assert_eq!(options.initial_tab.title.as_deref(), Some("Work"));
    }

    #[test]
    fn cli_additional_command_tabs_inherit_selected_profile_env() {
        let profile_env = test_env(&[("ZED_TERMINAL_PROFILE", "work")]);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                env: profile_env.clone(),
                title: Some("Work".into()),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "--new-tab-command",
            "cmd /C cli-tab",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.initial_tab.title.as_deref(), Some("Work"));
        assert_eq!(options.additional_tabs[0].env, profile_env);
        assert_eq!(options.additional_tabs[0].title, None);
    }

    #[test]
    fn configured_shell_tabs_do_not_inherit_layout_env() {
        let config = TerminalStartupConfig {
            env: test_env(&[("ZED_TERMINAL_ROOT", "1")]),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: None,
                command: None,
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.initial_tab.env, HashMap::default());
        assert_eq!(options.initial_tab.title, None);
        assert_eq!(options.initial_tab.shell, None);
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].command, None);
        assert_eq!(options.additional_tabs[0].env, HashMap::default());
        assert_eq!(options.additional_tabs[0].title, None);
        assert_eq!(options.additional_tabs[0].shell, None);
    }

    #[test]
    fn rejects_env_on_configured_shell_tab() {
        let config = TerminalStartupConfig {
            tabs: vec![TerminalStartupTabConfig {
                env: test_env(&[("ZED_TERMINAL_TAB", "1")]),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");

        let error = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect_err("tab-level env without a command should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("failed to resolve configured startup tabs"));
        assert!(message.contains("environment variables require a command"));
    }

    #[test]
    fn applies_default_startup_profile() {
        let profile_dir = temp_test_dir();
        let profile_tab_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(profile_dir.clone()),
                command: Some("cmd /C \"echo profile\"".into()),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(profile_tab_dir.clone()),
                    command: Some("pwsh -NoLogo".into()),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            working_directory: None,
            command: None,
            tabs: Vec::new(),
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_initial_working_directory(&options, &profile_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo profile".into()],
            })
        );
        assert_eq!(options.additional_tabs.len(), 1);
        assert_tab_working_directory(&options.additional_tabs[0], &profile_tab_dir);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );

        std_fs::remove_dir_all(profile_dir).ok();
        std_fs::remove_dir_all(profile_tab_dir).ok();
    }

    #[test]
    fn cli_selects_startup_profile() {
        let root_dir = temp_test_dir();
        let work_dir = temp_test_dir();
        let admin_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(work_dir.clone()),
                command: Some("cmd /C work".into()),
                tabs: Vec::new(),
                ..TerminalStartupProfileConfig::default()
            },
        );
        profiles.insert(
            "admin".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(admin_dir.clone()),
                command: Some("cmd /C admin".into()),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: None,
                    command: Some("pwsh -NoLogo".into()),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            working_directory: Some(root_dir.clone()),
            command: Some("cmd /C root".into()),
            tabs: Vec::new(),
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--profile", "admin"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_initial_working_directory(&options, &admin_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "admin".into()],
            })
        );
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );

        std_fs::remove_dir_all(root_dir).ok();
        std_fs::remove_dir_all(work_dir).ok();
        std_fs::remove_dir_all(admin_dir).ok();
    }

    #[test]
    fn cli_overrides_selected_startup_profile_initial_tab() {
        let profile_dir = temp_test_dir();
        let cli_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(profile_dir.clone()),
                command: Some("cmd /C profile".into()),
                tabs: Vec::new(),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--profile",
            "work",
            "-d",
            cli_dir.to_str().unwrap(),
            "--",
            "pwsh",
            "-NoLogo",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_initial_working_directory(&options, &cli_dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );

        std_fs::remove_dir_all(profile_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
    }

    #[test]
    fn rejects_missing_startup_profile() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: None,
                command: None,
                tabs: Vec::new(),
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--profile", "missing"])
            .expect("failed to parse cli args");

        let error = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect_err("missing profile should be rejected");
        let message = format!("{error:#}");

        assert!(message.contains("startup profile not found: missing"));
        assert!(message.contains("Available profiles: work"));
    }

    #[test]
    fn paths_mode_ignores_requested_startup_profile() {
        let config = TerminalStartupConfig {
            default_profile: Some("missing".into()),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal", "--paths", "--profile", "missing"])
            .expect("failed to parse cli args");
        let command = TerminalCliCommand::from_cli_and_startup_config(cli, config)
            .expect("paths mode should not resolve startup profiles");

        assert!(matches!(command, TerminalCliCommand::PrintPaths { .. }));
    }

    #[test]
    fn rejects_unknown_startup_config_fields() {
        let error =
            settings::parse_json_with_comments::<TerminalStartupConfig>(r#"{"profile": "bad"}"#)
                .expect_err("unknown startup config field should be rejected");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_empty_configured_startup_command() {
        let config = TerminalStartupConfig {
            working_directory: None,
            command: Some("".into()),
            tabs: Vec::new(),
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from(["zed-terminal"]).expect("failed to parse cli args");

        let error = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect_err("empty startup command should be rejected");

        assert!(
            error
                .to_string()
                .contains("failed to resolve configured new terminal tab")
        );
    }

    #[test]
    fn parses_short_working_directory_flag() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from(["zed-terminal", "-d", dir.to_str().unwrap()])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.initial_tab.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_cwd_alias() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from(["zed-terminal", "--cwd", dir.to_str().unwrap()])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &dir);
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_windows_terminal_starting_directory_alias() {
        let dir = temp_test_dir();
        let cli =
            Cli::try_parse_from(["zed-terminal", "--startingDirectory", dir.to_str().unwrap()])
                .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &dir);
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_positional_working_directory() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from(["zed-terminal", dir.to_str().unwrap()])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &dir);
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_additional_startup_tabs() {
        let initial_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let third_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "-d",
            initial_dir.to_str().unwrap(),
            "--new-tab",
            second_dir.to_str().unwrap(),
            "--tab",
            third_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &initial_dir);
        assert_eq!(options.additional_tabs.len(), 2);
        assert_eq!(
            options.additional_tabs[0].working_directory.as_deref(),
            Some(dunce::canonicalize(&second_dir).unwrap().as_path())
        );
        assert_eq!(
            options.additional_tabs[1].working_directory.as_deref(),
            Some(dunce::canonicalize(&third_dir).unwrap().as_path())
        );
        assert_eq!(options.additional_tabs[0].command, None);
        assert_eq!(options.additional_tabs[1].command, None);

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(second_dir).ok();
        std_fs::remove_dir_all(third_dir).ok();
    }

    #[test]
    fn parses_additional_startup_tab_titles() {
        let first_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            first_dir.to_str().unwrap(),
            "--new-tab-title",
            "Logs",
            "--tab",
            second_dir.to_str().unwrap(),
            "--tab-title",
            "Shell",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 2);
        assert_tab_working_directory(&options.additional_tabs[0], &first_dir);
        assert_eq!(options.additional_tabs[0].title.as_deref(), Some("Logs"));
        assert_tab_working_directory(&options.additional_tabs[1], &second_dir);
        assert_eq!(options.additional_tabs[1].title.as_deref(), Some("Shell"));

        std_fs::remove_dir_all(first_dir).ok();
        std_fs::remove_dir_all(second_dir).ok();
    }

    #[test]
    fn parses_additional_startup_profile_tabs() {
        let directory_tab_dir = temp_test_dir();
        let profile_dir = temp_test_dir();
        let profile_extra_dir = temp_test_dir();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                working_directory: Some(profile_dir.clone()),
                command: Some("cmd /C work".into()),
                title: Some("Work".into()),
                env: test_env(&[("ZED_TERMINAL_PROFILE", "work")]),
                tabs: vec![TerminalStartupTabConfig {
                    working_directory: Some(profile_extra_dir.clone()),
                    title: Some("Logs".into()),
                    ..TerminalStartupTabConfig::default()
                }],
                ..TerminalStartupProfileConfig::default()
            },
        );
        let config = TerminalStartupConfig {
            profiles,
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            directory_tab_dir.to_str().unwrap(),
            "--new-tab-profile",
            "work",
            "--new-tab-profile-title",
            "Work Override",
            "--new-tab-profile-split",
            "left",
            "--new-tab-command",
            "cmd /C build",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 3);
        assert_tab_working_directory(&options.additional_tabs[0], &directory_tab_dir);
        assert_eq!(options.additional_tabs[0].command, None);

        let profile_tab = &options.additional_tabs[1];
        assert_tab_working_directory(profile_tab, &profile_dir);
        assert_ne!(
            profile_tab.working_directory.as_deref(),
            Some(dunce::canonicalize(&profile_extra_dir).unwrap().as_path())
        );
        assert_eq!(
            profile_tab.command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "work".into()],
            })
        );
        assert_eq!(
            profile_tab.env,
            test_env(&[("ZED_TERMINAL_PROFILE", "work")])
        );
        assert_eq!(profile_tab.title.as_deref(), Some("Work Override"));
        assert_eq!(profile_tab.split, Some(TerminalStartupSplitDirection::Left));

        assert_eq!(
            options.additional_tabs[2].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "build".into()],
            })
        );

        std_fs::remove_dir_all(directory_tab_dir).ok();
        std_fs::remove_dir_all(profile_dir).ok();
        std_fs::remove_dir_all(profile_extra_dir).ok();
    }

    #[test]
    fn parses_additional_startup_tab_commands() {
        let first_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            first_dir.to_str().unwrap(),
            "--new-tab-command",
            "cmd /C \"echo one\"",
            "--tab",
            second_dir.to_str().unwrap(),
            "--tab-command",
            "pwsh -NoLogo",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 4);
        assert_eq!(
            options.additional_tabs[0].working_directory.as_deref(),
            Some(dunce::canonicalize(&first_dir).unwrap().as_path())
        );
        assert_eq!(options.additional_tabs[0].command, None);
        assert_eq!(
            options.additional_tabs[1].working_directory.as_deref(),
            Some(dunce::canonicalize(&second_dir).unwrap().as_path())
        );
        assert_eq!(options.additional_tabs[1].command, None);
        assert_eq!(options.additional_tabs[2].working_directory, None);
        assert_eq!(
            options.additional_tabs[2].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo one".into()],
            })
        );
        assert_eq!(options.additional_tabs[3].working_directory, None);
        assert_eq!(
            options.additional_tabs[3].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );

        std_fs::remove_dir_all(first_dir).ok();
        std_fs::remove_dir_all(second_dir).ok();
    }

    #[test]
    fn parses_additional_startup_tab_command_directories() {
        let first_dir = temp_test_dir();
        let command_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab",
            first_dir.to_str().unwrap(),
            "--new-tab-command",
            "cmd /C \"echo one\"",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 2);
        assert_tab_working_directory(&options.additional_tabs[0], &first_dir);
        assert_eq!(options.additional_tabs[0].command, None);
        assert_tab_working_directory(&options.additional_tabs[1], &command_dir);
        assert_eq!(
            options.additional_tabs[1].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo one".into()],
            })
        );

        std_fs::remove_dir_all(first_dir).ok();
        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn parses_additional_startup_tab_command_titles() {
        let command_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab-command",
            "cmd /C \"echo one\"",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
            "--new-tab-command-title",
            "Build",
            "--tab-command",
            "pwsh -NoLogo",
            "--tab-command-title",
            "Shell Command",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 2);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "echo one".into()],
            })
        );
        assert_tab_working_directory(&options.additional_tabs[0], &command_dir);
        assert_eq!(options.additional_tabs[0].title.as_deref(), Some("Build"));
        assert_eq!(options.additional_tabs[1].working_directory, None);
        assert_eq!(
            options.additional_tabs[1].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );
        assert_eq!(
            options.additional_tabs[1].title.as_deref(),
            Some("Shell Command")
        );

        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn maps_fewer_command_directories_to_first_command_tabs() {
        let command_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab-command",
            "cmd /C one",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
            "--new-tab-command",
            "pwsh -NoLogo",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.additional_tabs.len(), 2);
        assert_tab_working_directory(&options.additional_tabs[0], &command_dir);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "cmd".into(),
                args: vec!["/C".into(), "one".into()],
            })
        );
        assert_eq!(options.additional_tabs[1].working_directory, None);
        assert_eq!(
            options.additional_tabs[1].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );

        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn normalizes_blank_cli_startup_tab_titles() {
        let tab_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--title",
            "  ",
            "--new-tab",
            tab_dir.to_str().unwrap(),
            "--new-tab-title",
            "\t",
            "--new-tab-command",
            "cmd /C one",
            "--new-tab-command-title",
            "",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.initial_tab.title, None);
        assert_eq!(options.additional_tabs.len(), 2);
        assert_eq!(options.additional_tabs[0].title, None);
        assert_eq!(options.additional_tabs[1].title, None);

        std_fs::remove_dir_all(tab_dir).ok();
    }

    #[test]
    fn parses_command_only_additional_startup_tab() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-command", "cargo --version"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.additional_tabs,
            vec![LaunchTab {
                working_directory: None,
                command: Some(LaunchCommand {
                    program: "cargo".into(),
                    args: vec!["--version".into()],
                }),
                env: HashMap::default(),
                title: None,
                shell: None,
                split: None,
            }]
        );
    }

    #[test]
    fn rejects_empty_additional_startup_tab_command() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-command", ""])
            .expect("failed to parse cli args");

        let error = LaunchOptions::from_cli(cli).expect_err("empty command should be rejected");

        assert!(error.to_string().contains("failed to parse startup tab 2"));
    }

    #[test]
    fn rejects_unmatched_additional_startup_tab_title() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-title", "Logs"])
            .expect("failed to parse cli args");

        let error = LaunchOptions::from_cli(cli).expect_err("unmatched tab title should fail");

        assert!(
            error
                .to_string()
                .contains("startup tab title requires a matching --new-tab")
        );
    }

    #[test]
    fn rejects_unmatched_additional_startup_profile_tab_title() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-profile-title", "Work"])
            .expect("failed to parse cli args");

        let error =
            LaunchOptions::from_cli(cli).expect_err("unmatched profile tab title should fail");

        assert!(
            error
                .to_string()
                .contains("startup profile tab title requires a matching --new-tab-profile")
        );
    }

    #[test]
    fn rejects_unmatched_additional_startup_profile_tab_split() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-profile-split", "down"])
            .expect("failed to parse cli args");

        let error =
            LaunchOptions::from_cli(cli).expect_err("unmatched profile tab split should fail");

        assert!(
            error
                .to_string()
                .contains("startup profile tab split requires a matching --new-tab-profile")
        );
    }

    #[test]
    fn rejects_additional_startup_profile_tab_with_no_startup_config() {
        let error = Cli::try_parse_from([
            "zed-terminal",
            "--no-startup-config",
            "--new-tab-profile",
            "work",
        ])
        .expect_err("profile tabs should require startup config");

        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn rejects_unmatched_additional_startup_tab_command_directory() {
        let command_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab-command-directory",
            command_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");

        let error =
            LaunchOptions::from_cli(cli).expect_err("unmatched command directory should fail");

        assert!(
            error
                .to_string()
                .contains("startup command tab directory requires a matching --new-tab-command")
        );

        std_fs::remove_dir_all(command_dir).ok();
    }

    #[test]
    fn rejects_unmatched_additional_startup_tab_command_title() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-command-title", "Build"])
            .expect("failed to parse cli args");

        let error = LaunchOptions::from_cli(cli).expect_err("unmatched command title should fail");

        assert!(
            error
                .to_string()
                .contains("startup command tab title requires a matching --new-tab-command")
        );
    }

    #[test]
    fn rejects_invalid_additional_startup_tab_command_directory() {
        let file = env::temp_dir().join(format!(
            "zed-terminal-test-command-dir-{}",
            uuid::Uuid::new_v4()
        ));
        std_fs::write(&file, "not a directory").expect("failed to create temp file");
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "--new-tab-command",
            "cmd /C one",
            "--new-tab-command-directory",
            file.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");

        let error =
            LaunchOptions::from_cli(cli).expect_err("non-directory command cwd should fail");
        let message = format!("{error:#}");

        assert!(message.contains("failed to resolve startup tab 2 working directory"));
        assert!(message.contains("working directory is not a directory"));

        std_fs::remove_file(file).ok();
    }

    #[test]
    fn rejects_unclosed_additional_startup_tab_command_quote() {
        let cli = Cli::try_parse_from(["zed-terminal", "--new-tab-command", "\"unterminated"])
            .expect("failed to parse cli args");

        let error =
            LaunchOptions::from_cli(cli).expect_err("unterminated quote should be rejected");

        assert!(error.to_string().contains("failed to parse startup tab 2"));
    }

    #[test]
    fn collects_unique_startup_working_directories() {
        let initial_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            initial_dir.to_str().unwrap(),
            "--new-tab",
            second_dir.to_str().unwrap(),
            "--new-tab",
            initial_dir.to_str().unwrap(),
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.startup_working_directories(),
            vec![
                dunce::canonicalize(&initial_dir).unwrap(),
                dunce::canonicalize(&second_dir).unwrap()
            ]
        );
        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(second_dir).ok();
    }

    #[test]
    fn startup_working_directories_include_new_terminal_tab_template() {
        let initial_dir = temp_test_dir();
        let new_terminal_dir = temp_test_dir();
        let additional_dir = temp_test_dir();
        let options = LaunchOptions {
            path_options: TerminalPathOptions {
                data_dir: PathBuf::from("data"),
                config_dir: PathBuf::from("config"),
            },
            initial_tab: LaunchTab {
                working_directory: Some(initial_dir.clone()),
                command: None,
                env: HashMap::default(),
                title: None,
                shell: None,
                split: None,
            },
            additional_tabs: vec![LaunchTab {
                working_directory: Some(additional_dir.clone()),
                command: None,
                env: HashMap::default(),
                title: None,
                shell: None,
                split: None,
            }],
            new_terminal_tab: LaunchTab {
                working_directory: Some(new_terminal_dir.clone()),
                command: None,
                env: HashMap::default(),
                title: None,
                shell: None,
                split: None,
            },
        };

        assert_eq!(
            options.startup_working_directories(),
            vec![
                initial_dir.clone(),
                new_terminal_dir.clone(),
                additional_dir.clone()
            ]
        );

        std_fs::remove_dir_all(initial_dir).ok();
        std_fs::remove_dir_all(new_terminal_dir).ok();
        std_fs::remove_dir_all(additional_dir).ok();
    }

    #[test]
    fn runtime_new_window_uses_new_tab_template_without_replaying_startup_tabs() {
        let configured_dir = temp_test_dir();
        let cli_dir = temp_test_dir();
        let additional_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(configured_dir.clone()),
            command: Some("configured-task".into()),
            title: Some("Configured".into()),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(additional_dir.clone()),
                title: Some("Extra".into()),
                ..TerminalStartupTabConfig::default()
            }],
            ..TerminalStartupConfig::default()
        };
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "-d",
            cli_dir.to_str().expect("temp path should be utf8"),
            "--title",
            "CLI",
            "--",
            "pwsh",
        ])
        .expect("cli should parse");
        let launch_options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("failed to build launch options");

        let window_options = launch_options.runtime_new_window_options();

        assert!(window_options.additional_tabs.is_empty());
        assert_eq!(window_options.initial_tab, launch_options.new_terminal_tab);
        assert_eq!(
            window_options.new_terminal_tab,
            launch_options.new_terminal_tab
        );
        assert_eq!(
            window_options.initial_tab.command,
            Some(LaunchCommand {
                program: "configured-task".into(),
                args: Vec::new()
            })
        );
        assert_eq!(
            window_options.initial_tab.title.as_deref(),
            Some("Configured")
        );
        assert_initial_working_directory(&window_options, &cli_dir);
        assert_eq!(
            window_options.startup_working_directories(),
            vec![dunce::canonicalize(&cli_dir).unwrap()]
        );

        std_fs::remove_dir_all(configured_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
        std_fs::remove_dir_all(additional_dir).ok();
    }

    #[test]
    fn parses_startup_command_after_separator() {
        let cli = Cli::try_parse_from(["zed-terminal", "--", "echo", "hello"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.initial_tab.working_directory, None);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "echo".into(),
                args: vec!["hello".into()],
            })
        );
    }

    #[test]
    fn parses_directory_and_startup_command() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            dir.to_str().unwrap(),
            "--",
            "pwsh",
            "-NoLogo",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_flag_directory_and_startup_command() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from([
            "zed-terminal",
            "-d",
            dir.to_str().unwrap(),
            "--",
            "cargo",
            "--version",
        ])
        .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_initial_working_directory(&options, &dir);
        assert_eq!(
            options.initial_tab.command,
            Some(LaunchCommand {
                program: "cargo".into(),
                args: vec!["--version".into()],
            })
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn maps_launch_command_to_spawn_task() {
        let dir = temp_test_dir();
        let command = LaunchCommand {
            program: "cmd".into(),
            args: vec!["/C".into(), "echo hello".into()],
        };

        let mut env = HashMap::default();
        env.insert("ZED_TERMINAL_TEST".into(), "1".into());

        let task = command.into_spawn_task(Some(dir.clone()), env.clone());

        assert_eq!(task.id, TaskId("zed-terminal:cmd /C \"echo hello\"".into()));
        assert_eq!(task.full_label, "cmd /C \"echo hello\"");
        assert_eq!(task.label, "cmd /C \"echo hello\"");
        assert_eq!(task.command, Some("cmd".into()));
        assert_eq!(task.args, vec!["/C", "echo hello"]);
        assert_eq!(task.command_label, "cmd /C \"echo hello\"");
        assert_eq!(task.cwd.as_deref(), Some(dir.as_path()));
        assert_eq!(task.env, env);
        assert!(task.use_new_terminal);
        assert!(task.allow_concurrent_runs);
        assert_eq!(task.reveal, RevealStrategy::Always);
        assert_eq!(task.reveal_target, RevealTarget::Center);
        assert_eq!(task.hide, HideStrategy::Never);
        assert_eq!(task.shell, Shell::System);
        assert!(task.show_summary);
        assert!(task.show_command);
        assert!(!task.show_rerun);
        assert_eq!(task.save, SaveStrategy::None);
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_non_directory_working_directory() {
        let dir = temp_test_dir();
        let file = dir.join("file.txt");
        std_fs::write(&file, "").expect("failed to write temp test file");

        let error = resolve_working_directory(&file).expect_err("file path should be rejected");

        assert!(
            error
                .to_string()
                .contains("working directory is not a directory")
        );
        std_fs::remove_dir_all(dir).ok();
    }
}
