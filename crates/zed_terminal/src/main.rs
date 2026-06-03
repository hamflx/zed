use std::{
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
use clap::{Parser, ValueHint};
use client::{Client, UserStore};
use collections::HashMap;
use fs::RealFs;
use futures::StreamExt;
use gpui::{
    Action, App, AppContext as _, Bounds, Context, KeyBinding, Menu, MenuItem, SharedString,
    SystemWindowTabController, TaskExt, Window, WindowBounds, WindowOptions, actions, px, size,
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
use terminal_view::{default_working_directory, terminal_panel::TerminalPanel};
use theme::{ActiveTheme, ThemeRegistry};
use theme_settings::load_user_theme;
use workspace::WorkspaceSettings;
use workspace::{AppState, Event as WorkspaceEvent, Workspace, WorkspaceStore};

actions!(
    zed_terminal,
    [
        OpenSettingsFile,
        OpenStartupConfigFile,
        OpenKeymapFile,
        OpenConfigDirectory,
        OpenLogsDirectory,
        NewTerminalTab
    ]
);

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Action)]
#[action(namespace = zed_terminal)]
#[serde(deny_unknown_fields)]
struct NewTerminalTabWithProfile {
    profile: String,
}

const WINDOW_WIDTH: f32 = 1100.0;
const WINDOW_HEIGHT: f32 = 720.0;
const APP_TITLE: &str = TERMINAL_APP_NAME;
const TERMINAL_APP_NAME: &str = "Zed Terminal";
const TERMINAL_APP_NAME_LOWERCASE: &str = "zed-terminal";
const TERMINAL_KEYMAP_PATH: &str = "keymaps/zed-terminal.json";
const TERMINAL_STARTUP_CONFIG_FILE: &str = "terminal.json";

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
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config"
        ]
    )]
    print_paths: bool,

    #[arg(
        long = "list-profiles",
        conflicts_with_all = [
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "new_tabs",
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
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config"
        ],
        help = "Include hidden startup profiles when listing profiles"
    )]
    all_profiles: bool,

    #[arg(
        long = "no-startup-config",
        conflicts_with_all = [
            "profile",
            "list_profiles",
            "validate_startup_config",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config"
        ]
    )]
    no_startup_config: bool,

    #[arg(
        long = "validate-startup-config",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "validate_keymap",
            "print_startup_config_schema",
            "init_config",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "new_tabs",
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
            "validate_startup_config",
            "validate_keymap",
            "init_config",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "new_tabs",
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
            "validate_startup_config",
            "print_startup_config_schema",
            "validate_keymap",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "new_tabs",
            "new_tab_commands",
            "command"
        ],
        help = "Create missing standalone config files without opening a terminal window"
    )]
    init_config: bool,

    #[arg(
        long = "validate-keymap",
        conflicts_with_all = [
            "print_paths",
            "list_profiles",
            "validate_startup_config",
            "print_startup_config_schema",
            "init_config",
            "no_startup_config",
            "profile",
            "working_directory",
            "directory",
            "new_tabs",
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
        long = "new-tab",
        visible_alias = "tab",
        value_name = "DIRECTORY",
        value_hint = ValueHint::DirPath
    )]
    new_tabs: Vec<PathBuf>,

    #[arg(
        long = "new-tab-command",
        visible_alias = "tab-command",
        value_name = "COMMAND",
        value_hint = ValueHint::CommandString,
        allow_hyphen_values = true
    )]
    new_tab_commands: Vec<String>,

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
    ValidateStartupConfig {
        path_options: TerminalPathOptions,
        startup_config: TerminalStartupConfig,
    },
    PrintStartupConfigSchema {
        path_options: TerminalPathOptions,
    },
    InitConfig {
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
    new_terminal_shell: Option<Shell>,
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
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    shell: Option<TerminalStartupShellConfig>,
    #[serde(default)]
    env: HashMap<String, String>,
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

impl TerminalCliCommand {
    fn from_cli_and_config_file(cli: Cli) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;
        let startup_config = if cli.print_paths
            || cli.no_startup_config
            || cli.validate_keymap
            || cli.print_startup_config_schema
            || cli.init_config
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
            Self::ValidateStartupConfig { path_options, .. } => path_options,
            Self::PrintStartupConfigSchema { path_options } => path_options,
            Self::InitConfig { path_options } => path_options,
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
        let mut initial_tab = startup_config
            .initial_tab(profile)
            .context("failed to resolve configured initial startup tab")?;
        if let Some(working_directory) = working_directory {
            initial_tab.working_directory = Some(working_directory);
        }
        if let Some(command) = command {
            initial_tab.command = Some(command);
            initial_tab.env = inherited_env.clone();
            initial_tab.shell = None;
        }
        let mut additional_tabs = startup_config
            .additional_tabs(profile)
            .context("failed to resolve configured startup tabs")?;
        additional_tabs.extend(LaunchTab::additional_from_cli(
            &cli.new_tabs,
            &cli.new_tab_commands,
            &inherited_env,
            inherited_shell.as_ref(),
        )?);

        Ok(Self {
            path_options,
            initial_tab,
            additional_tabs,
            new_terminal_shell: inherited_shell,
        })
    }

    fn startup_working_directories(&self) -> Vec<PathBuf> {
        let mut directories = Vec::new();
        for tab in std::iter::once(&self.initial_tab).chain(self.additional_tabs.iter()) {
            let Some(working_directory) = tab.working_directory.as_ref() else {
                continue;
            };
            if !directories.contains(working_directory) {
                directories.push(working_directory.clone());
            }
        }
        directories
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
        commands: &[String],
        inherited_env: &HashMap<String, String>,
        inherited_shell: Option<&Shell>,
    ) -> Result<Vec<Self>> {
        let mut tabs = Vec::with_capacity(directories.len() + commands.len());

        for directory in directories {
            tabs.push(Self {
                working_directory: Some(resolve_working_directory(directory).with_context(
                    || format!("failed to resolve startup tab {}", tabs.len() + 2),
                )?),
                command: None,
                env: HashMap::default(),
                title: None,
                shell: inherited_shell.cloned(),
            });
        }

        for command in commands {
            tabs.push(Self {
                working_directory: None,
                command: Some(
                    LaunchCommand::from_command_line(command).with_context(|| {
                        format!("failed to parse startup tab {}", tabs.len() + 2)
                    })?,
                ),
                env: inherited_env.clone(),
                title: None,
                shell: None,
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
        })
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

        let mut validation = Self::validate_layout(&TerminalStartupLayout {
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

            let profile_validation = Self::validate_layout(&TerminalStartupLayout {
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
            format!("initial tab for {}", layout.label),
        )?;

        for (index, tab) in layout.tabs.iter().enumerate() {
            LaunchTab::from_config(
                tab.working_directory.as_deref(),
                tab.command.as_deref(),
                layout.env,
                &tab.env,
                tab.title.as_deref(),
                shell.as_ref(),
                tab.shell.as_ref(),
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
                LaunchTab::from_config(
                    tab.working_directory.as_deref(),
                    tab.command.as_deref(),
                    layout.env,
                    &tab.env,
                    tab.title.as_deref(),
                    shell.as_ref(),
                    tab.shell.as_ref(),
                    format!("tab {} for {}", index + 2, layout.label),
                )
            })
            .collect()
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
        let profile = profile.trim();
        if profile.is_empty() {
            bail!("startup profile name is empty");
        }

        self.initial_tab(Some(profile))
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
        TerminalCliCommand::ValidateKeymap { .. } => run_keymap_validation(),
        TerminalCliCommand::Launch(launch_options) => launch_terminal(launch_options),
    }
}

fn run_keymap_validation() {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            match validate_keymaps(cx) {
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

fn print_startup_config_schema() -> Result<()> {
    print!("{}", format_startup_config_schema()?);
    Ok(())
}

fn print_config_initialization() -> Result<()> {
    let initialization = initialize_terminal_config_files()?;
    print!("{}", format_config_initialization(&initialization));
    Ok(())
}

fn initialize_terminal_config_files() -> Result<TerminalConfigInitialization> {
    initialize_terminal_config_files_at(TerminalConfigFilePaths {
        settings_file: paths::settings_file().clone(),
        global_settings_file: paths::global_settings_file().clone(),
        keymap_file: paths::keymap_file().clone(),
        startup_config_file: active_terminal_startup_config_file(),
    })
}

struct TerminalConfigFilePaths {
    settings_file: PathBuf,
    global_settings_file: PathBuf,
    keymap_file: PathBuf,
    startup_config_file: PathBuf,
}

fn initialize_terminal_config_files_at(
    file_paths: TerminalConfigFilePaths,
) -> Result<TerminalConfigInitialization> {
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

fn validate_keymaps(cx: &mut App) -> Result<TerminalKeymapValidation> {
    let default_binding_count =
        KeymapFile::load_asset(TERMINAL_KEYMAP_PATH, Some(KeybindSource::Default), cx)
            .context("failed to validate zed terminal default keymap")?
            .len();
    let (user_keymap_content, user_keymap_source) = read_user_keymap_content()?;
    let user_binding_count =
        load_keymap_content_for_validation("terminal keymap file", &user_keymap_content, cx)?;

    Ok(TerminalKeymapValidation {
        default_binding_count,
        user_binding_count,
        user_keymap_source,
    })
}

fn read_user_keymap_content() -> Result<(String, TerminalUserKeymapSource)> {
    let keymap_file = paths::keymap_file();
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

fn launch_tab_for_profile(profile: &str) -> Result<LaunchTab> {
    TerminalStartupConfig::load(&active_terminal_startup_config_file())?
        .profile_initial_tab(profile)
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

fn init(launch_options: LaunchOptions, cx: &mut App) -> Result<()> {
    component::init();
    menu::init();
    zed_actions::init();

    cx.on_action(|_: &zed_actions::Quit, cx| cx.quit());
    cx.on_action(open_settings_file);
    cx.on_action(open_startup_config_file);
    cx.on_action(open_keymap_file);
    cx.on_action(open_config_directory);
    cx.on_action(open_logs_directory);
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
    terminal_view::init(cx);

    open_terminal_window(app_state, launch_options, cx)?;
    cx.activate(true);
    Ok(())
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

fn set_app_menus(cx: &mut App) {
    let mut shell_items = vec![MenuItem::action("New Tab", NewTerminalTab)];
    let profile_entries = startup_profile_menu_entries();
    if !profile_entries.is_empty() {
        shell_items.push(MenuItem::submenu(Menu::new("New Tab With Profile").items(
            profile_entries.into_iter().map(|entry| {
                MenuItem::action(
                    entry.label,
                    NewTerminalTabWithProfile {
                        profile: entry.profile,
                    },
                )
            }),
        )));
    }
    shell_items.extend([
        MenuItem::action(
            "Close Tab",
            workspace::CloseActiveItem {
                close_pinned: false,
                save_intent: None,
            },
        ),
        MenuItem::separator(),
        MenuItem::action("Next Tab", workspace::ActivateNextItem::default()),
        MenuItem::action("Previous Tab", workspace::ActivatePreviousItem::default()),
    ]);

    cx.set_menus(vec![
        Menu::new("Zed Terminal").items(vec![
            MenuItem::action("Open Settings File", zed_actions::OpenSettingsFile),
            MenuItem::action("Open Startup Config File", OpenStartupConfigFile),
            MenuItem::action("Open Keymap File", zed_actions::OpenKeymapFile),
            MenuItem::action("Open Config Directory", OpenConfigDirectory),
            MenuItem::action("Open Logs Directory", OpenLogsDirectory),
            MenuItem::separator(),
            MenuItem::action("Quit", zed_actions::Quit),
        ]),
        Menu::new("Shell").items(shell_items),
        Menu::new("Terminal").items(vec![
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
        ]),
        Menu::new("Pane").items(vec![
            MenuItem::action("Split Right", workspace::SplitRight::default()),
            MenuItem::action("Split Down", workspace::SplitDown::default()),
        ]),
    ]);
}

fn build_window_options(_: Option<uuid::Uuid>, _: &mut App) -> WindowOptions {
    WindowOptions::default()
}

fn open_terminal_window(
    app_state: Arc<AppState>,
    launch_options: LaunchOptions,
    cx: &mut App,
) -> Result<()> {
    let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
    let bounds = Bounds::centered(None, window_size, cx);
    let startup_working_directories = launch_options.startup_working_directories();
    let new_terminal_working_directory = launch_options.initial_tab.working_directory.clone();
    let new_terminal_shell = launch_options.new_terminal_shell.clone();
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
                workspace.register_action(move |workspace, _: &NewTerminalTab, window, cx| {
                    let working_directory = new_terminal_working_directory
                        .clone()
                        .or_else(|| default_working_directory(workspace, cx));
                    let shell = new_terminal_shell.clone();
                    TerminalPanel::add_center_terminal(
                        workspace,
                        window,
                        cx,
                        move |project, cx| {
                            if let Some(shell) = shell {
                                project.create_terminal_shell_with_shell(
                                    working_directory,
                                    shell,
                                    cx,
                                )
                            } else {
                                project.create_terminal_shell(working_directory, cx)
                            }
                        },
                    )
                    .detach_and_log_err(cx);
                });
                let profile_project = project.clone();
                workspace.register_action(
                    move |workspace, action: &NewTerminalTabWithProfile, window, cx| {
                        match launch_tab_for_profile(&action.profile) {
                            Ok(tab) => {
                                if let Some(working_directory) = tab.working_directory.clone() {
                                    profile_project.update(cx, |project, cx| {
                                        project
                                            .find_or_create_worktree(&working_directory, true, cx)
                                            .detach_and_log_err(cx);
                                    });
                                }
                                add_launch_tab(workspace, window, cx, tab);
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
            workspace.update(cx, |workspace, cx| {
                add_launch_tab(workspace, window, cx, initial_tab);
                for tab in additional_tabs {
                    add_launch_tab(workspace, window, cx, tab);
                }
            });

            window.defer(cx, set_terminal_window_title);
            workspace
        },
    )
    .context("failed to open terminal window")?;

    Ok(())
}

fn add_launch_tab(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    tab: LaunchTab,
) {
    let working_directory = tab.working_directory;
    let command = tab.command;
    let env = tab.env;
    let title = tab.title;
    let shell = tab.shell;
    TerminalPanel::add_center_terminal_with_custom_title(
        workspace,
        window,
        cx,
        title,
        move |project, cx| {
            if let Some(command) = command {
                project.create_terminal_task(command.into_spawn_task(working_directory, env), cx)
            } else if let Some(shell) = shell {
                project.create_terminal_shell_with_shell(working_directory, shell, cx)
            } else {
                project.create_terminal_shell(working_directory, cx)
            }
        },
    )
    .detach_and_log_err(cx);
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
    fn initializes_missing_config_files_without_overwriting_existing_files() {
        let root_dir = temp_test_dir();
        let config_dir = root_dir.join("config");
        let settings_file = config_dir.join("settings.json");
        let global_settings_file = config_dir.join("global_settings.json");
        let keymap_file = config_dir.join("keymap.json");
        let startup_config_file = config_dir.join("terminal.json");
        std_fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std_fs::write(&keymap_file, "custom keymap\n").expect("failed to write keymap");

        let initialization = initialize_terminal_config_files_at(TerminalConfigFilePaths {
            settings_file: settings_file.clone(),
            global_settings_file: global_settings_file.clone(),
            keymap_file: keymap_file.clone(),
            startup_config_file: startup_config_file.clone(),
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
        assert!(options.additional_tabs.is_empty());

        std_fs::remove_dir_all(configured_dir).ok();
        std_fs::remove_dir_all(cli_dir).ok();
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
            options.new_terminal_shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
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
    fn profile_shell_is_selected_for_shell_tabs_and_new_terminal_tabs() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
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
            options.new_terminal_shell,
            Some(shell_with_args("pwsh.exe", &["-NoLogo"]))
        );
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
            options.new_terminal_shell,
            Some(Shell::Program("pwsh.exe".into()))
        );
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
        assert_eq!(options.new_terminal_shell, None);
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
                .contains("failed to resolve configured initial startup tab")
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
