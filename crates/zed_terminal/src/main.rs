use std::{
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
use fs::RealFs;
use futures::StreamExt;
use gpui::{
    App, AppContext as _, Bounds, KeyBinding, Menu, MenuItem, SharedString,
    SystemWindowTabController, TaskExt, Window, WindowBounds, WindowOptions, actions, px, size,
};
use language::LanguageRegistry;
use node_runtime::NodeRuntime;
use project::Project;
use reqwest_client::ReqwestClient;
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
    working_directory: Option<PathBuf>,
    command: Option<LaunchCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalPathOptions {
    data_dir: PathBuf,
    config_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchCommand {
    program: String,
    args: Vec<String>,
}

impl LaunchOptions {
    fn from_cli(cli: Cli) -> Result<Self> {
        let path_options =
            TerminalPathOptions::from_cli(cli.user_data_dir.as_deref(), cli.config_dir.as_deref())
                .context("failed to resolve terminal paths")?;
        let command = LaunchCommand::from_args(cli.command);
        let working_directory = cli
            .working_directory
            .or(cli.directory)
            .map(|directory| resolve_working_directory(&directory))
            .transpose()?;

        Ok(Self {
            path_options,
            print_paths: cli.print_paths,
            working_directory,
            command,
        })
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

impl LaunchCommand {
    fn from_args(args: Vec<String>) -> Option<Self> {
        let mut args = args.into_iter();
        let program = args.next()?;
        Some(Self {
            program,
            args: args.collect(),
        })
    }

    fn into_spawn_task(self, cwd: Option<PathBuf>) -> SpawnInTerminal {
        let label = self.display_label();

        SpawnInTerminal {
            id: TaskId(format!("zed-terminal:{label}")),
            full_label: label.clone(),
            label: label.clone(),
            command: Some(self.program),
            args: self.args,
            command_label: label,
            cwd,
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

fn main() {
    let launch_options = match LaunchOptions::from_cli(Cli::parse()) {
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

fn init(launch_options: LaunchOptions, cx: &mut App) -> Result<()> {
    component::init();
    menu::init();
    zed_actions::init();

    cx.on_action(|_: &zed_actions::Quit, cx| cx.quit());
    cx.on_action(open_settings_file);
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
    let initial_working_directory = launch_options.working_directory;
    let initial_command = launch_options.command;

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
            if let Some(working_directory) = initial_working_directory.clone() {
                project.update(cx, |project, cx| {
                    project
                        .find_or_create_worktree(&working_directory, true, cx)
                        .detach_and_log_err(cx);
                });
            }

            let new_terminal_working_directory = initial_working_directory.clone();
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

            let terminal_working_directory = initial_working_directory.clone();
            let terminal_command = initial_command.clone();
            workspace.update(cx, |workspace, cx| {
                TerminalPanel::add_center_terminal(workspace, window, cx, |project, cx| {
                    if let Some(command) = terminal_command {
                        project.create_terminal_task(
                            command.into_spawn_task(terminal_working_directory),
                            cx,
                        )
                    } else {
                        project.create_terminal_shell(terminal_working_directory, cx)
                    }
                })
                .detach_and_log_err(cx);
            });

            window.defer(cx, set_terminal_window_title);
            workspace
        },
    )
    .context("failed to open terminal window")?;

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir() -> PathBuf {
        let path = env::temp_dir().join(format!("zed-terminal-test-{}", uuid::Uuid::new_v4()));
        std_fs::create_dir_all(&path).expect("failed to create temp test directory");
        path
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
    fn parses_short_working_directory_flag() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from(["zed-terminal", "-d", dir.to_str().unwrap()])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.working_directory.as_deref(),
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

        assert_eq!(
            options.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_windows_terminal_starting_directory_alias() {
        let dir = temp_test_dir();
        let cli =
            Cli::try_parse_from(["zed-terminal", "--startingDirectory", dir.to_str().unwrap()])
                .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_positional_working_directory() {
        let dir = temp_test_dir();
        let cli = Cli::try_parse_from(["zed-terminal", dir.to_str().unwrap()])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(
            options.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        std_fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_startup_command_after_separator() {
        let cli = Cli::try_parse_from(["zed-terminal", "--", "echo", "hello"])
            .expect("failed to parse cli args");
        let options = LaunchOptions::from_cli(cli).expect("failed to build launch options");

        assert_eq!(options.working_directory, None);
        assert_eq!(
            options.command,
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

        assert_eq!(
            options.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        assert_eq!(
            options.command,
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

        assert_eq!(
            options.working_directory.as_deref(),
            Some(dunce::canonicalize(&dir).unwrap().as_path())
        );
        assert_eq!(
            options.command,
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

        let task = command.into_spawn_task(Some(dir.clone()));

        assert_eq!(task.id, TaskId("zed-terminal:cmd /C \"echo hello\"".into()));
        assert_eq!(task.full_label, "cmd /C \"echo hello\"");
        assert_eq!(task.label, "cmd /C \"echo hello\"");
        assert_eq!(task.command, Some("cmd".into()));
        assert_eq!(task.args, vec!["/C", "echo hello"]);
        assert_eq!(task.command_label, "cmd /C \"echo hello\"");
        assert_eq!(task.cwd.as_deref(), Some(dir.as_path()));
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
