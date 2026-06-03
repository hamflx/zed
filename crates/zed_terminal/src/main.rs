use std::{
    collections::BTreeMap,
    env, fs as std_fs,
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
    App, AppContext as _, Bounds, Context, KeyBinding, Menu, MenuItem, SharedString,
    SystemWindowTabController, TaskExt, Window, WindowBounds, WindowOptions, actions, px, size,
};
use language::LanguageRegistry;
use node_runtime::NodeRuntime;
use project::Project;
use reqwest_client::ReqwestClient;
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

    #[arg(long = "paths")]
    print_paths: bool,

    #[arg(long = "no-startup-config", conflicts_with = "profile")]
    no_startup_config: bool,

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
struct LaunchOptions {
    path_options: TerminalPathOptions,
    print_paths: bool,
    initial_tab: LaunchTab,
    additional_tabs: Vec<LaunchTab>,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchCommand {
    program: String,
    args: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupConfig {
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    tabs: Vec<TerminalStartupTabConfig>,
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, TerminalStartupProfileConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupProfileConfig {
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    tabs: Vec<TerminalStartupTabConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalStartupTabConfig {
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

impl LaunchOptions {
    #[cfg(test)]
    fn from_cli(cli: Cli) -> Result<Self> {
        Self::from_cli_and_startup_config(cli, TerminalStartupConfig::default())
    }

    fn from_cli_and_config_file(cli: Cli) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;
        let startup_config = if cli.print_paths || cli.no_startup_config {
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
        let command = LaunchCommand::from_args(cli.command);
        let working_directory = cli
            .working_directory
            .or(cli.directory)
            .map(|directory| resolve_working_directory(&directory))
            .transpose()?;
        let startup_config = if cli.print_paths || cli.no_startup_config {
            TerminalStartupConfig::default()
        } else {
            startup_config
        };
        let profile = if cli.print_paths || cli.no_startup_config {
            None
        } else {
            cli.profile.as_deref()
        };
        let inherited_env = startup_config
            .inherited_env(profile)
            .context("failed to resolve configured startup environment")?;
        let mut initial_tab = startup_config
            .initial_tab(profile)
            .context("failed to resolve configured initial startup tab")?;
        if let Some(working_directory) = working_directory {
            initial_tab.working_directory = Some(working_directory);
        }
        if let Some(command) = command {
            initial_tab.command = Some(command);
            initial_tab.env = inherited_env.clone();
        }
        let mut additional_tabs = startup_config
            .additional_tabs(profile)
            .context("failed to resolve configured startup tabs")?;
        additional_tabs.extend(LaunchTab::additional_from_cli(
            &cli.new_tabs,
            &cli.new_tab_commands,
            &inherited_env,
        )?);

        Ok(Self {
            path_options,
            print_paths: cli.print_paths,
            initial_tab,
            additional_tabs,
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
    ) -> Result<Vec<Self>> {
        let mut tabs = Vec::with_capacity(directories.len() + commands.len());

        for directory in directories {
            tabs.push(Self {
                working_directory: Some(resolve_working_directory(directory).with_context(
                    || format!("failed to resolve startup tab {}", tabs.len() + 2),
                )?),
                command: None,
                env: HashMap::default(),
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
            });
        }

        Ok(tabs)
    }

    fn from_config(
        working_directory: Option<&Path>,
        command: Option<&str>,
        inherited_env: &HashMap<String, String>,
        tab_env: &HashMap<String, String>,
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
        let env = if command.is_some() {
            let mut env = inherited_env.clone();
            env.extend(tab_env.clone());
            env
        } else {
            HashMap::default()
        };

        Ok(Self {
            working_directory,
            command,
            env,
        })
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

    fn selected_layout(
        &self,
        requested_profile: Option<&str>,
    ) -> Result<TerminalStartupLayout<'_>> {
        let Some(profile_name) = requested_profile.or(self.default_profile.as_deref()) else {
            return Ok(TerminalStartupLayout {
                working_directory: self.working_directory.as_deref(),
                command: self.command.as_deref(),
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
                    self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
        })?;

        Ok(TerminalStartupLayout {
            working_directory: profile.working_directory.as_deref(),
            command: profile.command.as_deref(),
            env: &profile.env,
            tabs: &profile.tabs,
            label: format!("startup profile {profile_name:?}"),
        })
    }

    fn initial_tab(&self, requested_profile: Option<&str>) -> Result<LaunchTab> {
        let layout = self.selected_layout(requested_profile)?;
        LaunchTab::from_config(
            layout.working_directory,
            layout.command,
            layout.env,
            &HashMap::default(),
            format!("initial tab for {}", layout.label),
        )
    }

    fn inherited_env(&self, requested_profile: Option<&str>) -> Result<HashMap<String, String>> {
        Ok(self.selected_layout(requested_profile)?.env.clone())
    }

    fn additional_tabs(&self, requested_profile: Option<&str>) -> Result<Vec<LaunchTab>> {
        let layout = self.selected_layout(requested_profile)?;
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
                    format!("tab {} for {}", index + 2, layout.label),
                )
            })
            .collect()
    }
}

struct TerminalStartupLayout<'a> {
    working_directory: Option<&'a Path>,
    command: Option<&'a str>,
    env: &'a HashMap<String, String>,
    tabs: &'a [TerminalStartupTabConfig],
    label: String,
}

fn main() {
    let launch_options = match LaunchOptions::from_cli_and_config_file(Cli::parse()) {
        Ok(launch_options) => launch_options,
        Err(error) => {
            eprintln!("failed to launch zed terminal: {error:#}");
            process::exit(2);
        }
    };

    if let Err(error) = install_terminal_paths(&launch_options.path_options) {
        eprintln!("failed to launch zed terminal: {error:#}");
        process::exit(2);
    }

    if launch_options.print_paths {
        print_terminal_paths();
        return;
    }

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
        Menu::new("Shell").items(vec![
            MenuItem::action("New Tab", NewTerminalTab),
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
                    TerminalPanel::add_center_terminal(
                        workspace,
                        window,
                        cx,
                        move |project, cx| project.create_terminal_shell(working_directory, cx),
                    )
                    .detach_and_log_err(cx);
                });
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
    TerminalPanel::add_center_terminal(workspace, window, cx, move |project, cx| {
        if let Some(command) = command {
            project.create_terminal_task(command.into_spawn_task(working_directory, env), cx)
        } else {
            project.create_terminal_shell(working_directory, cx)
        }
    })
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
    let settings_path = paths::settings_file();
    if let Some(parent) = settings_path.parent() {
        std_fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir {parent:?}"))?;
    }

    if !settings_path.exists() {
        std_fs::write(
            settings_path,
            settings::initial_user_settings_content().as_ref(),
        )
        .with_context(|| format!("failed to create settings file {settings_path:?}"))?;
    }

    let global_settings_path = paths::global_settings_file();
    if !global_settings_path.exists() {
        std_fs::write(global_settings_path, "{}\n").with_context(|| {
            format!("failed to create global settings file {global_settings_path:?}")
        })?;
    }

    let keymap_path = paths::keymap_file();
    if !keymap_path.exists() {
        std_fs::write(keymap_path, settings::initial_keymap_content().as_ref())
            .with_context(|| format!("failed to create keymap file {keymap_path:?}"))?;
    }

    let startup_config_path = active_terminal_startup_config_file();
    if !startup_config_path.exists() {
        std_fs::write(
            &startup_config_path,
            initial_terminal_startup_config_content(),
        )
        .with_context(|| format!("failed to create startup config file {startup_config_path:?}"))?;
    }

    // Prime the abstract filesystem for platforms that use a non-std backend.
    if let Err(error) = cx.foreground_executor().block_on(fs.load(settings_path)) {
        log::warn!("failed to prime settings file {settings_path:?}: {error:?}");
    }
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
    if let Some(parent) = paths::settings_file().parent()
        && let Err(error) = std_fs::create_dir_all(parent)
    {
        log::warn!("failed to create settings directory {parent:?}: {error:?}");
        return false;
    }

    if !paths::settings_file().exists()
        && let Err(error) = std_fs::write(
            paths::settings_file(),
            settings::initial_user_settings_content().as_ref(),
        )
    {
        log::warn!(
            "failed to create settings file {:?}: {error:?}",
            paths::settings_file()
        );
        return false;
    }

    true
}

fn ensure_startup_config_file() -> bool {
    let startup_config_file = active_terminal_startup_config_file();
    if let Some(parent) = startup_config_file.parent()
        && let Err(error) = std_fs::create_dir_all(parent)
    {
        log::warn!("failed to create startup config directory {parent:?}: {error:?}");
        return false;
    }

    if !startup_config_file.exists()
        && let Err(error) = std_fs::write(
            &startup_config_file,
            initial_terminal_startup_config_content(),
        )
    {
        log::warn!(
            "failed to create startup config file {:?}: {error:?}",
            startup_config_file
        );
        return false;
    }

    true
}

fn ensure_keymap_file() -> bool {
    if let Some(parent) = paths::keymap_file().parent()
        && let Err(error) = std_fs::create_dir_all(parent)
    {
        log::warn!("failed to create keymap directory {parent:?}: {error:?}");
        return false;
    }

    if !paths::keymap_file().exists()
        && let Err(error) = std_fs::write(
            paths::keymap_file(),
            settings::initial_keymap_content().as_ref(),
        )
    {
        log::warn!(
            "failed to create keymap file {:?}: {error:?}",
            paths::keymap_file()
        );
        return false;
    }

    true
}

fn initial_terminal_startup_config_content() -> &'static str {
    r#"// Zed Terminal startup layout.
// Command strings use the same shell-like quoting rules as --new-tab-command.
// Environment variables apply to command-backed startup tabs only.
{
  "working_directory": null,
  "command": null,
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
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert!(options.print_paths);
        assert_eq!(options.path_options.data_dir, data_dir);
        assert_eq!(
            options.path_options.config_dir,
            options.path_options.data_dir.join("config")
        );
        std_fs::remove_dir_all(options.path_options.data_dir).ok();
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
    fn parses_initial_terminal_startup_config_content() {
        let config: TerminalStartupConfig =
            settings::parse_json_with_comments(initial_terminal_startup_config_content())
                .expect("initial terminal startup config should parse");

        assert_eq!(config, TerminalStartupConfig::default());
    }

    #[test]
    fn applies_configured_startup_tabs() {
        let initial_dir = temp_test_dir();
        let second_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(initial_dir.clone()),
            command: Some("cmd /C \"echo configured\"".into()),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(second_dir.clone()),
                command: Some("pwsh -NoLogo".into()),
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
        assert_eq!(options.additional_tabs.len(), 1);
        assert_tab_working_directory(&options.additional_tabs[0], &second_dir);
        assert_eq!(
            options.additional_tabs[0].command,
            Some(LaunchCommand {
                program: "pwsh".into(),
                args: vec!["-NoLogo".into()],
            })
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
    fn no_startup_config_ignores_configured_startup_tabs() {
        let configured_dir = temp_test_dir();
        let config = TerminalStartupConfig {
            working_directory: Some(configured_dir.clone()),
            command: Some("cmd /C configured".into()),
            tabs: vec![TerminalStartupTabConfig {
                working_directory: Some(configured_dir.clone()),
                command: Some("cmd /C tab".into()),
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
        let options =
            LaunchOptions::from_cli_and_config_file(cli).expect("failed to build launch options");

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
    fn profile_env_is_inherited_by_profile_command_tabs() {
        let profile_env = test_env(&[("ZED_TERMINAL_PROFILE", "work"), ("COMMON", "profile")]);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                command: Some("cmd /C profile".into()),
                env: profile_env.clone(),
                tabs: vec![TerminalStartupTabConfig {
                    command: Some("pwsh -NoLogo".into()),
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
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].env, options.initial_tab.env);
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
    }

    #[test]
    fn cli_additional_command_tabs_inherit_selected_profile_env() {
        let profile_env = test_env(&[("ZED_TERMINAL_PROFILE", "work")]);
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "work".into(),
            TerminalStartupProfileConfig {
                env: profile_env.clone(),
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
        assert_eq!(options.additional_tabs[0].env, profile_env);
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
        assert_eq!(options.additional_tabs.len(), 1);
        assert_eq!(options.additional_tabs[0].command, None);
        assert_eq!(options.additional_tabs[0].env, HashMap::default());
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
        let options = LaunchOptions::from_cli_and_startup_config(cli, config)
            .expect("paths mode should not resolve startup profiles");

        assert!(options.print_paths);
        assert_eq!(options.initial_tab.working_directory, None);
        assert_eq!(options.initial_tab.command, None);
        assert!(options.additional_tabs.is_empty());
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
