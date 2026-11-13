//! Home: the chat page before anything is sent. One big message box (the chat's own,
//! moved here), saved prompts as chips, the last few chats as cards, the week in numbers
//! and a line saying which model is ready.

use gtk::pango;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

use super::Page;
use crate::{clamp, models, prefs, store, util, widgets, window};

/// What the status card's button does.
#[derive(Clone, Copy, PartialEq)]
enum Act {
    Change,
    Start,
    Marketplace,
}

pub(crate) struct Home {
    pub root: gtk::Box,
    /// Where the message box sits while Home shows.
    pub slot: gtk::Box,
    note: gtk::Label,
    /// Saved prompts in centred lines of three.
    prompts: gtk::Box,
    recent_head: gtk::Box,
    recent: gtk::Grid,
    week: gtk::Box,
    status_title: gtk::Label,
    status_sub: gtk::Label,
    status_bar: gtk::LevelBar,
    status_btn: gtk::Button,
    act: Rc<Cell<Act>>,
}

pub(super) fn build() -> Home {
    let root = widgets::vbox(0);
    root.add_css_class("home");
    root.set_vexpand(true);
    let inner = widgets::vbox(0);
    inner.add_css_class("home-inner");
    root.append(&clamp::Clamp::new(&inner, 760));

    let h1 = widgets::label("What can I help with?", "home-title");
    h1.set_xalign(0.5);
    h1.set_wrap(true);
    inner.append(&h1);
    let note = widgets::label("", "home-note");
    note.set_xalign(0.5);
    note.set_wrap(true);
    note.set_visible(false);
    inner.append(&note);
    let slot = widgets::vbox(0);
    slot.add_css_class("home-slot");
    inner.append(&slot);

    let prompts = widgets::vbox(8);
    prompts.add_css_class("home-prompts");
    inner.append(&prompts);

    let recent_head = widgets::hbox(8);
    recent_head.add_css_class("home-sechead");
    let rt = widgets::label("Pick up where you left off", "home-sechead-title");
    rt.set_hexpand(true);
    rt.set_xalign(0.0);
    recent_head.append(&rt);
    let all = gtk::Button::with_label("All chats");
    all.add_css_class("flat");
    all.add_css_class("link-button");
    all.connect_clicked(|_| window::navigate("chats"));
    recent_head.append(&all);
    inner.append(&recent_head);
    let recent = gtk::Grid::new();
    recent.set_column_homogeneous(true);
    recent.set_column_spacing(10);
    recent.set_row_spacing(10);
    recent.add_css_class("home-recent");
    inner.append(&recent);

    let week = widgets::hbox(10);
    week.set_homogeneous(true);
    week.add_css_class("home-week");
    inner.append(&week);

    let status = widgets::hbox(12);
    status.add_css_class("home-status");
    let spark = gtk::Image::from_icon_name("chat-spark-symbolic");
    spark.add_css_class("spark");
    spark.set_valign(gtk::Align::Center);
    status.append(&spark);
    let tx = widgets::vbox(1);
    tx.set_valign(gtk::Align::Center);
    let status_title = widgets::label("", "home-status-title");
    status_title.set_xalign(0.0);
    status_title.set_ellipsize(pango::EllipsizeMode::End);
    let status_sub = widgets::label("", "home-status-sub");
    status_sub.set_xalign(0.0);
    status_sub.set_ellipsize(pango::EllipsizeMode::End);
    tx.append(&status_title);
    tx.append(&status_sub);
    status.append(&tx);
    let status_bar = widgets::usage_bar();
    status_bar.add_css_class("home-status-bar");
    status_bar.set_hexpand(true);
    status_bar.set_valign(gtk::Align::Center);
    status.append(&status_bar);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    status.append(&spacer);
    status_bar.bind_property("visible", &spacer, "visible").transform_to(|_, v: bool| Some(!v)).sync_create().build();
    let status_btn = gtk::Button::with_label("Change");
    status_btn.add_css_class("flat");
    status_btn.set_valign(gtk::Align::Center);
    status.append(&status_btn);
    inner.append(&status);

    let act = Rc::new(Cell::new(Act::Change));
    let a2 = act.clone();
    status_btn.connect_clicked(move |b| match a2.get() {
        Act::Change => super::open_picker(),
        Act::Marketplace => window::navigate("marketplace"),
        Act::Start => {
            b.set_sensitive(false);
            let b = b.clone();
            crate::cmd::background(
                || crate::llama::api::control("start"),
                move |r| {
                    b.set_sensitive(true);
                    match r {
                        Ok(m) => window::toast(&m),
                        Err(e) => window::toast(&format!("Couldn't start Nyx AI Core: {e}")),
                    }
                    models::refresh();
                },
            );
        }
    });

    let w2 = week.clone();
    window::on_narrow(move |narrow| w2.set_visible(!narrow && w2.first_child().is_some()));
    let sb = status_bar.clone();
    window::on_narrow(move |narrow| {
        if narrow {
            sb.set_visible(false);
        }
    });

    Home { root, slot, note, prompts, recent_head, recent, week, status_title, status_sub, status_bar, status_btn, act }
}

/// Everything on Home: the line under the heading, prompts, recent chats, the week, the model.
pub(super) fn fill(p: &Page) {
    refresh(p);
    fill_prompts(p);
    fill_recent(&p.home);
    fill_week(&p.home);
}

/// The parts that follow the chat's settings and the model: the line under the heading
/// and the status card.
pub(super) fn refresh(p: &Page) {
    let h = &p.home;
    let (temporary, folder) = {
        let st = p.st.borrow();
        (st.chat().temporary, st.chat().folder.and_then(store::folder))
    };
    let note = if temporary {
        "A temporary chat: it isn't saved, and it's gone when you leave it.".to_string()
    } else if let Some(f) = folder {
        format!("A new chat in {}, with that folder's settings.", f.name)
    } else {
        String::new()
    };
    h.note.set_text(&note);
    h.note.set_visible(!note.is_empty());
    refresh_status(p);
}

fn fill_prompts(p: &Page) {
    let h = &p.home;
    widgets::clear(&h.prompts);
    let prompts = store::prompts().unwrap_or_default();
    h.prompts.set_visible(!prompts.is_empty());
    for (i, pr) in prompts.into_iter().take(6).enumerate() {
        if i % 3 == 0 {
            let line = widgets::hbox(8);
            line.set_halign(gtk::Align::Center);
            h.prompts.append(&line);
        }
        let Some(line) = h.prompts.last_child().and_downcast::<gtk::Box>() else { continue };
        let b = gtk::Button::new();
        b.add_css_class("prompt-chip");
        let row = widgets::hbox(8);
        row.append(&widgets::label(&format!("/{}", pr.command), "prompt-chip-command"));
        if !pr.title.is_empty() {
            row.append(&widgets::label(&pr.title, "prompt-chip-title"));
        }
        b.set_child(Some(&row));
        b.set_tooltip_text(Some(&util::clip(&pr.content, 200)));
        b.connect_clicked(move |_| {
            if let Some(p) = super::page() {
                p.composer.insert_prompt(&pr.content);
            }
        });
        line.append(&b);
    }
}

/// The first line of a reply, without Markdown's marks.
fn preview(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```")).unwrap_or("");
    let line = line.trim_start_matches(['#', '>', '-', '*', ' ']);
    util::clip(&line.replace("**", "").replace('`', ""), 140)
}

fn fill_recent(h: &Home) {
    while let Some(c) = h.recent.first_child() {
        h.recent.remove(&c);
    }
    let chats: Vec<store::Chat> = store::chats(false).unwrap_or_default().into_iter().filter(|c| !c.temporary).take(4).collect();
    h.recent_head.set_visible(!chats.is_empty());
    h.recent.set_visible(!chats.is_empty());
    for (i, c) in chats.iter().enumerate() {
        let b = gtk::Button::new();
        b.add_css_class("recent-card");
        let v = widgets::vbox(3);
        let t = widgets::label(&c.title, "recent-card-title");
        t.set_xalign(0.0);
        t.set_ellipsize(pango::EllipsizeMode::End);
        v.append(&t);
        let meta = if c.model.is_empty() { util::ago(c.updated) } else { format!("{} · {}", util::model_label(&c.model), util::ago(c.updated)) };
        let m = widgets::label(&meta, "recent-card-meta");
        m.set_xalign(0.0);
        m.set_ellipsize(pango::EllipsizeMode::End);
        v.append(&m);
        let snippet = store::last_reply(c.id).ok().flatten().map(|t| preview(&t)).unwrap_or_default();
        let s = widgets::label(if snippet.is_empty() { "No reply yet" } else { &snippet }, "recent-card-preview");
        s.set_xalign(0.0);
        s.set_ellipsize(pango::EllipsizeMode::End);
        v.append(&s);
        b.set_child(Some(&v));
        b.set_tooltip_text(Some(&c.title));
        let id = c.id;
        b.connect_clicked(move |_| super::open(id));
        h.recent.attach(&b, (i % 2) as i32, (i / 2) as i32, 1, 1);
    }
}

fn kcard(icon: &str, key: &str, value: &str) -> gtk::Box {
    let card = widgets::vbox(4);
    card.add_css_class("home-kcard");
    let k = widgets::hbox(6);
    k.add_css_class("home-kcard-key");
    k.append(&gtk::Image::from_icon_name(icon));
    let kl = widgets::label(key, "");
    kl.set_ellipsize(pango::EllipsizeMode::End);
    k.append(&kl);
    card.append(&k);
    let v = widgets::label(value, "home-kcard-value");
    v.set_xalign(0.0);
    v.add_css_class("tnum");
    card.append(&v);
    card
}

fn fill_week(h: &Home) {
    widgets::clear(&h.week);
    let week = store::week().unwrap_or(store::Week { chats: 0, words: 0 });
    if week.chats > 0 || week.words > 0 {
        h.week.append(&kcard("chat-message-new-symbolic", "chats this week", &util::count(week.chats as u64)));
        h.week.append(&kcard("document-edit-symbolic", "words written for you", &util::count(week.words)));
        if let Some((model, wps)) = store::fastest().ok().flatten() {
            h.week.append(&kcard("chat-zap-symbolic", &format!("fastest: {}", util::model_label(&model)), &format!("{wps:.0} words/s")));
        }
    }
    h.week.set_visible(h.week.first_child().is_some() && !window::narrow());
}

/// "this computer", or the server's own name.
fn host_name() -> String {
    let name = prefs::get().host().name;
    if name.eq_ignore_ascii_case("this computer") { "this computer".into() } else { name }
}

/// Which model is ready: loaded, loading, waiting for the first message, or why not.
fn refresh_status(p: &Page) {
    let h = &p.home;
    let model = p.st.borrow().chat().model.clone();
    let label = util::model_label(&model);
    let mut act = Act::Change;
    let mut bar: Option<f64> = None;
    let (title, sub) = match models::online() {
        None => ("Connecting to the model server…".to_string(), host_name()),
        Some(false) => {
            let host = prefs::get().host();
            let local = host.backend == prefs::Backend::LlamaCpp && prefs::normalize_host(&host.url, host.backend).contains("127.0.0.1");
            let err = models::error();
            let rest = err.strip_prefix("The model server isn't reachable.").unwrap_or(&err).trim().to_string();
            if local && crate::llama::api::installed() {
                act = Act::Start;
            }
            ("The model server isn't reachable".to_string(), if rest.is_empty() { host_name() } else { rest })
        }
        Some(true) if models::installed().is_empty() => {
            act = Act::Marketplace;
            ("No models yet".to_string(), "Download one from the Marketplace to start chatting".to_string())
        }
        Some(true) if model.is_empty() => ("Pick a model to start".to_string(), format!("{} models on {}", models::installed().len(), host_name())),
        Some(true) if !models::is_installed(&model) => {
            (format!("{label} isn't installed"), format!("Pick another model, or download it on {}", host_name()))
        }
        Some(true) if models::loading().is_some_and(|m| m == model) => (format!("Loading {label}…"), "Getting it into memory".to_string()),
        Some(true) if models::is_loaded(&model) => {
            let running = models::running().into_iter().find(|r| r.name == model);
            let vram = running.as_ref().map(|r| r.size_vram).unwrap_or(0);
            let total = prefs::get().vram_bytes;
            let sub = match (vram, total) {
                (0, _) => format!("Loaded on {}", host_name()),
                (v, 0) => format!("Loaded on {} · {} of graphics memory", host_name(), util::size(v)),
                (v, t) => {
                    bar = Some((v as f64 / t as f64).min(1.0));
                    format!("Loaded on {} · {} of {} graphics memory", host_name(), util::size(v), util::size(t))
                }
            };
            (format!("{label} is ready"), sub)
        }
        Some(true) => (format!("{label} is ready to load"), "It loads into memory with your first message".to_string()),
    };
    h.status_title.set_text(&title);
    h.status_sub.set_text(&sub);
    h.status_sub.set_tooltip_text(Some(&sub));
    h.status_bar.set_visible(bar.is_some() && !window::narrow());
    h.status_bar.set_value(bar.unwrap_or(0.0));
    h.act.set(act);
    match act {
        Act::Change => h.status_btn.set_label("Change"),
        Act::Start => h.status_btn.set_label("Start Nyx AI Core"),
        Act::Marketplace => h.status_btn.set_label("Open Marketplace"),
    }
    h.status_btn.set_visible(act != Act::Change || models::online() == Some(true));
}
