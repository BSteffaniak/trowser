#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Command-line interface for trowser.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use trowser_browser_cdp::discover_browser_binary;
use trowser_cli_schema::{Cli, Command, ServerCommand};
use trowser_client::{ClientError, TrowserClient};
use trowser_config::{ConfigPaths, TrowserConfig};
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

    match cli.command.unwrap_or(Command::Doctor) {
        Command::Open { url } => run_open(&paths, &config, &url, cli.json).await,
        Command::Server { command } => run_server(&command, &paths, cli.json).await,
        Command::Status => run_status(&paths, cli.json).await,
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
        Err(ClientError::Connect { .. }) => {
            run_server_start_background(paths, false).await?;
            Ok(TrowserClient::new(paths.clone()))
        }
        Err(error) => Err(error.into()),
    }
}

async fn run_server_start_background(paths: &ConfigPaths, json: bool) -> Result<u8> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("server")
        .arg("start")
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _child = command.spawn()?;

    let client = TrowserClient::new(paths.clone());
    for _ in 0..50 {
        if client.send(&Request::Status).await.is_ok() {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "server": "started",
                        "socket": paths.server_socket_path(),
                    })
                );
            } else {
                println!("trowser daemon started");
            }
            return Ok(0);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    anyhow::bail!(
        "timed out waiting for trowser daemon at {}",
        paths.server_socket_path().display()
    )
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
