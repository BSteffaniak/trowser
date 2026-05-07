#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Command-line interface for trowser.

use std::fs::OpenOptions;
use std::io::{Write, stdout};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, read};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, size,
};
use trowser_browser_cdp::discover_browser_binary;
use trowser_cli_schema::{Cli, Command, ServerCommand};
use trowser_client::{ClientError, TrowserClient};
use trowser_config::{ConfigPaths, TROWSER_DAEMON_READY_FILE_ENV, TrowserConfig};
use trowser_ipc::{Request, Response, ServerStatus};
use trowser_server::TrowserServer;
use trowser_terminal::{TerminalRenderOptions, render_plain_text};
use trowser_terminal_image::detect_from_env;

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
        Command::Open { url, interactive } if interactive => {
            run_interactive(&paths, &config, Some(url)).await
        }
        Command::Open {
            url,
            interactive: _,
        } => run_open(&paths, &config, &url, cli.json).await,
        Command::Interactive => run_interactive(&paths, &config, None).await,
        Command::Server { command } => run_server(&command, &paths, cli.json).await,
        Command::Status => run_status(&paths, cli.json).await,
        Command::Follow { link_id } => run_follow(&paths, &config, link_id, cli.json).await,
        Command::Tabs => run_tabs(&paths, cli.json).await,
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
    let opened = client
        .send(&Request::Open {
            url: url.to_string(),
        })
        .await?;

    let Response::Opened { tab_id } = opened else {
        anyhow::bail!("unexpected open response: {opened:?}");
    };

    let snapshot = client.send(&Request::Snapshot { tab_id }).await?;
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
                image_capabilities: detect_from_env(),
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
    let followed = client.send(&Request::FollowLink { link_id }).await?;
    let Response::Opened { tab_id } = followed else {
        anyhow::bail!("unexpected follow response: {followed:?}");
    };

    let snapshot = client.send(&Request::Snapshot { tab_id }).await?;
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
                image_capabilities: detect_from_env(),
            },
        );
        print!("{rendered}");
    }

    Ok(0)
}

async fn run_interactive(
    paths: &ConfigPaths,
    config: &TrowserConfig,
    initial_url: Option<String>,
) -> Result<u8> {
    let client = ensure_server(paths).await?;
    let mut page = if let Some(url) = initial_url {
        open_and_snapshot(&client, &url).await?
    } else {
        active_snapshot_or_empty(&client).await
    };
    let mut rendered = render_page_lines(&page, config);
    let mut offset = 0usize;
    let mut message = String::from("q quit · j/k scroll · o open · f follow · r reload");
    let _guard = TerminalGuard::enter()?;

    loop {
        draw_viewport(&rendered, offset, &message)?;
        let Event::Key(key) = read()? else {
            continue;
        };
        match key {
            KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => break,
            KeyEvent {
                code: KeyCode::Char('j') | KeyCode::Down,
                ..
            } => {
                offset = offset
                    .saturating_add(1)
                    .min(rendered.len().saturating_sub(1));
            }
            KeyEvent {
                code: KeyCode::Char('k') | KeyCode::Up,
                ..
            } => offset = offset.saturating_sub(1),
            KeyEvent {
                code: KeyCode::Char('g'),
                ..
            } => offset = 0,
            KeyEvent {
                code: KeyCode::Char('G'),
                ..
            } => offset = rendered.len().saturating_sub(1),
            KeyEvent {
                code: KeyCode::Char('o'),
                ..
            } => {
                if let Some(url) = prompt_input("Open URL: ")? {
                    page = open_and_snapshot(&client, &url).await?;
                    rendered = render_page_lines(&page, config);
                    offset = 0;
                    message = format!("opened {url}");
                }
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                ..
            } => {
                if let Some(raw) = prompt_input("Follow link: ")?
                    && let Ok(link_id) = raw.trim().parse::<u32>()
                {
                    page = follow_and_snapshot(&client, link_id).await?;
                    rendered = render_page_lines(&page, config);
                    offset = 0;
                    message = format!("followed link {link_id}");
                }
            }
            KeyEvent {
                code: KeyCode::Char('r'),
                ..
            } => {
                page = reload_and_snapshot(&client).await?;
                rendered = render_page_lines(&page, config);
                offset = 0;
                message = "reloaded".to_string();
            }
            _ => {}
        }
    }

    Ok(0)
}

async fn open_and_snapshot(
    client: &TrowserClient,
    url: &str,
) -> Result<trowser_render::SemanticPage> {
    let opened = client
        .send(&Request::Open {
            url: url.to_string(),
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
    let followed = client.send(&Request::FollowLink { link_id }).await?;
    let Response::Opened { tab_id } = followed else {
        anyhow::bail!("unexpected follow response: {followed:?}");
    };
    snapshot_tab(client, tab_id).await
}

async fn reload_and_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    let reloaded = client.send(&Request::Reload).await?;
    let Response::Opened { tab_id } = reloaded else {
        anyhow::bail!("unexpected reload response: {reloaded:?}");
    };
    snapshot_tab(client, tab_id).await
}

async fn snapshot_tab(
    client: &TrowserClient,
    tab_id: trowser_session_models::TabId,
) -> Result<trowser_render::SemanticPage> {
    let snapshot = client.send(&Request::Snapshot { tab_id }).await?;
    let Response::Snapshot { page } = snapshot else {
        anyhow::bail!("unexpected snapshot response: {snapshot:?}");
    };
    Ok(page)
}

async fn active_snapshot_or_empty(client: &TrowserClient) -> trowser_render::SemanticPage {
    match client.send(&Request::ActiveSnapshot).await {
        Ok(Response::ActiveSnapshot { page, .. }) => page,
        _ => trowser_render::SemanticPage {
            url: String::new(),
            title: "trowser".to_string(),
            blocks: vec![trowser_render::SemanticBlock::Paragraph {
                text: "Press o to open a URL.".to_string(),
                layout: None,
                region: trowser_render::SemanticRegion::Main,
            }],
        },
    }
}

fn render_page_lines(page: &trowser_render::SemanticPage, config: &TrowserConfig) -> Vec<String> {
    render_plain_text(
        page,
        &TerminalRenderOptions {
            width: config.render.max_width,
            image_capabilities: detect_from_env(),
        },
    )
    .lines()
    .map(str::to_string)
    .collect()
}

fn draw_viewport(lines: &[String], offset: usize, message: &str) -> Result<()> {
    let (width, height) = size()?;
    let body_height = usize::from(height.saturating_sub(2));
    let mut out = stdout();
    execute!(out, MoveTo(0, 0), Clear(ClearType::All), Hide)?;
    execute!(out, Print("trowser"), MoveTo(0, 1))?;
    for (row, line) in lines.iter().skip(offset).take(body_height).enumerate() {
        execute!(
            out,
            MoveTo(0, u16::try_from(row + 1).unwrap_or(u16::MAX)),
            Print(truncate_for_terminal(line, width))
        )?;
    }
    execute!(
        out,
        MoveTo(0, height.saturating_sub(1)),
        Clear(ClearType::CurrentLine),
        Print(truncate_for_terminal(message, width))
    )?;
    out.flush()?;
    Ok(())
}

fn truncate_for_terminal(text: &str, width: u16) -> String {
    text.chars().take(usize::from(width)).collect()
}

fn prompt_input(prompt: &str) -> Result<Option<String>> {
    let (_, height) = size()?;
    let mut out = stdout();
    let mut input = String::new();
    loop {
        execute!(
            out,
            MoveTo(0, height.saturating_sub(1)),
            Clear(ClearType::CurrentLine),
            Print(prompt),
            Print(&input)
        )?;
        out.flush()?;
        let Event::Key(key) = read()? else {
            continue;
        };
        match key.code {
            KeyCode::Enter => return Ok(Some(input)),
            KeyCode::Esc => return Ok(None),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(ch) => input.push(ch),
            _ => {}
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, Hide)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), Show, LeaveAlternateScreen);
    }
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
        Err(error) if should_restart_daemon(&error) => {
            remove_stale_socket(paths)?;
            run_server_start_background(paths, false).await?;
            Ok(TrowserClient::new(paths.clone()))
        }
        Err(error) => Err(error.into()),
    }
}

fn should_restart_daemon(error: &ClientError) -> bool {
    match error {
        ClientError::Connect { .. } => true,
        ClientError::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
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
    let mut command = std::process::Command::new(exe);
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

async fn run_server_stop(paths: &ConfigPaths, json: bool) -> Result<()> {
    let client = TrowserClient::new(paths.clone());
    let response = client.send(&Request::Shutdown).await?;
    if response != Response::Ok {
        anyhow::bail!("unexpected shutdown response: {response:?}");
    }
    if json {
        println!("{}", serde_json::json!({ "server": "stopped" }));
    } else {
        println!("trowser daemon stopped");
    }
    Ok(())
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
            println!("{marker} {} {} {}", tab.id.as_str(), tab.title, tab.url);
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
            })
        );
    } else {
        println!("config: {}", paths.config_file().display());
        println!("runtime: {}", paths.runtime_dir().display());
        println!("data: {}", paths.data_dir().display());
        println!("state: {}", paths.state_dir().display());
        println!("logs: {}", paths.log_dir().display());
        println!("socket: {}", paths.server_socket_path().display());
    }
    0
}

fn run_doctor(config: &TrowserConfig, json: bool) -> u8 {
    let image_caps = detect_from_env();
    let browser = discover_browser_binary(config.browser.binary.as_ref());
    if json {
        println!(
            "{}",
            serde_json::json!({
                "browser_backend": config.browser.backend.as_str(),
                "semantic_first": config.render.semantic_first,
                "terminal_images": image_caps,
                "chromium_binary": browser.binary.as_ref(),
            })
        );
    } else {
        println!("backend: {}", config.browser.backend);
        println!("semantic first: {}", config.render.semantic_first);
        println!("terminal images: {:?}", image_caps.protocols);
        match &browser.binary {
            Some(path) => println!("chromium binary: {}", path.display()),
            None => println!("chromium binary: not found"),
        }
    }
    0
}
