//! The message box: text, attachments, tool toggles, voice and send.

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use sha2::Digest;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::send;
use crate::store::{self, FileRef};
use crate::{models, paths, prefs, tools, util, voice, widgets, window};

/// A suggestion: what it shows, and what picking it does.
type Pick = (String, Rc<dyn Fn()>);
/// Title, subtitle, and what picking it does.
type Suggestion = (String, String, Rc<dyn Fn()>);

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    /// Image file in the attachments folder.
    pub image: Option<String>,
    pub file: Option<FileRef>,
}

pub struct Composer {
    pub root: gtk::Box,
    strip: gtk::Box,
    text: gtk::TextView,
    text_scroll: gtk::ScrolledWindow,
    placeholder: gtk::Label,
    /// "/ prompts · @ model · # knowledge", on Home in wide windows.
    hint: gtk::Label,
    send: gtk::Button,
    mic: gtk::Button,
    web: gtk::ToggleButton,
    web_label: gtk::Label,
    think: gtk::ToggleButton,
    think_label: gtk::Label,
    tools_btn: gtk::MenuButton,
    tools_label: gtk::Label,
    tools_count: gtk::Label,
    kb_btn: gtk::MenuButton,
    kb_label: gtk::Label,
    /// How full the model's memory is with this chat, after the toggles.
    ctx: gtk::Box,
    ctx_ring: gtk::DrawingArea,
    ctx_frac: Rc<Cell<f64>>,
    ctx_label: gtk::Label,
    reasoning: gtk::MenuButton,
    /// The composer sits on Home, big.
    home: Cell<bool>,
    note: gtk::Label,
    attachments: RefCell<Vec<Attachment>>,
    streaming: Cell<bool>,
    busy: Cell<u32>,
    suggest: gtk::Popover,
    suggest_list: gtk::ListBox,
    suggest_items: RefCell<Vec<Pick>>,
    syncing: Cell<bool>,
}

/// The message bar's buttons: all one square size.
const BAR_BUTTON: i32 = 34;

/// Give a message-bar button its square size, centred in the row.
fn square(w: &impl IsA<gtk::Widget>) {
    w.set_size_request(BAR_BUTTON, BAR_BUTTON);
    w.set_halign(gtk::Align::Center);
    w.set_valign(gtk::Align::Center);
}

/// An on/off pill for the message bar: icon and name.
fn chip(icon: &str, text: &str, tip: &str) -> (gtk::ToggleButton, gtk::Label) {
    let b = gtk::ToggleButton::new();
    b.add_css_class("bar-pill");
    let row = widgets::hbox(6);
    row.append(&gtk::Image::from_icon_name(icon));
    let l = widgets::label(text, "");
    row.append(&l);
    b.set_child(Some(&row));
    b.set_tooltip_text(Some(tip));
    b.set_valign(gtk::Align::Center);
    (b, l)
}

/// A pill that opens a menu: icon, name and an optional count.
fn menu_pill(icon: &str, text: &str, tip: &str) -> (gtk::MenuButton, gtk::Label, gtk::Label) {
    let b = gtk::MenuButton::new();
    b.add_css_class("bar-pill");
    let row = widgets::hbox(6);
    row.append(&gtk::Image::from_icon_name(icon));
    let l = widgets::label(text, "");
    row.append(&l);
    let n = widgets::label("", "pill-count");
    n.add_css_class("tnum");
    row.append(&n);
    b.set_child(Some(&row));
    b.set_tooltip_text(Some(tip));
    b.set_valign(gtk::Align::Center);
    (b, l, n)
}

/// The memory ring: how much of the model's context the chat fills.
fn ring(frac: Rc<Cell<f64>>) -> gtk::DrawingArea {
    let ring = gtk::DrawingArea::new();
    ring.set_content_width(18);
    ring.set_content_height(18);
    ring.set_valign(gtk::Align::Center);
    ring.set_draw_func(move |w, cr, width, height| {
        let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
        let r = cx.min(cy) - 2.0;
        let fg = w.color();
        cr.set_line_width(3.0);
        cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.22);
        cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
        let f = frac.get().clamp(0.0, 1.0);
        if f > 0.0 {
            let c = if w.parent().is_some_and(|p| p.has_css_class("hot")) {
                crate::theme::Rgb::hex(&crate::theme::palette().danger)
            } else {
                crate::theme::Rgb::hex(&crate::theme::palette().accent)
            };
            cr.set_source_rgb(c.0, c.1, c.2);
            let start = -std::f64::consts::FRAC_PI_2;
            cr.arc(cx, cy, r, start, start + f * std::f64::consts::TAU);
            let _ = cr.stroke();
        }
    });
    ring
}

/// Switch the chat on screen to `name`, as if picked in the picker.
fn switch_model(name: &str) {
    let Some(p) = super::page() else { return };
    p.st.borrow_mut().chat_mut().model = name.to_string();
    crate::models::pick(name);
    super::save_chat(&p);
    super::refresh_header(&p);
    super::refresh_banners(&p);
}

impl Composer {
    pub fn new() -> Rc<Composer> {
        let root = widgets::vbox(0);
        root.add_css_class("composer-wrap");
        let frame = widgets::vbox(6);
        frame.add_css_class("composer");
        root.append(&frame);

        let strip = widgets::hbox(8);
        strip.add_css_class("attach-strip");
        strip.set_visible(false);
        frame.append(&strip);

        let text = gtk::TextView::new();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.add_css_class("composer-text");
        text.set_accepts_tab(false);
        text.set_top_margin(4);
        text.set_bottom_margin(4);
        let placeholder = widgets::label("Ask a follow-up…", "composer-placeholder");
        placeholder.set_can_target(false);
        let hint = widgets::label("/ prompts · @ model · # knowledge", "composer-hint");
        hint.set_can_target(false);
        hint.set_visible(false);
        let tover = gtk::Overlay::new();
        let tscroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .propagate_natural_height(true)
            .max_content_height(220)
            .child(&text)
            .build();
        tover.set_child(Some(&tscroll));
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_halign(gtk::Align::Start);
        tover.add_overlay(&placeholder);
        hint.set_valign(gtk::Align::Start);
        hint.set_halign(gtk::Align::End);
        tover.add_overlay(&hint);
        frame.append(&tover);
        let ph = placeholder.clone();
        text.buffer().connect_changed(move |b| ph.set_visible(b.char_count() == 0));

        let row = widgets::hbox(6);
        row.add_css_class("composer-row");
        // Files, images and screenshots, from one +.
        let attach = gtk::MenuButton::new();
        attach.set_icon_name("list-add-symbolic");
        attach.add_css_class("bar-menu");
        attach.set_tooltip_text(Some("Attach a file, image or screenshot"));
        square(&attach);
        let attach_pop = gtk::Popover::new();
        attach_pop.add_css_class("menu");
        let attach_list = widgets::vbox(1);
        let files = gtk::Button::with_label("Attach files or images…");
        let shot = gtk::Button::with_label("Screenshot of an area  (Ctrl+Shift+S)");
        for b in [&files, &shot] {
            b.add_css_class("flat");
            b.add_css_class("menu-row");
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let p = attach_pop.clone();
            b.connect_clicked(move |_| p.popdown());
            attach_list.append(b);
        }
        attach_pop.set_child(Some(&attach_list));
        attach.set_popover(Some(&attach_pop));
        row.append(&attach);

        let chips = widgets::hbox(6);
        chips.set_hexpand(true);
        let (web, web_label) = chip("web-browser-symbolic", "Web search", "Web search");
        let (think, think_label) = chip("emoji-objects-symbolic", "Think", "Think before answering");
        let (tools_btn, tools_label, tools_count) =
            menu_pill("applications-engineering-symbolic", "Tools", "Tools for this chat (location, calculator, files…)");
        let (kb_btn, kb_label, _) = menu_pill("accessories-dictionary-symbolic", "Knowledge", "Knowledge: answer from your documents (#)");
        let reasoning = gtk::MenuButton::new();
        reasoning.add_css_class("reasoning-level");
        reasoning.set_tooltip_text(Some("Reasoning effort for this chat"));
        reasoning.set_visible(false);
        for w in [
            web.upcast_ref::<gtk::Widget>(),
            think.upcast_ref(),
            reasoning.upcast_ref(),
            tools_btn.upcast_ref(),
            kb_btn.upcast_ref(),
        ] {
            chips.append(w);
        }
        let fill = widgets::hbox(0);
        fill.set_hexpand(true);
        chips.append(&fill);
        // Memory use: a ring and "14% of memory", quiet like the pills beside it.
        let ctx = widgets::hbox(6);
        ctx.add_css_class("ctx-readout");
        ctx.set_valign(gtk::Align::Center);
        ctx.set_visible(false);
        let ctx_frac = Rc::new(Cell::new(0.0));
        let ctx_ring = ring(ctx_frac.clone());
        let ctx_label = widgets::label("", "ctx-label");
        ctx.append(&ctx_ring);
        ctx.append(&ctx_label);
        chips.append(&ctx);
        // Chips wrap away on narrow windows rather than widening them.
        let chip_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&chips)
            .build();
        row.append(&chip_scroll);

        let mic = gtk::Button::from_icon_name("audio-input-microphone-symbolic");
        mic.add_css_class("flat");
        mic.add_css_class("bar-toggle");
        mic.set_tooltip_text(Some("Dictate (Ctrl+Space)"));
        square(&mic);
        row.append(&mic);
        let send = gtk::Button::from_icon_name("go-up-symbolic");
        send.add_css_class("suggested-action");
        send.add_css_class("send-button");
        send.set_tooltip_text(Some("Send (Enter)"));
        square(&send);
        row.append(&send);
        frame.append(&row);

        let note = widgets::label("", "composer-note");
        note.set_xalign(0.5);
        note.set_visible(false);
        root.append(&note);

        let suggest = gtk::Popover::new();
        suggest.set_autohide(false);
        suggest.set_has_arrow(false);
        suggest.set_position(gtk::PositionType::Top);
        suggest.add_css_class("suggest");
        let suggest_list = gtk::ListBox::new();
        suggest_list.set_selection_mode(gtk::SelectionMode::Single);
        let sscroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(260)
            .propagate_natural_height(true)
            .child(&suggest_list)
            .build();
        sscroll.set_size_request(320, -1);
        suggest.set_child(Some(&sscroll));
        suggest.set_parent(&frame);

        let c = Rc::new(Composer {
            root,
            strip,
            text,
            text_scroll: tscroll,
            placeholder,
            hint,
            send,
            mic,
            web,
            web_label,
            think,
            think_label,
            tools_btn,
            tools_label,
            tools_count,
            kb_btn,
            kb_label,
            ctx,
            ctx_ring,
            ctx_frac,
            ctx_label,
            reasoning,
            home: Cell::new(false),
            note,
            attachments: RefCell::new(Vec::new()),
            streaming: Cell::new(false),
            busy: Cell::new(0),
            suggest,
            suggest_list,
            suggest_items: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
        });
        c.connect(&files, &shot);
        let me = Rc::downgrade(&c);
        window::on_narrow(move |_| {
            if let Some(c) = me.upgrade() {
                c.sync_labels();
            }
        });
        c
    }

    /// Home's big box, or the bar under a conversation.
    pub fn set_home(&self, on: bool) {
        self.home.set(on);
        if on {
            self.root.add_css_class("big");
        } else {
            self.root.remove_css_class("big");
        }
        self.text_scroll.set_min_content_height(if on { 60 } else { -1 });
        self.placeholder.set_text(if on { "Ask anything, or drop a file here" } else { "Ask a follow-up…" });
        self.sync_labels();
    }

    /// Pills keep their names in wide windows; in narrow ones only the ones that are on.
    fn sync_labels(&self) {
        let narrow = window::narrow();
        let home = self.home.get();
        self.web_label.set_visible(!narrow || self.web.is_active());
        self.think_label.set_visible(!narrow || self.think.is_active());
        self.tools_label.set_visible(!narrow);
        self.kb_label.set_visible(home && !narrow);
        self.hint.set_visible(home && !narrow);
        self.ctx_label.set_visible(!narrow);
    }

    fn connect(self: &Rc<Self>, attach: &gtk::Button, shot: &gtk::Button) {
        let me = Rc::downgrade(self);
        self.send.connect_clicked(move |_| {
            if let Some(c) = me.upgrade() {
                if c.streaming.get() {
                    super::stop();
                } else {
                    c.submit();
                }
            }
        });
        let me = Rc::downgrade(self);
        attach.connect_clicked(move |_| {
            let me = me.clone();
            widgets::open_files(
                "Attach",
                &[
                    ("Images and documents", &["*.png", "*.jpg", "*.jpeg", "*.webp", "*.gif", "*.pdf", "*.txt", "*.md", "*.*"]),
                    ("Images", &["*.png", "*.jpg", "*.jpeg", "*.webp", "*.gif"]),
                ],
                move |files| {
                    if let Some(c) = me.upgrade() {
                        c.attach_paths(files);
                    }
                },
            );
        });
        let me = Rc::downgrade(self);
        shot.connect_clicked(move |_| {
            if let Some(c) = me.upgrade() {
                c.screenshot();
            }
        });
        let me = Rc::downgrade(self);
        self.mic.connect_clicked(move |_| {
            if let Some(c) = me.upgrade() {
                c.toggle_dictation();
            }
        });

        // Tool chips change this chat's tool list.
        let me = Rc::downgrade(self);
        self.web.connect_toggled(move |b| {
            if let Some(c) = me.upgrade() {
                c.sync_labels();
                if !c.syncing.get() {
                    set_tools(&["web_search", "web_fetch"], b.is_active());
                }
            }
        });
        let me = Rc::downgrade(self);
        self.think.connect_toggled(move |b| {
            if let Some(c) = me.upgrade() {
                c.sync_labels();
                if !c.syncing.get()
                    && let Some(p) = super::page()
                {
                    p.st.borrow_mut().chat_mut().params.think = Some(b.is_active());
                    super::save_chat(&p);
                    c.reasoning.set_sensitive(b.is_active());
                }
            }
        });
        self.reasoning.set_create_popup_func(|m| m.set_popover(Some(&reasoning_popover())));
        self.tools_btn.set_create_popup_func(|m| m.set_popover(Some(&tools_popover())));
        self.kb_btn.set_create_popup_func(|m| m.set_popover(Some(&knowledge_popover())));

        // Keys: Enter sends, Shift+Enter breaks the line, Esc stops.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(c) = me.upgrade() else { return glib::Propagation::Proceed };
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            if c.suggest.is_visible() {
                match key {
                    gdk::Key::Up | gdk::Key::Down => {
                        let rows = c.suggest_items.borrow().len() as i32;
                        let cur = c.suggest_list.selected_row().map(|r| r.index()).unwrap_or(0);
                        let next = if key == gdk::Key::Up { (cur - 1).max(0) } else { (cur + 1).min(rows - 1) };
                        if let Some(r) = c.suggest_list.row_at_index(next) {
                            c.suggest_list.select_row(Some(&r));
                        }
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                        let i = c.suggest_list.selected_row().map(|r| r.index()).unwrap_or(0) as usize;
                        let f = c.suggest_items.borrow().get(i).map(|x| x.1.clone());
                        c.suggest.popdown();
                        if let Some(f) = f {
                            f();
                        }
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::Escape => {
                        c.suggest.popdown();
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            match key {
                gdk::Key::Return | gdk::Key::KP_Enter if (prefs::get().enter_sends && !shift) || ctrl => {
                    if !c.streaming.get() {
                        c.submit();
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Escape if c.streaming.get() => {
                    super::stop();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.text.add_controller(keys);

        let me = Rc::downgrade(self);
        self.text.buffer().connect_changed(move |_| {
            if let Some(c) = me.upgrade() {
                c.update_suggestions();
            }
        });
        let me = Rc::downgrade(self);
        self.suggest_list.connect_row_activated(move |_, row| {
            if let Some(c) = me.upgrade() {
                let f = c.suggest_items.borrow().get(row.index() as usize).map(|x| x.1.clone());
                c.suggest.popdown();
                if let Some(f) = f {
                    f();
                }
            }
        });

        // Pasting an image attaches it.
        let me = Rc::downgrade(self);
        self.text.connect_paste_clipboard(move |tv| {
            let Some(c) = me.upgrade() else { return };
            let cb = tv.clipboard();
            if cb.formats().contains_type(gdk::Texture::static_type()) && !cb.formats().contain_mime_type("text/plain") {
                tv.stop_signal_emission_by_name("paste-clipboard");
                let c2 = c.clone();
                cb.read_texture_async(gtk::gio::Cancellable::NONE, move |r| {
                    if let Ok(Some(tex)) = r {
                        c2.attach_texture(&tex);
                    }
                });
            }
        });
    }

    pub fn focus(&self) {
        self.text.grab_focus();
    }

    fn text(&self) -> String {
        widgets::buffer_text(&self.text)
    }

    pub fn insert_prompt(&self, content: &str) {
        let pr = prefs::get();
        let clip = content.contains("{{CLIPBOARD}}").then(|| crate::cmd::output(&["wl-paste", "-n"]).unwrap_or_default());
        let filled = tools::expand_variables(content, &pr, None, clip.as_deref());
        self.text.buffer().set_text(&filled);
        self.focus();
    }

    /// Quote `selection` at the end of the message, then `ask` after it.
    pub fn quote(&self, selection: &str, ask: &str) {
        let buf = self.text.buffer();
        let current = widgets::buffer_text(&self.text);
        let quoted: String = selection.trim().lines().map(|l| format!("> {l}\n")).collect();
        let mut text = current.trim_end().to_string();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&quoted);
        text.push('\n');
        text.push_str(ask);
        buf.set_text(&text);
        buf.place_cursor(&buf.end_iter());
        self.focus();
    }

    /// Show how many tokens of the context the chat uses; `None` hides it.
    pub fn set_context(&self, used: Option<(u64, u64)>) {
        let Some((used, limit)) = used else {
            self.ctx.set_visible(false);
            return;
        };
        let f = (used as f64 / limit as f64).min(1.0);
        self.ctx_frac.set(f);
        self.ctx_ring.queue_draw();
        self.ctx_label.set_text(&format!("{:.0}% of memory", (f * 100.0).max(1.0)));
        if f >= 0.9 {
            self.ctx.add_css_class("hot");
        } else {
            self.ctx.remove_css_class("hot");
        }
        self.ctx.set_tooltip_text(Some(&format!(
            "How much of the model's memory this chat uses: {} of {} tokens ({:.0}%)",
            util::count(used),
            util::count(limit),
            f * 100.0
        )));
        self.ctx.set_visible(true);
    }

    pub fn clear_draft_for_switch(&self) {
        self.suggest.popdown();
    }

    pub(super) fn submit(self: &Rc<Self>) {
        if self.busy.get() > 0 {
            window::toast("Still reading an attachment…");
            return;
        }
        let text = self.text().trim().to_string();
        let atts = self.attachments.borrow().clone();
        if text.is_empty() && atts.is_empty() {
            return;
        }
        let Some(p) = super::page() else { return };
        self.text.buffer().set_text("");
        self.attachments.borrow_mut().clear();
        self.redraw_strip();
        send::submit(&p, text, atts);
    }

    pub fn set_streaming(&self, on: bool) {
        self.streaming.set(on);
        self.send.set_icon_name(if on { "media-playback-stop-symbolic" } else { "go-up-symbolic" });
        self.send.set_tooltip_text(Some(if on { "Stop (Esc)" } else { "Send (Enter)" }));
        if on {
            self.send.remove_css_class("suggested-action");
            self.send.add_css_class("destructive-action");
        } else {
            self.send.remove_css_class("destructive-action");
            self.send.add_css_class("suggested-action");
        }
    }

    /// Sync chips with the chat and model on screen.
    pub fn refresh(&self) {
        let Some(p) = super::page() else { return };
        let (chat, streaming) = {
            let st = p.st.borrow();
            (st.chat().clone(), super::streaming(st.id()))
        };
        self.set_streaming(streaming);
        let pr = prefs::get();
        let folder_tools = chat.folder.and_then(store::folder).map(|f| f.tools).filter(|t| !t.is_empty());
        let enabled = chat.params.tools.clone().or(folder_tools).unwrap_or_else(|| pr.tools_enabled.clone());
        let info = models::info(&chat.model);
        let tools_ok = info.as_ref().is_none_or(|i| i.has("tools"));
        self.syncing.set(true);
        let web_on = if tools_ok {
            enabled.iter().any(|t| t == "web_search")
        } else {
            chat.params.tools.as_ref().is_some_and(|t| t.iter().any(|x| x == "web_search"))
        };
        self.web.set_active(web_on);
        // Web search has its own pill; the rest count under Tools.
        let others = tools::BUILTIN
            .iter()
            .filter(|t| !matches!(t.name, "web_search" | "web_fetch"))
            .filter(|t| t.name != "get_location" || pr.location_mode != "off")
            .filter(|t| enabled.iter().any(|e| e == t.name))
            .count();
        self.tools_count.set_text(&if tools_ok && others > 0 { others.to_string() } else { String::new() });
        self.tools_count.set_visible(tools_ok && others > 0);
        if tools_ok && others > 0 {
            self.tools_btn.add_css_class("selected");
        } else {
            self.tools_btn.remove_css_class("selected");
        }
        let thinks = info.as_ref().is_some_and(|i| i.has("thinking"));
        self.think.set_visible(thinks);
        self.think.set_active(chat.params.think.unwrap_or(pr.think));
        let levels = info.as_ref().map(|i| i.reasoning_efforts.as_slice()).unwrap_or(&[]);
        let hidden = pr.reasoning_hidden.get(&chat.model).map(Vec::as_slice).unwrap_or(&[]);
        let choices = crate::llama::reasoning::options(levels, hidden);
        self.reasoning.set_visible(!choices.is_empty());
        self.reasoning.set_sensitive(chat.params.think.unwrap_or(pr.think));
        let level = crate::llama::reasoning::effective(&choices, chat.params.reasoning_effort.as_deref())
            .and_then(crate::llama::reasoning::label).unwrap_or("");
        let label = if level.is_empty() { "Reasoning".to_string() } else { format!("Reasoning: {level}") };
        self.reasoning.set_label(&label);
        self.syncing.set(false);
        self.web.set_tooltip_text(Some(if tools_ok {
            "Web search"
        } else {
            "Web search before every message (this model can't decide when to search)"
        }));
        let kb = chat.knowledge.len();
        self.kb_btn.set_visible(!store::kbases().unwrap_or_default().is_empty());
        if kb > 0 {
            self.kb_btn.add_css_class("selected");
        } else {
            self.kb_btn.remove_css_class("selected");
        }
        let mut notes = Vec::new();
        if !tools_ok && info.is_some() {
            notes.push(format!("{} can't use tools", chat.model));
        }
        if chat.params.json {
            notes.push("JSON replies".into());
        }
        self.note.set_text(&notes.join(" · "));
        self.note.set_visible(!notes.is_empty());
        self.sync_labels();
    }

    // ---------- Suggestions: / prompts, @ models, # knowledge ----------

    fn update_suggestions(self: &Rc<Self>) {
        let t = self.text();
        let first = t.chars().next();
        let word_done = t.contains(char::is_whitespace);
        let mut items: Vec<Suggestion> = Vec::new();
        // "/model" (or the start of it) lists models; "/model qwen" filters them.
        let model_query = match t.strip_prefix("/model") {
            Some(rest) if rest.is_empty() || rest.starts_with(' ') => Some(rest.trim().to_lowercase()),
            _ if !word_done && t.len() > 1 && "/model".starts_with(t.as_str()) => Some(String::new()),
            _ => None,
        };
        if let Some(q) = model_query {
            for m in models::chat_models().into_iter().filter(|m| m.to_lowercase().contains(&q)) {
                let me = Rc::downgrade(self);
                let name = m.clone();
                items.push((
                    util::model_label(&m),
                    if models::is_loaded(&m) { "Loaded".into() } else { "Switch to this model".into() },
                    Rc::new(move || {
                        if let Some(c) = me.upgrade() {
                            c.text.buffer().set_text("");
                        }
                        switch_model(&name);
                    }),
                ));
            }
        }
        match first {
            Some('/') if !word_done && t.starts_with("/model") => {}
            Some('/') if !word_done => {
                let q = t[1..].to_lowercase();
                for pr in store::prompts().unwrap_or_default() {
                    if pr.command.to_lowercase().contains(&q) || pr.title.to_lowercase().contains(&q) {
                        let me = Rc::downgrade(self);
                        let content = pr.content.clone();
                        items.push((
                            format!("/{}", pr.command),
                            pr.title.clone(),
                            Rc::new(move || {
                                if let Some(c) = me.upgrade() {
                                    c.insert_prompt(&content);
                                }
                            }),
                        ));
                    }
                }
            }
            Some('@') if !word_done => {
                let q = t[1..].to_lowercase();
                for m in models::chat_models().into_iter().filter(|m| m.to_lowercase().contains(&q)) {
                    let me = Rc::downgrade(self);
                    let name = m.clone();
                    items.push((
                        m.clone(),
                        "Switch model".into(),
                        Rc::new(move || {
                            if let Some(c) = me.upgrade() {
                                c.text.buffer().set_text("");
                            }
                            switch_model(&name);
                        }),
                    ));
                }
            }
            Some('#') if !word_done => {
                let q = t[1..].to_lowercase();
                for kb in store::kbases().unwrap_or_default().into_iter().filter(|k| k.name.to_lowercase().contains(&q)) {
                    let me = Rc::downgrade(self);
                    let id = kb.id;
                    items.push((
                        format!("#{}", kb.name),
                        format!("{} document{}", kb.docs.len(), if kb.docs.len() == 1 { "" } else { "s" }),
                        Rc::new(move || {
                            if let Some(c) = me.upgrade() {
                                c.text.buffer().set_text("");
                            }
                            if let Some(p) = super::page() {
                                let mut st = p.st.borrow_mut();
                                let k = &mut st.chat_mut().knowledge;
                                if !k.contains(&id) {
                                    k.push(id);
                                }
                                drop(st);
                                super::save_chat(&p);
                                window::toast("Knowledge added to this chat");
                                p.composer.refresh();
                            }
                        }),
                    ));
                }
            }
            _ => {}
        }
        while let Some(r) = self.suggest_list.first_child() {
            self.suggest_list.remove(&r);
        }
        if items.is_empty() {
            self.suggest.popdown();
            self.suggest_items.borrow_mut().clear();
            return;
        }
        for (title, sub, _) in &items {
            let v = widgets::vbox(1);
            v.add_css_class("suggest-row");
            let t = widgets::label(title, "mono");
            t.set_ellipsize(pango::EllipsizeMode::End);
            v.append(&t);
            if !sub.is_empty() {
                let s = widgets::label(sub, "dim");
                s.add_css_class("small-text");
                s.set_ellipsize(pango::EllipsizeMode::End);
                v.append(&s);
            }
            self.suggest_list.append(&v);
        }
        if let Some(r) = self.suggest_list.row_at_index(0) {
            self.suggest_list.select_row(Some(&r));
        }
        *self.suggest_items.borrow_mut() = items.into_iter().map(|(t, _, f)| (t, f)).collect();
        self.suggest.popup();
    }

    // ---------- Attachments ----------

    fn redraw_strip(self: &Rc<Self>) {
        widgets::clear(&self.strip);
        let atts = self.attachments.borrow().clone();
        self.strip.set_visible(!atts.is_empty() || self.busy.get() > 0);
        for (i, a) in atts.iter().enumerate() {
            let chip = widgets::hbox(6);
            chip.add_css_class("attach-chip");
            if let Some(img) = &a.image {
                let pic = gtk::Picture::for_filename(img);
                pic.set_content_fit(gtk::ContentFit::Cover);
                pic.set_size_request(36, 36);
                pic.add_css_class("thumb");
                chip.append(&pic);
            } else {
                chip.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            }
            let l = widgets::label(&a.name, "");
            l.set_ellipsize(pango::EllipsizeMode::Middle);
            l.set_max_width_chars(18);
            chip.append(&l);
            if let Some(f) = &a.file {
                let n = widgets::label(&format!("{}k", f.chars / 1000), "dim");
                n.add_css_class("tnum");
                n.add_css_class("small-text");
                chip.append(&n);
            }
            let x = gtk::Button::from_icon_name("window-close-symbolic");
            x.add_css_class("flat");
            x.add_css_class("tiny");
            let me = Rc::downgrade(self);
            x.connect_clicked(move |_| {
                if let Some(c) = me.upgrade() {
                    if i < c.attachments.borrow().len() {
                        c.attachments.borrow_mut().remove(i);
                    }
                    c.redraw_strip();
                }
            });
            chip.append(&x);
            self.strip.append(&chip);
        }
        if self.busy.get() > 0 {
            let sp = gtk::Spinner::new();
            sp.start();
            self.strip.append(&sp);
        }
    }

    pub fn attach_paths(self: &Rc<Self>, paths_in: Vec<PathBuf>) {
        for path in paths_in {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") {
                match store_copy(&path, &ext) {
                    Ok(dest) => self.attachments.borrow_mut().push(Attachment { name, image: Some(dest), file: None }),
                    Err(e) => window::toast(&format!("Couldn't attach {name}: {e}")),
                }
                continue;
            }
            self.busy.set(self.busy.get() + 1);
            let me = Rc::downgrade(self);
            crate::cmd::background(
                move || -> anyhow::Result<FileRef> {
                    let text = tools::files::extract_text(&path)?;
                    let hash = hex16(text.as_bytes());
                    let dest = paths::attachments_dir().join(format!("{hash}.txt"));
                    std::fs::create_dir_all(paths::attachments_dir())?;
                    std::fs::write(&dest, &text)?;
                    Ok(FileRef { name: name.clone(), path: dest.to_string_lossy().into(), chars: text.chars().count() })
                },
                move |r| {
                    let Some(c) = me.upgrade() else { return };
                    c.busy.set(c.busy.get().saturating_sub(1));
                    match r {
                        Ok(f) => c.attachments.borrow_mut().push(Attachment { name: f.name.clone(), image: None, file: Some(f) }),
                        Err(e) => window::toast(&format!("{e:#}")),
                    }
                    c.redraw_strip();
                },
            );
        }
        self.redraw_strip();
        self.focus();
    }

    fn attach_texture(self: &Rc<Self>, tex: &gdk::Texture) {
        let bytes = tex.save_to_png_bytes();
        let hash = hex16(&bytes);
        let dest = paths::attachments_dir().join(format!("{hash}.png"));
        let _ = std::fs::create_dir_all(paths::attachments_dir());
        if std::fs::write(&dest, &bytes).is_ok() {
            self.attachments.borrow_mut().push(Attachment {
                name: "Pasted image".into(),
                image: Some(dest.to_string_lossy().into()),
                file: None,
            });
            self.redraw_strip();
        }
    }

    pub fn screenshot(self: &Rc<Self>) {
        if !crate::cmd::present("grim") || !crate::cmd::present("slurp") {
            window::toast("Screenshots need grim and slurp");
            return;
        }
        let me = Rc::downgrade(self);
        crate::cmd::background(capture_region, move |r| match r {
            Ok(path) => {
                if let Some(c) = me.upgrade() {
                    c.attach_paths(vec![path]);
                    let model = super::page().map(|p| p.st.borrow().chat().model.clone()).unwrap_or_default();
                    if models::info(&model).is_some_and(|i| !i.has("vision")) {
                        window::toast(&format!("{model} can't see images. Pick a vision model to ask about it."));
                    }
                }
            }
            Err(e) if e.to_string().contains("cancel") => {}
            Err(e) => window::toast(&format!("Screenshot failed: {e}")),
        });
    }

    pub fn toggle_dictation(self: &Rc<Self>) {
        if voice::recording() {
            self.mic.remove_css_class("recording");
            self.mic.set_sensitive(false);
            let me = Rc::downgrade(self);
            voice::stop_and_transcribe(move |r| {
                let Some(c) = me.upgrade() else { return };
                c.mic.set_sensitive(true);
                match r {
                    Ok(text) if !text.trim().is_empty() => {
                        let buf = c.text.buffer();
                        let mut end = buf.end_iter();
                        let sep = if buf.char_count() > 0 { " " } else { "" };
                        buf.insert(&mut end, &format!("{sep}{}", text.trim()));
                        if prefs::get().hands_free {
                            c.submit();
                        } else {
                            c.focus();
                        }
                    }
                    Ok(_) => window::toast("Didn't catch anything"),
                    Err(e) => window::toast(&format!("{e:#}")),
                }
            });
        } else {
            match voice::start_recording() {
                Ok(()) => {
                    self.mic.add_css_class("recording");
                    voice::stop_speaking();
                }
                Err(e) => window::toast(&format!("{e:#}")),
            }
        }
    }
}

/// Turn tools on or off for the chat on screen.
fn set_tools(names: &[&str], on: bool) {
    let Some(p) = super::page() else { return };
    {
        let mut st = p.st.borrow_mut();
        let pr = prefs::get();
        let folder_tools = st.chat().folder.and_then(store::folder).map(|f| f.tools).filter(|t| !t.is_empty());
        let chat = st.chat_mut();
        let mut list = chat.params.tools.clone().or(folder_tools).unwrap_or(pr.tools_enabled);
        list.retain(|t| !names.contains(&t.as_str()));
        if on {
            list.extend(names.iter().map(|s| s.to_string()));
        }
        chat.params.tools = Some(list);
    }
    super::save_chat(&p);
    p.composer.refresh();
}

fn tools_popover() -> gtk::Popover {
    let pop = gtk::Popover::new();
    let v = widgets::vbox(4);
    v.add_css_class("popover-body");
    v.append(&widgets::label("TOOLS FOR THIS CHAT", "menu-heading-small"));
    let Some(p) = super::page() else { return pop };
    let chat = p.st.borrow().chat().clone();
    let pr = prefs::get();
    if models::info(&chat.model).is_some_and(|i| !i.has("tools")) {
        let n = widgets::label(
            &format!("{} can't call tools. Turn on Web to search before every message instead.", chat.model),
            "dim",
        );
        n.set_wrap(true);
        n.set_max_width_chars(36);
        v.append(&n);
    }
    let folder_tools = chat.folder.and_then(store::folder).map(|f| f.tools).filter(|t| !t.is_empty());
    let enabled = chat.params.tools.clone().or(folder_tools).unwrap_or_else(|| pr.tools_enabled.clone());
    let tools_ok = models::info(&chat.model).is_none_or(|i| i.has("tools"));
    for t in tools::BUILTIN {
        if t.name == "get_location" && pr.location_mode == "off" {
            continue;
        }
        let c = gtk::CheckButton::with_label(t.title);
        let on = if !tools_ok && t.name == "web_search" {
            chat.params.tools.as_ref().is_some_and(|l| l.iter().any(|x| x == "web_search"))
        } else {
            enabled.iter().any(|e| e == t.name)
        };
        c.set_active(on);
        c.set_tooltip_text(Some(t.description));
        let name = t.name;
        c.connect_toggled(move |c| {
            if name == "web_search" {
                set_tools(&["web_search", "web_fetch"], c.is_active());
            } else {
                set_tools(&[name], c.is_active());
            }
        });
        v.append(&c);
    }
    let mcp = tools::mcp::definitions().len();
    if mcp > 0 {
        let l = widgets::label(&format!("Plus {mcp} tool{} from MCP servers.", if mcp == 1 { "" } else { "s" }), "dim");
        v.append(&l);
    }
    let reset = gtk::Button::with_label("Use the defaults from Settings");
    reset.add_css_class("flat");
    let pop2 = pop.clone();
    reset.connect_clicked(move |_| {
        if let Some(p) = super::page() {
            p.st.borrow_mut().chat_mut().params.tools = None;
            super::save_chat(&p);
            p.composer.refresh();
        }
        pop2.popdown();
    });
    v.append(&reset);
    pop.set_child(Some(&v));
    pop
}

fn reasoning_popover() -> gtk::Popover {
    let pop = gtk::Popover::new();
    let v = widgets::vbox(4);
    v.add_css_class("popover-body");
    v.append(&widgets::label("REASONING LEVEL", "menu-heading-small"));
    let Some(p) = super::page() else { return pop };
    let model = p.st.borrow().chat().model.clone();
    let levels = models::info(&model).map(|i| i.reasoning_efforts).unwrap_or_default();
    let hidden = prefs::get().reasoning_hidden.get(&model).cloned().unwrap_or_default();
    for (label, level) in crate::llama::reasoning::options(&levels, &hidden) {
        let button = gtk::Button::with_label(label);
        button.add_css_class("flat");
        let pop2 = pop.clone();
        button.connect_clicked(move |_| {
            if let Some(p) = super::page() {
                p.st.borrow_mut().chat_mut().params.reasoning_effort = Some(level.to_owned());
                super::save_chat(&p);
                p.composer.refresh();
            }
            pop2.popdown();
        });
        v.append(&button);
    }
    pop.set_child(Some(&v));
    pop
}

fn knowledge_popover() -> gtk::Popover {
    let pop = gtk::Popover::new();
    let v = widgets::vbox(4);
    v.add_css_class("popover-body");
    v.append(&widgets::label("ANSWER FROM", "menu-heading-small"));
    let Some(p) = super::page() else { return pop };
    let selected = p.st.borrow().chat().knowledge.clone();
    for kb in store::kbases().unwrap_or_default() {
        let c = gtk::CheckButton::with_label(&kb.name);
        c.set_active(selected.contains(&kb.id));
        let id = kb.id;
        c.connect_toggled(move |c| {
            if let Some(p) = super::page() {
                {
                    let mut st = p.st.borrow_mut();
                    let k = &mut st.chat_mut().knowledge;
                    k.retain(|x| *x != id);
                    if c.is_active() {
                        k.push(id);
                    }
                }
                super::save_chat(&p);
                p.composer.refresh();
            }
        });
        v.append(&c);
    }
    let manage = gtk::Button::with_label("Manage knowledge…");
    manage.add_css_class("flat");
    let pop2 = pop.clone();
    manage.connect_clicked(move |_| {
        pop2.popdown();
        window::navigate("knowledge");
    });
    v.append(&manage);
    pop.set_child(Some(&v));
    pop
}

fn hex16(bytes: &[u8]) -> String {
    let d = sha2::Sha256::digest(bytes);
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Copy an image into the attachments folder, named by its content.
fn store_copy(path: &Path, ext: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    let dest = paths::attachments_dir().join(format!("{}.{ext}", hex16(&bytes)));
    std::fs::create_dir_all(paths::attachments_dir())?;
    if !dest.exists() {
        std::fs::write(&dest, &bytes)?;
    }
    Ok(dest.to_string_lossy().into())
}

/// Let the user drag out an area with slurp, then capture it with grim.
pub fn capture_region() -> anyhow::Result<PathBuf> {
    let geometry = crate::cmd::run(&["slurp"]).map_err(|_| anyhow::anyhow!("cancelled"))?;
    let dir = paths::attachments_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("screenshot-{}.png", util::now()));
    let t = tmp.to_string_lossy().to_string();
    crate::cmd::run(&["grim", "-g", geometry.trim(), &t])?;
    Ok(tmp)
}
