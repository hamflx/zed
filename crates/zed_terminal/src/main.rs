use std::{
    any::TypeId,
    collections::BTreeMap,
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
use clap::{Parser, ValueEnum, ValueHint};
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
        OpenConfigDirectory,
        OpenLogsDirectory,
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
const TERMINAL_PROFILE_COMMAND_PALETTE_MAX_RESULTS: usize = 100;

static TERMINAL_LOG_FILE: OnceLock<PathBuf> = OnceLock::new();
static TERMINAL_OLD_LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Debug, Parser)]
#[command(
    name = "zed-terminal",
    version,
    about = "Launch the standalone Zed terminal."
)]
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
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config",
            "doctor"
        ]
    )]
    print_paths: bool,

    #[arg(
        long = "list-profiles",
        conflicts_with_all = [
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
        help = "List configured startup profiles without opening a terminal window"
    )]
    list_profiles: bool,

    #[arg(
        long = "all-profiles",
        requires = "list_profiles",
        conflicts_with_all = [
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config",
            "doctor"
        ],
        help = "Include hidden startup profiles when listing profiles"
    )]
    all_profiles: bool,

    #[arg(
        long = "no-startup-config",
        conflicts_with_all = [
            "profile",
            "list_profiles",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config",
            "doctor"
        ],
        help = "Print the resolved startup layout without opening a terminal window"
    )]
    print_startup_layout: bool,

    #[arg(
        long = "set-default-profile",
        value_name = "NAME",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
        help = "Set the default startup profile in terminal.json without opening a terminal window"
    )]
    set_default_profile: Option<String>,

    #[arg(
        long = "clear-default-profile",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
        help = "Clear the default startup profile in terminal.json without opening a terminal window"
    )]
    clear_default_profile: bool,

    #[arg(
        long = "validate-startup-config",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_keymap",
            "print_startup_config_schema",
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
        long = "print-startup-config-schema",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "validate_keymap",
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
            "validate_startup_config",
            "print_startup_config_schema",
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
        long = "doctor",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "all_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "print_startup_config_schema",
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
        long = "validate-keymap",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "print_startup_layout",
            "set_default_profile",
            "clear_default_profile",
            "validate_startup_config",
            "print_startup_config_schema",
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
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
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
    PrintPaths(TerminalPathOptions),
    ListProfiles {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
        include_hidden: bool,
    },
    SetDefaultProfile {
        path_options: TerminalPathOptions,
        profile: String,
    },
    ClearDefaultProfile {
        path_options: TerminalPathOptions,
    },
    ValidateStartupConfig {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
    },
    PrintStartupLayout(LaunchOptions),
    PrintStartupConfigSchema {
        path_options: TerminalPathOptions,
    },
    InitConfig {
        path_options: TerminalPathOptions,
    },
    Doctor {
        path_options: TerminalPathOptions,
    },
    ValidateKeymap {
        path_options: TerminalPathOptions,
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
struct TerminalStartupProfileMenuEntry {
    profile: String,
    label: String,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalStartupConfigValidation {
    layout_count: usize,
    tab_count: usize,
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

impl TerminalCliCommand {
    fn from_cli_and_config_file(cli: Cli) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;
        let startup_config = if cli.print_paths
            || cli.no_startup_config
            || cli.set_default_profile.is_some()
            || cli.clear_default_profile
            || cli.validate_keymap
            || cli.print_startup_config_schema
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
            return Ok(Self::PrintPaths(path_options));
        }

        if cli.list_profiles {
            return Ok(Self::ListProfiles {
                path_options,
                startup_config,
                include_hidden: cli.all_profiles,
            });
        }

        if cli.print_startup_layout {
            return Ok(Self::PrintStartupLayout(LaunchOptions::from_cli_parts(
                cli,
                startup_config,
                path_options,
            )?));
        }

        if let Some(profile) = cli.set_default_profile {
            return Ok(Self::SetDefaultProfile {
                path_options,
                profile,
            });
        }

        if cli.clear_default_profile {
            return Ok(Self::ClearDefaultProfile { path_options });
        }

        if cli.validate_startup_config {
            return Ok(Self::ValidateStartupConfig {
                path_options,
                startup_config,
            });
        }

        if cli.print_startup_config_schema {
            return Ok(Self::PrintStartupConfigSchema { path_options });
        }

        if cli.init_config {
            return Ok(Self::InitConfig { path_options });
        }

        if cli.doctor {
            return Ok(Self::Doctor { path_options });
        }

        if cli.validate_keymap {
            return Ok(Self::ValidateKeymap { path_options });
        }

        Ok(Self::Launch(LaunchOptions::from_cli_parts(
            cli,
            startup_config,
            path_options,
        )?))
    }

    fn path_options(&self) -> &TerminalPathOptions {
        match self {
            Self::PrintPaths(path_options) => path_options,
            Self::ListProfiles { path_options, .. } => path_options,
            Self::SetDefaultProfile { path_options, .. } => path_options,
            Self::ClearDefaultProfile { path_options } => path_options,
            Self::ValidateStartupConfig { path_options, .. } => path_options,
            Self::PrintStartupLayout(launch_options) => &launch_options.path_options,
            Self::PrintStartupConfigSchema { path_options } => path_options,
            Self::InitConfig { path_options } => path_options,
            Self::Doctor { path_options } => path_options,
            Self::ValidateKeymap { path_options } => path_options,
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
        TerminalCliCommand::Doctor { path_options } => {
            run_terminal_doctor(path_options.clone());
            return;
        }
        TerminalCliCommand::PrintStartupLayout(launch_options) => {
            print_startup_layout(launch_options);
            return;
        }
        _ => {}
    }

    if let Err(error) = install_terminal_paths(command.path_options()) {
        eprintln!("failed to run zed terminal: {error:#}");
        process::exit(2);
    }

    match command {
        TerminalCliCommand::PrintPaths(_) => print_terminal_paths(),
        TerminalCliCommand::ListProfiles {
            startup_config,
            include_hidden,
            ..
        } => print_startup_profiles(&startup_config, include_hidden),
        TerminalCliCommand::ValidateStartupConfig { startup_config, .. } => {
            if let Err(error) = print_startup_config_validation(&startup_config) {
                eprintln!("failed to validate terminal startup config: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::PrintStartupLayout(_) => {
            unreachable!("startup layout printing is handled before path install")
        }
        TerminalCliCommand::SetDefaultProfile { profile, .. } => {
            if let Err(error) = print_default_profile_update(&profile) {
                eprintln!("failed to set default startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::ClearDefaultProfile { .. } => {
            if let Err(error) = print_clear_default_profile_update() {
                eprintln!("failed to clear default startup profile: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::PrintStartupConfigSchema { .. } => {
            if let Err(error) = print_startup_config_schema() {
                eprintln!("failed to print terminal startup config schema: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::InitConfig { .. } => {
            if let Err(error) = print_config_initialization() {
                eprintln!("failed to initialize terminal config files: {error:#}");
                process::exit(2);
            }
        }
        TerminalCliCommand::Doctor { .. } => unreachable!("doctor is handled before path install"),
        TerminalCliCommand::ValidateKeymap { .. } => run_keymap_validation(),
        TerminalCliCommand::Launch(launch_options) => launch_terminal(launch_options),
    }
}

fn run_terminal_doctor(path_options: TerminalPathOptions) {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            let report = diagnose_terminal(&path_options, cx);
            print!("{}", format_doctor_report(&report));
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

fn run_keymap_validation() {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            match validate_keymaps(paths::keymap_file(), cx) {
                Ok(validation) => {
                    print!(
                        "{}",
                        format_keymap_validation(paths::keymap_file(), &validation)
                    );
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

fn print_terminal_paths() {
    println!("config_dir: {}", paths::config_dir().display());
    println!("data_dir: {}", paths::data_dir().display());
    println!("logs_dir: {}", paths::logs_dir().display());
    println!("settings_file: {}", paths::settings_file().display());
    println!(
        "startup_config_file: {}",
        active_terminal_startup_config_file().display()
    );
    println!(
        "startup_config_schema_file: {}",
        active_terminal_startup_config_schema_file().display()
    );
    println!(
        "global_settings_file: {}",
        paths::global_settings_file().display()
    );
    println!("keymap_file: {}", paths::keymap_file().display());
    println!("themes_dir: {}", paths::themes_dir().display());
    println!("log_file: {}", terminal_log_file().display());
}

fn print_startup_profiles(startup_config: &TerminalStartupConfig, include_hidden: bool) {
    print!(
        "{}",
        format_startup_profiles(
            startup_config,
            &active_terminal_startup_config_file(),
            include_hidden
        )
    );
}

fn print_startup_config_validation(startup_config: &TerminalStartupConfig) -> Result<()> {
    let validation = startup_config.validate()?;
    print!(
        "{}",
        format_startup_config_validation(&active_terminal_startup_config_file(), &validation)
    );
    Ok(())
}

fn print_startup_layout(launch_options: &LaunchOptions) {
    print!(
        "{}",
        format_startup_layout(
            launch_options,
            &terminal_startup_config_file(&launch_options.path_options.config_dir),
        )
    );
}

fn print_startup_config_schema() -> Result<()> {
    print!("{}", format_startup_config_schema()?);
    Ok(())
}

fn print_config_initialization() -> Result<()> {
    let initialization = initialize_terminal_config_files()?;
    print!("{}", format_config_initialization(&initialization));
    Ok(())
}

fn print_default_profile_update(profile: &str) -> Result<()> {
    let update = set_default_startup_profile(&active_terminal_startup_config_file(), profile)?;
    print!("{}", format_default_profile_update(&update));
    Ok(())
}

fn print_clear_default_profile_update() -> Result<()> {
    let update = clear_default_startup_profile(&active_terminal_startup_config_file())?;
    print!("{}", format_default_profile_update(&update));
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
    startup_config_file: PathBuf,
    startup_config_schema_file: PathBuf,
}

impl TerminalConfigFilePaths {
    fn from_path_options(path_options: &TerminalPathOptions) -> Self {
        Self {
            settings_file: path_options.config_dir.join("settings.json"),
            global_settings_file: path_options.config_dir.join("global_settings.json"),
            keymap_file: path_options.config_dir.join("keymap.json"),
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

fn set_default_startup_profile(path: &Path, profile: &str) -> Result<TerminalDefaultProfileUpdate> {
    let profile = normalize_startup_profile_name(profile)?;
    update_default_startup_profile(path, Some(profile))
}

fn clear_default_startup_profile(path: &Path) -> Result<TerminalDefaultProfileUpdate> {
    update_default_startup_profile(path, None)
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
    let summaries = startup_config.profile_summaries(include_hidden);
    let hidden_count = startup_config
        .profiles
        .values()
        .filter(|profile| profile.hidden)
        .count();
    let visible_count = startup_config.profiles.len() - hidden_count;
    let mut output = String::new();

    writeln!(
        &mut output,
        "startup_config_file: {}",
        startup_config_file.display()
    )
    .expect("writing to string should not fail");

    if summaries.is_empty() {
        if startup_config.profiles.is_empty() {
            writeln!(&mut output, "No startup profiles configured.")
                .expect("writing to string should not fail");
        } else if include_hidden {
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
        visible_count, hidden_count
    )
    .expect("writing to string should not fail");

    for profile in summaries {
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
        if let Some(description) = profile.description {
            writeln!(&mut output, "  description: {description}")
                .expect("writing to string should not fail");
        }
        if let Some(icon) = profile.icon {
            writeln!(&mut output, "  icon: {icon}").expect("writing to string should not fail");
        }
        if let Some(color) = profile.color {
            writeln!(&mut output, "  color: {color}").expect("writing to string should not fail");
        }
        writeln!(&mut output, "  tabs: {}", profile.tab_count)
            .expect("writing to string should not fail");
    }

    output
}

fn format_startup_layout(launch_options: &LaunchOptions, startup_config_file: &Path) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "tabs: {}",
        1 + launch_options.additional_tabs.len()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "new_terminal_tab:").expect("writing to string should not fail");
    format_startup_layout_tab_body(&mut output, "  ", &launch_options.new_terminal_tab);

    for (index, tab) in std::iter::once(&launch_options.initial_tab)
        .chain(launch_options.additional_tabs.iter())
        .enumerate()
    {
        format_startup_layout_tab(&mut output, index + 1, tab);
    }

    output
}

fn format_startup_layout_tab(output: &mut String, tab_number: usize, tab: &LaunchTab) {
    writeln!(output, "- tab {tab_number}").expect("writing to string should not fail");
    format_startup_layout_tab_body(output, "  ", tab);
}

fn format_startup_layout_tab_body(output: &mut String, prefix: &str, tab: &LaunchTab) {
    let kind = if tab.command.is_some() {
        "command"
    } else {
        "shell"
    };
    writeln!(output, "{prefix}kind: {kind}").expect("writing to string should not fail");
    writeln!(
        output,
        "{prefix}placement: {}",
        tab.split
            .map(|direction| format!("split {}", direction.as_str()))
            .unwrap_or_else(|| "tab".into())
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

    writeln!(output, "{prefix}env: {} variables", tab.env.len())
        .expect("writing to string should not fail");
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

fn format_startup_config_validation(
    startup_config_file: &Path,
    validation: &TerminalStartupConfigValidation,
) -> String {
    let mut output = String::new();
    writeln!(
        &mut output,
        "startup_config_file: {}",
        startup_config_file.display()
    )
    .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(&mut output, "layouts: {}", validation.layout_count)
        .expect("writing to string should not fail");
    writeln!(&mut output, "tabs: {}", validation.tab_count)
        .expect("writing to string should not fail");
    output
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

fn format_keymap_validation(keymap_file: &Path, validation: &TerminalKeymapValidation) -> String {
    let mut output = String::new();
    writeln!(&mut output, "keymap_file: {}", keymap_file.display())
        .expect("writing to string should not fail");
    writeln!(&mut output, "status: ok").expect("writing to string should not fail");
    writeln!(
        &mut output,
        "default_bindings: {}",
        validation.default_binding_count
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "user_keymap_source: {}",
        validation.user_keymap_source.as_str()
    )
    .expect("writing to string should not fail");
    writeln!(
        &mut output,
        "user_bindings: {}",
        validation.user_binding_count
    )
    .expect("writing to string should not fail");
    output
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

fn init(launch_options: LaunchOptions, cx: &mut App) -> Result<()> {
    component::init();
    menu::init();
    zed_actions::init();

    cx.on_action(|_: &zed_actions::Quit, cx| cx.quit());
    cx.on_action(open_settings_file);
    cx.on_action(open_startup_config_file);
    cx.on_action(open_startup_config_schema_file);
    cx.on_action(open_keymap_file);
    cx.on_action(open_config_directory);
    cx.on_action(open_logs_directory);
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
        TypeId::of::<OpenLogsDirectory>(),
        TypeId::of::<OpenStartupConfigFile>(),
        TypeId::of::<OpenStartupConfigSchemaFile>(),
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
        MenuItem::action("Open Config Directory", OpenConfigDirectory),
        MenuItem::action("Open Logs Directory", OpenLogsDirectory),
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

fn open_config_directory(_: &OpenConfigDirectory, cx: &mut App) {
    open_directory(paths::config_dir(), "config", cx);
}

fn open_logs_directory(_: &OpenLogsDirectory, cx: &mut App) {
    open_directory(paths::logs_dir(), "logs", cx);
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
        let TerminalCliCommand::PrintPaths(path_options) = command else {
            panic!("expected paths mode");
        };

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
        assert_command_palette_action_visible(&filter, &OpenStartupConfigFile);
        assert_command_palette_action_visible(&filter, &OpenStartupConfigSchemaFile);
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
    fn formats_startup_profile_list() {
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
        let config = TerminalStartupConfig {
            default_profile: Some("work".into()),
            profiles,
            ..TerminalStartupConfig::default()
        };

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
        let output = format_startup_config_validation(
            Path::new("terminal.json"),
            &TerminalStartupConfigValidation {
                layout_count: 2,
                tab_count: 4,
            },
        );

        assert_eq!(
            output,
            "startup_config_file: terminal.json\nstatus: ok\nlayouts: 2\ntabs: 4\n"
        );
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
    fn formats_config_initialization() {
        let output = format_config_initialization(&TerminalConfigInitialization {
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
        });

        assert_eq!(
            output,
            "status: ok\nsettings_file: created settings.json\nkeymap_file: existing keymap.json\n"
        );
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
    fn formats_doctor_report() {
        let output = format_doctor_report(&TerminalDoctorReport {
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
        });

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
    fn diagnose_terminal_config_files_reports_startup_config_schema_file() {
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
        let startup_config_file = config_dir.join("terminal.json");
        let startup_config_schema_file = config_dir.join("terminal.schema.json");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(&keymap_file, "custom keymap\n").expect("failed to write keymap");

        let initialization = initialize_terminal_config_files_at(TerminalConfigFilePaths {
            settings_file: settings_file.clone(),
            global_settings_file: global_settings_file.clone(),
            keymap_file: keymap_file.clone(),
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
    fn formats_keymap_validation() {
        let output = format_keymap_validation(
            Path::new("keymap.json"),
            &TerminalKeymapValidation {
                default_binding_count: 31,
                user_binding_count: 2,
                user_keymap_source: TerminalUserKeymapSource::File,
            },
        );

        assert_eq!(
            output,
            "keymap_file: keymap.json\nstatus: ok\ndefault_bindings: 31\nuser_keymap_source: file\nuser_bindings: 2\n"
        );
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

        let TerminalCliCommand::ValidateStartupConfig { startup_config, .. } = command else {
            panic!("expected startup config validation mode");
        };

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

        let TerminalCliCommand::PrintStartupLayout(options) = command else {
            panic!("expected startup layout printing mode");
        };

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

        let TerminalCliCommand::PrintStartupLayout(options) = command else {
            panic!("expected startup layout printing mode");
        };

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

        assert!(matches!(command, TerminalCliCommand::InitConfig { .. }));

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

        assert!(matches!(command, TerminalCliCommand::Doctor { .. }));

        std_fs::remove_dir_all(data_dir).ok();
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

        assert!(matches!(command, TerminalCliCommand::ValidateKeymap { .. }));
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
        } = command
        else {
            panic!("expected set default profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);
        assert_eq!(profile, "work");

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

        let TerminalCliCommand::ClearDefaultProfile { path_options } = command else {
            panic!("expected clear default profile mode");
        };

        assert_eq!(path_options.data_dir, data_dir);
        assert_eq!(path_options.config_dir, config_dir);

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
    fn non_launch_modes_reject_startup_title_arguments() {
        for mode in [
            "--init-config",
            "--doctor",
            "--validate-startup-config",
            "--print-startup-config-schema",
            "--validate-keymap",
            "--list-profiles",
            "--set-default-profile",
            "--clear-default-profile",
        ] {
            let mode_args = if mode == "--set-default-profile" {
                vec!["zed-terminal", mode, "work"]
            } else {
                vec!["zed-terminal", mode]
            };

            let args = if mode == "--set-default-profile" {
                vec!["zed-terminal", mode, "work", "--title", "Production"]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--title", "Production"]);
                args
            };
            assert_cli_conflict(&args, "initial title should conflict with non-launch modes");

            let args = if mode == "--set-default-profile" {
                vec!["zed-terminal", mode, "work", "--new-tab-title", "Logs"]
            } else {
                let mut args = mode_args.clone();
                args.extend(["--new-tab-title", "Logs"]);
                args
            };
            assert_cli_conflict(
                &args,
                "startup tab title should conflict with non-launch modes",
            );

            let args = if mode == "--set-default-profile" {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
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

            let args = if mode == "--set-default-profile" {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
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

            let args = if mode == "--set-default-profile" {
                vec![
                    "zed-terminal",
                    mode,
                    "work",
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

        assert!(matches!(command, TerminalCliCommand::PrintPaths(_)));
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
