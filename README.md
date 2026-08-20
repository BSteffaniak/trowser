# trowser

`trowser` is an experimental Rust terminal web browser.

The goal is a daemon-backed terminal browser that uses real browser engines for web compatibility while rendering readable terminal-native output first.

## MVP direction

- Daemon + terminal client architecture.
- Chromium/CDP backend first.
- Backend-agnostic shared browser model.
- Semantic/accessibility rendering before pixel rendering.
- Kitty/Sixel/iTerm2 images as optional enhancements with text fallback.
- macOS and Linux first, Windows best effort.

## Current status

Initial workspace scaffold plus a daemon/client IPC vertical slice, Chromium/CDP semantic extraction, and a BMUX-backed interactive terminal client:

```bash
# Open the interactive browser (the daemon is started automatically when needed)
cargo run -p trowser_cli -- interactive

# Open a URL directly in the interactive browser
cargo run -p trowser_cli -- open https://example.com

# Start the daemon explicitly in the foreground
cargo run -p trowser_cli -- server start --foreground

# In another terminal, query the daemon
cargo run -p trowser_cli -- status

# Stop the daemon
cargo run -p trowser_cli -- server stop
```

The interactive client uses `bmux_tui_runtime` for input, command scheduling, redraw, resize, and shutdown. Trowser projects sanitized browser semantics into BMUX text, interaction, and protocol-neutral image scenes. BMUX owns terminal capability detection, Kitty/Sixel/iTerm2 encoding, image reconciliation, cleanup, and the Crossterm terminal lifecycle. Unsupported graphics hosts continue to receive semantic text and media placeholders.

`doctor` reports BMUX terminal image capabilities and looks for a Chromium-compatible binary. CDP extraction currently launches a short-lived Chromium instance per navigation and uses DOM-based semantic extraction; accessibility-tree extraction and persistent browser process reuse are next.

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See `AGENTS.md` for architecture and validation rules.
