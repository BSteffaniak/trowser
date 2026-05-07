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

Initial workspace scaffold plus a working local daemon/client IPC vertical slice:

```bash
# Start the daemon in the foreground
cargo run -p trowser_cli -- server start --foreground

# In another terminal, query the daemon
cargo run -p trowser_cli -- status

# Open a URL through the daemon and render the placeholder semantic page
cargo run -p trowser_cli -- open https://example.com

# Stop the daemon
cargo run -p trowser_cli -- server stop
```

`doctor` detects terminal image support and looks for a Chromium-compatible binary. Real CDP navigation/extraction is the next implementation step.

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See `AGENTS.md` for architecture and validation rules.
