//! The widgets of a reply that's still being written.

use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::view::{self, Thinking, ToolCard};
use crate::markdown::Markdown;
use crate::ollama::chat::{Stats, ToolCall};
use crate::store::Source;
use crate::{tools, util, widgets};

pub struct Live {
    pub root: gtk::Box,
    stats: gtk::Label,
    spinner: gtk::Spinner,
    body: gtk::Box,
    md: RefCell<Option<Rc<Markdown>>>,
    content: RefCell<String>,
    thinking: RefCell<Option<Thinking>>,
    think_text: RefCell<String>,
    think_start: Cell<Option<Instant>>,
    pub think_secs: Cell<f64>,
    pending: Cell<bool>,
    cards: RefCell<Vec<(ToolCall, ToolCard)>>,
    sources: RefCell<Vec<Source>>,
}

impl Live {
    pub fn new(model: &str) -> Rc<Live> {
        let root = widgets::vbox(8);
        root.add_css_class("assistant-msg");
        root.add_css_class("live");
        root.set_hexpand(true);
        let head = widgets::hbox(8);
        head.add_css_class("msg-head");
        let spark = gtk::Image::from_icon_name("chat-spark-symbolic");
        spark.add_css_class("spark");
        head.append(&spark);
        let name = widgets::short_label(&crate::util::model_label(model), "model-name", gtk::pango::EllipsizeMode::End);
        name.set_tooltip_text(Some(model));
        name.set_hexpand(true);
        head.append(&name);
        let spinner = gtk::Spinner::new();
        spinner.start();
        head.append(&spinner);
        let stats = widgets::label("", "msg-stats");
        stats.add_css_class("tnum");
        head.append(&stats);
        root.append(&head);
        let body = widgets::vbox(10);
        root.append(&body);
        Rc::new(Live {
            root,
            stats,
            spinner,
            body,
            md: RefCell::new(None),
            content: RefCell::new(String::new()),
            thinking: RefCell::new(None),
            think_text: RefCell::new(String::new()),
            think_start: Cell::new(None),
            think_secs: Cell::new(0.0),
            pending: Cell::new(false),
            cards: RefCell::new(Vec::new()),
            sources: RefCell::new(Vec::new()),
        })
    }

    /// A new assistant message begins (after tool results).
    pub fn round_start(&self) {
        self.flush();
        *self.md.borrow_mut() = None;
        self.content.borrow_mut().clear();
        *self.thinking.borrow_mut() = None;
        self.think_text.borrow_mut().clear();
        self.think_start.set(None);
        self.think_secs.set(0.0);
    }

    pub fn delta(self: &Rc<Self>, content: &str, thinking: &str) {
        // An empty thought (just a newline) doesn't open a Thinking block.
        if !thinking.is_empty() && (self.thinking.borrow().is_some() || !thinking.trim().is_empty()) {
            if self.thinking.borrow().is_none() {
                let t = view::thinking("", "Thinking…", true);
                self.body.append(&t.root);
                *self.thinking.borrow_mut() = Some(t);
                self.think_start.set(Some(Instant::now()));
            }
            self.think_text.borrow_mut().push_str(thinking);
        }
        if !content.is_empty() {
            self.end_thinking();
            if self.md.borrow().is_none() {
                let md = Markdown::new();
                md.set_sources(self.sources.borrow().clone());
                self.body.append(&md.root);
                *self.md.borrow_mut() = Some(md);
            }
            self.content.borrow_mut().push_str(content);
        }
        // Rendering Markdown on every token is wasteful; batch to ~15 fps.
        if !self.pending.replace(true) {
            let me = self.clone();
            glib::timeout_add_local_once(Duration::from_millis(66), move || me.flush());
        }
    }

    fn end_thinking(&self) {
        if let Some(start) = self.think_start.take() {
            let secs = start.elapsed().as_secs_f64();
            self.think_secs.set(secs);
            if let Some(t) = self.thinking.borrow().as_ref() {
                t.title.set_text(&format!("Thought for {}", crate::pulls::duration(secs.round().max(1.0) as u64)));
            }
        }
    }

    fn flush(&self) {
        self.pending.set(false);
        if let Some(t) = self.thinking.borrow().as_ref() {
            t.body.set_text(&self.think_text.borrow());
        }
        if let Some(md) = self.md.borrow().as_ref() {
            md.set(&self.content.borrow());
        }
    }

    pub fn round_end(&self, stats: &Stats) {
        self.end_thinking();
        self.flush();
        if stats.tokens > 0 && stats.tokens_per_sec > 0.0 {
            self.stats.set_text(&format!("{} tok · {:.1} tok/s", stats.tokens, stats.tokens_per_sec));
        }
    }

    pub fn tool_start(&self, call: &ToolCall) {
        self.flush();
        let card = view::tool_card(call, true);
        self.body.append(&card.root);
        self.cards.borrow_mut().push((call.clone(), card));
    }

    pub fn tool_result(&self, call: &ToolCall, output: &tools::Output) {
        if let Some((_, card)) = self.cards.borrow().iter().rev().find(|(c, _)| c == call) {
            card.set_result(&output.text, output.failed, &output.sources);
        }
        if !output.sources.is_empty() {
            *self.sources.borrow_mut() = output.sources.clone();
        }
    }

    pub fn approval(&self, title: &str, detail: &str, reply: tokio::sync::oneshot::Sender<bool>) {
        let card = widgets::vbox(10);
        card.add_css_class("approval");
        card.append(&widgets::label(title, "approval-title"));
        let d = widgets::label(detail, "code-block");
        d.set_wrap(true);
        d.set_selectable(true);
        card.append(&d);
        let row = widgets::hbox(8);
        row.set_halign(gtk::Align::End);
        let no = gtk::Button::with_label("Decline");
        let yes = gtk::Button::with_label("Allow");
        yes.add_css_class("suggested-action");
        row.append(&no);
        row.append(&yes);
        card.append(&row);
        let reply = Rc::new(RefCell::new(Some(reply)));
        let answer = {
            let (card, row) = (card.clone(), row.clone());
            move |ok: bool| {
                if let Some(r) = reply.borrow_mut().take() {
                    let _ = r.send(ok);
                }
                card.remove(&row);
                card.append(&widgets::label(if ok { "Allowed" } else { "Declined" }, "dim"));
            }
        };
        let a = answer.clone();
        yes.connect_clicked(move |_| a(true));
        no.connect_clicked(move |_| answer(false));
        self.body.append(&card);
        yes.grab_focus();
    }

    pub fn error(&self, message: &str) {
        self.flush();
        self.body.append(&widgets::banner(&util::esc(message), true));
    }

    pub fn finish(&self) {
        self.flush();
        self.spinner.stop();
        self.spinner.set_visible(false);
    }

    /// All the text written so far (for reading aloud).
    pub fn text(&self) -> String {
        self.content.borrow().clone()
    }
}
