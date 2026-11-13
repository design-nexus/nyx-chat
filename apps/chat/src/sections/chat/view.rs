//! Drawing the conversation: user bubbles, assistant replies with their
//! thinking, tool calls and sources, and branch navigation.

use gtk::pango;
use gtk::prelude::*;
use serde_json::Value;
use std::rc::Rc;

use super::{Page, send};
use crate::markdown::Markdown;
use crate::ollama::chat::{Stats, ToolCall};
use crate::store::{self, Message, Source};
use crate::{jsonview, tools, util, widgets, window};

/// Rebuild the conversation from the store.
pub(super) fn render(p: &Rc<Page>) {
    widgets::clear(&p.column);
    if let Some(parent) = p.live_box.parent().and_downcast::<gtk::Box>() {
        parent.remove(&p.live_box);
    }
    let (all, leaf, chat_id, json_mode) = {
        let st = p.st.borrow();
        (st.all.clone(), st.chat().leaf, st.id(), st.chat().params.json)
    };
    let streams: Vec<Rc<send::Stream>> = chat_id
        .map(|id| super::STREAMS.with(|s| s.borrow().iter().filter(|x| x.chat == id).cloned().collect()))
        .unwrap_or_default();
    let streaming_ids: Vec<i64> = streams.iter().flat_map(|s| s.ids()).collect();
    let path = store::path_to(&all, leaf);
    let empty = path.is_empty() && streams.is_empty();
    super::set_home(p, empty);
    p.column.set_visible(!empty);
    if empty {
        super::home::fill(p);
        return;
    }

    let mut i = 0;
    while i < path.len() {
        let m = &path[i];
        if streaming_ids.contains(&m.id) {
            break;
        }
        if m.role == "user" {
            p.column.append(&user_bubble(p, m, &all));
            i += 1;
            continue;
        }
        // An assistant turn: everything up to the next user message.
        let end = path[i..].iter().position(|x| x.role == "user").map(|k| i + k).unwrap_or(path.len());
        let chain: Vec<Message> = path[i..end].iter().filter(|x| !streaming_ids.contains(&x.id)).cloned().collect();
        if !chain.is_empty() {
            p.column.append(&turn(p, &chain, &all, json_mode));
        }
        i = end;
    }
    if !streams.is_empty() {
        widgets::clear(&p.live_box);
        for s in &streams {
            if let Some(parent) = s.live.root.parent().and_downcast::<gtk::Box>() {
                parent.remove(&s.live.root);
            }
            p.live_box.append(&s.live.root);
        }
        p.column.append(&p.live_box);
    }
    super::refresh_context(p);
}

// ---------- User messages ----------

fn user_bubble(p: &Rc<Page>, m: &Message, all: &[Message]) -> gtk::Widget {
    let outer = widgets::vbox(4);
    outer.add_css_class("user-turn");
    outer.set_halign(gtk::Align::End);
    let bubble = widgets::vbox(8);
    bubble.add_css_class("user-msg");
    if !m.images.is_empty() {
        let row = widgets::hbox(6);
        for path in &m.images {
            row.append(&thumbnail(path, 140));
        }
        bubble.append(&row);
    }
    if !m.files.is_empty() {
        let row = gtk::FlowBox::new();
        row.set_selection_mode(gtk::SelectionMode::None);
        for f in &m.files {
            let chip = widgets::hbox(6);
            chip.add_css_class("file-chip");
            chip.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            let name = widgets::short_label(&f.name, "", pango::EllipsizeMode::Middle);
            name.set_max_width_chars(32);
            chip.append(&name);
            chip.set_tooltip_text(Some(&format!("{} characters", f.chars)));
            row.insert(&chip, -1);
        }
        bubble.append(&row);
    }
    if !m.content.is_empty() {
        let l = gtk::Label::new(Some(&m.content));
        l.set_wrap(true);
        l.set_wrap_mode(pango::WrapMode::WordChar);
        l.set_xalign(0.0);
        l.set_selectable(true);
        l.set_can_focus(false);
        l.add_css_class("user-text");
        bubble.append(&l);
    }
    outer.append(&bubble);

    let actions = widgets::hbox(2);
    actions.add_css_class("msg-actions");
    actions.set_halign(gtk::Align::End);
    let sibs = store::siblings(all, m);
    if sibs.len() > 1 {
        actions.append(&branch_nav(&sibs, m.id, all));
    }
    let content = m.content.clone();
    actions.append(&icon_button("edit-copy-symbolic", "Copy", move |b| {
        b.clipboard().set_text(&content);
        window::toast("Copied");
    }));
    let (p2, m2, b2) = (p.clone(), m.clone(), bubble.clone());
    actions.append(&icon_button("document-edit-symbolic", "Edit", move |_| edit_user(&p2, &m2, &b2)));
    let (p2, id) = (p.clone(), m.id);
    actions.append(&icon_button("chat-branch-symbolic", "Branch into a new chat from here", move |_| {
        send::branch(&p2, id)
    }));
    outer.append(&actions);
    outer.upcast()
}

pub(super) fn thumbnail(path: &str, size: i32) -> gtk::Widget {
    let pic = gtk::Picture::for_filename(path);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_size_request(size, size * 3 / 4);
    pic.add_css_class("thumb");
    let frame = gtk::Button::new();
    frame.set_child(Some(&pic));
    frame.add_css_class("flat");
    frame.add_css_class("thumb-btn");
    let p = path.to_string();
    frame.connect_clicked(move |_| {
        let _ = gtk::gio::AppInfo::launch_default_for_uri(&gtk::gio::File::for_path(&p).uri(), gtk::gio::AppLaunchContext::NONE);
    });
    frame.upcast()
}

fn edit_user(p: &Rc<Page>, m: &Message, bubble: &gtk::Box) {
    widgets::clear(bubble);
    bubble.add_css_class("editing");
    let (sw, tv) = widgets::text_area(&m.content, false, 60);
    sw.set_size_request(420, -1);
    bubble.append(&sw);
    let row = widgets::hbox(8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Send");
    save.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&save);
    bubble.append(&row);
    let p2 = p.clone();
    cancel.connect_clicked(move |_| render(&p2));
    let (p2, m2, tv2) = (p.clone(), m.clone(), tv.clone());
    save.connect_clicked(move |_| {
        let text = widgets::buffer_text(&tv2);
        if text.trim().is_empty() {
            return;
        }
        send::resend_edited(&p2, &m2, text.trim().to_string());
    });
    tv.grab_focus();
}

/// Selecting text in a reply offers Ask and Explain, which quote it into the
/// message box.
fn selection_menu(p: &Rc<Page>, body: &gtk::Box) {
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (p2, b2) = (Rc::downgrade(p), body.downgrade());
    click.connect_released(move |_, _, x, y| {
        let (Some(p), Some(body)) = (p2.upgrade(), b2.upgrade()) else { return };
        // Let the label finish the selection first.
        gtk::glib::idle_add_local_once(move || {
            let Some(text) = selected_text(body.upcast_ref()) else { return };
            let pop = gtk::Popover::new();
            pop.set_has_arrow(false);
            pop.set_autohide(true);
            pop.add_css_class("menu");
            let row = widgets::hbox(2);
            for (label, ask) in [("Ask", ""), ("Explain", "Explain this.")] {
                let b = gtk::Button::with_label(label);
                b.add_css_class("flat");
                b.add_css_class("menu-row");
                let (p3, pop2, t) = (p.clone(), pop.clone(), text.clone());
                b.connect_clicked(move |_| {
                    pop2.popdown();
                    p3.composer.quote(&t, ask);
                });
                row.append(&b);
            }
            pop.set_child(Some(&row));
            pop.set_parent(&body);
            pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32 + 6, 1, 1)));
            pop.connect_closed(|pop| {
                let pop = pop.clone();
                gtk::glib::idle_add_local_once(move || pop.unparent());
            });
            pop.popup();
        });
    });
    body.add_controller(click);
}

/// The text selected in any label under `w`.
fn selected_text(w: &gtk::Widget) -> Option<String> {
    if let Some(l) = w.downcast_ref::<gtk::Label>()
        && let Some((a, b)) = l.selection_bounds()
        && a != b
    {
        let text: String = l.text().chars().skip(a.min(b) as usize).take((a - b).unsigned_abs() as usize).collect();
        return (!text.trim().is_empty()).then_some(text);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(t) = selected_text(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}

/// Edit a reply in place: the corrected text is kept, nothing is regenerated.
fn edit_reply(p: &Rc<Page>, m: &Message, body: &gtk::Box) {
    widgets::clear(body);
    let (sw, tv) = widgets::text_area(&m.content, false, 120);
    body.append(&sw);
    let row = widgets::hbox(8);
    row.set_halign(gtk::Align::End);
    let note = widgets::label("Later messages build on your version.", "dim");
    note.add_css_class("small-text");
    note.set_hexpand(true);
    row.append(&note);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&save);
    body.append(&row);
    let p2 = p.clone();
    cancel.connect_clicked(move |_| render(&p2));
    let (p2, id, tv2) = (p.clone(), m.id, tv.clone());
    save.connect_clicked(move |_| {
        let text = widgets::buffer_text(&tv2);
        if !text.trim().is_empty() {
            send::save_edited_reply(&p2, id, text.trim_end().to_string());
        }
    });
    tv.grab_focus();
}

fn branch_nav(sibs: &[i64], current: i64, all: &[Message]) -> gtk::Widget {
    let bx = widgets::hbox(0);
    bx.add_css_class("branch-nav");
    let pos = sibs.iter().position(|x| *x == current).unwrap_or(0);
    let go = |to: i64, all: Vec<Message>| {
        move |_: &gtk::Button| {
            let Some(p) = super::page() else { return };
            let leaf = store::deepest(&all, to);
            {
                let mut st = p.st.borrow_mut();
                st.chat_mut().leaf = leaf;
            }
            if let Some(id) = super::current_id() {
                let _ = store::set_leaf(id, leaf);
            }
            render(&p);
        }
    };
    let prev = gtk::Button::from_icon_name("go-previous-symbolic");
    prev.add_css_class("flat");
    prev.add_css_class("tiny");
    prev.set_sensitive(pos > 0);
    if pos > 0 {
        prev.connect_clicked(go(sibs[pos - 1], all.to_vec()));
    }
    let l = widgets::label(&format!("{}/{}", pos + 1, sibs.len()), "tnum");
    l.add_css_class("dim");
    l.add_css_class("small-text");
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    next.add_css_class("flat");
    next.add_css_class("tiny");
    next.set_sensitive(pos + 1 < sibs.len());
    if pos + 1 < sibs.len() {
        next.connect_clicked(go(sibs[pos + 1], all.to_vec()));
    }
    bx.append(&prev);
    bx.append(&l);
    bx.append(&next);
    bx.set_tooltip_text(Some("Other versions"));
    bx.upcast()
}

/// A reply's action with its name beside the icon.
fn text_button(icon: &str, text: &str, tip: &str, f: impl Fn(&gtk::Button) + 'static) -> gtk::Button {
    let b = widgets::icon_label_button(icon, text);
    b.add_css_class("flat");
    b.add_css_class("reply-action");
    b.set_tooltip_text(Some(tip));
    b.connect_clicked(f);
    b
}

type MenuItem<'a> = (&'a str, Box<dyn Fn()>);
type OwnedItem = (String, Box<dyn Fn()>);

/// ⋯ with a reply's other actions. `narrow_extra` is listed too while the window is
/// narrow, where its own button is hidden.
fn more_menu(items: Vec<MenuItem<'_>>, narrow_extra: MenuItem<'_>) -> gtk::MenuButton {
    let m = gtk::MenuButton::new();
    m.set_icon_name("view-more-symbolic");
    m.add_css_class("flat");
    m.add_css_class("reply-more");
    m.set_tooltip_text(Some("Edit, continue, branch, delete"));
    let items: Rc<Vec<OwnedItem>> = Rc::new(items.into_iter().map(|(t, f)| (t.to_string(), f)).collect());
    let extra: Rc<OwnedItem> = Rc::new((narrow_extra.0.to_string(), narrow_extra.1));
    m.set_create_popup_func(move |m| {
        let pop = gtk::Popover::new();
        pop.add_css_class("menu");
        let v = widgets::vbox(1);
        let mut rows: Vec<usize> = (0..items.len()).collect();
        if window::narrow() {
            rows.insert(0, usize::MAX);
        }
        for i in rows {
            let label = if i == usize::MAX { extra.0.clone() } else { items[i].0.clone() };
            let b = gtk::Button::with_label(&label);
            b.add_css_class("flat");
            b.add_css_class("menu-row");
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let (items, extra, pop2) = (items.clone(), extra.clone(), pop.clone());
            b.connect_clicked(move |_| {
                pop2.popdown();
                if i == usize::MAX { (extra.1)() } else { (items[i].1)() }
            });
            v.append(&b);
        }
        pop.set_child(Some(&v));
        m.set_popover(Some(&pop));
    });
    m
}

/// A reply's speed in words: "41 words/s · 2.1 s to start".
fn speed_text(chain: &[Message], first: &Message, last: &Message, tokens: i64) -> String {
    let words: usize = chain.iter().filter(|m| m.role == "assistant").map(|m| m.content.split_whitespace().count()).sum();
    let start = Stats::from_json(&first.extra["stats"], first.tokens, first.tps).first_token_ms();
    let mut parts = Vec::new();
    if last.tps > 0.0 && last.tokens > 0 {
        let last_words = last.content.split_whitespace().count() as f64;
        let wps = last_words * last.tps / last.tokens as f64;
        if wps >= 1.0 {
            parts.push(format!("{wps:.0} words/s"));
        }
    }
    if parts.is_empty() {
        parts.push(if words > 0 { format!("{words} words") } else { format!("{tokens} tokens") });
    }
    if start > 0 {
        parts.push(format!("{:.1} s to start", start as f64 / 1000.0));
    }
    parts.join(" · ")
}

pub(super) fn icon_button(icon: &str, tip: &str, f: impl Fn(&gtk::Button) + 'static) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("tiny");
    b.set_tooltip_text(Some(tip));
    b.connect_clicked(f);
    b
}

// ---------- Assistant turns ----------

/// One assistant turn. When the answer was part of a comparison, its
/// siblings from the same group are shown beside it.
fn turn(p: &Rc<Page>, chain: &[Message], all: &[Message], json_mode: bool) -> gtk::Widget {
    let first = &chain[0];
    let group = first.extra["group"].as_i64();
    let group_sibs: Vec<&Message> = match group {
        Some(g) => all.iter().filter(|m| m.parent == first.parent && m.extra["group"].as_i64() == Some(g)).collect(),
        None => vec![],
    };
    if group_sibs.len() > 1 {
        let row = widgets::hbox(14);
        row.set_homogeneous(true);
        row.add_css_class("compare-row");
        if crate::window::narrow() {
            row.set_orientation(gtk::Orientation::Vertical);
        }
        for s in group_sibs {
            let selected = s.id == first.id;
            let c: Vec<Message> = if selected { chain.to_vec() } else { chain_from(all, s.id) };
            let w = answer(p, &c, all, json_mode, Some(selected));
            row.append(&w);
        }
        return row.upcast();
    }
    answer(p, chain, all, json_mode, None)
}

/// The reply's token count and speed; a click shows the details.
fn stats_button(text: &str, first: &Message, last: &Message, tokens: i64) -> gtk::MenuButton {
    let b = gtk::MenuButton::new();
    b.add_css_class("flat");
    b.add_css_class("msg-stats");
    let row = widgets::hbox(5);
    row.append(&gtk::Image::from_icon_name("chat-zap-symbolic"));
    row.append(&widgets::label(text, "tnum"));
    b.set_child(Some(&row));
    b.set_tooltip_text(Some("Details"));
    let first_stats = Stats::from_json(&first.extra["stats"], first.tokens, first.tps);
    let last_stats = Stats::from_json(&last.extra["stats"], last.tokens, last.tps);
    let total_ms: u64 = if first.id == last.id {
        last_stats.total_ms
    } else {
        first_stats.total_ms + last_stats.total_ms
    };
    b.set_create_popup_func(move |m| {
        let pop = gtk::Popover::new();
        let v = widgets::vbox(6);
        v.add_css_class("popover-body");
        v.add_css_class("stats-popover");
        let grid = gtk::Grid::new();
        grid.set_column_spacing(18);
        grid.set_row_spacing(6);
        let secs = |ms: u64| if ms >= 10_000 { format!("{:.0} s", ms as f64 / 1000.0) } else { format!("{:.2} s", ms as f64 / 1000.0) };
        let mut rows: Vec<(&str, String)> = Vec::new();
        if first_stats.total_ms > 0 {
            rows.push(("First token", secs(first_stats.first_token_ms())));
        }
        if last_stats.tokens_per_sec > 0.0 {
            rows.push(("Speed", format!("{:.1} tok/s", last_stats.tokens_per_sec)));
        }
        rows.push(("Reply", format!("{tokens} tokens")));
        if last_stats.prompt_tokens > 0 {
            rows.push(("Prompt", format!("{} tokens", last_stats.prompt_tokens)));
        }
        if total_ms > 0 {
            rows.push(("Total time", secs(total_ms)));
        }
        if first_stats.load_ms >= 100 {
            rows.push(("Loading the model", secs(first_stats.load_ms)));
        }
        rows.push(("Ended", last_stats.reason().to_string()));
        for (i, (k, val)) in rows.iter().enumerate() {
            grid.attach(&widgets::label(k, "dim"), 0, i as i32, 1, 1);
            let l = widgets::label(val, "tnum");
            l.set_xalign(1.0);
            grid.attach(&l, 1, i as i32, 1, 1);
        }
        v.append(&grid);
        pop.set_child(Some(&v));
        m.set_popover(Some(&pop));
    });
    b
}

/// The messages from `start` down its newest branch, until the next user message.
fn chain_from(all: &[Message], start: i64) -> Vec<Message> {
    let mut out = Vec::new();
    let mut cur = all.iter().find(|m| m.id == start);
    while let Some(m) = cur {
        if m.role == "user" {
            break;
        }
        out.push(m.clone());
        cur = all.iter().filter(|x| x.parent == Some(m.id)).max_by_key(|x| x.id);
    }
    out
}

fn answer(p: &Rc<Page>, chain: &[Message], all: &[Message], json_mode: bool, compare: Option<bool>) -> gtk::Widget {
    let first = &chain[0];
    let root = widgets::vbox(8);
    root.add_css_class("assistant-msg");
    if compare == Some(true) {
        root.add_css_class("selected");
    }

    let head = widgets::hbox(8);
    head.add_css_class("msg-head");
    let spark = gtk::Image::from_icon_name("chat-spark-symbolic");
    spark.add_css_class("spark");
    head.append(&spark);
    let model = widgets::short_label(&util::model_label(&first.model), "model-name", pango::EllipsizeMode::End);
    model.set_tooltip_text(Some(&first.model));
    head.append(&model);
    if let Some(t) = chrono::DateTime::from_timestamp(first.created, 0) {
        head.append(&widgets::label(&t.with_timezone(&chrono::Local).format("%H:%M").to_string(), "msg-time"));
    }
    let last = chain.iter().rev().find(|m| m.role == "assistant").unwrap_or(first);
    let tokens: i64 = chain.iter().map(|m| m.tokens).sum();
    if chain.iter().any(|m| m.extra["edited"].as_bool() == Some(true)) {
        let t = widgets::tag("Edited", "");
        t.set_tooltip_text(Some("You changed this reply"));
        head.append(&t);
    }
    if let Some(n) = first.extra["trimmed"].as_u64() {
        let t = widgets::tag(&format!("{n} earlier left out"), "");
        t.set_tooltip_text(Some("Older messages were left out so the chat fits the model's context"));
        head.append(&t);
    }
    root.append(&head);

    let body = widgets::vbox(10);
    let mut sources: Vec<Source> = Vec::new();
    let mut idx = 0;
    while idx < chain.len() {
        let m = &chain[idx];
        match m.role.as_str() {
            "assistant" => {
                // Knowledge lookups and searches done before the model answered.
                for pre in m.extra["pre"].as_array().into_iter().flatten() {
                    let call = ToolCall { name: pre["name"].as_str().unwrap_or("").into(), arguments: pre["arguments"].clone() };
                    let card = tool_card(&call, false);
                    let s = if call.name == "web_search" { m.sources() } else { vec![] };
                    card.set_result(pre["text"].as_str().unwrap_or(""), pre["failed"].as_bool().unwrap_or(false), &s);
                    body.append(&card.root);
                }
                if !m.sources().is_empty() {
                    sources = m.sources();
                }
                if !m.thinking.trim().is_empty() {
                    let secs = m.extra["think_secs"].as_f64();
                    let title = match secs {
                        Some(s) if s >= 1.0 => format!("Thought for {}", crate::pulls::duration(s.round() as u64)),
                        _ => "Thoughts".to_string(),
                    };
                    body.append(&thinking(&m.thinking, &title, false).root);
                }
                if !m.content.trim().is_empty() {
                    body.append(&content_widget(&m.content, json_mode, &sources));
                }
                // Tool calls, each with the result that followed it.
                for (k, call) in m.tool_calls.iter().enumerate() {
                    let result = chain.get(idx + 1 + k).filter(|r| r.role == "tool");
                    let card = tool_card(call, false);
                    if let Some(r) = result {
                        let s: Vec<Source> = r.sources();
                        card.set_result(&r.content, r.extra["failed"].as_bool().unwrap_or(false), &s);
                        if !s.is_empty() {
                            sources = s;
                        }
                    } else {
                        card.set_result("Stopped before this tool ran.", true, &[]);
                    }
                    body.append(&card.root);
                }
                if let Some(e) = m.error() {
                    body.append(&widgets::banner(&util::esc(&e), true));
                }
            }
            "tool" => {} // shown with its call
            _ => {}
        }
        idx += 1;
    }
    root.append(&body);
    selection_menu(p, &body);
    if !sources.is_empty() {
        root.append(&source_chips(&sources));
    }

    // Actions.
    let actions = widgets::hbox(2);
    actions.add_css_class("msg-actions");
    if compare.is_none() {
        let sibs: Vec<i64> = all.iter().filter(|x| x.parent == first.parent && x.role == "assistant").map(|x| x.id).collect();
        if sibs.len() > 1 {
            actions.append(&branch_nav(&sibs, first.id, all));
        }
    }
    let text: String = chain
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let t2 = text.clone();
    actions.append(&text_button("edit-copy-symbolic", "Copy", "Copy the reply", move |b| {
        b.clipboard().set_text(&t2);
        window::toast("Copied");
    }));
    let (p2, parent, model_name) = (p.clone(), first.parent, first.model.clone());
    actions.append(&text_button("view-refresh-symbolic", "Regenerate", "Write this reply again", move |_| {
        if let Some(u) = parent {
            send::generate(&p2, u, vec![model_name.clone()]);
        }
    }));
    let t3 = text.clone();
    let read = text_button("audio-speakers-symbolic", "Read aloud", "Read the reply aloud", move |_| crate::voice::speak(&t3));
    read.add_css_class("wide-only");
    read.set_visible(!window::narrow());
    actions.append(&read);
    // The reply's last written message: what Edit changes and Continue extends.
    let tail = chain.iter().rev().find(|m| m.role == "assistant" && !m.content.is_empty() && m.tool_calls.is_empty()).cloned();
    let last_id = chain.last().map(|l| l.id);
    let is_tip = !all.iter().any(|x| x.parent.is_some() && x.parent == last_id);
    // Edit, continue, branch and delete, behind ⋯.
    let mut more: Vec<MenuItem<'_>> = Vec::new();
    if let Some(tail) = tail.clone()
        && is_tip
        && compare.is_none()
    {
        let stats = Stats::from_json(&tail.extra["stats"], tail.tokens, tail.tps);
        let (p2, id) = (p.clone(), tail.id);
        if stats.cut_off() {
            let b = gtk::Button::with_label("Continue");
            b.add_css_class("chip");
            b.set_tooltip_text(Some(if stats.stopped { "Pick up where it stopped" } else { "It hit the length limit; keep writing" }));
            b.connect_clicked(move |_| send::continue_reply(&p2, id));
            actions.append(&b);
        } else {
            more.push(("Continue writing", Box::new(move || send::continue_reply(&p2, id))));
        }
    }
    if let Some(tail) = tail {
        let (p2, body2) = (p.clone(), body.clone());
        more.push(("Edit this reply", Box::new(move || edit_reply(&p2, &tail, &body2))));
    }
    if let Some(end) = chain.last() {
        let (p2, id) = (p.clone(), end.id);
        more.push(("Branch into a new chat from here", Box::new(move || send::branch(&p2, id))));
    }
    let (p2, first_id) = (p.clone(), first.id);
    more.push(("Delete this answer", Box::new(move || send::delete_answer(&p2, first_id))));
    let t4 = text.clone();
    actions.append(&more_menu(more, ("Read aloud", Box::new(move || crate::voice::speak(&t4)))));
    if compare == Some(false) {
        let (p2, id) = (p.clone(), first.id);
        let use_it = gtk::Button::with_label("Continue with this");
        use_it.add_css_class("chip");
        use_it.connect_clicked(move |_| {
            let all = p2.st.borrow().all.clone();
            let leaf = store::deepest(&all, id);
            p2.st.borrow_mut().chat_mut().leaf = leaf;
            if let Some(cid) = super::current_id() {
                let _ = store::set_leaf(cid, leaf);
            }
            render(&p2);
        });
        actions.append(&use_it);
    }
    if tokens > 0 {
        let spacer = widgets::hbox(0);
        spacer.set_hexpand(true);
        actions.append(&spacer);
        actions.append(&stats_button(&speed_text(chain, first, last, tokens), first, last, tokens));
    }
    root.append(&actions);
    if is_tip && compare.is_none() {
        let list: Vec<String> = chain
            .last()
            .and_then(|m| m.extra["followups"].as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !list.is_empty() {
            let flow = gtk::FlowBox::new();
            flow.add_css_class("followups");
            flow.set_selection_mode(gtk::SelectionMode::None);
            flow.set_max_children_per_line(3);
            flow.set_column_spacing(6);
            flow.set_row_spacing(6);
            for q in list {
                let b = gtk::Button::with_label(&q);
                b.add_css_class("chip");
                if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                    l.set_wrap(true);
                    l.set_xalign(0.0);
                }
                let p2 = p.clone();
                b.connect_clicked(move |_| {
                    p2.composer.insert_prompt(&q);
                    p2.composer.submit();
                });
                flow.insert(&b, -1);
            }
            root.append(&flow);
        }
    }
    root.upcast()
}

/// A reply's text: a JSON tree in JSON mode (when it parses), else Markdown.
pub(super) fn content_widget(text: &str, json_mode: bool, sources: &[Source]) -> gtk::Widget {
    if json_mode && let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
        return jsonview::view(&v, text);
    }
    let md = Markdown::new();
    md.set_sources(sources.to_vec());
    md.set(text);
    let root = md.root.clone();
    // Keep the renderer alive as long as its widget.
    unsafe { root.set_data("markdown", md) };
    root.upcast()
}

pub(super) fn source_chips(sources: &[Source]) -> gtk::Widget {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_max_children_per_line(4);
    flow.add_css_class("sources");
    for (i, s) in sources.iter().enumerate() {
        let host = s.url.split("//").nth(1).and_then(|r| r.split('/').next()).unwrap_or(&s.url).trim_start_matches("www.");
        let b = gtk::LinkButton::with_label(&s.url, &format!("{} {}", i + 1, host));
        b.add_css_class("source-chip");
        b.set_tooltip_text(Some(&s.title));
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.set_max_width_chars(22);
        }
        flow.insert(&b, -1);
    }
    flow.upcast()
}

// ---------- Thinking ----------

pub(super) struct Thinking {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub body: gtk::Label,
}

pub(super) fn thinking(text: &str, title: &str, expanded: bool) -> Thinking {
    let root = widgets::vbox(0);
    root.add_css_class("thinking");
    let button = gtk::Button::new();
    button.add_css_class("disclosure");
    let row = widgets::hbox(8);
    let icon = gtk::Image::from_icon_name(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" });
    row.append(&icon);
    row.append(&gtk::Image::from_icon_name("emoji-objects-symbolic"));
    let t = widgets::label(title, "thinking-title");
    row.append(&t);
    button.set_child(Some(&row));
    let body = widgets::label(text, "thinking-body");
    body.set_wrap(true);
    body.set_wrap_mode(pango::WrapMode::WordChar);
    body.set_selectable(true);
    body.set_can_focus(false);
    let rev = gtk::Revealer::new();
    rev.set_child(Some(&body));
    rev.set_reveal_child(expanded);
    rev.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 150 });
    let r2 = rev.clone();
    button.connect_clicked(move |_| {
        let open = !r2.reveals_child();
        r2.set_reveal_child(open);
        icon.set_icon_name(Some(if open { "pan-down-symbolic" } else { "pan-end-symbolic" }));
    });
    root.append(&button);
    root.append(&rev);
    Thinking { root, title: t, body }
}

// ---------- Tool cards ----------

pub(super) struct ToolCard {
    pub root: gtk::Box,
    status: gtk::Box,
    result: gtk::Box,
}

fn summary(call: &ToolCall) -> String {
    let a = &call.arguments;
    for k in ["query", "url", "expression", "command", "path", "fact"] {
        if let Some(s) = a[k].as_str() {
            return util::clip(s, 80);
        }
    }
    match a.as_object() {
        Some(o) if !o.is_empty() => util::clip(&a.to_string(), 80),
        _ => String::new(),
    }
}

pub(super) fn tool_card(call: &ToolCall, running: bool) -> ToolCard {
    let root = widgets::vbox(0);
    root.add_css_class("tool-card");
    let button = gtk::Button::new();
    button.add_css_class("tool-head");
    let row = widgets::hbox(8);
    let icon = match call.name.as_str() {
        "web_search" | "web_fetch" => "web-browser-symbolic",
        "get_location" => "mark-location-symbolic",
        "knowledge" => "accessories-dictionary-symbolic",
        _ => "applications-engineering-symbolic",
    };
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&widgets::label(&tools::title_for(&call.name), "tool-title"));
    let s = widgets::label(&summary(call), "tool-summary");
    s.set_ellipsize(pango::EllipsizeMode::End);
    s.set_hexpand(true);
    row.append(&s);
    let status = widgets::hbox(0);
    if running {
        let sp = gtk::Spinner::new();
        sp.start();
        status.append(&sp);
    }
    row.append(&status);
    button.set_child(Some(&row));
    root.append(&button);

    let inner = widgets::vbox(8);
    inner.add_css_class("tool-body");
    if call.arguments.as_object().is_some_and(|o| !o.is_empty()) {
        let args = widgets::label(&serde_json::to_string_pretty(&call.arguments).unwrap_or_default(), "code-block");
        args.set_wrap(true);
        args.set_wrap_mode(pango::WrapMode::WordChar);
        args.set_selectable(true);
        inner.append(&args);
    }
    let result = widgets::vbox(6);
    inner.append(&result);
    let rev = gtk::Revealer::new();
    rev.set_child(Some(&inner));
    rev.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 150 });
    let r2 = rev.clone();
    button.connect_clicked(move |_| r2.set_reveal_child(!r2.reveals_child()));
    root.append(&rev);
    ToolCard { root, status, result }
}

impl ToolCard {
    pub fn set_result(&self, text: &str, failed: bool, sources: &[Source]) {
        widgets::clear(&self.status);
        let icon = gtk::Image::from_icon_name(if failed { "dialog-warning-symbolic" } else { "object-select-symbolic" });
        icon.add_css_class(if failed { "danger-text" } else { "accent-text" });
        self.status.append(&icon);
        widgets::clear(&self.result);
        if !sources.is_empty() {
            self.result.append(&source_chips(sources));
        }
        let l = widgets::label(&util::clip(text.trim(), 4000), "tool-result");
        l.add_css_class("mono");
        l.set_wrap(true);
        l.set_wrap_mode(pango::WrapMode::WordChar);
        l.set_selectable(true);
        l.set_can_focus(false);
        self.result.append(&l);
        if failed {
            self.root.add_css_class("failed");
        }
    }
}
