//! The main window: the header (the app and the chat or page, the sidebar toggle; on the
//! chat page the model picker in the middle and the chat's actions at the end; then the
//! chat search, the temporary-chat toggle and settings), a sidebar with the pages, the
//! chat list and the model server at its foot, and a stack of pages built the first time
//! they're shown.

use gtk::prelude::*;
use nyx_ui::chrome;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::sections::{self, Section};
use crate::{chatlist, hub, models, prefs, pulls, settings_dialog, theme, widgets};

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    nav: gtk::Box,
    nav_items: HashMap<&'static str, gtk::Button>,
    pages: HashMap<&'static str, gtk::Widget>,
    sections: Vec<Section>,
    current: &'static str,
    /// Pages visited before this one, for Alt+←.
    history: Vec<&'static str>,
    overlay: gtk::Overlay,
    search: gtk::SearchEntry,
    header: chrome::AppHeader,
    /// The chat page's parts of the header, shown only on it: the chat list button
    /// (narrow windows), the model picker, and the chat's actions.
    chat_parts: [gtk::Box; 3],
}

type NarrowHook = Box<dyn Fn(bool)>;

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NARROW_HOOKS: RefCell<Vec<NarrowHook>> = const { RefCell::new(Vec::new()) };
    /// The window is wide enough for extras (1200 px and up).
    static ROOMY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    static ROOMY_HOOKS: RefCell<Vec<NarrowHook>> = const { RefCell::new(Vec::new()) };
    /// A page has a side panel open and needs the sidebar's room.
    static NEEDS_ROOM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

pub fn current() -> &'static str {
    ui().map(|u| u.borrow().current).unwrap_or("")
}

pub fn narrow() -> bool {
    NARROW.with(|n| n.get())
}

/// Run `f` now and whenever the window crosses the compact width.
/// Run `f` now and whenever the window crosses 1200 px: extras that make the
/// wide layout wider (capability tags) show only above it.
pub fn on_roomy(f: impl Fn(bool) + 'static) {
    f(ROOMY.with(|r| r.get()));
    ROOMY_HOOKS.with(|h| h.borrow_mut().push(Box::new(f)));
}

/// A side panel opened or closed: under 1200 px, an open panel takes the
/// sidebar's labels, so the window still fits a half-screen tile.
pub fn set_needs_room(on: bool) {
    NEEDS_ROOM.with(|n| n.set(on));
    if let Some(w) = window() {
        relayout(&w);
    }
}

pub fn on_narrow(f: impl Fn(bool) + 'static) {
    f(narrow());
    NARROW_HOOKS.with(|h| h.borrow_mut().push(Box::new(f)));
}

/// Set up the app once (theme, icons, models) without showing a window.
pub fn init() {
    thread_local! { static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    theme::install();
    install_icons();
    // Temporary chats left behind by a crash.
    let _ = crate::store::purge_temporary();
    models::refresh();
    let p = prefs::get();
    if p.mcp_servers.iter().any(|s| s.enabled) {
        crate::rt::detach(crate::tools::mcp::restart(p.mcp_servers.clone()));
    }
    // Keep reachability fresh: a server that comes up later is noticed.
    glib::timeout_add_seconds_local(20, || {
        if models::online() != Some(true) {
            models::refresh();
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_seconds_local_once(8, models::daily_update_check);
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if let Some(ui) = ui() {
        let window = ui.borrow().window.clone();
        if let Some(s) = section {
            navigate(s);
        }
        window.present();
        return;
    }
    init();
    build(app);
    // The window always opens on the chat page, unless asked for another.
    let start = section.unwrap_or("chat").to_string();
    // Settings opens over the chat page.
    if start == "settings" {
        navigate("chat");
    }
    navigate(&start);
    if let Some(ui) = ui() {
        ui.borrow().window.present();
    }
    // Developer aid: CHAT_AUTOSEND="question" sends it a few seconds after
    // the window shows, for checking layout with a real compositor and grim.
    if let Ok(text) = std::env::var("CHAT_AUTOSEND") {
        glib::timeout_add_local_once(std::time::Duration::from_millis(3500), move || sections::chat::send_text(&text));
    }
}

/// A few symbolic icons icon themes may lack, and the app icon for
/// running before installation.
fn install_icons() {
    const ICONS: &[(&str, &str)] = &[
        ("chat-models-symbolic.svg", include_str!("../data/icons/chat-models-symbolic.svg")),
        ("chat-store-symbolic.svg", include_str!("../data/icons/chat-store-symbolic.svg")),
        ("chat-stats-symbolic.svg", include_str!("../data/icons/chat-stats-symbolic.svg")),
        ("chat-branch-symbolic.svg", include_str!("../data/icons/chat-branch-symbolic.svg")),
        ("io.github.design_nexus.nyx.Chat.svg", include_str!("../../../crates/nyx-icons/apps/io.github.design_nexus.nyx.Chat.svg")),
    ];
    let dir = crate::paths::cache_dir().join("icons");
    for (name, svg) in ICONS {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = crate::cmd::atomic_write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}

fn nav_button(icon: &str, title: &str, description: &str) -> (gtk::Button, gtk::Box) {
    let button = gtk::Button::new();
    button.add_css_class("nav-item");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(&gtk::Image::from_icon_name(icon));
    let l = widgets::label(title, "nav-label");
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.add_css_class("compact-hide");
    content.append(&l);
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(if description.is_empty() { title } else { description }));
    (button, content)
}

/// A group label's `+` button: its tooltip and what it does.
pub type NavAdd<'a> = Option<(&'a str, Box<dyn Fn()>)>;

/// A small UPPERCASE group label for the sidebar, with an optional flat `+`.
pub fn nav_group(title: &str, first: bool, add: NavAdd<'_>) -> gtk::Box {
    let row = widgets::hbox(4);
    row.add_css_class("nav-group");
    if first {
        row.add_css_class("first");
    }
    let l = widgets::label(&title.to_uppercase(), "nav-group-label");
    l.set_hexpand(true);
    l.add_css_class("compact-hide");
    row.append(&l);
    if let Some((tip, f)) = add {
        let b = widgets::icon_button("list-add-symbolic", tip);
        b.add_css_class("nav-add");
        b.add_css_class("compact-hide");
        b.connect_clicked(move |_| f());
        row.append(&b);
    }
    row
}

fn build(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder().application(app).title("Chat").default_width(1280).default_height(800).build();
    window.add_css_class("chat-window");
    // No client-side titlebar: Hyprland manages the window.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common::default());
    window.set_icon_name(Some("io.github.design_nexus.nyx.Chat"));

    let sections = sections::all();
    let search = search_entry();

    // ----- Sidebar -----
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut nav_items = HashMap::new();
    let mut last_group = "";
    for s in &sections {
        if s.group != last_group {
            list.append(&nav_group(s.group, last_group.is_empty(), None));
            last_group = s.group;
        }
        let (button, content) = nav_button(s.icon, s.title, s.description);
        if s.id == "models" {
            // Active downloads.
            let r = widgets::label("", "nav-readout");
            r.add_css_class("compact-hide");
            let r2 = r.clone();
            hub::on(&hub::PULLS, &r, move || {
                let n = pulls::active_count();
                r2.set_text(&if n > 0 { format!("↓{n}") } else { String::new() });
            });
            content.append(&r);
        }
        let id = s.id;
        button.connect_clicked(move |_| navigate(id));
        list.append(&button);
        nav_items.insert(s.id, button);
    }
    // The chats follow the pages, driven by the search at the top of the sidebar.
    let chats = chatlist::build(&search, false);
    chats.add_css_class("compact-hide");
    list.append(&chats);
    let nav_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    nav.append(&nav_scroll);
    nav.append(&server_foot());

    // ----- Content -----
    let stack = gtk::Stack::new();
    stack.add_css_class("settings-content");
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });

    // ----- Header -----
    // The app and the chat or page, the sidebar toggle; on the chat page its model
    // picker in the middle and its actions at the end; then the chat search, the
    // temporary-chat toggle and Settings.
    let header = chrome::app_header(&window, "chat-message-new-symbolic", "Chat");
    let collapse = widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);
    let chat_parts = [widgets::hbox(2), widgets::hbox(0), widgets::hbox(2)];
    header.start.append(&chat_parts[0]);
    header.center.append(&chat_parts[1]);
    header.end.set_spacing(4);
    chat_parts[2].set_margin_end(4);
    header.end.append(&chat_parts[2]);
    header.end.append(&search);
    header.end.append(&temp_toggle());
    header.end.append(&chrome::settings_button(settings_dialog::open));
    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_sidebar(&nav);
    frame.set_page(&stack);
    let overlay = frame.install(&window);

    install_keys(&window, &search);
    window.connect_close_request(|_| {
        sections::chat::discard_temporary();
        glib::Propagation::Proceed
    });

    // Narrow windows (a tiled half-screen) get an icon-only sidebar. Tiling
    // resizes the window without touching its default size, so an invisible
    // layer over the window reports every resize as it happens, and the
    // layout follows on the next frame instead of after a polling delay.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    let w2 = window.clone();
    probe.connect_resize(move |_, _, _| {
        // Changing the layout during allocation is ignored; do it right after.
        let w = w2.clone();
        glib::idle_add_local_once(move || relayout(&w));
    });
    window.connect_default_width_notify(relayout);
    window.connect_realize(relayout);
    // A slow fallback, in case a resize slips by.
    let w2 = window.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        relayout(&w2);
        glib::ControlFlow::Continue
    });

    let ui = Ui {
        window,
        stack,
        nav,
        nav_items,
        pages: HashMap::new(),
        sections,
        current: "",
        history: Vec::new(),
        overlay,
        search,
        header,
        chat_parts,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));
}

/// The search's natural width in characters, in a full window and a narrow one. Its
/// minimum stays small, so the header still fits a half-screen tile.
const SEARCH_WIDE: i32 = 16;
const SEARCH_NARROW: i32 = 14;

/// The chat search, always in the header; the chats it finds list in the sidebar.
fn search_entry() -> gtk::SearchEntry {
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search chats  (Ctrl+F)"));
    search.add_css_class("bar-search");
    search.set_width_chars(8);
    search.set_max_width_chars(SEARCH_WIDE);
    search.update_property(&[gtk::accessible::Property::Label("Search chats")]);
    search.connect_stop_search(|e| e.set_text(""));
    search
}

/// Temporary chat: not saved. Its toggle sits in the header.
fn temp_toggle() -> gtk::ToggleButton {
    let temp = gtk::ToggleButton::new();
    temp.set_icon_name("view-conceal-symbolic");
    temp.add_css_class("flat");
    temp.add_css_class("bar-button");
    temp.add_css_class("temp-toggle");
    temp.set_valign(gtk::Align::Center);
    temp.set_tooltip_text(Some("Temporary chat: not saved (Ctrl+Shift+N)"));
    temp.connect_toggled(|b| {
        if !TEMP_SYNC.with(|s| s.get()) {
            sections::chat::set_temporary(b.is_active());
        }
    });
    TEMP_TOGGLE.with(|t| *t.borrow_mut() = Some(temp.clone()));
    temp
}

thread_local! {
    static TEMP_TOGGLE: RefCell<Option<gtk::ToggleButton>> = const { RefCell::new(None) };
    /// Set while the toggle is updated from the chat, so it doesn't act.
    static TEMP_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Show whether the chat on screen is temporary.
pub fn set_temporary_toggle(on: bool) {
    TEMP_TOGGLE.with(|t| {
        if let Some(b) = t.borrow().as_ref()
            && b.is_active() != on
        {
            TEMP_SYNC.with(|s| s.set(true));
            b.set_active(on);
            TEMP_SYNC.with(|s| s.set(false));
        }
    });
}

/// Show the chat search. In the icon-only sidebar the results have nowhere to
/// go, so the chat page's history popover opens instead.
fn open_search() {
    let Some(ui) = ui() else { return };
    let compact = ui.borrow().nav.has_css_class("compact");
    if compact {
        navigate("chat");
        sections::chat::open_history();
        return;
    }
    let s = ui.borrow().search.clone();
    s.grab_focus();
}

/// The model server, at the sidebar's foot: its dot, what it has, and the switcher
/// when there are several. The icon-only sidebar keeps the dot.
fn server_foot() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Vertical, 2);
    bar.add_css_class("server-foot");
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.append(&line);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("status-dot");
    dot.set_valign(gtk::Align::Center);
    line.append(&dot);
    let readout = widgets::label("Connecting…", "server-readout");
    readout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    readout.set_max_width_chars(22);
    readout.set_hexpand(true);
    readout.set_xalign(0.0);
    readout.add_css_class("compact-hide");
    line.append(&readout);

    let hosts = gtk::DropDown::from_strings(&[]);
    widgets::ellipsize_to(&hosts, 18);
    hosts.set_halign(gtk::Align::Start);
    hosts.add_css_class("host-switcher");
    HOST_SWITCH.with(|h| *h.borrow_mut() = Some(hosts.clone()));
    hosts.set_tooltip_text(Some("Model server"));
    bar.append(&hosts);
    let fill_hosts = {
        let hosts = hosts.clone();
        move || {
            let p = prefs::get();
            let names: Vec<String> = p.hosts.iter().map(|h| h.name.clone()).collect();
            // Replacing the list while the dropdown is handling a click on it
            // crashes GTK, so the list is only touched when the names changed.
            let current: Vec<String> = hosts
                .model()
                .and_downcast::<gtk::StringList>()
                .map(|l| (0..l.n_items()).filter_map(|i| l.string(i).map(String::from)).collect())
                .unwrap_or_default();
            if current != names {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                hosts.set_model(Some(&gtk::StringList::new(&refs)));
            }
            let selected = p.active_host.min(names.len().saturating_sub(1)) as u32;
            if hosts.selected() != selected {
                hosts.set_selected(selected);
            }
            hosts.set_visible(names.len() > 1 && !NAV_COMPACT.with(|c| c.get()));
        }
    };
    fill_hosts();
    hosts.connect_selected_notify(|d| {
        let i = d.selected() as usize;
        if i != prefs::get().active_host && i < prefs::get().hosts.len() {
            // After GTK has finished with the click.
            glib::idle_add_local_once(move || switch_host(i));
        }
    });
    hub::on(&hub::HOST, &hosts, fill_hosts);

    let refresh = {
        let (dot, readout, hosts) = (dot.clone(), readout.clone(), hosts.clone());
        move || {
            let p = prefs::get();
            let host = p.host();
            // The state first, so a narrow foot still shows it; with several servers
            // the dropdown names the server.
            let name = if p.hosts.len() > 1 { String::new() } else { format!(" · {}", host.name) };
            dot.remove_css_class("on");
            dot.remove_css_class("off");
            let mut text = match models::online() {
                Some(true) => {
                    dot.add_css_class("on");
                    let n = models::installed().len();
                    format!("{n} model{}", if n == 1 { "" } else { "s" })
                }
                Some(false) => {
                    dot.add_css_class("off");
                    "Offline".to_string()
                }
                None => "Connecting…".to_string(),
            };
            let downloads = pulls::active_count();
            if downloads > 0 {
                text.push_str(&format!(" · ↓{downloads}"));
            }
            text.push_str(&name);
            readout.set_text(&text);
            let url = crate::prefs::normalize_host(&host.url, host.backend);
            readout.set_tooltip_text(Some(&url));
            hosts.set_tooltip_text(Some(&url));
        }
    };
    refresh();
    let r = refresh.clone();
    hub::on(&hub::MODELS, &readout, r);
    let r = refresh.clone();
    hub::on(&hub::PULLS, &dot, r);
    hub::on(&hub::HOST, &bar, refresh);
    bar
}

thread_local! {
    /// The sidebar shows only icons.
    static NAV_COMPACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The model-server switcher at the sidebar's foot.
    static HOST_SWITCH: RefCell<Option<gtk::DropDown>> = const { RefCell::new(None) };
}

/// Every keyboard shortcut, for the F1 list and Settings → Keyboard.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Enter"], "Send (Shift+Enter for a new line; swappable in Settings)"),
    (&["Esc"], "Stop the reply, clear the search, or close"),
    (&["Ctrl", "N"], "New chat"),
    (&["Ctrl", "Shift", "N"], "Temporary chat, not saved"),
    (&["Ctrl", "F"], "Search chats"),
    (&["Ctrl", "Space"], "Dictate"),
    (&["Ctrl", "Shift", "S"], "Screenshot into the chat"),
    (&["Ctrl", "1–7"], "Go to a page in the sidebar"),
    (&["Alt", "←"], "Back"),
    (&["Ctrl", "B"], "Collapse or expand the sidebar"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Show these shortcuts (or ?)"),
    (&["Ctrl", "Q"], "Close"),
];

/// The shortcuts in a dialog.
pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::keys(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls without a scrollbar, which would cover the key caps.
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

fn install_keys(window: &gtk::ApplicationWindow, search: &gtk::SearchEntry) {
    // Capture phase: these work wherever focus is.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s2 = search.clone();
    let w2 = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        let typing = gtk::prelude::GtkWindowExt::focus(&w2)
            .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>() || f.ancestor(gtk::Entry::static_type()).is_some());
        let done = glib::Propagation::Stop;
        if settings_dialog::is_open() {
            return match key {
                gdk::Key::Escape => {
                    settings_dialog::escape();
                    done
                }
                gdk::Key::f if ctrl => {
                    settings_dialog::focus_search();
                    done
                }
                gdk::Key::q | gdk::Key::w if ctrl => {
                    w2.close();
                    done
                }
                _ => glib::Propagation::Proceed,
            };
        }
        match key {
            gdk::Key::f if ctrl => {
                open_search();
                done
            }
            gdk::Key::F1 => {
                show_shortcuts();
                done
            }
            gdk::Key::question if !ctrl && !typing => {
                show_shortcuts();
                done
            }
            gdk::Key::N if ctrl && shift => {
                sections::chat::set_temporary(!sections::chat::is_temporary());
                done
            }
            gdk::Key::n if ctrl => {
                sections::chat::new_chat(None);
                done
            }
            gdk::Key::b if ctrl => {
                toggle_sidebar();
                done
            }
            gdk::Key::q | gdk::Key::w if ctrl => {
                w2.close();
                done
            }
            gdk::Key::comma if ctrl => {
                settings_dialog::open();
                done
            }
            gdk::Key::space if ctrl && current() == "chat" => {
                sections::chat::toggle_dictation();
                done
            }
            gdk::Key::S if ctrl && shift => {
                sections::chat::screenshot();
                done
            }
            gdk::Key::Left if alt => {
                back();
                done
            }
            _ if ctrl && !alt && !shift && key.to_unicode().and_then(|c| c.to_digit(10)).is_some_and(|d| d >= 1) => {
                let n = key.to_unicode().and_then(|c| c.to_digit(10)).unwrap_or(1) as usize;
                let id = ui().and_then(|u| u.borrow().sections.get(n - 1).map(|s| s.id));
                if let Some(id) = id {
                    navigate(id);
                }
                done
            }
            gdk::Key::Escape if s2.has_focus() || s2.focus_child().is_some() || !s2.text().is_empty() => {
                s2.set_text("");
                gtk::prelude::GtkWindowExt::set_focus(&w2, None::<&gtk::Widget>);
                done
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);

    // The mouse's back button.
    let mouse_back = gtk::GestureClick::new();
    mouse_back.set_button(8);
    mouse_back.set_propagation_phase(gtk::PropagationPhase::Capture);
    mouse_back.connect_pressed(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        back();
    });
    window.add_controller(mouse_back);
}

pub fn switch_host(i: usize) {
    prefs::update(|p| p.active_host = i);
    if let Some(ui) = ui() {
        let current = ui.borrow().current;
        for id in ["models", "marketplace", "knowledge"] {
            if let Some(page) = ui.borrow_mut().pages.remove(id) {
                ui.borrow().stack.remove(&page);
            }
        }
        if matches!(current, "models" | "marketplace" | "knowledge") { go(current, false); }
    }
    hub::emit(&hub::HOST);
    models::switch_host();
}

/// The sidebar shows only icons: hide everything marked `compact-hide` and
/// centre the icons.
fn apply_compact(nav: &gtk::Box, compact: bool) {
    NAV_COMPACT.with(|c| c.set(compact));
    if compact {
        nav.add_css_class("compact");
    } else {
        nav.remove_css_class("compact");
    }
    fn walk(w: &gtk::Widget, compact: bool) {
        if w.has_css_class("compact-hide") {
            w.set_visible(!compact);
        }
        if w.has_css_class("nav-item")
            && let Some(content) = w.downcast_ref::<gtk::Button>().and_then(|b| b.child())
        {
            content.set_halign(if compact { gtk::Align::Center } else { gtk::Align::Fill });
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, compact);
            child = c.next_sibling();
        }
    }
    walk(nav.upcast_ref(), compact);
    HOST_SWITCH.with(|h| {
        if let Some(h) = h.borrow().as_ref() {
            h.set_visible(prefs::get().hosts.len() > 1 && !compact);
        }
    });
}

/// Fit the layout to the window's width. The wide layout must stay narrower
/// than a half-screen tile: if it didn't, GTK would keep the window at its
/// minimum, its width would never drop under the limit, and it would stay cut off.
fn relayout(w: &gtk::ApplicationWindow) {
    let Some(nav) = ui().map(|u| u.borrow().nav.clone()) else { return };
    let width = if w.width() > 0 { w.width() } else { w.default_width() };
    if width <= 0 {
        return;
    }
    // CHAT_DEBUG_WIDE keeps the wide layout, for measuring it in a tile.
    let narrow = width < 980 && std::env::var_os("CHAT_DEBUG_WIDE").is_none();
    let roomy = width >= 1200;
    settings_dialog::fit(w);
    if roomy != ROOMY.with(|r| r.replace(roomy)) {
        ROOMY_HOOKS.with(|h| {
            for f in h.borrow().iter() {
                f(roomy);
            }
        });
    }
    if narrow != NARROW.with(|n| n.get()) || !nav.has_css_class("sized") {
        nav.add_css_class("sized");
        set_narrow(narrow);
    }
    let compact = narrow || prefs::get().sidebar_collapsed || (NEEDS_ROOM.with(|n| n.get()) && !roomy);
    if compact != nav.has_css_class("compact") || !nav.has_css_class("laid-out") {
        nav.add_css_class("laid-out");
        apply_compact(&nav, compact);
    }
}

pub fn toggle_sidebar() {
    prefs::update(|p| p.sidebar_collapsed = !p.sidebar_collapsed);
    if let Some(w) = window() {
        relayout(&w);
    }
}

fn set_narrow(narrow: bool) {
    NARROW.with(|n| n.set(narrow));
    let Some(ui) = ui() else { return };
    ui.borrow().search.set_max_width_chars(if narrow { SEARCH_NARROW } else { SEARCH_WIDE });
    for page in ui.borrow().pages.values() {
        mark_page(page, narrow);
    }
    NARROW_HOOKS.with(|h| {
        for f in h.borrow().iter() {
            f(narrow);
        }
    });
}

/// Mark a page (or a scrolled page's body) `narrow` for the tighter padding.
pub fn mark_page(page: &gtk::Widget, narrow: bool) {
    let body = page.downcast_ref::<gtk::ScrolledWindow>().and_then(|s| s.child()).and_then(|v| v.first_child());
    let target = body.unwrap_or_else(|| page.clone());
    if narrow {
        target.add_css_class("narrow");
    } else {
        target.remove_css_class("narrow");
    }
}

fn ensure_built(id: &'static str) {
    let Some(ui) = ui() else { return };
    if ui.borrow().pages.contains_key(id) {
        return;
    }
    let build = ui.borrow().sections.iter().find(|s| s.id == id).map(|s| s.build);
    let Some(build) = build else { return };
    let page = build();
    mark_page(&page, narrow());
    let stack = ui.borrow().stack.clone();
    stack.add_named(&page, Some(id));
    ui.borrow_mut().pages.insert(id, page);
}

pub fn navigate(id: &str) {
    go(id, true);
}

/// Back to the page before this one.
pub fn back() {
    let prev = ui().and_then(|u| u.borrow_mut().history.pop());
    if let Some(id) = prev {
        go(id, false);
    }
}

fn go(id: &str, record: bool) {
    let Some(ui) = ui() else { return };
    // Settings is a card over the window, not a page.
    if id == "settings" {
        settings_dialog::open();
        return;
    }
    let resolved = {
        let u = ui.borrow();
        u.sections.iter().find(|s| s.id == id).or_else(|| u.sections.first()).map(|s| s.id)
    };
    let Some(id) = resolved else { return };
    ensure_built(id);
    let mut u = ui.borrow_mut();
    if record && u.current != id && !u.current.is_empty() {
        let prev = u.current;
        u.history.push(prev);
        if u.history.len() > 50 {
            u.history.remove(0);
        }
    }
    if let Some(prev) = u.nav_items.get(u.current) {
        prev.remove_css_class("active");
    }
    if let Some(b) = u.nav_items.get(id) {
        b.add_css_class("active");
    }
    u.stack.set_visible_child_name(id);
    u.current = id;
    for part in &u.chat_parts {
        part.set_visible(id == "chat");
    }
    drop(u);
    refresh_crumb();
}

/// The window title: the open chat's title on the chat page, else the page's name.
pub fn refresh_crumb() {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    let title = if u.current == "chat" {
        sections::chat::title()
    } else {
        u.sections.iter().find(|s| s.id == u.current).map(|s| s.title.to_string()).unwrap_or_default()
    };
    chrome::set_title(&u.window, "Chat", &title);
    u.header.set_subtitle(&title);
}

/// The chat page puts its chat list button, model picker and actions in the header.
pub fn set_chat_header(start: &gtk::Box, picker: &impl IsA<gtk::Widget>, actions: &gtk::Box) {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    u.chat_parts[0].append(start);
    u.chat_parts[1].append(picker);
    u.chat_parts[2].append(actions);
}

thread_local! {
    /// The toast showing now; a new one replaces it.
    static TOAST: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
}

type ToastAction<'a> = Option<(&'a str, Box<dyn Fn()>)>;

/// Show a short message at the bottom of the window.
pub fn toast(message: &str) {
    show_toast(message, None);
}

/// A toast with a button (Undo, Open, …). It stays a little longer.
pub fn toast_action(message: &str, action: &str, f: impl Fn() + 'static) {
    show_toast(message, Some((action, Box::new(f))));
}

fn show_toast(message: &str, action: ToastAction<'_>) {
    let Some(ui) = ui() else {
        eprintln!("nyx-chat: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    if let Some(old) = TOAST.with(|t| t.borrow_mut().take())
        && old.parent().is_some()
    {
        overlay.remove_overlay(&old);
    }
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bx.add_css_class("toast");
    bx.append(&label);
    let has_action = action.is_some();
    if let Some((text, f)) = action {
        let b = gtk::Button::with_label(text);
        b.add_css_class("flat");
        b.add_css_class("toast-action");
        b.set_valign(gtk::Align::Center);
        let (o, w) = (overlay.clone(), bx.downgrade());
        b.connect_clicked(move |_| {
            f();
            if let Some(w) = w.upgrade()
                && w.parent().is_some()
            {
                o.remove_overlay(&w);
            }
        });
        bx.append(&b);
    }
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    overlay.add_overlay(&bx);
    TOAST.with(|t| *t.borrow_mut() = Some(bx.clone()));
    let ms = if has_action { 6000 } else { 3500 };
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
        if bx.parent().is_some() {
            overlay.remove_overlay(&bx);
        }
    });
}
