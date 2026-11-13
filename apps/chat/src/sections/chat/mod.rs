//! The chat page: header with the model picker, the conversation, the
//! composer, and a side panel for per-chat settings and previews. Before
//! anything is sent it's Home, with the composer in the middle of the page.

mod composer;
mod home;
mod live;
mod panel;
mod send;
mod view;

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::store::{self, Chat, Message};
use crate::{chatlist, hub, models, prefs, widgets, window};

pub use send::Stream;

pub(crate) struct Page {
    model_dd: gtk::DropDown,
    load_bar: gtk::ProgressBar,
    load_pulse: RefCell<Option<glib::SourceId>>,
    model_list: RefCell<Vec<String>>,
    caps: gtk::Box,
    compare_btn: gtk::MenuButton,
    banners: gtk::Box,
    scroller: gtk::ScrolledWindow,
    column: gtk::Box,
    home: home::Home,
    /// Home is showing: nothing has been sent in this chat yet.
    at_home: Cell<bool>,
    /// Replies being written right now, side by side when comparing.
    live_box: gtk::Box,
    composer: Rc<composer::Composer>,
    /// The composer's place under the conversation, and the footer around it.
    composer_holder: gtk::Box,
    footer: gtk::Box,
    /// The conversation's actions in the header.
    actions: gtk::Box,
    settings_btn: gtk::ToggleButton,
    panel: gtk::Box,
    panel_stack: gtk::Stack,
    settings_box: gtk::Box,
    artifact_box: gtk::Box,
    jump: gtk::Button,
    follow: Cell<bool>,
    pub(crate) st: RefCell<State>,
}

#[derive(Default)]
pub(crate) struct State {
    /// None until the first message is sent; `draft` holds the settings till then.
    chat: Option<Chat>,
    draft: Chat,
    all: Vec<Message>,
}

impl State {
    pub(crate) fn chat(&self) -> &Chat {
        self.chat.as_ref().unwrap_or(&self.draft)
    }

    pub(crate) fn chat_mut(&mut self) -> &mut Chat {
        match self.chat.as_mut() {
            Some(c) => c,
            None => &mut self.draft,
        }
    }

    fn id(&self) -> Option<i64> {
        self.chat.as_ref().map(|c| c.id)
    }
}

thread_local! {
    static PAGE: RefCell<Option<Rc<Page>>> = const { RefCell::new(None) };
    /// Streams outlive the page's view of a chat: switching chats doesn't stop a reply.
    static STREAMS: RefCell<Vec<Rc<Stream>>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn page() -> Option<Rc<Page>> {
    PAGE.with(|p| p.borrow().clone())
}

pub fn current_id() -> Option<i64> {
    page().and_then(|p| p.st.borrow().id())
}

/// The chat on screen's title, for the top bar.
pub fn title() -> String {
    page().map(|p| p.st.borrow().chat().title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "New chat".into())
}

fn draft_chat(folder: Option<i64>, temporary: bool) -> Chat {
    let f = folder.and_then(store::folder);
    let model = f.as_ref().map(|f| f.model.clone()).filter(|m| !m.is_empty()).unwrap_or_else(models::default_model);
    let title = if temporary { "Temporary chat" } else { "New chat" };
    let mut c = Chat { title: title.into(), folder, model, temporary, ..Default::default() };
    if let Some(f) = f {
        c.knowledge = f.knowledge.clone();
    }
    c
}

pub fn build() -> gtk::Widget {
    let root = widgets::vbox(0);
    root.add_css_class("chat-page");

    // ----- In the window's header -----
    // The model picker in the middle; in a conversation, its actions at the end,
    // beside the window's own buttons.
    let end = widgets::hbox(2);
    end.add_css_class("chat-actions");

    // The load bar runs along the picker's bottom edge, so the picker is as
    // tall as the buttons beside it.
    let picker = gtk::Overlay::new();
    picker.set_valign(gtk::Align::Center);
    let model_dd = gtk::DropDown::from_strings(&[]);
    model_dd.add_css_class("model-picker");
    model_dd.set_enable_search(true);
    widgets::ellipsize_to(&model_dd, 22);
    model_dd.set_tooltip_text(Some("Model"));
    picker.set_child(Some(&model_dd));
    // Shows whether the model is in memory: pulsing while it loads, full once loaded.
    let load_bar = gtk::ProgressBar::new();
    load_bar.add_css_class("model-load");
    load_bar.set_pulse_step(0.12);
    load_bar.set_valign(gtk::Align::End);
    load_bar.set_can_target(false);
    picker.add_overlay(&load_bar);

    // The model's capabilities beside the picker, centred with it.
    let caps = widgets::hbox(4);
    caps.add_css_class("caps");
    caps.set_valign(gtk::Align::Center);
    let middle = widgets::hbox(8);
    middle.append(&picker);
    middle.append(&caps);

    let compare_btn = gtk::MenuButton::new();
    compare_btn.set_icon_name("list-add-symbolic");
    compare_btn.add_css_class("tool-button");
    compare_btn.set_tooltip_text(Some("Compare with other models"));
    end.append(&compare_btn);

    let settings_btn = gtk::ToggleButton::new();
    settings_btn.set_icon_name("preferences-other-symbolic");
    settings_btn.add_css_class("tool-button");
    settings_btn.set_tooltip_text(Some("This chat's prompt and parameters"));
    end.append(&settings_btn);

    let more = gtk::MenuButton::new();
    more.set_icon_name("view-more-symbolic");
    more.add_css_class("tool-button");
    more.set_tooltip_text(Some("More"));
    more.set_create_popup_func(|m| {
        let Some(p) = page() else { return };
        let chat = p.st.borrow().chat.clone();
        match chat {
            Some(c) => m.set_popover(Some(&chatlist::chat_menu(&c))),
            None => {
                let pop = gtk::Popover::new();
                pop.set_child(Some(&widgets::label("Send a message to start this chat.", "dim")));
                m.set_popover(Some(&pop));
            }
        }
    });
    end.append(&more);
    window::set_chat_header(&middle, &end);

    let banners = widgets::vbox(0);
    banners.add_css_class("chat-banners");
    root.append(&banners);

    // ----- Body -----
    let body = widgets::hbox(0);
    body.set_vexpand(true);
    let center = widgets::vbox(0);
    center.set_hexpand(true);

    let column = widgets::vbox(18);
    column.add_css_class("messages");
    let home = home::build();
    let col_wrap = widgets::vbox(0);
    col_wrap.add_css_class("messages-wrap");
    col_wrap.append(&home.root);
    col_wrap.append(&crate::clamp::Clamp::new(&column, 780));
    let live_box = widgets::hbox(14);
    live_box.add_css_class("live-box");
    live_box.set_homogeneous(true);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&col_wrap)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroller));
    let jump = gtk::Button::from_icon_name("go-down-symbolic");
    jump.add_css_class("jump-bottom");
    jump.set_halign(gtk::Align::Center);
    jump.set_valign(gtk::Align::End);
    jump.set_tooltip_text(Some("Jump to the latest message"));
    jump.set_visible(false);
    overlay.add_overlay(&jump);
    center.append(&overlay);

    let composer = composer::Composer::new();
    let composer_holder = widgets::vbox(0);
    composer_holder.append(&composer.root);
    let composer_clamp = crate::clamp::Clamp::new(&composer_holder, 780);
    composer_clamp.add_css_class("composer-clamp");
    // The message bar is its own solid footer under the conversation, not a
    // card floating over it: the chat view ends where the footer begins.
    let footer = widgets::vbox(0);
    footer.add_css_class("chat-footer");
    footer.set_vexpand(false);
    footer.append(&composer_clamp);
    center.append(&footer);
    body.append(&center);

    let panel = widgets::vbox(0);
    panel.add_css_class("side-panel");
    panel.set_visible(false);
    let panel_stack = gtk::Stack::new();
    panel_stack.set_vexpand(true);
    let settings_box = widgets::vbox(0);
    let settings_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&settings_box)
        .build();
    panel_stack.add_named(&settings_scroll, Some("settings"));
    let artifact_box = widgets::vbox(10);
    artifact_box.add_css_class("artifact-panel");
    panel_stack.add_named(&artifact_box, Some("artifact"));
    panel.append(&panel_stack);
    let panel_line = nyx_ui::frame::pane_line(gtk::Orientation::Vertical);
    panel.bind_property("visible", &panel_line, "visible").sync_create().build();
    body.append(&panel_line);
    body.append(&panel);
    root.append(&body);

    let pg = Rc::new(Page {
        model_dd: model_dd.clone(),
        load_bar,
        load_pulse: RefCell::new(None),
        model_list: RefCell::new(Vec::new()),
        caps,
        compare_btn: compare_btn.clone(),
        banners,
        scroller: scroller.clone(),
        column: column.clone(),
        home,
        at_home: Cell::new(false),
        live_box,
        composer,
        composer_holder,
        footer: footer.clone(),
        actions: end.clone(),
        settings_btn: settings_btn.clone(),
        panel: panel.clone(),
        panel_stack: panel_stack.clone(),
        settings_box,
        artifact_box,
        jump: jump.clone(),
        follow: Cell::new(true),
        st: RefCell::new(State { draft: draft_chat(None, false), ..Default::default() }),
    });
    PAGE.with(|p| *p.borrow_mut() = Some(pg.clone()));

    // Follow new text until the reader scrolls up, and again once they're back
    // at the bottom. Only the reader's own scrolling turns following off: layout
    // changes move the adjustment too, and mustn't be mistaken for that.
    let adj = scroller.vadjustment();
    let p2 = Rc::downgrade(&pg);
    adj.connect_changed(move |a| {
        // This fires while the view is being laid out; moving the position
        // then is dropped (the content stays drawn at the old offset and the
        // newest message ends up hidden), so move it once layout is done.
        if let Some(p) = p2.upgrade()
            && p.follow.get()
        {
            let a = a.clone();
            glib::idle_add_local_once(move || a.set_value(a.upper() - a.page_size()));
        }
    });
    let p2 = Rc::downgrade(&pg);
    adj.connect_value_changed(move |a| {
        if let Some(p) = p2.upgrade() {
            let at_bottom = a.value() + a.page_size() >= a.upper() - 48.0;
            if at_bottom {
                p.follow.set(true);
            }
            p.jump.set_visible(!p.follow.get());
        }
    });
    let wheel =
        gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    let p2 = Rc::downgrade(&pg);
    wheel.connect_scroll(move |_, _, dy| {
        if dy < 0.0
            && let Some(p) = p2.upgrade()
        {
            p.follow.set(false);
            p.jump.set_visible(true);
        }
        glib::Propagation::Proceed
    });
    scroller.add_controller(wheel);
    let keys = gtk::EventControllerKey::new();
    let p2 = Rc::downgrade(&pg);
    keys.connect_key_pressed(move |_, key, _, _| {
        if matches!(key, gdk::Key::Page_Up | gdk::Key::Up | gdk::Key::Home)
            && let Some(p) = p2.upgrade()
        {
            p.follow.set(false);
            p.jump.set_visible(true);
        }
        glib::Propagation::Proceed
    });
    scroller.add_controller(keys);
    let (s3, p3) = (scroller.clone(), Rc::downgrade(&pg));
    jump.connect_clicked(move |_| {
        if let Some(p) = p3.upgrade() {
            p.follow.set(true);
        }
        scroll_to_bottom_now(&s3);
    });

    let p2 = pg.clone();
    settings_btn.connect_toggled(move |b| {
        if b.is_active() {
            panel::fill(&p2);
            p2.panel_stack.set_visible_child_name("settings");
        }
        p2.panel.set_visible(b.is_active());
        window::set_needs_room(b.is_active());
    });

    model_dd.connect_selected_notify(|dd| {
        // Refreshing the picker (its list changes when the chat's model does)
        // from inside its own click handler crashes GTK, so this waits until
        // GTK is done. Reading the selection then also means the picker's own
        // refreshes settle first and aren't mistaken for a pick.
        let dd = dd.clone();
        glib::idle_add_local_once(move || {
            let Some(p) = page() else { return };
            let Some(name) = p.model_list.borrow().get(dd.selected() as usize).cloned() else { return };
            let changed = {
                let mut st = p.st.borrow_mut();
                let c = st.chat_mut();
                if c.model != name {
                    c.model = name.clone();
                    c.compare.retain(|m| *m != name);
                    true
                } else {
                    false
                }
            };
            if changed {
                models::pick(&name);
                save_chat(&p);
                refresh_header(&p);
                if p.panel.is_visible() && p.panel_stack.visible_child_name().as_deref() == Some("settings") {
                    panel::fill(&p);
                }
            }
        });
    });

    compare_btn.set_create_popup_func(|m| {
        if let Some(p) = page() {
            m.set_popover(Some(&compare_popover(&p)));
        }
    });

    // Files dropped anywhere on the page are attached.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(|_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<std::path::PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if let Some(p) = page() {
            p.composer.attach_paths(paths);
        }
        true
    });
    root.add_controller(drop);

    window::on_narrow({
        let lb = pg.live_box.clone();
        let col = pg.column.clone();
        move |narrow| {
            lb.set_orientation(if narrow { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
            fn walk(w: &gtk::Widget, narrow: bool) {
                if w.has_css_class("wide-only") {
                    w.set_visible(!narrow);
                }
                let mut c = w.first_child();
                while let Some(x) = c {
                    walk(&x, narrow);
                    c = x.next_sibling();
                }
            }
            walk(col.upcast_ref(), narrow);
        }
    });

    // The capability tags would make the toolbar too wide for a half-screen tile.
    let caps = pg.caps.clone();
    window::on_roomy(move |roomy| caps.set_visible(roomy));

    crate::markdown::set_preview_handler(|lang, code| {
        if let Some(p) = page() {
            panel::show_artifact(&p, lang, code);
        }
    });

    let p2 = Rc::downgrade(&pg);
    hub::on(&hub::MODELS, &root, move || {
        if let Some(p) = p2.upgrade() {
            // A draft picks up the default once models are known. A chosen model
            // that isn't installed stays, with the banner saying so.
            {
                let mut st = p.st.borrow_mut();
                if st.chat.is_none() && st.draft.model.is_empty() {
                    st.draft.model = models::default_model();
                }
            }
            refresh_header(&p);
            refresh_banners(&p);
            p.composer.refresh();
        }
    });
    let p2 = Rc::downgrade(&pg);
    hub::on(&hub::HOST, &root, move || {
        if let Some(p) = p2.upgrade() {
            refresh_banners(&p);
        }
    });

    // Home's cards and figures follow the chats.
    let p2 = Rc::downgrade(&pg);
    hub::on(&hub::CHATS, &root, move || {
        if let Some(p) = p2.upgrade()
            && p.at_home.get()
        {
            home::fill(&p);
        }
    });
    let p2 = Rc::downgrade(&pg);
    hub::on(&hub::LIBRARY, &root, move || {
        if let Some(p) = p2.upgrade()
            && p.at_home.get()
        {
            home::fill(&p);
        }
    });

    // The page opens as Home; the last chats are a click away on it.
    render(&pg);
    refresh_header(&pg);
    refresh_banners(&pg);
    root.upcast()
}

/// Show Home or the conversation: on Home the composer sits under the heading, the
/// footer and the conversation's actions go, and so does the side panel.
pub(crate) fn set_home(p: &Page, on: bool) {
    let was = p.at_home.replace(on);
    let root = p.composer.root.clone();
    let target = if on { &p.home.slot } else { &p.composer_holder };
    if root.parent().as_ref() != Some(target.upcast_ref::<gtk::Widget>()) {
        let focused = root.focus_child().is_some();
        if let Some(parent) = root.parent().and_downcast::<gtk::Box>() {
            parent.remove(&root);
        }
        target.append(&root);
        if focused || (was && !on) {
            p.composer.focus();
        }
    }
    p.composer.set_home(on);
    p.home.root.set_visible(on);
    p.footer.set_visible(!on);
    p.actions.set_visible(!on);
    if on && p.settings_btn.is_active() {
        p.settings_btn.set_active(false);
    }
    if was != on {
        refresh_banners(p);
        window::refresh_nav();
        window::refresh_crumb();
    }
}

/// Home is on screen (or would be, on the chat page).
pub fn is_home() -> bool {
    page().is_some_and(|p| p.at_home.get())
}

/// Go Home: a new chat, unless the one on screen hasn't been sent yet.
pub fn show_home() {
    let Some(p) = page() else { return };
    if p.at_home.get() && p.st.borrow().chat.is_none() {
        return;
    }
    reset_draft(&p, None, false);
}

/// Open the model picker in the header.
pub(crate) fn open_picker() {
    let Some(p) = page() else { return };
    if p.model_dd.is_sensitive() {
        p.model_dd.emit_by_name::<()>("activate", &[]);
    } else {
        window::navigate("models");
    }
}

pub(crate) fn save_chat(p: &Page) {
    let st = p.st.borrow();
    if let Some(c) = &st.chat {
        let _ = store::save_chat(c);
    }
}

/// Jump to the newest message once layout is done. Growth after that is
/// followed by the adjustment's `changed` handler while `follow` is on.
pub(crate) fn scroll_to_bottom_now(s: &gtk::ScrolledWindow) {
    let s = s.clone();
    glib::idle_add_local_once(move || {
        let a = s.vadjustment();
        a.set_value(a.upper() - a.page_size());
    });
}

fn compare_popover(p: &Rc<Page>) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let v = widgets::vbox(4);
    v.add_css_class("popover-body");
    v.append(&widgets::label("COMPARE WITH", "menu-heading-small"));
    let note = widgets::label("Each extra model answers the same message side by side.", "dim");
    note.set_wrap(true);
    note.set_max_width_chars(34);
    v.append(&note);
    let (main, compare) = {
        let st = p.st.borrow();
        (st.chat().model.clone(), st.chat().compare.clone())
    };
    let others: Vec<String> = models::chat_models().into_iter().filter(|m| *m != main).collect();
    if others.is_empty() {
        v.append(&widgets::label("Install another model to compare.", "dim"));
    }
    for m in others {
        let c = gtk::CheckButton::with_label(&m);
        c.set_active(compare.contains(&m));
        let p2 = p.clone();
        c.connect_toggled(move |c| {
            {
                let mut st = p2.st.borrow_mut();
                let list = &mut st.chat_mut().compare;
                list.retain(|x| *x != m);
                if c.is_active() {
                    if list.len() >= 2 {
                        c.set_active(false);
                        window::toast("Compare up to three models at once");
                        return;
                    }
                    list.push(m.clone());
                }
            }
            save_chat(&p2);
            refresh_header(&p2);
        });
        v.append(&c);
    }
    pop.set_child(Some(&v));
    pop
}

/// Title, model list, capability tags.
pub(crate) fn refresh_header(p: &Page) {
    let (model, compare) = {
        let st = p.st.borrow();
        let c = st.chat();
        (c.model.clone(), c.compare.clone())
    };
    let mut list = models::chat_models();
    if !model.is_empty() && !list.contains(&model) {
        list.insert(0, model.clone());
    }
    if list.is_empty() {
        p.model_list.borrow_mut().clear();
        p.model_dd.set_model(Some(&gtk::StringList::new(&["No models"])));
        p.model_dd.set_selected(0);
    } else if *p.model_list.borrow() != list {
        let labels: Vec<String> = list.iter().map(|m| crate::util::clip_middle(&crate::util::model_label(m), 48)).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        *p.model_list.borrow_mut() = list.clone();
        p.model_dd.set_model(Some(&gtk::StringList::new(&refs)));
    }
    let idx = list.iter().position(|m| *m == model).map(|i| i as u32).unwrap_or(gtk::INVALID_LIST_POSITION);
    if !list.is_empty() && p.model_dd.selected() != idx {
        p.model_dd.set_selected(idx);
    }
    p.model_dd.set_sensitive(!list.is_empty());
    widgets::clear(&p.caps);
    if let Some(info) = models::info(&model) {
        for (cap, icon, tip) in [
            ("vision", "view-reveal-symbolic", "Vision: this model can read images"),
            ("tools", "applications-engineering-symbolic", "Tools: this model can call tools"),
            ("thinking", "emoji-objects-symbolic", "Thinking: this model can reason before answering"),
        ] {
            if info.has(cap) {
                let img = gtk::Image::from_icon_name(icon);
                img.add_css_class("cap-icon");
                img.set_tooltip_text(Some(tip));
                p.caps.append(&img);
            }
        }
    }
    if !compare.is_empty() {
        p.caps.append(&widgets::tag(&format!("+{} compared", compare.len()), "accent"));
    }
    p.compare_btn.set_visible(models::chat_models().len() > 1);
    p.composer.refresh();
    refresh_load(p);
    refresh_context(p);
    window::refresh_crumb();
    window::set_temporary_toggle(p.st.borrow().chat().temporary);
}

/// The context meter, after a reply or a switch.
pub(crate) fn refresh_context(p: &Page) {
    let (limit, used) = {
        let st = p.st.borrow();
        let path = store::path_to(&st.all, st.chat().leaf);
        (send::context_limit(st.chat()), send::context_used(&path))
    };
    p.composer.set_context(used.filter(|_| limit > 0).map(|u| (u, limit as u64)));
}

/// The bar under the picker: pulsing while the model loads, full when it's
/// in memory, empty when it isn't.
pub(crate) fn refresh_load(p: &Page) {
    let model = p.st.borrow().chat().model.clone();
    let loading = models::loading().is_some_and(|m| m == model);
    let loaded = !loading && models::is_loaded(&model);
    let running = p.load_pulse.borrow().is_some();
    if loading && !running {
        let bar = p.load_bar.clone();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(90), move || {
            bar.pulse();
            glib::ControlFlow::Continue
        });
        *p.load_pulse.borrow_mut() = Some(id);
    } else if !loading && let Some(id) = p.load_pulse.borrow_mut().take() {
        id.remove();
    }
    if !loading {
        p.load_bar.set_fraction(if loaded { 1.0 } else { 0.0 });
    }
    for c in ["loading", "loaded"] {
        p.load_bar.remove_css_class(c);
    }
    if loading {
        p.load_bar.add_css_class("loading");
    } else if loaded {
        p.load_bar.add_css_class("loaded");
    }
    let tip = if model.is_empty() {
        "Model".to_string()
    } else if loading {
        format!("{model}\nLoading into memory…")
    } else if loaded {
        let pinned = prefs::get().keep_loaded && prefs::get().last_model == model;
        format!("{model}\nLoaded{}", if pinned { ", and stays loaded until you pick another model" } else { "" })
    } else {
        format!("{model}\nNot loaded yet; it loads with the first message")
    };
    p.model_dd.set_tooltip_text(Some(&tip));
    if p.at_home.get() {
        home::refresh(p);
    }
}

/// Offline server, missing model, temporary chat.
pub(crate) fn refresh_banners(p: &Page) {
    widgets::clear(&p.banners);
    if p.st.borrow().chat().temporary {
        let b = widgets::banner(
            "<b>Temporary chat.</b> It isn't saved or searchable, and it's deleted when you leave it.",
            false,
        );
        let keep = gtk::Button::with_label("Keep it");
        keep.set_valign(gtk::Align::Center);
        keep.set_tooltip_text(Some("Save this chat with the others"));
        keep.connect_clicked(|_| {
            let Some(p) = page() else { return };
            {
                let mut st = p.st.borrow_mut();
                let c = st.chat_mut();
                c.temporary = false;
                if c.title == "Temporary chat" {
                    c.title = "New chat".into();
                }
            }
            save_chat(&p);
            refresh_banners(&p);
            refresh_header(&p);
            hub::emit(&hub::CHATS);
        });
        b.append(&keep);
        p.banners.append(&b);
    }
    // Home's status card says this instead.
    if p.at_home.get() {
        return;
    }
    match models::online() {
        Some(false) => {
            let host = prefs::get().host();
            let local = host.backend == prefs::Backend::LlamaCpp
                && crate::prefs::normalize_host(&host.url, host.backend).contains("127.0.0.1");
            let b = widgets::banner(&unreachable_markup(), true);
            if local && crate::llama::api::installed() {
                let start = gtk::Button::with_label("Start Nyx AI Core");
                start.set_valign(gtk::Align::Center);
                start.connect_clicked(|b| {
                    b.set_sensitive(false);
                    crate::cmd::background(
                        || crate::llama::api::control("start"),
                        |r| {
                            match r {
                                Ok(m) => window::toast(&m),
                                Err(e) => window::toast(&format!("Couldn't start Nyx AI Core: {e}")),
                            }
                            models::refresh();
                        },
                    );
                });
                b.append(&start);
            } else if local {
                b.append(&widgets::label("Install Nyx AI Core, the shared local model service, from its installer.", "dim"));
            }
            p.banners.append(&b);
        }
        Some(true) if models::installed().is_empty() => {
            let b = widgets::banner("<b>No models yet.</b> Download one from the Marketplace to start chatting.", false);
            let go = gtk::Button::with_label("Open Marketplace");
            go.set_valign(gtk::Align::Center);
            go.connect_clicked(|_| window::navigate("marketplace"));
            b.append(&go);
            p.banners.append(&b);
        }
        Some(true) => {
            let model = p.st.borrow().chat().model.clone();
            if !model.is_empty() && !models::is_installed(&model) {
                let b = widgets::banner(
                    &format!(
                        "<b>{}</b> isn't installed on this server. Pick another model or download it.",
                        crate::util::esc(&model)
                    ),
                    true,
                );
                let dl = gtk::Button::with_label("Download");
                dl.set_valign(gtk::Align::Center);
                let m = model.clone();
                dl.connect_clicked(move |_| {
                    crate::pulls::start(&m);
                    window::navigate("models");
                });
                b.append(&dl);
                p.banners.append(&b);
            }
        }
        None => {}
    }
}

/// The offline banner's text: a bold lead-in, then the error without repeating it.
pub(crate) fn unreachable_markup() -> String {
    const LEAD: &str = "The model server isn't reachable.";
    let err = models::error();
    let rest = err.strip_prefix(LEAD).unwrap_or(&err).trim();
    format!("<b>{LEAD}</b> {}", crate::util::esc(rest))
}

/// Rebuild the conversation from the store.
pub(crate) fn render(p: &Rc<Page>) {
    view::render(p);
}

// ---------- Public API ----------

pub fn open(id: i64) {
    window::navigate("chat");
    if let Some(p) = page() {
        show(&p, id);
    }
}

/// Leaving the chat on screen: a temporary one is deleted (unless it's the one
/// being opened again).
fn leave(p: &Page, next: Option<i64>) {
    let current = p.st.borrow().chat.clone();
    if let Some(c) = current
        && c.temporary
        && Some(c.id) != next
    {
        STREAMS.with(|s| {
            for x in s.borrow().iter().filter(|x| x.chat == c.id) {
                x.stop();
            }
        });
        let _ = store::delete_chat(c.id);
        p.st.borrow_mut().chat = None;
    }
}

/// Delete the temporary chat on screen (the window is closing).
pub fn discard_temporary() {
    if let Some(p) = page() {
        leave(&p, None);
    }
    let _ = store::purge_temporary();
}

pub fn is_temporary() -> bool {
    page().is_some_and(|p| p.st.borrow().chat().temporary)
}

/// Start a chat that isn't kept, or go back to a normal one.
pub fn set_temporary(on: bool) {
    if is_temporary() == on {
        return;
    }
    start_chat(None, on);
    window::toast(if on { "Temporary chat: it's deleted when you leave it" } else { "New chat" });
}

/// Put a chat on the page (without switching pages: `build` uses this too).
fn show(p: &Rc<Page>, id: i64) {
    leave(p, Some(id));
    let Ok(Some(chat)) = store::chat(id) else {
        window::toast("That chat no longer exists");
        return;
    };
    {
        let mut st = p.st.borrow_mut();
        st.all = store::messages(id).unwrap_or_default();
        st.chat = Some(chat);
    }
    p.follow.set(true);
    prefs::update(|pr| pr.last_chat = id);
    p.composer.clear_draft_for_switch();
    render(p);
    refresh_header(p);
    refresh_banners(p);
    if p.panel.is_visible() && p.panel_stack.visible_child_name().as_deref() == Some("settings") {
        panel::fill(p);
    }
    hub::emit(&hub::CHATS);
    scroll_to_bottom_now(&p.scroller);
    p.composer.focus();
}

pub fn new_chat(folder: Option<i64>) {
    start_chat(folder, false);
}

fn start_chat(folder: Option<i64>, temporary: bool) {
    window::navigate("chat");
    let Some(p) = page() else { return };
    reset_draft(&p, folder, temporary);
}

/// Put a new, unsent chat on the page: Home.
fn reset_draft(p: &Rc<Page>, folder: Option<i64>, temporary: bool) {
    let p = p.clone();
    leave(&p, None);
    {
        let mut st = p.st.borrow_mut();
        st.chat = None;
        st.all.clear();
        st.draft = draft_chat(folder, temporary);
    }
    prefs::update(|pr| pr.last_chat = 0);
    render(&p);
    refresh_header(&p);
    refresh_banners(&p);
    if p.panel.is_visible() {
        panel::fill(&p);
    }
    hub::emit(&hub::CHATS);
    p.composer.focus();
}

/// Re-read a chat's settings (renamed, moved, …) if it's the one on screen.
pub fn reload_if_current(id: i64) {
    let Some(p) = page() else { return };
    if current_id() != Some(id) {
        return;
    }
    if let Ok(Some(c)) = store::chat(id) {
        p.st.borrow_mut().chat = Some(c);
        refresh_header(&p);
    }
}

/// A chat was deleted.
pub fn closed(id: i64) {
    STREAMS.with(|s| {
        for x in s.borrow().iter().filter(|x| x.chat == id) {
            x.stop();
        }
    });
    if current_id() == Some(id) {
        if let Some(p) = page() {
            p.st.borrow_mut().chat = None;
        }
        new_chat(None);
    }
}

pub fn toggle_dictation() {
    if let Some(p) = page() {
        p.composer.toggle_dictation();
    }
}

pub fn screenshot() {
    window::navigate("chat");
    if let Some(p) = page() {
        p.composer.screenshot();
    }
}

/// Send a message as if typed (the CHAT_AUTOSEND developer aid uses this).
pub fn send_text(text: &str) {
    if let Some(p) = page() {
        p.composer.insert_prompt(text);
        p.composer.submit();
    }
}

/// Attach files from outside (the command line).
pub fn attach(paths: Vec<std::path::PathBuf>) {
    window::navigate("chat");
    if let Some(p) = page() {
        p.composer.attach_paths(paths);
    }
}

pub fn streaming(chat: Option<i64>) -> bool {
    STREAMS.with(|s| s.borrow().iter().any(|x| Some(x.chat) == chat))
}

pub(crate) fn add_stream(s: Rc<Stream>) {
    STREAMS.with(|v| v.borrow_mut().push(s));
}

pub(crate) fn remove_stream(s: &Rc<Stream>) {
    STREAMS.with(|v| v.borrow_mut().retain(|x| !Rc::ptr_eq(x, s)));
}

/// Stop every reply in the chat on screen.
pub fn stop() {
    let Some(id) = current_id() else { return };
    STREAMS.with(|s| {
        for x in s.borrow().iter().filter(|x| x.chat == id) {
            x.stop();
        }
    });
}
