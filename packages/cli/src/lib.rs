#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Command-line interface for trowser.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use trowser_browser_cdp::discover_browser_binary;
use trowser_cli_schema::{Cli, Command, ServerCommand, SessionCommand, TabCloseArgs, TabsCommand};
use trowser_client::{ClientError, TrowserClient};
use trowser_config::{ConfigPaths, TROWSER_DAEMON_READY_FILE_ENV, TrowserConfig};
use trowser_ipc::{Request, Response, ServerStatus, ViewportHint};
use trowser_server::TrowserServer;
use trowser_session_models::ActionId;
use trowser_terminal::{TerminalRenderOptions, render_plain_text};

mod bmux_interactive;
mod trace;

/// Runs the trowser CLI.
///
/// # Errors
///
/// Returns an error when config loading or command execution fails.
pub async fn run() -> Result<u8> {
    trace::init_trace();
    let cli = Cli::parse();
    apply_runtime_override(cli.runtime.as_deref());
    apply_config_override(cli.config.as_ref());

    let paths = ConfigPaths::resolve();
    let config = TrowserConfig::load(&paths)?;

    match cli.command.unwrap_or(Command::Interactive) {
        Command::Open {
            url,
            non_interactive,
        } if non_interactive || cli.json => run_open(&paths, &config, &url, cli.json).await,
        Command::Open {
            url,
            non_interactive: _,
        } => run_bmux_interactive(&paths, &config, Some(url)).await,
        Command::Interactive => run_bmux_interactive(&paths, &config, None).await,
        Command::Server { command } => run_server(&command, &paths, cli.json).await,
        Command::Session { command } => run_session(&command, &paths, cli.json).await,
        Command::Tabs { command } => run_tabs_command(&command, &paths, &config, cli.json).await,
        Command::Status => run_status(&paths, cli.json).await,
        Command::Follow { link_id } => run_follow(&paths, &config, link_id, cli.json).await,
        Command::Back => {
            run_navigation(
                &paths,
                &config,
                Request::Back {
                    viewport: Some(config_viewport_hint(&config)),
                },
                cli.json,
            )
            .await
        }
        Command::Forward => {
            run_navigation(
                &paths,
                &config,
                Request::Forward {
                    viewport: Some(config_viewport_hint(&config)),
                },
                cli.json,
            )
            .await
        }
        Command::Click { control_id } => {
            run_navigation(
                &paths,
                &config,
                Request::ClickControl { control_id },
                cli.json,
            )
            .await
        }
        Command::Input { control_id, text } => {
            run_navigation(
                &paths,
                &config,
                Request::InputText { control_id, text },
                cli.json,
            )
            .await
        }
        Command::Paths => Ok(run_paths(&paths, cli.json)),
        Command::Doctor => Ok(run_doctor(&config, cli.json)),
    }
}

fn apply_runtime_override(runtime: Option<&str>) {
    if let Some(runtime) = runtime
        && !runtime.trim().is_empty()
    {
        // SAFETY: CLI bootstrap before any threads are spawned.
        unsafe { std::env::set_var(trowser_config::TROWSER_RUNTIME_NAME_ENV, runtime) };
    }
}

fn apply_config_override(config: Option<&std::path::PathBuf>) {
    if let Some(config) = config {
        // SAFETY: CLI bootstrap before any threads are spawned.
        unsafe { std::env::set_var(trowser_config::TROWSER_CONFIG_ENV, config) };
    }
}

async fn run_open(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    url: &str,
    json: bool,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let viewport = config_viewport_hint(config);
    let opened = client
        .send(&Request::Open {
            url: url.to_string(),
            viewport: Some(viewport),
        })
        .await?;

    let Response::Opened { tab_id } = opened else {
        anyhow::bail!("unexpected open response: {opened:?}");
    };

    let snapshot = client
        .send(&Request::Snapshot {
            tab_id,
            viewport: Some(viewport),
        })
        .await?;
    let Response::Snapshot { page } = snapshot else {
        anyhow::bail!("unexpected snapshot response: {snapshot:?}");
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&page)?);
    } else {
        let rendered = render_plain_text(
            &page,
            &TerminalRenderOptions {
                width: config.render.max_width,
            },
        );
        print!("{rendered}");
    }

    Ok(0)
}

async fn run_follow(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    link_id: u32,
    json: bool,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let viewport = config_viewport_hint(config);
    let followed = client
        .send(&Request::FollowLink {
            link_id,
            viewport: Some(viewport),
        })
        .await?;
    let Response::Opened { tab_id } = followed else {
        anyhow::bail!("unexpected follow response: {followed:?}");
    };

    let snapshot = client
        .send(&Request::Snapshot {
            tab_id,
            viewport: Some(viewport),
        })
        .await?;
    let Response::Snapshot { page } = snapshot else {
        anyhow::bail!("unexpected snapshot response: {snapshot:?}");
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&page)?);
    } else {
        let rendered = render_plain_text(
            &page,
            &TerminalRenderOptions {
                width: config.render.max_width,
            },
        );
        print!("{rendered}");
    }

    Ok(0)
}

async fn run_navigation(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    request: Request,
    json: bool,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let page = navigation_and_snapshot(&client, request).await?;
    print_page(config, &page, json)?;
    Ok(0)
}

async fn run_maybe_navigation(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    request: Request,
    json: bool,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    match client.send(&request).await? {
        Response::Opened { tab_id } => {
            let page = snapshot_tab(&client, tab_id).await?;
            print_page(config, &page, json)?;
        }
        Response::Ok if json => println!("{}", serde_json::json!({ "ok": true })),
        Response::Ok => println!("ok"),
        Response::Error { message } => anyhow::bail!(message),
        unexpected => anyhow::bail!("unexpected navigation response: {unexpected:?}"),
    }
    Ok(0)
}

fn print_page(
    config: &TrowserConfig,
    page: &trowser_render::SemanticPage,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(page)?);
    } else {
        let rendered = render_plain_text(
            page,
            &TerminalRenderOptions {
                width: config.render.max_width,
            },
        );
        print!("{rendered}");
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "small crossterm input loop is clearer while interactive mode is still minimal"
)]
async fn run_bmux_interactive(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    initial_url: Option<String>,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let page = if let Some(url) = initial_url {
        open_and_snapshot(&client, &url).await?
    } else {
        active_snapshot_or_empty(&client).await
    };
    bmux_interactive::run(client, config, page).await
}

async fn open_and_snapshot(
    client: &TrowserClient,
    url: &str,
) -> Result<trowser_render::SemanticPage> {
    let opened = client
        .send(&Request::Open {
            url: url.to_string(),
            viewport: current_viewport_hint(),
        })
        .await?;
    let Response::Opened { tab_id } = opened else {
        anyhow::bail!("unexpected open response: {opened:?}");
    };
    snapshot_tab(client, tab_id).await
}

async fn follow_and_snapshot(
    client: &TrowserClient,
    link_id: u32,
) -> Result<trowser_render::SemanticPage> {
    let followed = client
        .send(&Request::FollowLink {
            link_id,
            viewport: current_viewport_hint(),
        })
        .await?;
    let Response::Opened { tab_id } = followed else {
        anyhow::bail!("unexpected follow response: {followed:?}");
    };
    snapshot_tab(client, tab_id).await
}

async fn close_active_tab_and_snapshot(
    client: &TrowserClient,
) -> Result<trowser_render::SemanticPage> {
    let active = client
        .send(&Request::ActiveSnapshot {
            viewport: current_viewport_hint(),
        })
        .await?;
    let Response::ActiveSnapshot { tab_id, .. } = active else {
        return Ok(empty_page("Press o to open a URL."));
    };
    match client.send(&Request::CloseTab { tab_id }).await? {
        Response::Opened { tab_id } => snapshot_tab(client, tab_id).await,
        Response::Ok => Ok(empty_page("Press o to open a URL.")),
        Response::Error { message } => anyhow::bail!(message),
        unexpected => anyhow::bail!("unexpected close response: {unexpected:?}"),
    }
}

async fn click_and_snapshot(
    client: &TrowserClient,
    control_id: u32,
) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::ClickControl { control_id }).await
}

async fn activate_or_follow_link(
    client: &TrowserClient,
    link_id: u32,
    action_id: Option<ActionId>,
) -> Result<trowser_render::SemanticPage> {
    if let Some(action_id) = action_id {
        navigation_and_snapshot(
            client,
            Request::ActivateAction {
                action_id,
                viewport: current_viewport_hint(),
            },
        )
        .await
    } else {
        follow_and_snapshot(client, link_id).await
    }
}

async fn activate_or_click_control(
    client: &TrowserClient,
    control_id: u32,
    action_id: Option<ActionId>,
) -> Result<trowser_render::SemanticPage> {
    if let Some(action_id) = action_id {
        navigation_and_snapshot(
            client,
            Request::ActivateAction {
                action_id,
                viewport: current_viewport_hint(),
            },
        )
        .await
    } else {
        click_and_snapshot(client, control_id).await
    }
}

async fn input_and_snapshot(
    client: &TrowserClient,
    control_id: u32,
    text: String,
) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::InputText { control_id, text }).await
}

async fn input_control_or_action(
    client: &TrowserClient,
    control_id: u32,
    action_id: Option<ActionId>,
    text: String,
) -> Result<trowser_render::SemanticPage> {
    if let Some(action_id) = action_id {
        navigation_and_snapshot(
            client,
            Request::InputTextAction {
                action_id,
                text,
                viewport: current_viewport_hint(),
            },
        )
        .await
    } else {
        input_and_snapshot(client, control_id, text).await
    }
}

async fn navigation_and_snapshot(
    client: &TrowserClient,
    request: Request,
) -> Result<trowser_render::SemanticPage> {
    let response = client.send(&request).await?;
    match response {
        Response::Opened { tab_id } => snapshot_tab(client, tab_id).await,
        Response::Error { message } => anyhow::bail!(message),
        unexpected => anyhow::bail!("unexpected navigation response: {unexpected:?}"),
    }
}

async fn snapshot_tab(
    client: &TrowserClient,
    tab_id: trowser_session_models::TabId,
) -> Result<trowser_render::SemanticPage> {
    let snapshot = client
        .send(&Request::Snapshot {
            tab_id,
            viewport: current_viewport_hint(),
        })
        .await?;
    let Response::Snapshot { page } = snapshot else {
        anyhow::bail!("unexpected snapshot response: {snapshot:?}");
    };
    Ok(page)
}

async fn active_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    match client
        .send(&Request::ActiveSnapshot {
            viewport: current_viewport_hint(),
        })
        .await?
    {
        Response::ActiveSnapshot { page, .. } => Ok(page),
        Response::Error { message } => anyhow::bail!(message),
        unexpected => anyhow::bail!("unexpected active snapshot response: {unexpected:?}"),
    }
}

async fn active_snapshot_or_empty(client: &TrowserClient) -> trowser_render::SemanticPage {
    active_snapshot(client)
        .await
        .unwrap_or_else(|_| empty_page("Press o to open a URL."))
}

fn empty_page(message: &str) -> trowser_render::SemanticPage {
    trowser_render::SemanticPage {
        url: String::new(),
        title: "trowser".to_string(),
        blocks: vec![trowser_render::SemanticBlock::Paragraph {
            text: message.to_string(),
            layout: None,
            region: trowser_render::SemanticRegion::Main,
        }],
        layout_tree: None,
        viewport: None,
        scroll_y_px: 0,
    }
}

fn current_viewport_hint() -> Option<ViewportHint> {
    bmux_tui::crossterm::terminal_size().ok().map(|size| {
        ViewportHint::from_terminal_cells(size.width, viewport_body_height(size.height))
    })
}

const fn config_viewport_hint(config: &TrowserConfig) -> ViewportHint {
    ViewportHint {
        width_cells: config.render.max_width,
        height_cells: 40,
    }
}

const fn viewport_body_height(height_cells: u16) -> u16 {
    let body_height = height_cells.saturating_sub(2);
    if body_height == 0 { 1 } else { body_height }
}

async fn run_session(command: &SessionCommand, paths: &ConfigPaths, json: bool) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let request = match command {
        SessionCommand::Save => Request::SaveSession,
        SessionCommand::Restore => Request::RestoreSession,
        SessionCommand::Clear => Request::ClearSession,
    };
    let response = client.send(&request).await?;
    match response {
        Response::Ok => {
            if json {
                println!("{}", serde_json::json!({ "ok": true }));
            } else {
                println!("ok");
            }
        }
        Response::Opened { tab_id } => {
            if json {
                println!("{}", serde_json::json!({ "active_tab": tab_id.as_str() }));
            } else {
                println!("restored session; active tab {}", tab_id.as_str());
            }
        }
        Response::Error { message } => anyhow::bail!(message),
        unexpected => anyhow::bail!("unexpected session response: {unexpected:?}"),
    }
    Ok(0)
}

async fn run_tabs_command(
    command: &TabsCommand,
    paths: &ConfigPaths,
    config: &TrowserConfig,
    json: bool,
) -> Result<u8> {
    match command {
        TabsCommand::List => run_tabs(paths, json).await,
        TabsCommand::Switch { tab_id } => {
            run_navigation(
                paths,
                config,
                Request::SwitchTab {
                    tab_id: trowser_session_models::TabId::new(tab_id.as_str()),
                },
                json,
            )
            .await
        }
        TabsCommand::Close(args) => run_tab_close(args, paths, config, json).await,
        TabsCommand::CloseAll => run_close_all_tabs(paths, json).await,
        TabsCommand::Next => run_navigation(paths, config, Request::NextTab, json).await,
        TabsCommand::Previous => run_navigation(paths, config, Request::PreviousTab, json).await,
    }
}

async fn run_tab_close(
    args: &TabCloseArgs,
    paths: &ConfigPaths,
    config: &TrowserConfig,
    json: bool,
) -> Result<u8> {
    match (args.all, args.tab_id.as_ref()) {
        (true, None) => run_close_all_tabs(paths, json).await,
        (true, Some(_)) => anyhow::bail!("use either a tab id or --all, not both"),
        (false, Some(tab_id)) => {
            run_maybe_navigation(
                paths,
                config,
                Request::CloseTab {
                    tab_id: trowser_session_models::TabId::new(tab_id.as_str()),
                },
                json,
            )
            .await
        }
        (false, None) => anyhow::bail!("missing tab id; pass a tab id or --all"),
    }
}

async fn run_close_all_tabs(paths: &ConfigPaths, json: bool) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let outcome = close_all_tabs(&client).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "closed_tabs": outcome.closed_tabs,
                "active_tab": outcome.active_tab.as_deref(),
            })
        );
    } else {
        let noun = if outcome.closed_tabs.len() == 1 {
            "tab"
        } else {
            "tabs"
        };
        println!("closed {} {noun}", outcome.closed_tabs.len());
    }

    Ok(0)
}

struct CloseAllTabsOutcome {
    closed_tabs: Vec<String>,
    active_tab: Option<String>,
}

async fn close_all_tabs(client: &TrowserClient) -> Result<CloseAllTabsOutcome> {
    let response = client.send(&Request::ListTabs).await?;
    let Response::Tabs(snapshot) = response else {
        anyhow::bail!("unexpected tabs response: {response:?}");
    };
    let mut closed_tabs = Vec::with_capacity(snapshot.tabs.len());

    for tab in snapshot.tabs {
        let tab_id = tab.id;
        let tab_id_text = tab_id.as_str().to_string();
        match client.send(&Request::CloseTab { tab_id }).await? {
            Response::Ok | Response::Opened { .. } => closed_tabs.push(tab_id_text),
            unexpected => anyhow::bail!("unexpected close response: {unexpected:?}"),
        }
    }

    let active_tab = current_active_tab(client).await?;
    Ok(CloseAllTabsOutcome {
        closed_tabs,
        active_tab,
    })
}

async fn current_active_tab(client: &TrowserClient) -> Result<Option<String>> {
    let response = client.send(&Request::ListTabs).await?;
    let Response::Tabs(snapshot) = response else {
        anyhow::bail!("unexpected tabs response: {response:?}");
    };
    Ok(snapshot
        .active_tab
        .map(|tab_id| tab_id.as_str().to_string()))
}

async fn run_server(command: &ServerCommand, paths: &ConfigPaths, json: bool) -> Result<u8> {
    let config = TrowserConfig::load(paths)?;
    let server = TrowserServer::new(paths.clone(), config);
    match command {
        ServerCommand::Start { foreground } => {
            if !foreground {
                return run_server_start_background(paths, json).await;
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "server": "start",
                        "foreground": true,
                        "socket": paths.server_socket_path(),
                    })
                );
            } else {
                println!(
                    "trowser daemon listening on {}",
                    paths.server_socket_path().display()
                );
            }
            server.run_foreground().await?;
        }
        ServerCommand::Stop => run_server_stop(paths, json).await?,
        ServerCommand::Status => {
            run_status(paths, json).await?;
        }
    }
    Ok(0)
}

async fn ensure_server(paths: &ConfigPaths) -> Result<TrowserClient> {
    let client = TrowserClient::new(paths.clone());
    match client.send(&Request::Status).await {
        Ok(_) => Ok(client),
        Err(error) if should_recover_daemon(&error) => {
            let allow_legacy_scan = !matches!(&error, ClientError::Connect { .. });
            let _ = terminate_daemon(paths, allow_legacy_scan).await?;
            remove_stale_runtime_files(paths)?;
            run_server_start_background(paths, false).await?;
            Ok(TrowserClient::new(paths.clone()))
        }
        Err(error) => Err(error.into()),
    }
}

fn should_recover_daemon(error: &ClientError) -> bool {
    match error {
        ClientError::Connect { .. } => true,
        ClientError::Io(error) => matches!(
            error.kind(),
            ErrorKind::UnexpectedEof
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
        ),
        ClientError::Encode(_)
        | ClientError::Server(_)
        | ClientError::TransportUnavailable { .. } => false,
    }
}

fn remove_stale_socket(paths: &ConfigPaths) -> Result<()> {
    let socket = paths.server_socket_path();
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    Ok(())
}

fn remove_stale_pid_file(paths: &ConfigPaths) -> Result<()> {
    let pid_file = paths.daemon_pid_path();
    if pid_file.exists() {
        std::fs::remove_file(pid_file)?;
    }
    Ok(())
}

fn remove_stale_runtime_files(paths: &ConfigPaths) -> Result<()> {
    remove_stale_socket(paths)?;
    remove_stale_pid_file(paths)
}

async fn run_server_start_background(paths: &ConfigPaths, json: bool) -> Result<u8> {
    remove_stale_socket(paths)?;
    std::fs::create_dir_all(paths.runtime_dir())?;
    std::fs::create_dir_all(paths.log_dir())?;

    let ready_file = paths.runtime_dir().join("daemon.ready");
    if ready_file.exists() {
        std::fs::remove_file(&ready_file)?;
    }

    let log_path = paths.log_dir().join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_for_stderr = log.try_clone()?;

    let exe = std::env::current_exe()?;
    let mut command = ProcessCommand::new(exe);
    command
        .arg("server")
        .arg("start")
        .arg("--foreground")
        .env(TROWSER_DAEMON_READY_FILE_ENV, &ready_file)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_for_stderr));
    let mut child = command.spawn()?;

    loop {
        if ready_file.exists() {
            let client = TrowserClient::new(paths.clone());
            if client.send(&Request::Status).await.is_ok() {
                report_server_started(paths, &log_path, json);
                return Ok(0);
            }
            if let Err(error) = child.kill() {
                eprintln!("failed to kill unhealthy trowser daemon: {error}");
            }
            anyhow::bail!(
                "trowser daemon wrote readiness file but failed health check; log: {}",
                log_path.display()
            );
        }

        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "trowser daemon exited during startup with status {status}; log: {}",
                log_path.display()
            );
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn report_server_started(paths: &ConfigPaths, log_path: &std::path::Path, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "server": "started",
                "socket": paths.server_socket_path(),
                "log": log_path,
            })
        );
    } else {
        println!("trowser daemon started");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOutcome {
    Stopped,
    NotRunning,
}

async fn run_server_stop(paths: &ConfigPaths, json: bool) -> Result<()> {
    let outcome = request_shutdown_or_terminate(paths).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "server": match outcome {
                    StopOutcome::Stopped => "stopped",
                    StopOutcome::NotRunning => "not_running",
                }
            })
        );
    } else {
        match outcome {
            StopOutcome::Stopped => println!("trowser daemon stopped"),
            StopOutcome::NotRunning => println!("trowser daemon is not running"),
        }
    }
    Ok(())
}

async fn request_shutdown_or_terminate(paths: &ConfigPaths) -> Result<StopOutcome> {
    let client = TrowserClient::new(paths.clone());
    match client.send(&Request::Shutdown).await {
        Ok(Response::Ok) => {
            if wait_for_daemon_shutdown(paths, Duration::from_secs(2)).await {
                remove_stale_runtime_files(paths)?;
                Ok(StopOutcome::Stopped)
            } else {
                terminate_daemon(paths, true).await
            }
        }
        Ok(response) => anyhow::bail!("unexpected shutdown response: {response:?}"),
        Err(error) if should_recover_daemon(&error) => terminate_daemon(paths, true).await,
        Err(error) => Err(error.into()),
    }
}

async fn wait_for_daemon_shutdown(paths: &ConfigPaths, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !paths.server_socket_path().exists() {
            return true;
        }

        let client = TrowserClient::new(paths.clone());
        if let Err(error) = client.send(&Request::Status).await
            && should_recover_daemon(&error)
        {
            return true;
        }

        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn terminate_daemon(paths: &ConfigPaths, allow_legacy_scan: bool) -> Result<StopOutcome> {
    if let Some(pid) = read_pid_file(paths)? {
        if process_exists(pid) {
            terminate_pid(pid).await?;
            remove_stale_runtime_files(paths)?;
            return Ok(StopOutcome::Stopped);
        }
        remove_stale_runtime_files(paths)?;
        return Ok(StopOutcome::NotRunning);
    }

    if !allow_legacy_scan {
        remove_stale_runtime_files(paths)?;
        return Ok(StopOutcome::NotRunning);
    }

    match legacy_daemon_pids()?[..] {
        [] => {
            remove_stale_runtime_files(paths)?;
            Ok(StopOutcome::NotRunning)
        }
        [pid] => {
            terminate_pid(pid).await?;
            remove_stale_runtime_files(paths)?;
            Ok(StopOutcome::Stopped)
        }
        ref pids => anyhow::bail!(
            "found multiple trowser daemon processes without a pid file: {pids:?}; stop one manually or use a runtime-specific pid file"
        ),
    }
}

fn read_pid_file(paths: &ConfigPaths) -> Result<Option<u32>> {
    let pid_file = paths.daemon_pid_path();
    if !pid_file.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&pid_file)?;
    if let Some(pid) = parse_pid(contents.trim()) {
        Ok(Some(pid))
    } else {
        std::fs::remove_file(pid_file)?;
        Ok(None)
    }
}

fn parse_pid(value: &str) -> Option<u32> {
    let pid = value.parse::<u32>().ok()?;
    (pid > 0).then_some(pid)
}

async fn terminate_pid(pid: u32) -> Result<()> {
    send_signal(pid, "TERM")?;
    if wait_for_pid_exit(pid, Duration::from_secs(3)).await {
        return Ok(());
    }

    send_signal(pid, "KILL")?;
    if wait_for_pid_exit(pid, Duration::from_secs(2)).await {
        return Ok(());
    }

    anyhow::bail!("failed to stop trowser daemon process {pid}")
}

fn send_signal(pid: u32, signal: &str) -> Result<()> {
    let status = ProcessCommand::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() || !process_exists(pid) {
        return Ok(());
    }
    anyhow::bail!("failed to send SIG{signal} to trowser daemon process {pid}")
}

async fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_exists(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn process_exists(pid: u32) -> bool {
    ProcessCommand::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn legacy_daemon_pids() -> Result<Vec<u32>> {
    let output = ProcessCommand::new("ps")
        .args(["-axo", "pid=,command="])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("failed to list processes while looking for a legacy trowser daemon");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(legacy_daemon_pids_from_ps(&stdout, std::process::id()))
}

fn legacy_daemon_pids_from_ps(output: &str, current_pid: u32) -> Vec<u32> {
    output
        .lines()
        .filter_map(parse_ps_line)
        .filter(|(pid, command)| *pid != current_pid && daemon_process_matches(command))
        .map(|(pid, _)| pid)
        .collect()
}

fn parse_ps_line(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim_start();
    let split_index = trimmed.find(char::is_whitespace)?;
    let (pid, command) = trimmed.split_at(split_index);
    Some((parse_pid(pid)?, command.trim_start()))
}

fn daemon_process_matches(command: &str) -> bool {
    let command = command.replace('\0', " ");
    command.contains("trowser")
        && command.contains(" server ")
        && command.contains(" start")
        && command.contains("--foreground")
}

async fn run_status(paths: &ConfigPaths, json: bool) -> Result<u8> {
    let client = TrowserClient::new(paths.clone());
    let response = client.send(&Request::Status).await?;
    let Response::Status(status) = response else {
        anyhow::bail!("unexpected status response: {response:?}");
    };
    print_status(&status, json)?;
    Ok(0)
}

async fn run_tabs(paths: &ConfigPaths, json: bool) -> Result<u8> {
    let client = TrowserClient::new(paths.clone());
    let response = client.send(&Request::ListTabs).await?;
    let Response::Tabs(snapshot) = response else {
        anyhow::bail!("unexpected tabs response: {response:?}");
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else if snapshot.tabs.is_empty() {
        println!("no tabs");
    } else {
        for tab in &snapshot.tabs {
            let marker = if tab.active { "*" } else { " " };
            let lifecycle = if tab.sleeping { " sleeping" } else { "" };
            println!(
                "{marker} {}{lifecycle} {} {}",
                tab.id.as_str(),
                tab.title,
                tab.url
            );
        }
    }

    Ok(0)
}

fn print_status(status: &ServerStatus, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
    } else {
        println!("trowser {} ({})", status.version, status.runtime_name);
    }
    Ok(())
}

fn run_paths(paths: &ConfigPaths, json: bool) -> u8 {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "config_dir": paths.config_dir(),
                "config_file": paths.config_file(),
                "runtime_dir": paths.runtime_dir(),
                "data_dir": paths.data_dir(),
                "state_dir": paths.state_dir(),
                "log_dir": paths.log_dir(),
                "server_socket": paths.server_socket_path(),
                "daemon_pid": paths.daemon_pid_path(),
            })
        );
    } else {
        println!("config: {}", paths.config_file().display());
        println!("runtime: {}", paths.runtime_dir().display());
        println!("data: {}", paths.data_dir().display());
        println!("state: {}", paths.state_dir().display());
        println!("logs: {}", paths.log_dir().display());
        println!("socket: {}", paths.server_socket_path().display());
        println!("pid: {}", paths.daemon_pid_path().display());
    }
    0
}

fn run_doctor(config: &TrowserConfig, json: bool) -> u8 {
    let image_caps = bmux_image::host_caps::detect_from_env();
    let image_protocols = format!("{image_caps:?}");
    let browser = discover_browser_binary(config.browser.binary.as_ref());
    if json {
        println!(
            "{}",
            serde_json::json!({
                "browser_backend": config.browser.backend.as_str(),
                "semantic_first": config.render.semantic_first,
                "terminal_images": image_protocols,
                "chromium_binary": browser.binary.as_ref(),
            })
        );
    } else {
        println!("backend: {}", config.browser.backend);
        println!("semantic first: {}", config.render.semantic_first);
        println!("terminal images: {image_protocols}");
        match &browser.binary {
            Some(path) => println!("chromium binary: {}", path.display()),
            None => println!("chromium binary: not found"),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::{daemon_process_matches, legacy_daemon_pids_from_ps, parse_pid, parse_ps_line};

    #[test]
    fn parses_valid_positive_pid() {
        assert_eq!(parse_pid("1234"), Some(1234));
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("not-a-pid"), None);
    }

    #[test]
    fn parses_ps_lines_with_commands() {
        assert_eq!(
            parse_ps_line("  42 /tmp/trowser server start --foreground"),
            Some((42, "/tmp/trowser server start --foreground"))
        );
        assert_eq!(parse_ps_line("not-a-pid /tmp/trowser"), None);
    }

    #[test]
    fn matches_only_foreground_trowser_daemons() {
        assert!(daemon_process_matches(
            "/repo/target/debug/trowser server start --foreground"
        ));
        assert!(daemon_process_matches(
            "/repo/target/debug/trowser\0server\0start\0--foreground"
        ));
        assert!(!daemon_process_matches(
            "/repo/target/debug/trowser server stop"
        ));
        assert!(!daemon_process_matches(
            "/usr/bin/other server start --foreground"
        ));
    }

    #[test]
    fn finds_legacy_daemon_pids_from_ps_output() {
        let output = "\
              10 /repo/target/debug/trowser server start --foreground\n\
              11 /repo/target/debug/trowser server stop\n\
              12 /usr/bin/other server start --foreground\n\
              13 /repo/target/debug/trowser server start --foreground\n";
        assert_eq!(legacy_daemon_pids_from_ps(output, 13), vec![10]);
    }
}
