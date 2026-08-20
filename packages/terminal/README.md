# trowser_terminal

Browser-specific terminal projection and interactive application state for trowser.

The crate owns the `RenderedDocument` semantic projection, browser action identities, viewport state, prompts, search, tab presentation, and the `bmux_tui_runtime::Program` state machine. It adapts sanitized `trowser_render` pages into BMUX frame text, committed interaction regions, and protocol-neutral image contributions.

BMUX owns terminal input, presentation cadence, Crossterm lifecycle, image capability detection, Kitty/Sixel/iTerm2 encoding, image reconciliation, and graphics cleanup. Unsupported image hosts therefore retain trowser's semantic text, alt text, labels, links, and media placeholders.

Browser processes and daemon state remain behind `trowser_client` and IPC; this crate does not launch or control browser backends.
