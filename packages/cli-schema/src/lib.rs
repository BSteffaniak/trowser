#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! CLI schema for trowser.

use clap::{Args, Parser, Subcommand, ValueEnum};
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
    /// Command to run. With no command, trowser opens interactive terminal browser mode.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level trowser commands.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Open a URL in interactive trowser mode by default.
    Open {
        /// URL to open.
        url: String,
        /// Render once to stdout and exit instead of entering interactive mode.
        #[arg(long)]
        non_interactive: bool,
    },
    /// Start interactive terminal browser mode.
    Interactive,
    /// Manage the trowser daemon.
    Server {
        /// Server command.
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Manage persisted trowser sessions.
    Session {
        /// Session command.
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Print daemon/client status.
    Status,
    /// Follow a numbered link from the active page.
    Follow {
        /// Link id to follow.
        link_id: u32,
    },
    /// Navigate the active tab backward.
    Back,
    /// Navigate the active tab forward.
    Forward,
    /// Click/activate a numbered form control.
    Click {
        /// Control id to click.
        control_id: u32,
    },
    /// Input text into a numbered form control.
    Input {
        /// Control id to fill.
        control_id: u32,
        /// Text to input.
        text: String,
    },
    /// Manage browser tabs.
    Tab {
        /// Tab command.
        #[command(subcommand)]
        command: TabCommand,
    },
    /// Switch to a tab.
    #[command(hide = true)]
    Switch {
        /// Tab id to activate.
        tab_id: String,
    },
    /// Close a tab.
    #[command(hide = true)]
    Close {
        /// Tab id to close.
        tab_id: String,
    },
    /// Switch to the next tab.
    #[command(hide = true)]
    NextTab,
    /// Switch to the previous tab.
    #[command(hide = true)]
    PreviousTab,
    /// List daemon tabs.
    #[command(hide = true)]
    Tabs,
    /// Print resolved paths.
    Paths,
    /// Print terminal capability information.
    Doctor,
}

/// Tab commands.
#[derive(Debug, Clone, Subcommand)]
pub enum TabCommand {
    /// List daemon tabs.
    List,
    /// Switch to a tab.
    Switch {
        /// Tab id to activate.
        tab_id: String,
    },
    /// Close a tab, or all tabs with `--all`.
    Close(TabCloseArgs),
    /// Close all tabs.
    CloseAll,
    /// Switch to the next tab.
    Next,
    /// Switch to the previous tab.
    Previous,
}

/// Arguments for closing tabs.
#[derive(Debug, Clone, Args)]
pub struct TabCloseArgs {
    /// Tab id to close. Omit when using `--all`.
    pub tab_id: Option<String>,
    /// Close all tabs.
    #[arg(long)]
    pub all: bool,
}

/// Session commands.
#[derive(Debug, Clone, Subcommand)]
pub enum SessionCommand {
    /// Save the current session snapshot.
    Save,
    /// Restore the persisted session snapshot.
    Restore,
    /// Clear the persisted session snapshot.
    Clear,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, TabCommand};

    #[test]
    fn parses_tab_list_command() {
        let cli = Cli::try_parse_from(["trowser", "tab", "list"]).expect("parse command");

        let Some(Command::Tab {
            command: TabCommand::List,
        }) = cli.command
        else {
            panic!("expected tab list command");
        };
    }

    #[test]
    fn parses_tab_close_all_flag() {
        let cli = Cli::try_parse_from(["trowser", "tab", "close", "--all"]).expect("parse command");

        let Some(Command::Tab {
            command: TabCommand::Close(args),
        }) = cli.command
        else {
            panic!("expected tab close command");
        };
        assert!(args.all);
        assert!(args.tab_id.is_none());
    }

    #[test]
    fn parses_tab_close_all_command() {
        let cli = Cli::try_parse_from(["trowser", "tab", "close-all"]).expect("parse command");

        let Some(Command::Tab {
            command: TabCommand::CloseAll,
        }) = cli.command
        else {
            panic!("expected tab close-all command");
        };
    }

    #[test]
    fn parses_legacy_tabs_command() {
        let cli = Cli::try_parse_from(["trowser", "tabs"]).expect("parse command");

        let Some(Command::Tabs) = cli.command else {
            panic!("expected legacy tabs command");
        };
    }
}
