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

Initial workspace scaffold. The browser backend and terminal UI are not implemented yet.

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See `AGENTS.md` for architecture and validation rules.
