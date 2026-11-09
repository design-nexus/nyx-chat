# Chat — notes for working on this repo

- GTK4 (gtk4-rs 0.11) + Rust, no libadwaita. Nyx Design 2.0 (`use_design_2`, see `STYLE.md`):
  one surface, cards without outlines, no lines. The theme, widgets, settings card and window
  frame come from the `nyx-*` crates.
  Every color is a `@theme_*` token or one of the derived mixes at the top of `style.css`
  (`@hairline`, `@nav_active`, …); Pango markup that bakes colors (code highlighting, inline
  code) is rebuilt on `hub::THEME`. The unified header (`chrome::app_header`) has the app with
  the chat or page as its subtitle and the sidebar toggle; on the chat page its model picker
  and capability tags in the middle and its actions at the end (the chat page hands them over
  with `window::set_chat_header`; they show only on it); then the chat search (it filters the
  sidebar's chat list), the temporary-chat toggle and Settings. The header's minimum must fit
  a half-screen tile, so the search keeps a small `width_chars` and caps its natural width. The model server (dot, models, switcher) sits at the sidebar's foot.
  No status bar. Numbers use tabular figures (`tnum`); mono is for code, commands and names.
- Settings is a card over the window (`nyx_ui::settings_card`, set up in `settings_dialog.rs`), not a page:
  `sections/settings.rs::build` fills one page with every group and the card shows one group
  at a time; `navigate("settings")` opens it. Groups are matched by name in its `GROUPS`.
- Pages have no title: the header's subtitle names the page (the chat's title on the chat page,
  `window::refresh_crumb`) and `Section::description` is the nav item's tooltip.
- Threads: all network/disk work runs on the tokio runtime in `rt.rs`. `rt::spawn` hands a
  result back to the GTK thread; `rt::stream` delivers a stream of events. Never block the
  GTK thread. `store::with` opens one SQLite connection per thread (WAL), so async code can
  use the store directly.
- Change notification: `hub.rs` (`THEME`, `MODELS`, `CHATS`, `PULLS`, `LIBRARY`, `HOST`).
  Subscribers are tied to a widget and dropped with it.
- Chat flow: `sections/chat/send.rs` stores the user message, creates one assistant row per
  model (compare mode shares `extra.group`), and runs `ollama::chat::run`, which loops
  stream → tool calls → results. Each round after tools is its own assistant row; tool
  results are `role = "tool"` rows. Knowledge lookups and pre-searches (for models without
  tools) are recorded in the reply's `extra.pre`. Messages form a tree (`parent`); a chat's
  `leaf` picks the branch shown.
- Streams outlive the view (`STREAMS` in `sections/chat/mod.rs`): switching chats doesn't stop
  a reply; the live widget is re-attached when the chat is shown again.
- Markdown (`markdown.rs`) keeps one widget per top-level block and only rebuilds blocks
  whose source changed, so streaming stays cheap.
- Don't wake the dGPU: `GSK_RENDERER=ngl` in `main.rs`; `hw.rs` only calls `nvidia-smi`
  while `runtime_status` isn't `suspended`, and caches VRAM in prefs.
- Control (`src/control.rs`): `nyx_core::control` on the app's bus name, for Nyx Assistant: `chat.list`, `search`, `read`, `open`, `send` (`sections::chat::send_text`).
- Local models: every llama.cpp host on this computer (any loopback address and port) is Nyx AI
  Core, through `llama/core.rs` and `nyx-ai-client`. AI Core owns model and service settings:
  Chat reads them (`presets::pull`) and sends only what the person edits; it never pushes its
  own defaults or unloads models to make room. Don't swap a missing model silently
  (`models::default_model`, `models::stand_in`).
- Checks: `cargo clippy -- -D warnings`, `cargo test` (the AI Core tests run against a fake
  socket; network tests: `cargo test -- --ignored` with `OLLAMA_URL` pointing at a server that
  has `all-minilm`).
- Visual check: a test copy on a private virtual monitor, with scratch
  `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME`:
  `NYX_CHAT_APP_ID=io.github.design_nexus.nyx.ChatTest nyx-chat --section models` (a separate
  instance, so the open copy is untouched). `CHAT_SNAPSHOT_ARENA="prompt"` plays an arena round.
- Layout: a wrapping label asks for its whole line as natural width, so sidebar notes cap
  `max_width_chars`. Long one-line text (model names, titles, paths) goes through `widgets::short_label`;
  drop-downs ellipsize via `widgets::ellipsize` (or `ellipsize_to`; it sets the expression
  before the factories, since setting it brings back GTK's default factory). The wide layout (sidebar
  expanded) must also fit a half-screen tile (~780 px): if it's wider, GTK keeps the window at
  its minimum, the width never drops under 980 and the window stays cut off.
  `CHAT_DEBUG_WIDE=1` forces the wide layout; extras that widen it go behind
  `window::on_roomy`, and side panels call `window::set_needs_room`.
  `CHAT_AUTOSEND="text"` sends a message in a normal (visible) window, for checking with grim.
- Never move a scroll adjustment inside its `changed` handler: that runs during layout and
  GTK drops the move, leaving content drawn at the old offset. Defer it (idle or tick).
