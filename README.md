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

Initial workspace scaffold plus a working local daemon/client IPC vertical slice and first-pass Chromium/CDP semantic extraction:

```bash
# Start the daemon in the foreground
cargo run -p trowser_cli -- server start --foreground

# In another terminal, query the daemon
cargo run -p trowser_cli -- status

# Open a URL through the daemon, launch Chromium/CDP, and render semantic page text
cargo run -p trowser_cli -- open https://example.com

# Stop the daemon
cargo run -p trowser_cli -- server stop
```

`doctor` detects terminal image support and looks for a Chromium-compatible binary. CDP extraction currently launches a short-lived Chromium instance per navigation and uses DOM-based semantic extraction; accessibility-tree extraction and persistent browser process reuse are next.

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See `AGENTS.md` for architecture and validation rules.
