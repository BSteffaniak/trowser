# AGENTS.md

Guidance for AI coding agents working on `trowser`.

`trowser` is a Rust terminal web browser with a daemon + terminal-client architecture. It should stay browser-backend agnostic, render readable semantic content first, and treat graphics/media as progressive enhancements.

## Core Architecture Boundaries (REQUIRED)

### Browser backend boundary

- Core crates must remain browser-backend agnostic.
- Chromium/CDP-specific code belongs only in `packages/browser-cdp/**`.
- Future Firefox/WebDriver BiDi code must live in a dedicated backend crate such as `packages/browser-bidi/**` or `packages/browser-firefox/**`.
- Shared browser traits, capabilities, and backend-neutral policy types belong in `packages/browser/**`.
- Do not expose CDP, WebDriver, Chromium, or Firefox protocol types from shared crates.
- Backend-specific crates may translate their native protocol into shared `trowser_browser`, `trowser_render`, and `trowser_ipc` types.

### Daemon/client boundary

- The daemon owns browser processes, browser contexts, tabs, cookies/session state, resource policy, and backend lifecycle.
- The terminal client owns terminal input, keybindings, viewport state, local rendering, and user interaction.
- The terminal client must communicate with the daemon through `packages/ipc/**` contracts and `packages/client/**` APIs.
- Do not make terminal rendering code launch or control browser processes directly.
- Do not make browser backend crates depend on terminal UI crates.

### IPC/protocol boundary

- IPC wire models belong in `packages/ipc/**` or domain-specific `packages/*/models/**` crates.
- Protocol evolution must be explicit: add versioning, capability flags, or backward-compatible enum variants.
- Serialization changes require tests that cover request/response round trips.
- Avoid leaking implementation-only structs into wire models.

### Rendering boundary

- Terminal rendering must consume backend-neutral render models from `packages/render/**`.
- Rendering priority is:
  1. Accessibility/semantic tree output.
  2. DOM-derived semantic fallback.
  3. Terminal-native images through Kitty/Sixel/iTerm2 when available.
  4. Screenshot/pixel/block rendering only as an optional fallback mode.
- Web content must never be emitted raw to the terminal. Strip or escape control characters and terminal escape sequences.
- Plain text terminal fallback must remain useful.

## Workspace / Dependency Rules

- Use the Rust workspace pattern under `packages/**`.
- Prefer reusing generic crates from `bmux` over creating local equivalents. For example, use `bmux_codec` for binary serde wire encoding instead of maintaining a `trowser_codec` fork/wrapper.
- If a `bmux_*` crate is truly domain-agnostic enough for trowser, depend on it directly from the bmux git repository and document why it is safe to share.
- Add new dependencies only to root `Cargo.toml` under `[workspace.dependencies]`.
- Package manifests must use `workspace = true`; do not inline versions in package `Cargo.toml` files.
- New external dependencies must specify a full patch version and usually `default-features = false`.
- Prefer small, feature-gated dependencies over always-on heavy stacks.
- Use `BTreeMap` and `BTreeSet`; do not use `HashMap` or `HashSet` unless there is a documented performance reason.
- Every crate must have a `fail-on-warnings` feature and propagate it to internal dependencies.
- Keep feature defaults minimal. Convenience features such as `all`, `media`, or `image-protocols` may aggregate optional functionality.

## Crate Pattern

- `packages/trowser` is the facade crate.
- `packages/cli` owns the binary entrypoint and command dispatch.
- `packages/cli-schema` owns clap parser types.
- `packages/config` owns config loading, paths, env overrides, and default policies.
- `packages/client` owns daemon client APIs.
- `packages/server` owns daemon runtime orchestration.
- `packages/ipc` owns IPC request/response contracts.
- Wire encoding should use shared `bmux_codec`; do not add a local codec crate unless trowser needs a genuinely different wire format.
- `packages/browser` owns backend-neutral browser abstractions.
- `packages/browser-cdp` owns Chromium/CDP implementation details.
- `packages/render` owns semantic render models and sanitization.
- `packages/terminal` owns terminal UI abstractions.
- `packages/terminal-image` owns terminal image protocol detection/selection.
- `packages/*/models` crates contain serde/wire/data types only.

`*_models` crates may contain:

- Structs, enums, type aliases.
- Serde derives and serde defaults.
- Simple parsing/conversion helpers for those types.

`*_models` crates must not contain:

- Browser process logic.
- IPC transport code.
- Terminal rendering logic.
- Service orchestration.
- Heavy dependencies.

## CLI Rules

- Keep `packages/cli/src/main.rs` tiny.
- Put parser structs/enums in `packages/cli-schema`.
- Put command dispatch in `packages/cli`.
- Prefer explicit subcommands over hidden behavior.
- Commands that output machine-readable data should support `--json` once the output shape stabilizes.

## Config / Path Rules

- Support platform defaults and env overrides.
- Use these env names consistently:
  - `TROWSER_CONFIG`
  - `TROWSER_CONFIG_DIR`
  - `TROWSER_RUNTIME_DIR`
  - `TROWSER_RUNTIME_NAME`
  - `TROWSER_DATA_DIR`
  - `TROWSER_STATE_DIR`
  - `TROWSER_LOG_DIR`
  - `TROWSER_TRACE`
- Config files should be TOML.
- Ephemeral session/cache files may be JSON.
- Config structs must use serde defaults for schema evolution.
- Cache/marker files should be treated as hints where possible, not hard requirements.

## Browser Backend / Memory Rules

- Do not implement a browser engine in `trowser`; delegate HTML/CSS/JS/networking/security to real browser engines.
- The daemon must own browser lifecycle and process cleanup.
- Memory efficiency should be achieved through policy:
  - Block heavy resources where configured.
  - Limit tabs/pages.
  - Suspend or close idle tabs.
  - Reuse browser processes when safe.
  - Avoid always-on video/image processing.
- Any feature that starts additional browser processes must document memory impact.
- CDP is the MVP backend, but shared APIs must not prevent WebDriver BiDi or custom backends later.

## Terminal / Media Rules

- Terminal graphics are optional progressive enhancements.
- Kitty/Sixel/iTerm2 support must degrade to text/alt/link fallback.
- Video and audio are post-MVP unless explicitly requested.
- Prefer external media handoff before attempting terminal-native video/audio.
- Terminal compatibility changes require smoke tests across plain terminals, tmux when possible, and image-capable terminals when available.

## Rust Style Guidelines

Every Rust crate should include:

```rust
#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
```

Additional style rules:

- Public fallible functions must include a `# Errors` rustdoc section.
- Add `#[must_use]` to constructors and getters that return values other than `Result` or `Option`.
- Do not add redundant `#[must_use]` to functions returning `Result` or `Option`.
- Avoid broad wildcard imports in production code.
- Prefer config structs over functions with many parameters or multiple bool arguments.
- Keep helper functions private unless they are part of the crate API.
- Fix clippy warnings at the root cause; do not add crate-level `allow` suppressions for new code.

## Profiling / Tracing

- Use the `profiling` crate for hot paths such as navigation, semantic extraction, IPC, and terminal rendering.
- Keep profiling zero-cost unless a tracing feature is enabled.
- The CLI should support `TROWSER_TRACE=1` for local timing diagnostics once tracing is wired.
- Do not add noisy always-on logs in raw terminal rendering paths.

## Required Validation After Code Changes

Run these commands after any non-doc code change:

1. `cargo fmt`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test`
4. `cargo machete --with-metadata` when available

If dependency/security configuration exists, also run:

- `cargo deny check`

Treat failures as blocking unless the user explicitly asks for a checkpoint before fixes are complete.

## Change-to-Tests Mapping

- IPC/protocol/codec changes:
  - Run serialization/round-trip tests.
  - Run all required validation commands.
- Terminal input/rendering/image changes:
  - Run terminal smoke tests when available.
  - Include tmux/image-protocol compatibility checks when relevant.
- Browser backend/CDP changes:
  - Run Chromium integration tests when available.
  - Include tests for backend capability translation and process cleanup.
- Config/path changes:
  - Run config load/path resolution tests.
- Docs-only changes:
  - No mandatory runtime commands, but keep examples accurate.

## Completion Reporting Format

When finishing a task, report validation exactly like this:

- `cargo fmt` - PASS/FAIL/SKIPPED
- `cargo clippy --all-targets -- -D warnings` - PASS/FAIL/SKIPPED
- `cargo test` - PASS/FAIL/SKIPPED
- `cargo machete --with-metadata` - PASS/FAIL/SKIPPED
- `cargo deny check` - PASS/FAIL/SKIPPED, if applicable

If a command is skipped, explain why.
