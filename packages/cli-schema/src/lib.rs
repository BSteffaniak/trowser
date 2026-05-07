#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! CLI schema for trowser.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Top-level trowser CLI.
#[derive(Debug, Clone, Parser)]
#[command(name = "trowser", version)]
#[command(about = "Experimental daemon-backed terminal web browser")]
pub struct Cli {
    /// Override config file path.
    #[arg(long, env = "TROWSER_CONFIG")]
    pub config: Option<PathBuf>,
    /// Select an isolated runtime name.
    #[arg(long, env = "TROWSER_RUNTIME_NAME")]
    pub runtime: Option<String>,
    /// Output machine-readable JSON when supported.
    #[arg(long, global = true)]
    pub json: bool,
    /// Command to run. With no command, trowser opens the terminal UI placeholder.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level trowser commands.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Open a URL in trowser.
    Open {
        /// URL to open.
        url: String,
    },
    /// Manage the trowser daemon.
    Server {
        /// Server command.
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Print daemon/client status.
    Status,
    /// Print resolved paths.
    Paths,
    /// Print terminal capability information.
    Doctor,
}

/// Daemon commands.
#[derive(Debug, Clone, Subcommand)]
pub enum ServerCommand {
    /// Start the daemon.
    Start {
        /// Run in the foreground instead of daemonizing.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon.
    Stop,
    /// Print daemon status.
    Status,
}

/// Browser backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BrowserBackendArg {
    /// Chromium via Chrome `DevTools` Protocol.
    ChromiumCdp,
}
