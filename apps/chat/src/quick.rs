//! Quick ask: a small window for one-off questions, optionally about the
//! highlighted text or the clipboard. "Open in chat" keeps the exchange.

use gtk::prelude::*;
use gtk::{gdk, glib};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::markdown::Markdown;
use crate::ollama::api::Conn;
use crate::ollama::chat::{self, Event, Msg, Request};
use crate::store::{self, NewMessage};
use crate::{models, prefs, rt, tools, util, widgets};

thread_local! {
    static QUICK: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

struct Quick {
    window: gtk::Window,
    model: RefCell<String>,
    context: RefCell<Option<String>>,
    history: RefCell<Vec<(String, String)>>,
    answer: Rc<Markdown>,
    text: RefCell<String>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    input: gtk::Entry,
    status: gtk::Label,
}

/// Quick ask's model, and a note when its chosen one isn't installed.
fn quick_model() -> (String, Option<String>) {
    let want = prefs::get().quick_model;
    if want.is_empty() || models::is_installed(&want) || models::online() != Some(true) {
        return (if want.is_empty() { models::default_model() } else { want }, None);
    }
    let other = models::default_model();
    let note = format!("{} isn't installed; asking {} instead.", util::model_label(&want), util::model_label(&other));
    (other, Some(note))
}

/// Open (or focus) the window. `context` is text the question is about.
pub fn open(app: &gtk::Application, context: Option<String>) {
    if let Some(w) = QUICK.with(|q| q.borrow().clone()) {
        w.close();
    }
    crate::window::init();
    let window = gtk::Window::builder().application(app).title("Quick ask").default_width(720).default_height(480).build();
    window.add_css_class("chat-window");
    window.add_css_class("quick-window");
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));

    let root = widgets::vbox(10);
    root.add_css_class("quick-body");
    let head = widgets::hbox(8);
    let t = widgets::label("Quick ask", "panel-title");
    t.set_hexpand(true);
    head.append(&t);
    let (model, note) = quick_model();
    let ml = widgets::short_label(&util::model_label(&model), "mono", gtk::pango::EllipsizeMode::Middle);
    ml.set_max_width_chars(40);
    ml.add_css_class("dim");
    ml.add_css_class("small-text");
    head.append(&ml);
    let open_btn = gtk::Button::with_label("Open in chat");
    open_btn.set_sensitive(false);
    head.append(&open_btn);
    root.append(&head);

    if let Some(c) = context.as_ref().filter(|c| !c.trim().is_empty()) {
        let chip = widgets::vbox(4);
        chip.add_css_class("quick-context");
        chip.append(&widgets::label("ABOUT THIS TEXT", "panel-heading"));
        let l = widgets::label(&util::clip(c.trim(), 400), "dim");
        l.set_wrap(true);
        l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        chip.append(&l);
        root.append(&chip);
    }

    let answer = Markdown::new();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&answer.root)
        .build();
    root.append(&scroll);
    let status = widgets::label(note.as_deref().unwrap_or(""), "dim");
    status.add_css_class("small-text");
    root.append(&status);

    let row = widgets::hbox(8);
    let input = gtk::Entry::new();
    input.set_hexpand(true);
    input.set_placeholder_text(Some(if context.is_some() {
        "Ask about it, or press Enter to explain it"
    } else {
        "Ask anything"
    }));
    row.append(&input);
    let send = gtk::Button::from_icon_name("go-up-symbolic");
    send.add_css_class("suggested-action");
    row.append(&send);
    root.append(&row);
    window.set_child(Some(&root));

    let q = Rc::new(Quick {
        window: window.clone(),
        model: RefCell::new(model),
        context: RefCell::new(context),
        history: RefCell::new(Vec::new()),
        answer,
        text: RefCell::new(String::new()),
        cancel: RefCell::new(None),
        input: input.clone(),
        status,
    });

    // The model list may still be loading when the window opens.
    let (q2, ml2) = (q.clone(), ml.clone());
    crate::hub::on(&crate::hub::MODELS, &ml, move || {
        let (m, note) = quick_model();
        ml2.set_text(&util::model_label(&m));
        *q2.model.borrow_mut() = m.clone();
        let empty = m.is_empty() && models::online() == Some(true);
        q2.status.set_text(if empty { "No models are installed yet." } else { note.as_deref().unwrap_or("") });
    });
    let q2 = q.clone();
    let (s2, ob) = (scroll.clone(), open_btn.clone());
    let go = Rc::new(move || ask(&q2, &s2, &ob));
    let g2 = go.clone();
    input.connect_activate(move |_| g2());
    send.connect_clicked(move |_| go());
    let q2 = q.clone();
    open_btn.connect_clicked(move |_| {
        open_in_chat(&q2);
        q2.window.close();
    });

    let keys = gtk::EventControllerKey::new();
    let w2 = window.clone();
    let q2 = q.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::Escape => {
                if let Some(c) = q2.cancel.borrow().as_ref()
                    && !c.load(Ordering::Relaxed)
                {
                    c.store(true, Ordering::Relaxed);
                    return glib::Propagation::Stop;
                }
                w2.close();
                glib::Propagation::Stop
            }
            gdk::Key::w | gdk::Key::q if ctrl => {
                w2.close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);
    window.connect_close_request(|_| {
        QUICK.with(|q| *q.borrow_mut() = None);
        glib::Propagation::Proceed
    });
    QUICK.with(|qq| *qq.borrow_mut() = Some(window.clone()));
    window.present();
    input.grab_focus();
}

fn ask(q: &Rc<Quick>, scroll: &gtk::ScrolledWindow, open_btn: &gtk::Button) {
    if q.cancel.borrow().as_ref().is_some_and(|c| !c.load(Ordering::Relaxed)) || q.model.borrow().is_empty() {
        return;
    }
    let mut question = q.input.text().trim().to_string();
    let context = q.context.borrow_mut().take();
    if question.is_empty() {
        if context.is_none() {
            return;
        }
        question = "Explain this.".into();
    }
    let prompt = match &context {
        Some(c) => format!("{question}\n\n<text>\n{}\n</text>", c.trim()),
        None => question.clone(),
    };
    q.input.set_text("");
    // Keep the last answer in the history before the next question.
    let last = q.text.borrow().clone();
    if !last.is_empty()
        && let Some(h) = q.history.borrow_mut().last_mut()
    {
        h.1 = last;
    }
    q.history.borrow_mut().push((prompt, String::new()));
    q.text.borrow_mut().clear();
    q.answer.set("");
    q.status.set_text("Thinking…");

    let pr = prefs::get();
    let mut messages = Vec::new();
    let mut system = tools::now_line();
    let base = tools::expand_variables(&pr.system_prompt, &pr, None, None);
    if !base.trim().is_empty() {
        system.push_str("\n\n");
        system.push_str(base.trim());
    }
    messages.push(Msg::new("system", system));
    for (u, a) in q.history.borrow().iter() {
        messages.push(Msg::new("user", u.clone()));
        if !a.is_empty() {
            messages.push(Msg::new("assistant", a.clone()));
        }
    }
    // Quick answers use tools that never need a confirmation.
    let safe: Vec<String> =
        pr.tools_enabled.iter().filter(|t| tools::BUILTIN.iter().any(|i| i.name == t.as_str() && !i.asks)).cloned().collect();
    let model = q.model.borrow().clone();
    let md = prefs::for_model(&model);
    let local_llama = crate::llama::api::local_service();
    let info = models::info(&model);
    let can = |c: &str| info.as_ref().is_some_and(|i| i.has(c));
    let req = Request {
        conn: Conn::current(),
        model: model.clone(),
        messages,
        tools: if can("tools") { tools::definitions(&safe, pr.location_mode != "off") } else { vec![] },
        think: can("thinking").then_some(false),
        format: None,
        options: if local_llama { json!({ "temperature": md.temperature, "top_p": md.top_p, "top_k": md.top_k,
            "min_p": md.min_p, "repeat_penalty": md.repeat_penalty, "num_ctx": md.num_ctx }) }
            else { json!({ "temperature": pr.temperature, "num_ctx": pr.num_ctx }) },
        keep_alive: models::keep_alive_for(&model),
        max_rounds: 4,
        tool_ctx: tools::Ctx { prefs: pr },
    };
    let cancel = Arc::new(AtomicBool::new(false));
    *q.cancel.borrow_mut() = Some(cancel.clone());
    let (q2, s2, ob) = (q.clone(), scroll.clone(), open_btn.clone());
    rt::stream(
        move |tx| chat::run(req, tx, cancel),
        move |ev| match ev {
            Event::Delta { content, .. } if !content.is_empty() => {
                q2.status.set_text("");
                q2.text.borrow_mut().push_str(&content);
                q2.answer.set(&q2.text.borrow());
                let a = s2.vadjustment();
                a.set_value(a.upper());
            }
            Event::ToolStart(call) => q2.status.set_text(&format!("{}…", tools::title_for(&call.name))),
            Event::ToolResult { output, .. } if !output.sources.is_empty() => q2.answer.set_sources(output.sources),
            Event::Error(e) => q2.status.set_text(&e),
            Event::Done => {
                if let Some(c) = q2.cancel.borrow().as_ref() {
                    c.store(true, Ordering::Relaxed);
                }
                if q2.status.text() == "Thinking…" {
                    q2.status.set_text("");
                }
                let text = q2.text.borrow().clone();
                if let Some(h) = q2.history.borrow_mut().last_mut() {
                    h.1 = text;
                }
                ob.set_sensitive(true);
                q2.input.grab_focus();
            }
            _ => {}
        },
    );
}

fn open_in_chat(q: &Quick) {
    let host = prefs::get().host().name;
    let Ok(id) = store::create_chat(&q.model.borrow(), None, &host) else { return };
    let mut parent = None;
    for (u, a) in q.history.borrow().iter() {
        let Ok(uid) = store::add_message(NewMessage {
            chat: id,
            parent,
            role: "user",
            content: u,
            model: "",
            images: &[],
            files: &[],
            extra: Value::Null,
        }) else {
            return;
        };
        parent = Some(uid);
        if !a.is_empty()
            && let Ok(aid) = store::add_message(NewMessage {
                chat: id,
                parent,
                role: "assistant",
                content: a,
                model: &q.model.borrow(),
                images: &[],
                files: &[],
                extra: Value::Null,
            })
        {
            parent = Some(aid);
        }
    }
    if let Ok(Some(mut c)) = store::chat(id) {
        let first = q.history.borrow().first().map(|h| h.0.lines().next().unwrap_or("").to_string()).unwrap_or_default();
        c.title = util::clip(&first, 48);
        let _ = store::save_chat(&c);
    }
    if let Some(app) = q.window.application() {
        crate::window::present(&app, Some("chat"));
        crate::sections::chat::open(id);
    }
}
