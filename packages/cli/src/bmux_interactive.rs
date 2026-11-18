//! BMUX interactive runtime orchestration for the trowser CLI.

use std::io::stdout;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bmux_image::ImageConfig;
use bmux_tui::geometry::Rect;
use bmux_tui::terminal::Terminal;
use bmux_tui::terminal_io::{TerminalGuard, terminal_size};
use bmux_tui_runtime::{
    ImageTerminalPresenter, Runtime, RuntimeConfig, RuntimeError, TerminalInput,
};
use trowser_client::TrowserClient;
use trowser_config::TrowserConfig;
use trowser_ipc::{Request, Response, ViewportHint};
use trowser_terminal::application::BrowserApplicationRenderer;
use trowser_terminal::keymap::Keymap;
use trowser_terminal::layout::document_width;
use trowser_terminal::runtime::{
    BrowserAction, BrowserEffect, BrowserMessage, BrowserProgram, SCROLL_SYNC_DELAY,
};
use trowser_terminal::{RenderMode, TerminalRenderOptions, render_document_mode};

use crate::{
    activate_or_click_control, activate_or_follow_link, navigation_and_snapshot, snapshot_tab,
};

pub async fn run(
    client: TrowserClient,
    config: &TrowserConfig,
    initial_page: trowser_render::SemanticPage,
    initial_url: Option<String>,
) -> Result<u8> {
    let size = terminal_size()?;
    if size.width < 2 || size.height < 2 {
        anyhow::bail!(
            "interactive terminal is too small ({}x{}); resize it to at least 2x2 and retry",
            size.width,
            size.height
        );
    }
    // Validate key bindings before taking over the terminal.
    let keymap = Keymap::with_overrides(&config.keys)?;
    let document = render_document_mode(
        &initial_page,
        &TerminalRenderOptions {
            width: document_width(size.width),
        },
        RenderMode::default(),
    );
    let mut program = BrowserProgram::new(initial_page, document, size);
    program.set_keymap(keymap);
    program.set_effect(effect_factory(client.clone()));
    let startup =
        Some(initial_url.map_or(BrowserAction::LoadActiveSnapshot, BrowserAction::OpenUrl));

    let mut guard = TerminalGuard::enter(stdout())?;
    let capabilities = bmux_image::host_caps::detect_from_env();
    let writer = guard
        .writer_mut()
        .ok_or_else(|| anyhow::anyhow!("terminal writer unavailable"))?;
    let terminal = Terminal::new(writer, Rect::new(0, 0, size.width, size.height));
    let renderer = Arc::new(Mutex::new(BrowserApplicationRenderer::new()));
    let render_state = Arc::clone(&renderer);
    let presenter = ImageTerminalPresenter::with_commit(
        terminal,
        move |program: &mut BrowserProgram, frame: &mut bmux_tui::paint::PaintCx<'_, '_>| {
            render_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .render(program, frame);
        },
        |program: &mut BrowserProgram,
         hits: &bmux_tui::hit::HitMap,
         _: &bmux_tui::focus::FocusTrap| {
            BrowserApplicationRenderer::commit(program, hits);
        },
        capabilities,
        ImageConfig {
            enabled: config.render.images,
            ..ImageConfig::default()
        },
    );
    let (runtime, handle) = Runtime::new(program, presenter, RuntimeConfig::default());
    if let Some(action) = startup {
        handle
            .try_send(BrowserMessage::StartAction(action))
            .map_err(|_| anyhow::anyhow!("failed to queue initial browser navigation"))?;
    }
    let input = TerminalInput::start::<BrowserProgram>(handle, |error| BrowserMessage::Failed {
        message: error.to_string(),
        generation: u64::MAX,
    });
    // The runtime restores presentation state (images, pointer) before returning.
    let result = runtime.run().await;
    let mut input = input;
    input.shutdown().await;
    let presenter_error = match result {
        Ok(output) => {
            drop(output);
            None
        }
        Err(RuntimeError::Program {
            cleanup_error,
            output,
            ..
        }) => {
            drop(output);
            cleanup_error
        }
        Err(RuntimeError::Presenter {
            error,
            cleanup_error,
            output,
        }) => {
            drop(output);
            Some(cleanup_error.unwrap_or(error))
        }
    };
    let leave = guard.leave();
    if let Some(error) = presenter_error {
        return Err(error.into());
    }
    leave?;
    Ok(0)
}

fn effect_factory(client: TrowserClient) -> BrowserEffect {
    Arc::new(move |action, generation, size, mode| {
        let client = client.clone();
        if let BrowserAction::ScrollTo(y_px) = action {
            return scroll_sync_command(client, y_px, generation, size, mode);
        }
        bmux_tui_runtime::Command::replace(
            bmux_tui_runtime::CommandKey::new("browser.navigation"),
            async move {
                let result = execute_action(&client, action, size).await;
                Some(match result {
                    Ok(page) => {
                        let tabs = list_tabs(&client).await.unwrap_or_default();
                        let document = render_document_mode(
                            &page,
                            &TerminalRenderOptions {
                                width: document_width(size.width),
                            },
                            mode,
                        );
                        BrowserMessage::PageLoaded {
                            page: Box::new(page),
                            document: Box::new(document),
                            tabs,
                            generation,
                        }
                    }
                    Err(error) => BrowserMessage::Failed {
                        message: error.to_string(),
                        generation,
                    },
                })
            },
        )
    })
}

/// Debounced background scroll sync.
///
/// Each local scroll replaces the pending sync, so the daemon only scrolls and
/// re-extracts the page once scrolling pauses. Failures are dropped: the local
/// view is already correct and the next sync or navigation retries.
fn scroll_sync_command(
    client: TrowserClient,
    y_px: u32,
    generation: u64,
    size: bmux_tui::geometry::Size,
    mode: RenderMode,
) -> bmux_tui_runtime::Command<BrowserMessage> {
    bmux_tui_runtime::Command::replace(
        bmux_tui_runtime::CommandKey::new("browser.scroll-sync"),
        async move {
            tokio::time::sleep(SCROLL_SYNC_DELAY).await;
            let viewport = Some(viewport_for(size));
            let page = navigation_and_snapshot(&client, Request::ScrollTo { y_px, viewport })
                .await
                .ok()?;
            let document = render_document_mode(
                &page,
                &TerminalRenderOptions {
                    width: document_width(size.width),
                },
                mode,
            );
            Some(BrowserMessage::ScrollSynced {
                page: Box::new(page),
                document: Box::new(document),
                generation,
            })
        },
    )
}

fn viewport_for(size: bmux_tui::geometry::Size) -> ViewportHint {
    ViewportHint::from_terminal_cells(
        document_width(size.width),
        size.height.saturating_sub(2).max(1),
    )
}

async fn execute_action(
    client: &TrowserClient,
    action: BrowserAction,
    size: bmux_tui::geometry::Size,
) -> Result<trowser_render::SemanticPage> {
    let viewport = Some(viewport_for(size));
    match action {
        BrowserAction::LoadActiveSnapshot => active_snapshot_for(client, viewport).await,
        BrowserAction::OpenUrl(url) => {
            let response = client.send(&Request::Open { url, viewport }).await?;
            let Response::Opened { tab_id } = response else {
                anyhow::bail!("unexpected open response: {response:?}");
            };
            snapshot_tab(client, tab_id).await
        }
        BrowserAction::Search(_query) => active_snapshot_for(client, viewport).await,
        BrowserAction::Back => navigation_and_snapshot(client, Request::Back { viewport }).await,
        BrowserAction::Forward => {
            navigation_and_snapshot(client, Request::Forward { viewport }).await
        }
        BrowserAction::Reload => {
            navigation_and_snapshot(client, Request::Reload { viewport }).await
        }
        BrowserAction::FollowLink { link_id, action_id } => {
            activate_or_follow_link(client, link_id, action_id).await
        }
        BrowserAction::ClickControl {
            control_id,
            action_id,
        } => activate_or_click_control(client, control_id, action_id).await,
        BrowserAction::InputControl {
            control_id,
            action_id,
            text,
        } => crate::input_control_or_action(client, control_id, action_id, text).await,
        // Normally routed through `scroll_sync_command`; kept for completeness.
        BrowserAction::ScrollTo(y_px) => {
            navigation_and_snapshot(client, Request::ScrollTo { y_px, viewport }).await
        }
        BrowserAction::SwitchTab(tab_id) => {
            navigation_and_snapshot(client, Request::SwitchTab { tab_id }).await
        }
        BrowserAction::NextTab => navigation_and_snapshot(client, Request::NextTab).await,
        BrowserAction::PreviousTab => navigation_and_snapshot(client, Request::PreviousTab).await,
        BrowserAction::CloseTab => crate::close_active_tab_and_snapshot(client).await,
        BrowserAction::CloseAllTabs => {
            crate::close_all_tabs(client).await?;
            Ok(crate::empty_page("Press o to open a URL."))
        }
        BrowserAction::Activate(target) => {
            let _ = target;
            active_snapshot_for(client, viewport).await
        }
    }
}

async fn list_tabs(client: &TrowserClient) -> Result<Vec<trowser_session_models::TabSummary>> {
    match client.send(&Request::ListTabs).await? {
        Response::Tabs(snapshot) => Ok(snapshot.tabs),
        unexpected => anyhow::bail!("unexpected tabs response: {unexpected:?}"),
    }
}

async fn active_snapshot_for(
    client: &TrowserClient,
    viewport: Option<ViewportHint>,
) -> Result<trowser_render::SemanticPage> {
    match client.send(&Request::ActiveSnapshot { viewport }).await? {
        Response::ActiveSnapshot { page, .. } => Ok(page),
        unexpected => anyhow::bail!("unexpected active snapshot response: {unexpected:?}"),
    }
}
