#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Command-line interface for trowser.

use std::fs::OpenOptions;
use std::future::Future;
use std::io::{ErrorKind, Write, stdout};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEventKind, poll, read,
};
use crossterm::style::Print;
use crossterm::terminal::{
    BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use crossterm::{execute, queue};
use trowser_browser_cdp::discover_browser_binary;
use trowser_cli_schema::{Cli, Command, ServerCommand};
use trowser_client::{ClientError, TrowserClient};
use trowser_config::{ConfigPaths, TROWSER_DAEMON_READY_FILE_ENV, TrowserConfig};
use trowser_ipc::{Request, Response, ServerStatus};
use trowser_server::TrowserServer;
use trowser_session_models::ActionId;
use trowser_terminal::{
    ControlHitboxAction, RenderedDocument, TerminalRenderOptions, render_document,
    render_plain_text,
};
use trowser_terminal_image::detect_from_env;

mod trace;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

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
        Command::Back => run_navigation(&paths, &config, Request::Back, cli.json).await,
        Command::Forward => run_navigation(&paths, &config, Request::Forward, cli.json).await,
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
        Command::Switch { tab_id } => {
            run_navigation(
                &paths,
                &config,
                Request::SwitchTab {
                    tab_id: trowser_session_models::TabId::new(tab_id),
                },
                cli.json,
            )
            .await
        }
        Command::Close { tab_id } => {
            run_maybe_navigation(
                &paths,
                &config,
                Request::CloseTab {
                    tab_id: trowser_session_models::TabId::new(tab_id),
                },
                cli.json,
            )
            .await
        }
        Command::NextTab => run_navigation(&paths, &config, Request::NextTab, cli.json).await,
        Command::PreviousTab => {
            run_navigation(&paths, &config, Request::PreviousTab, cli.json).await
        }
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
                image_capabilities: detect_from_env(),
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
    let mut rendered = render_page_document(&page, config);
    let mut tab_status = tabs_status(&client).await.unwrap_or_default();
    let mut offset = 0usize;
    let mut search_query = String::new();
    let mut message = String::from(
        "q quit · j/k scroll · o/t open tab · Tab switch · x close · f follow · i input · c click · b back · F forward · r reload",
    );
    let _guard = TerminalGuard::enter()?;

    loop {
        draw_viewport(&page, &rendered, offset, &message, &tab_status)?;
        match read()? {
            Event::Key(key) => match key {
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
                        .min(rendered.lines.len().saturating_sub(1));
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
                } => offset = rendered.lines.len().saturating_sub(1),
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    if let Some((_, target)) = focused_action(&rendered, offset) {
                        page = activate_click_target(&client, target).await?;
                        rendered = render_page_document(&page, config);
                        tab_status = tabs_status(&client).await.unwrap_or_default();
                        offset = 0;
                        message = "activated focused action".to_string();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('a'),
                    ..
                } => {
                    if let Some((row, _)) = next_action(&rendered, offset.saturating_add(1), true) {
                        offset = row;
                        message = "focused next action".to_string();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('A'),
                    ..
                } => {
                    if let Some((row, _)) = next_action(&rendered, offset.saturating_sub(1), false)
                    {
                        offset = row;
                        message = "focused previous action".to_string();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('/'),
                    ..
                } => {
                    if let Some(query) = prompt_input("Search: ")? {
                        search_query = query;
                        if let Some(found) = find_line(&rendered, &search_query, 0, true) {
                            offset = found;
                            message = format!("found: {search_query}");
                        } else {
                            message = format!("not found: {search_query}");
                        }
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('n'),
                    ..
                } => {
                    if let Some(found) =
                        find_line(&rendered, &search_query, offset.saturating_add(1), true)
                    {
                        offset = found;
                        message = format!("found: {search_query}");
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('N'),
                    ..
                } => {
                    if let Some(found) =
                        find_line(&rendered, &search_query, offset.saturating_sub(1), false)
                    {
                        offset = found;
                        message = format!("found: {search_query}");
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('o' | 't'),
                    ..
                } => {
                    if let Some(url) = prompt_input("Open URL: ")? {
                        let request_id = next_request_id();
                        let client_clone = client.clone();
                        let url_clone = url.clone();
                        if let Some(updated_page) = load_with_ui(
                            &client,
                            &format!("loading {url}"),
                            Some(request_id.clone()),
                            async move {
                                navigate_and_snapshot_with_request(
                                    &client_clone,
                                    request_id,
                                    url_clone,
                                )
                                .await
                            },
                        )
                        .await?
                        {
                            page = updated_page;
                            rendered = render_page_document(&page, config);
                            tab_status = tabs_status(&client).await.unwrap_or_default();
                            offset = 0;
                            message = format!("opened {url}");
                        } else {
                            message = "load canceled".to_string();
                        }
                    }
                }
                KeyEvent {
                    code: KeyCode::Tab, ..
                } => match next_tab_and_snapshot(&client).await {
                    Ok(updated_page) => {
                        page = updated_page;
                        rendered = render_page_document(&page, config);
                        tab_status = tabs_status(&client).await.unwrap_or_default();
                        offset = 0;
                        message = "next tab".to_string();
                    }
                    Err(error) => message = error.to_string(),
                },
                KeyEvent {
                    code: KeyCode::BackTab,
                    ..
                } => match previous_tab_and_snapshot(&client).await {
                    Ok(updated_page) => {
                        page = updated_page;
                        rendered = render_page_document(&page, config);
                        tab_status = tabs_status(&client).await.unwrap_or_default();
                        offset = 0;
                        message = "previous tab".to_string();
                    }
                    Err(error) => message = error.to_string(),
                },
                KeyEvent {
                    code: KeyCode::Char('x'),
                    ..
                } => match close_active_tab_and_snapshot(&client).await {
                    Ok(updated_page) => {
                        page = updated_page;
                        rendered = render_page_document(&page, config);
                        tab_status = tabs_status(&client).await.unwrap_or_default();
                        offset = 0;
                        message = "closed tab".to_string();
                    }
                    Err(error) => message = error.to_string(),
                },
                KeyEvent {
                    code: KeyCode::Char(ch),
                    ..
                } if ('1'..='9').contains(&ch) => {
                    let index = usize::try_from(ch.to_digit(10).unwrap_or(1)).unwrap_or(1);
                    match switch_tab_index_and_snapshot(&client, index).await {
                        Ok(updated_page) => {
                            page = updated_page;
                            rendered = render_page_document(&page, config);
                            tab_status = tabs_status(&client).await.unwrap_or_default();
                            offset = 0;
                            message = format!("switched to tab {index}");
                        }
                        Err(error) => message = error.to_string(),
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
                        rendered = render_page_document(&page, config);
                        offset = 0;
                        message = format!("followed link {link_id}");
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('i'),
                    ..
                } => {
                    if let Some(raw) = prompt_input("Input control: ")?
                        && let Ok(control_id) = raw.trim().parse::<u32>()
                        && let Some(text) = prompt_input("Text: ")?
                    {
                        page = input_and_snapshot(&client, control_id, text).await?;
                        rendered = render_page_document(&page, config);
                        offset = 0;
                        message = format!("input text into control {control_id}");
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::NONE,
                    ..
                } => {
                    if let Some(raw) = prompt_input("Click control: ")?
                        && let Ok(control_id) = raw.trim().parse::<u32>()
                    {
                        page = click_and_snapshot(&client, control_id).await?;
                        rendered = render_page_document(&page, config);
                        offset = 0;
                        message = format!("clicked control {control_id}");
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('b') | KeyCode::Left,
                    ..
                } => match back_and_snapshot(&client).await {
                    Ok(updated_page) => {
                        page = updated_page;
                        rendered = render_page_document(&page, config);
                        offset = 0;
                        message = "back".to_string();
                    }
                    Err(error) => message = error.to_string(),
                },
                KeyEvent {
                    code: KeyCode::Char('F') | KeyCode::Right,
                    ..
                } => match forward_and_snapshot(&client).await {
                    Ok(updated_page) => {
                        page = updated_page;
                        rendered = render_page_document(&page, config);
                        offset = 0;
                        message = "forward".to_string();
                    }
                    Err(error) => message = error.to_string(),
                },
                KeyEvent {
                    code: KeyCode::Char('r'),
                    ..
                } => {
                    page = reload_and_snapshot(&client).await?;
                    rendered = render_page_document(&page, config);
                    offset = 0;
                    message = "reloaded".to_string();
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => {
                    offset = offset
                        .saturating_add(3)
                        .min(rendered.lines.len().saturating_sub(1));
                }
                MouseEventKind::ScrollUp => offset = offset.saturating_sub(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = target_at(&rendered, offset, mouse.row, mouse.column) {
                        match target {
                            ClickTarget::Link { link_id, action_id } => {
                                page = activate_or_follow_link(&client, link_id, action_id).await?;
                                rendered = render_page_document(&page, config);
                                offset = 0;
                                message = format!("followed link {link_id}");
                            }
                            ClickTarget::Control {
                                control_id,
                                action_id,
                                action: ControlHitboxAction::Click,
                            } => {
                                page = activate_or_click_control(&client, control_id, action_id)
                                    .await?;
                                rendered = render_page_document(&page, config);
                                offset = 0;
                                message = format!("clicked control {control_id}");
                            }
                            ClickTarget::Control {
                                control_id,
                                action_id,
                                action: ControlHitboxAction::InputText,
                            } => {
                                if let Some(text) = prompt_input("Text: ")? {
                                    page = input_control_or_action(
                                        &client, control_id, action_id, text,
                                    )
                                    .await?;
                                    rendered = render_page_document(&page, config);
                                    offset = 0;
                                    message = format!("input text into control {control_id}");
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(0)
}

fn next_request_id() -> String {
    format!("cli-{}", REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed))
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

async fn navigate_and_snapshot_with_request(
    client: &TrowserClient,
    request_id: String,
    url: String,
) -> Result<trowser_render::SemanticPage> {
    let opened = client.send(&Request::Navigate { request_id, url }).await?;
    let Response::Opened { tab_id } = opened else {
        anyhow::bail!("unexpected navigate response: {opened:?}");
    };
    snapshot_tab(client, tab_id).await
}

async fn load_with_ui<F>(
    client: &TrowserClient,
    label: &str,
    request_id: Option<String>,
    future: F,
) -> Result<Option<trowser_render::SemanticPage>>
where
    F: Future<Output = Result<trowser_render::SemanticPage>> + Send + 'static,
{
    let handle = tokio::spawn(future);
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut frame = 0usize;
    while !handle.is_finished() {
        draw_loading_status(label, frames[frame % frames.len()])?;
        frame += 1;
        if poll(Duration::from_millis(80))?
            && let Event::Key(key) = read()?
            && key.code == KeyCode::Esc
        {
            if let Some(request_id) = request_id {
                let _ = client.send(&Request::Cancel { request_id }).await;
            }
            handle.abort();
            return Ok(None);
        }
    }
    Ok(Some(handle.await??))
}

fn draw_loading_status(label: &str, frame: &str) -> Result<()> {
    let (width, height) = size()?;
    let mut out = stdout();
    queue!(
        out,
        MoveTo(0, height.saturating_sub(1)),
        Clear(ClearType::CurrentLine),
        Print(truncate_for_terminal(
            &format!("{frame} {label} · Esc cancel"),
            width,
        ))
    )?;
    out.flush()?;
    Ok(())
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
    navigation_and_snapshot(client, Request::Reload).await
}

async fn back_and_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::Back).await
}

async fn forward_and_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::Forward).await
}

async fn next_tab_and_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::NextTab).await
}

async fn switch_tab_index_and_snapshot(
    client: &TrowserClient,
    one_based_index: usize,
) -> Result<trowser_render::SemanticPage> {
    let response = client.send(&Request::ListTabs).await?;
    let Response::Tabs(snapshot) = response else {
        anyhow::bail!("unexpected tabs response: {response:?}");
    };
    let tab_id = snapshot
        .tabs
        .get(one_based_index.saturating_sub(1))
        .map(|tab| tab.id.clone());
    let Some(tab_id) = tab_id else {
        anyhow::bail!("unknown tab index {one_based_index}");
    };
    navigation_and_snapshot(client, Request::SwitchTab { tab_id }).await
}

async fn previous_tab_and_snapshot(client: &TrowserClient) -> Result<trowser_render::SemanticPage> {
    navigation_and_snapshot(client, Request::PreviousTab).await
}

async fn close_active_tab_and_snapshot(
    client: &TrowserClient,
) -> Result<trowser_render::SemanticPage> {
    let active = client.send(&Request::ActiveSnapshot).await?;
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
        navigation_and_snapshot(client, Request::ActivateAction { action_id }).await
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
        navigation_and_snapshot(client, Request::ActivateAction { action_id }).await
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
        navigation_and_snapshot(client, Request::InputTextAction { action_id, text }).await
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
    let snapshot = client.send(&Request::Snapshot { tab_id }).await?;
    let Response::Snapshot { page } = snapshot else {
        anyhow::bail!("unexpected snapshot response: {snapshot:?}");
    };
    Ok(page)
}

async fn tabs_status(client: &TrowserClient) -> Result<String> {
    let response = client.send(&Request::ListTabs).await?;
    let Response::Tabs(snapshot) = response else {
        anyhow::bail!("unexpected tabs response: {response:?}");
    };
    if snapshot.tabs.is_empty() {
        return Ok("no tabs".to_string());
    }
    let active_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.active)
        .map_or(1, |index| index + 1);
    Ok(format!("tab {active_index}/{}", snapshot.tabs.len()))
}

async fn active_snapshot_or_empty(client: &TrowserClient) -> trowser_render::SemanticPage {
    match client.send(&Request::ActiveSnapshot).await {
        Ok(Response::ActiveSnapshot { page, .. }) => page,
        _ => empty_page("Press o to open a URL."),
    }
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
    }
}

fn render_page_document(
    page: &trowser_render::SemanticPage,
    config: &TrowserConfig,
) -> RenderedDocument {
    render_document(
        page,
        &TerminalRenderOptions {
            width: config.render.max_width,
            image_capabilities: detect_from_env(),
        },
    )
}

fn draw_viewport(
    page: &trowser_render::SemanticPage,
    document: &RenderedDocument,
    offset: usize,
    message: &str,
    tab_status: &str,
) -> Result<()> {
    let (width, height) = size()?;
    let body_height = height.saturating_sub(2);
    let mut out = stdout();
    let scroll = scroll_percent(document, offset);
    let title = if page.title.is_empty() {
        page.url.as_str()
    } else {
        page.title.as_str()
    };
    let header = format!("trowser | {tab_status} | {scroll}% | {title}");

    queue!(out, BeginSynchronizedUpdate, Hide)?;
    queue!(
        out,
        MoveTo(0, 0),
        Clear(ClearType::CurrentLine),
        Print(truncate_for_terminal(&header, width))
    )?;

    for row in 0..body_height {
        let line = document
            .lines
            .get(offset + usize::from(row))
            .map_or("", |line| line.text.as_str());
        queue!(
            out,
            MoveTo(0, row.saturating_add(1)),
            Clear(ClearType::CurrentLine),
            Print(truncate_for_terminal(line, width))
        )?;
    }

    queue!(
        out,
        MoveTo(0, height.saturating_sub(1)),
        Clear(ClearType::CurrentLine),
        Print(truncate_for_terminal(message, width)),
        EndSynchronizedUpdate
    )?;
    out.flush()?;
    Ok(())
}

fn scroll_percent(document: &RenderedDocument, offset: usize) -> u8 {
    let max_offset = document.lines.len().saturating_sub(1);
    if max_offset == 0 {
        return 0;
    }
    u8::try_from((offset.saturating_mul(100) / max_offset).min(100)).unwrap_or(100)
}

fn find_line(
    document: &RenderedDocument,
    query: &str,
    start: usize,
    forward: bool,
) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let needle = query.to_lowercase();
    if forward {
        return document
            .lines
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, line)| line.text.to_lowercase().contains(&needle))
            .map(|(index, _)| index);
    }
    document
        .lines
        .iter()
        .enumerate()
        .take(start.saturating_add(1))
        .rev()
        .find(|(_, line)| line.text.to_lowercase().contains(&needle))
        .map(|(index, _)| index)
}

#[derive(Debug, Clone)]
enum ClickTarget {
    Link {
        link_id: u32,
        action_id: Option<ActionId>,
    },
    Control {
        control_id: u32,
        action_id: Option<ActionId>,
        action: ControlHitboxAction,
    },
}

async fn activate_click_target(
    client: &TrowserClient,
    target: ClickTarget,
) -> Result<trowser_render::SemanticPage> {
    match target {
        ClickTarget::Link { link_id, action_id } => {
            activate_or_follow_link(client, link_id, action_id).await
        }
        ClickTarget::Control {
            control_id,
            action_id,
            action: ControlHitboxAction::Click,
        } => activate_or_click_control(client, control_id, action_id).await,
        ClickTarget::Control {
            control_id,
            action_id,
            action: ControlHitboxAction::InputText,
        } => {
            let text = prompt_input("Text: ")?.unwrap_or_default();
            input_control_or_action(client, control_id, action_id, text).await
        }
    }
}

fn focused_action(document: &RenderedDocument, offset: usize) -> Option<(usize, ClickTarget)> {
    next_action(document, offset, true)
}

fn next_action(
    document: &RenderedDocument,
    start: usize,
    forward: bool,
) -> Option<(usize, ClickTarget)> {
    let mut iter: Box<dyn Iterator<Item = (usize, &trowser_terminal::RenderedLine)>> = if forward {
        Box::new(document.lines.iter().enumerate().skip(start))
    } else {
        Box::new(
            document
                .lines
                .iter()
                .enumerate()
                .take(start.saturating_add(1))
                .rev(),
        )
    };
    iter.find_map(|(row, line)| line_target(line).map(|target| (row, target)))
}

fn line_target(line: &trowser_terminal::RenderedLine) -> Option<ClickTarget> {
    line.links
        .first()
        .map(|link| ClickTarget::Link {
            link_id: link.link_id,
            action_id: link.action_id.clone(),
        })
        .or_else(|| {
            line.controls.first().map(|control| ClickTarget::Control {
                control_id: control.control_id,
                action_id: control.action_id.clone(),
                action: control.action,
            })
        })
}

fn target_at(
    document: &RenderedDocument,
    offset: usize,
    screen_row: u16,
    column: u16,
) -> Option<ClickTarget> {
    let (_, height) = size().ok()?;
    if screen_row == 0 || screen_row >= height.saturating_sub(1) {
        return None;
    }
    let line_index = offset + usize::from(screen_row.saturating_sub(1));
    let line = document.lines.get(line_index)?;
    line.links
        .iter()
        .find(|link| link.contains(column))
        .map(|link| ClickTarget::Link {
            link_id: link.link_id,
            action_id: link.action_id.clone(),
        })
        .or_else(|| {
            line.controls
                .iter()
                .find(|control| control.contains(column))
                .map(|control| ClickTarget::Control {
                    control_id: control.control_id,
                    action_id: control.action_id.clone(),
                    action: control.action,
                })
        })
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
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide,
            Clear(ClearType::All)
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), Show, DisableMouseCapture, LeaveAlternateScreen);
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
