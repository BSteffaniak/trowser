#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Command-line interface for trowser.

use anyhow::Result;
use clap::Parser;
use trowser_cli_schema::{Cli, Command, ServerCommand};
use trowser_config::{ConfigPaths, TrowserConfig};
use trowser_ipc::ServerStatus;
use trowser_server::TrowserServer;
use trowser_terminal_image::detect_from_env;

mod trace;

/// Runs the trowser CLI.
///
/// # Errors
///
/// Returns an error when config loading or command execution fails.
pub fn run() -> Result<u8> {
    trace::init_trace();
    let cli = Cli::parse();
    apply_runtime_override(cli.runtime.as_deref());
    apply_config_override(cli.config.as_ref());

    let paths = ConfigPaths::resolve();
    let config = TrowserConfig::load(&paths)?;

    match cli.command.unwrap_or(Command::Doctor) {
        Command::Open { url } => Ok(run_open(&url, cli.json)),
        Command::Server { command } => run_server(&command, &paths, cli.json),
        Command::Status => run_status(&paths, cli.json),
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

fn run_open(url: &str, json: bool) -> u8 {
    if json {
        println!(
            "{}",
            serde_json::json!({ "open": { "url": url, "implemented": false } })
        );
    } else {
        println!("open is not implemented yet: {url}");
    }
    0
}

fn run_server(command: &ServerCommand, paths: &ConfigPaths, json: bool) -> Result<u8> {
    let server = TrowserServer::new(paths.clone());
    match command {
        ServerCommand::Start { foreground } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "server": "start",
                        "foreground": foreground,
                        "implemented": false,
                        "socket": paths.server_socket_path(),
                    })
                );
            } else {
                println!(
                    "server start is not implemented yet (foreground: {foreground}, socket: {})",
                    paths.server_socket_path().display()
                );
            }
        }
        ServerCommand::Stop => print_not_implemented("server stop", json),
        ServerCommand::Status => print_status(&server.status(), json)?,
    }
    Ok(0)
}

fn run_status(paths: &ConfigPaths, json: bool) -> Result<u8> {
    let server = TrowserServer::new(paths.clone());
    print_status(&server.status(), json)?;
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
    if json {
        println!(
            "{}",
            serde_json::json!({
                "browser_backend": config.browser.backend.as_str(),
                "semantic_first": config.render.semantic_first,
                "terminal_images": image_caps,
            })
        );
    } else {
        println!("backend: {}", config.browser.backend);
        println!("semantic first: {}", config.render.semantic_first);
        println!("terminal images: {:?}", image_caps.protocols);
    }
    0
}

fn print_not_implemented(command: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({ "command": command, "implemented": false })
        );
    } else {
        println!("{command} is not implemented yet");
    }
}
