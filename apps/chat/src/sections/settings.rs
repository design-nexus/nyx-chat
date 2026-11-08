//! Settings.

use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use crate::prefs::{self, Backend, Host, McpServer};
use crate::widgets::{self, Group, Page};
use crate::{hf, hub, models, paths, store, tools, transfer, util, voice, window};

/// The choices for a chat that outgrows its context.
pub const OVERFLOW: &[(&str, &str)] =
    &[("rolling", "Leave out the oldest messages"), ("keep_first", "Keep the first exchange"), ("stop", "Stop and say so")];

/// Fill `page` with every settings group; the settings card shows one at a time.
pub fn build(page: &Page) {
    connection(page);
    local_server_settings(page);
    hugging_face(page);
    chat_defaults(page);
    tools_group(page);
    web_search(page);
    location(page);
    memories(page);
    knowledge(page);
    voice_group(page);
    quick_ask(page);
    mcp(page);
    data(page);
}

fn local_server_settings(page: &Page) {
    if !crate::llama::api::installed() { return; }
    let g = page.group("Local models (Nyx AI Core)");
    let p = prefs::get();
    let (r, _) = widgets::choice_row(
        "Models kept loaded",
        "More loaded models switch faster but use more memory. Shared by every Nyx app.",
        widgets::opts(&[("1", "One"), ("2", "Two"), ("4", "Four")]),
        &p.llama_models_max.to_string(),
        |v| {
            prefs::update(|p| p.llama_models_max = v.parse().unwrap_or(1));
            apply_local_service();
        },
    );
    g.add(&r);
    let (r, _) = widgets::choice_row(
        "Sleep after inactivity",
        "Unload idle models from memory. A model loads again for the next request.",
        widgets::opts(&[("-1", "Never"), ("300", "5 minutes"), ("900", "15 minutes"), ("3600", "1 hour")]),
        &p.llama_idle_seconds.to_string(),
        |v| {
            prefs::update(|p| p.llama_idle_seconds = v.parse().unwrap_or(-1));
            apply_local_service();
        },
    );
    g.add(&r);
}

/// Nyx AI Core takes these at once; nothing restarts.
fn apply_local_service() {
    let p = prefs::get();
    let (max, idle) = (p.llama_models_max, p.llama_idle_seconds);
    crate::rt::spawn(async move { crate::llama::core::configure_service(max, idle).await }, |r| {
        if let Err(e) = r {
            window::toast(&format!("Couldn't change Nyx AI Core: {}", crate::ollama::api::short_error(&format!("{e:#}"))));
        }
    });
}

fn hugging_face(page: &Page) {
    let g = page.group("Hugging Face");
    let controls = widgets::hbox(6);
    let status = widgets::label("", "dim");
    let connect = gtk::Button::with_label("Connect…");
    let disconnect = gtk::Button::with_label("Disconnect");
    let refresh = {
        let status = status.clone();
        let disconnect = disconnect.clone();
        move || {
            status.set_text(if hf::has_app_token() { "Connected" } else if hf::has_shared_token() {
                "Using existing Hugging Face login"
            } else { "Not connected" });
            disconnect.set_visible(hf::has_app_token());
        }
    };
    refresh();
    controls.append(&status);
    controls.append(&connect);
    controls.append(&disconnect);
    let r = widgets::row("Account", "A read token grants access to gated models. It may help with rate limits; public downloads are not necessarily faster.", Some(controls.upcast_ref()));
    g.add(&r);
    let refresh2 = refresh.clone();
    connect.connect_clicked(move |_| hugging_face_dialog(refresh2.clone()));
    disconnect.connect_clicked(move |_| {
        match hf::remove_app_token() {
            Ok(()) => { refresh(); window::toast("Hugging Face account disconnected"); },
            Err(e) => window::toast(&format!("Couldn't remove Hugging Face token: {e:#}")),
        }
    });
}

fn hugging_face_dialog(refresh: impl Fn() + Clone + 'static) {
    let (dialog, card) = widgets::dialog("Connect to Hugging Face", 460);
    let hint = widgets::label("Paste a Hugging Face read token. Create one at huggingface.co/settings/tokens. It is stored in a private file; Nyx AI Core uses it for downloads.", "dim");
    hint.set_wrap(true);
    card.append(&hint);
    let token = gtk::Entry::new();
    token.set_visibility(false);
    token.set_input_purpose(gtk::InputPurpose::Password);
    token.set_placeholder_text(Some("hf_…"));
    card.append(&token);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Connect");
    save.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&save);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let connect_dialog = dialog.clone();
    save.connect_clicked(move |button| {
        let value = token.text().trim().to_string();
        if value.is_empty() { token.grab_focus(); return; }
        button.set_sensitive(false);
        let (button, d, refresh) = (button.clone(), connect_dialog.clone(), refresh.clone());
        crate::rt::spawn(async move { hf::whoami(&value).await.map(|name| (name, value)) }, move |result| {
            button.set_sensitive(true);
            match result {
                Ok((name, value)) => match hf::save_token(&value) {
                    Ok(()) => { refresh(); d.close(); window::toast(&format!("Connected to Hugging Face as {name}.")); },
                    Err(e) => window::toast(&format!("Couldn't save Hugging Face token: {e:#}")),
                },
                Err(e) => window::toast(&format!("Hugging Face login failed: {e:#}")),
            }
        });
    });
    dialog.present();
}

/// A card with the text above a wide control (text areas, lists).
fn stacked(g: &Group, title: &str, desc: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = widgets::row(title, desc, None);
    row.set_orientation(gtk::Orientation::Vertical);
    row.set_spacing(8);
    row.add_css_class("tall");
    row.append(control);
    g.add(&row);
    row
}

fn entry_row(
    g: &Group,
    title: &str,
    desc: &str,
    value: &str,
    placeholder: &str,
    secret: bool,
    on: impl Fn(String) + 'static,
) -> gtk::Box {
    let e = gtk::Entry::new();
    e.set_text(value);
    e.set_placeholder_text(Some(placeholder));
    e.set_width_chars(24);
    if secret {
        e.set_visibility(false);
        e.set_input_purpose(gtk::InputPurpose::Password);
    }
    // Save shortly after typing stops.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let on = Rc::new(on);
    e.connect_changed(move |e| {
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let (t, on2, p2) = (e.text().to_string(), on.clone(), pending.clone());
        *pending.borrow_mut() = Some(glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || {
            p2.borrow_mut().take();
            on2(t);
        }));
    });
    let r = widgets::row(title, desc, Some(e.upcast_ref()));
    g.add(&r);
    r
}

fn slider_row(
    g: &Group,
    title: &str,
    desc: &str,
    (min, max, step): (f64, f64, f64),
    value: f64,
    digits: usize,
    on: impl Fn(f64) + 'static,
) {
    let b = widgets::hbox(8);
    let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    s.set_value(value);
    s.set_draw_value(false);
    s.set_size_request(160, -1);
    let r = widgets::label(&format!("{value:.digits$}"), "value-readout");
    r.add_css_class("mono");
    r.set_xalign(1.0);
    let r2 = r.clone();
    s.connect_value_changed(move |s| {
        let v = (s.value() / step).round() * step;
        r2.set_text(&format!("{v:.digits$}"));
        on(v);
    });
    b.append(&s);
    b.append(&r);
    g.add(&widgets::row(title, desc, Some(b.upcast_ref())));
}

// ---------- Connection ----------

fn connection(page: &Page) {
    let g = page.group("Connection");
    let list = widgets::vbox(6);
    g.add(&list);
    let fill = {
        let list = list.clone();
        Rc::new(move || {
            widgets::clear(&list);
            let p = prefs::get();
            for (i, h) in p.hosts.iter().enumerate() {
                let controls = widgets::hbox(6);
                if i == p.active_host {
                    controls.append(&widgets::tag("In use", "accent"));
                } else {
                    let b = gtk::Button::with_label("Use");
                    b.connect_clicked(move |_| window::switch_host(i));
                    controls.append(&b);
                }
                let e = gtk::Button::with_label("Edit");
                let h2 = h.clone();
                e.connect_clicked(move |_| host_dialog(Some(i), h2.clone()));
                controls.append(&e);
                if p.hosts.len() > 1 {
                    controls.append(&widgets::two_click("Remove", "Click again", move || {
                        prefs::update(|p| {
                            p.hosts.remove(i);
                            if p.active_host >= p.hosts.len() || p.active_host == i {
                                p.active_host = 0;
                            }
                        });
                        crate::hub::emit(&hub::HOST);
                        models::refresh();
                    }));
                }
                let desc = format!(
                    "<tt>{}</tt>{}",
                    util::esc(&prefs::normalize_host(&h.url, h.backend)),
                    if h.key.is_empty() { "" } else { " · with a key" }
                );
                let kind = if h.backend == Backend::Ollama {
                    "Ollama"
                } else if prefs::normalize_host(&h.url, h.backend) == crate::llama::core::URL {
                    "Nyx AI Core"
                } else {
                    "llama.cpp"
                };
                list.append(&widgets::row(&format!("{} · {kind}", h.name), &desc, Some(controls.upcast_ref())));
            }
        })
    };
    fill();
    let f2 = fill.clone();
    hub::on(&hub::HOST, &list, move || f2());
    let (r, _) = widgets::button_row(
        "Add a server",
        "Connect to another llama.cpp or Ollama server.",
        "Add…",
        |_| host_dialog(None, Host { backend: Backend::LlamaCpp, name: String::new(), url: String::new(), key: String::new() }),
    );
    g.add(&r);
    let (r, _) = widgets::button_row("Test the connection", "Check that the server in use answers.", "Test", |b| {
        b.set_sensitive(false);
        let b2 = b.clone();
        let conn = crate::ollama::api::Conn::current();
        let backend = conn.backend;
        crate::rt::spawn(async move { crate::ollama::api::version(&conn).await }, move |r| {
            b2.set_sensitive(true);
            match r {
                Ok(v) => window::toast(&format!("Connected to {}", if backend == Backend::LlamaCpp { v } else { format!("Ollama {v}") })),
                Err(e) => window::toast(&crate::ollama::api::short_error(&format!("{e:#}"))),
            }
            models::refresh();
        });
    });
    g.add(&r);
    if prefs::get().host().backend == Backend::LlamaCpp
        && prefs::normalize_host(&prefs::get().host().url, Backend::LlamaCpp) == "http://127.0.0.1:8080"
        && crate::llama::api::installed() {
        let controls = widgets::hbox(6);
        let state = widgets::label("", "mono");
        state.add_css_class("dim");
        controls.append(&state);
        let refresh_state = {
            let state = state.clone();
            move || {
                let s2 = state.clone();
                crate::cmd::background(crate::llama::api::service_state, move |s| s2.set_text(&s));
            }
        };
        refresh_state();
        let auto = gtk::Switch::new();
        auto.set_active(crate::llama::api::service_enabled());
        let updating = Rc::new(std::cell::Cell::new(false));
        let updating2 = updating.clone();
        let rs = refresh_state.clone();
        auto.connect_active_notify(move |sw| {
            if updating2.get() { return; }
            let enabled = sw.is_active();
            match crate::llama::api::set_service_enabled(enabled) {
                Ok(()) => {
                    window::toast(if enabled { "Nyx AI Core starts when an app needs it" } else { "Nyx AI Core stopped and won't start for apps" });
                    rs();
                    models::refresh();
                }
                Err(e) => {
                    updating2.set(true);
                    sw.set_active(!enabled);
                    updating2.set(false);
                    window::toast(&format!("Couldn't change Nyx AI Core: {e:#}"));
                }
            }
        });
        g.add(&widgets::row("Start Nyx AI Core for apps", "It starts when Chat or another Nyx app first needs a model. Turning this off stops it; Start below runs it for this session.", Some(auto.upcast_ref())));
        for action in ["start", "stop", "restart"] {
            let b = gtk::Button::with_label(&format!("{}{}", action[..1].to_uppercase(), &action[1..]));
            let rs = refresh_state.clone();
            b.connect_clicked(move |b| {
                b.set_sensitive(false);
                let (b2, rs2) = (b.clone(), rs.clone());
                crate::cmd::background(
                    move || crate::llama::api::control(action),
                    move |r| {
                        b2.set_sensitive(true);
                        match r {
                            Ok(m) => window::toast(&m),
                            Err(e) => window::toast(&format!("{e:#}")),
                        }
                        rs2();
                        models::refresh();
                    },
                );
            });
            controls.append(&b);
        }
        g.add(&widgets::row("Nyx AI Core on this computer", "The local model service Nyx apps share (llama.cpp).", Some(controls.upcast_ref())));
    }
}

fn host_dialog(index: Option<usize>, h: Host) {
    let (dialog, card) = widgets::dialog(if index.is_some() { "Edit server" } else { "Add a server" }, 460);
    card.append(&widgets::label("Name", "field-label"));
    card.append(&widgets::label("Backend", "field-label"));
    let backend = widgets::dropdown(&widgets::opts(&[("llama", "llama.cpp"), ("ollama", "Ollama")]), if h.backend == Backend::LlamaCpp { "llama" } else { "ollama" });
    card.append(&backend);
    let name = gtk::Entry::new();
    name.set_text(&h.name);
    name.set_placeholder_text(Some("Desktop"));
    card.append(&name);
    card.append(&widgets::label("Address", "field-label"));
    let url = gtk::Entry::new();
    url.set_text(&h.url);
    url.set_placeholder_text(Some("192.168.1.20:8080"));
    url.add_css_class("mono");
    card.append(&url);
    card.append(&widgets::label("Key (optional)", "field-label"));
    let key = gtk::Entry::new();
    key.set_text(&h.key);
    key.set_visibility(false);
    key.set_placeholder_text(Some("Sent as a bearer token, for servers behind a proxy"));
    card.append(&key);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&save);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    save.connect_clicked(move |_| {
        let host = Host {
            backend: if backend.selected() == 0 { Backend::LlamaCpp } else { Backend::Ollama },
            name: if name.text().trim().is_empty() { url.text().trim().to_string() } else { name.text().trim().to_string() },
            url: url.text().trim().to_string(),
            key: key.text().trim().to_string(),
        };
        if host.url.is_empty() {
            url.grab_focus();
            return;
        }
        let active = prefs::get().active_host;
        prefs::update(|p| match index {
            Some(i) if i < p.hosts.len() => p.hosts[i] = host,
            _ => p.hosts.push(host),
        });
        hub::emit(&hub::HOST);
        if index == Some(active) {
            models::refresh();
        }
        d.close();
    });
    dialog.present();
}

// ---------- Chat defaults ----------

fn chat_defaults(page: &Page) {
    let g = page.group("Chat defaults");
    let p = prefs::get();
    let dd_row = widgets::row(
        "Model for new chats",
        "The last one you picked follows you from chat to chat. Chats in a folder use the folder's model when it has one.",
        None,
    );
    let dd = gtk::DropDown::from_strings(&[]);
    widgets::ellipsize(&dd);
    dd.set_valign(gtk::Align::Center);
    dd_row.append(&dd);
    g.add(&dd_row);
    // (id, label); "" is "the last one you picked".
    let options: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let syncing = Rc::new(std::cell::Cell::new(false));
    let fill = {
        let (dd, options, syncing) = (dd.clone(), options.clone(), syncing.clone());
        move || {
            let mut ids = vec![String::new()];
            ids.extend(models::chat_models());
            let labels: Vec<String> = ids
                .iter()
                .map(|m| if m.is_empty() { "The last one you picked".to_string() } else { util::model_label(m) })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let cur = prefs::get().new_chat_model;
            // Showing the list must not count as choosing from it.
            syncing.set(true);
            dd.set_model(Some(&gtk::StringList::new(&refs)));
            dd.set_selected(ids.iter().position(|m| *m == cur).unwrap_or(0) as u32);
            syncing.set(false);
            *options.borrow_mut() = ids;
        }
    };
    fill();
    hub::on(&hub::MODELS, &dd, fill);
    dd.connect_selected_notify(move |d| {
        if syncing.get() {
            return;
        }
        if let Some(m) = options.borrow().get(d.selected() as usize).cloned() {
            prefs::update(|p| p.new_chat_model = m);
        }
    });
    let (r, _) = widgets::switch_row(
        "Keep the picked model loaded",
        "Preload the picked model for faster replies and keep it loaded on this computer.",
        p.keep_loaded,
        |on| {
            prefs::update(|p| p.keep_loaded = on);
            let last = prefs::get().last_model;
            if on {
                models::load(&last, None);
            } else {
                models::release_pinned();
            }
        },
    );
    g.add(&r);

    entry_row(&g, "Your name", "Models can use it through <tt>{{USER_NAME}}</tt>.", &p.user_name, "Optional", false, |v| {
        prefs::update(|p| p.user_name = v.trim().to_string())
    });
    let (sw, tv) = widgets::text_area(&p.system_prompt, false, 90);
    stacked(
        &g,
        "System prompt",
        "How models behave unless a chat or folder says otherwise. <tt>{{CURRENT_DATE}}</tt> and friends are filled in.",
        &sw,
    );
    tv.buffer().connect_changed(|b| {
        let t = b.text(&b.start_iter(), &b.end_iter(), false).to_string();
        prefs::update(|p| p.system_prompt = t);
    });
    if p.host().backend == Backend::Ollama {
    slider_row(
        &g,
        "Temperature",
        "Lower is focused and repeatable, higher is creative.",
        (0.0, 2.0, 0.05),
        p.temperature,
        2,
        |v| prefs::update(|p| p.temperature = v),
    );
    slider_row(&g, "Top P", "Limits sampling to the most likely tokens; lower values are more focused.",
        (0.05, 1.0, 0.05), p.top_p, 2, |v| prefs::update(|p| p.top_p = v));
    slider_row(&g, "Top K", "Limits sampling to this many likely tokens.",
        (1.0, 100.0, 1.0), p.top_k as f64, 0, |v| prefs::update(|p| p.top_k = v as i64));
    let (r, _) = widgets::choice_row(
        "Context length",
        "How much of the conversation the model keeps in mind. On this computer it applies the next time the model loads; more uses more memory.",
        widgets::opts(&[
            ("4096", "4K tokens"),
            ("8192", "8K tokens"),
            ("16384", "16K tokens"),
            ("32768", "32K tokens"),
            ("65536", "64K tokens"),
            ("131072", "128K tokens"),
        ]),
        &p.num_ctx.to_string(),
        |v| prefs::update(|p| p.num_ctx = v.parse().unwrap_or(8192)),
    );
    g.add(&r);
    }
    let (r, _) = widgets::choice_row(
        "When a chat outgrows its context",
        "Long chats can hold more than the model takes in. Each chat can change this in its own settings.",
        widgets::opts(OVERFLOW),
        &p.overflow,
        |v| prefs::update(|p| p.overflow = v),
    );
    g.add(&r);
    let (r, _) = widgets::switch_row(
        "Suggest follow-up questions",
        "After a reply, the model offers three questions you might ask next. It takes a moment of the model's time.",
        p.followups,
        |on| prefs::update(|p| p.followups = on),
    );
    g.add(&r);
    let (r, _) = widgets::choice_row(
        "Keep models loaded",
        "How long a model stays in memory after a reply, so the next one starts fast.",
        widgets::opts(&[
            ("0", "Unload right away"),
            ("5m", "5 minutes"),
            ("30m", "30 minutes"),
            ("1h", "1 hour"),
            ("-1", "Until Ollama stops"),
        ]),
        &p.keep_alive,
        |v| prefs::update(|p| p.keep_alive = v),
    );
    if p.host().backend == Backend::Ollama { g.add(&r); }
    let (r, _) = widgets::switch_row(
        "Think before answering",
        "Models that can reason show their thinking first. Slower, often better.",
        p.think,
        |on| prefs::update(|p| p.think = on),
    );
    g.add(&r);
    let (r, _) = widgets::switch_row(
        "Name chats automatically",
        "The model suggests a short title after the first reply.",
        p.auto_title,
        |on| prefs::update(|p| p.auto_title = on),
    );
    g.add(&r);
    let (r, _) =
        widgets::switch_row("Enter sends", "Otherwise Ctrl+Enter sends and Enter starts a new line.", p.enter_sends, |on| {
            prefs::update(|p| p.enter_sends = on)
        });
    g.add(&r);
}

// ---------- Tools ----------

fn tools_group(page: &Page) {
    let g = page.group("Tools");
    g.note("Models that support tools can use these on their own. Each chat can turn them on or off from the message box.");
    let p = prefs::get();
    for t in tools::BUILTIN {
        let name = t.name;
        let (r, _) = widgets::switch_row(t.title, t.description, p.tool_on(name), move |on| {
            prefs::update(|p| {
                p.tools_enabled.retain(|x| x != name && !(name == "web_search" && x == "web_fetch"));
                if on {
                    p.tools_enabled.push(name.to_string());
                }
            });
        });
        g.add(&r);
    }
    let folders = widgets::vbox(6);
    let fill = {
        let folders = folders.clone();
        Rc::new(move || {
            widgets::clear(&folders);
            let list = prefs::get().allowed_folders;
            if list.is_empty() {
                folders.append(&widgets::label("No folders yet.", "dim"));
            }
            for f in list {
                let row = widgets::hbox(8);
                let l = widgets::short_label(&f, "mono", gtk::pango::EllipsizeMode::Middle);
                l.set_hexpand(true);
                row.append(&l);
                let rm = gtk::Button::from_icon_name("list-remove-symbolic");
                rm.add_css_class("flat");
                rm.set_tooltip_text(Some("Remove"));
                let f2 = f.clone();
                rm.connect_clicked(move |_| {
                    prefs::update(|p| p.allowed_folders.retain(|x| *x != f2));
                    if let Some(w) = SETTINGS_FOLDERS.with(|s| s.borrow().clone()) {
                        w();
                    }
                });
                row.append(&rm);
                folders.append(&row);
            }
            let add = gtk::Button::with_label("Allow a folder…");
            add.set_halign(gtk::Align::Start);
            add.connect_clicked(|_| {
                widgets::pick_folder(|path| {
                    let s = paths::pretty(&path);
                    prefs::update(|p| {
                        if !p.allowed_folders.contains(&s) {
                            p.allowed_folders.push(s);
                        }
                    });
                    if let Some(w) = SETTINGS_FOLDERS.with(|s| s.borrow().clone()) {
                        w();
                    }
                });
            });
            folders.append(&add);
        })
    };
    SETTINGS_FOLDERS.with(|s| *s.borrow_mut() = Some(fill.clone()));
    fill();
    stacked(&g, "Folders models may read", "Read files only looks inside these folders and the folders within them.", &folders);
    let (r, _) = widgets::choice_row(
        "Tool rounds per reply",
        "How many times a model may call tools before it has to answer.",
        widgets::opts(&[("3", "3"), ("5", "5"), ("8", "8"), ("12", "12"), ("20", "20")]),
        &p.max_tool_rounds.to_string(),
        |v| prefs::update(|p| p.max_tool_rounds = v.parse().unwrap_or(8)),
    );
    g.add(&r);
}

thread_local! {
    static SETTINGS_FOLDERS: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

// ---------- Web search ----------

fn web_search(page: &Page) {
    let g = page.group("Web search");
    let p = prefs::get();
    let searx = entry_row(
        &g,
        "SearXNG address",
        "Your instance, with the JSON format turned on.",
        &p.searxng_url,
        "https://search.example.org",
        false,
        |v| prefs::update(|p| p.searxng_url = v.trim().to_string()),
    );
    let brave = entry_row(&g, "Brave Search key", "From the Brave Search API dashboard.", &p.brave_key, "API key", true, |v| {
        prefs::update(|p| p.brave_key = v.trim().to_string())
    });
    let tavily = entry_row(&g, "Tavily key", "From tavily.com.", &p.tavily_key, "API key", true, |v| {
        prefs::update(|p| p.tavily_key = v.trim().to_string())
    });
    let show = {
        let (s, b, t) = (searx.clone(), brave.clone(), tavily.clone());
        move |backend: &str| {
            s.set_visible(backend == "searxng");
            b.set_visible(backend == "brave");
            t.set_visible(backend == "tavily");
        }
    };
    show(&p.search_backend);
    let (r, _) = widgets::choice_row(
        "Search engine",
        "DuckDuckGo needs no account. The others need an address or a key.",
        widgets::opts(&[("duckduckgo", "DuckDuckGo"), ("searxng", "SearXNG"), ("brave", "Brave Search"), ("tavily", "Tavily")]),
        &p.search_backend,
        move |v| {
            show(&v);
            prefs::update(|p| p.search_backend = v);
        },
    );
    g.list.prepend(&r);
    let (r, _) = widgets::choice_row(
        "Results per search",
        "More results give more to go on, and a longer prompt.",
        widgets::opts(&[("3", "3"), ("5", "5"), ("8", "8"), ("10", "10")]),
        &p.search_results.to_string(),
        |v| prefs::update(|p| p.search_results = v.parse().unwrap_or(5)),
    );
    g.add(&r);
    let (r, _) = widgets::switch_row(
        "Read the top pages",
        "Include the text of the first three results, not just their snippets. Slower.",
        p.search_fetch_pages,
        |on| prefs::update(|p| p.search_fetch_pages = on),
    );
    g.add(&r);
}

// ---------- Location ----------

fn location(page: &Page) {
    let g = page.group("Location");
    g.note("Off by default. When on, models can ask where you are, for weather, local times and nearby places.");
    let p = prefs::get();
    let manual = entry_row(
        &g,
        "Your location",
        "A city or region, as you'd like it given.",
        &p.manual_location,
        "Lisbon, Portugal",
        false,
        |v| prefs::update(|p| p.manual_location = v.trim().to_string()),
    );
    manual.set_visible(p.location_mode == "manual");
    let geoclue_note = widgets::row(
        "Desktop location isn't available",
        "It needs GeoClue (the <tt>geoclue</tt> package). Until then, use your IP address or type a place.",
        None,
    );
    geoclue_note.set_visible(p.location_mode == "desktop" && !tools::location::geoclue_installed());
    g.add(&geoclue_note);
    let (m2, gn) = (manual.clone(), geoclue_note.clone());
    let (r, _) = widgets::choice_row(
        "Share location",
        "Where the location comes from.",
        widgets::opts(&[
            ("off", "Off"),
            ("desktop", "Desktop location"),
            ("ip", "Approximate, from IP address"),
            ("manual", "Typed in"),
        ]),
        &p.location_mode,
        move |v| {
            m2.set_visible(v == "manual");
            gn.set_visible(v == "desktop" && !tools::location::geoclue_installed());
            let on = v != "off";
            prefs::update(|p| {
                p.location_mode = v;
                if on && !p.tool_on("get_location") {
                    p.tools_enabled.push("get_location".into());
                }
            });
        },
    );
    g.list.prepend(&r);
    let (r, _) = widgets::switch_row(
        "Include in every chat",
        "Put your location in the system prompt, for models that can't use tools.",
        p.location_in_prompt,
        |on| prefs::update(|p| p.location_in_prompt = on),
    );
    g.add(&r);
    let (r, _) = widgets::button_row("Check", "See what a model would be told.", "Where am I?", |b| {
        b.set_sensitive(false);
        let b2 = b.clone();
        let pr = prefs::get();
        crate::rt::spawn(async move { tools::location::lookup(&pr).await }, move |r| {
            b2.set_sensitive(true);
            match r {
                Ok(l) => window::toast(&l.describe()),
                Err(e) => window::toast(&format!("{e:#}")),
            }
        });
    });
    g.add(&r);
}

// ---------- Memories ----------

fn memories(page: &Page) {
    let g = page.group("Memories");
    g.note("Facts every chat knows about you. Models with the Save memories tool can suggest new ones.");
    let list = widgets::vbox(6);
    g.add(&list);
    let fill = {
        let list = list.clone();
        move || {
            widgets::clear(&list);
            let mems = store::memories().unwrap_or_default();
            if mems.is_empty() {
                list.append(&widgets::label("Nothing remembered yet.", "dim"));
            }
            for (id, text) in mems {
                let controls = widgets::hbox(6);
                let e = gtk::Button::with_label("Edit");
                let t2 = text.clone();
                e.connect_clicked(move |_| {
                    widgets::prompt_text("Edit memory", "", &t2, "Save", move |v| {
                        if !v.trim().is_empty() {
                            let _ = store::update_memory(id, &v);
                            hub::emit(&hub::LIBRARY);
                        }
                    })
                });
                controls.append(&e);
                controls.append(&widgets::two_click("Forget", "Click again", move || {
                    let _ = store::delete_memory(id);
                    hub::emit(&hub::LIBRARY);
                }));
                let row = widgets::hbox(10);
                row.add_css_class("settings-option");
                let l = widgets::label(&text, "");
                l.set_wrap(true);
                l.set_hexpand(true);
                row.append(&l);
                controls.set_valign(gtk::Align::Center);
                row.append(&controls);
                list.append(&row);
            }
        }
    };
    fill();
    hub::on(&hub::LIBRARY, &list, fill);
    let (r, _) =
        widgets::button_row("Add a memory", "One short sentence, like “I use Arch Linux and Hyprland.”", "Add…", |_| {
            widgets::prompt_text("Add a memory", "", "", "Add", |v| {
                if !v.trim().is_empty() {
                    let _ = store::add_memory(&v);
                    hub::emit(&hub::LIBRARY);
                }
            })
        });
    g.add(&r);
}

// ---------- Knowledge ----------

fn knowledge(page: &Page) {
    let g = page.group("Knowledge");
    let p = prefs::get();
    entry_row(
        &g,
        "Embedding model",
        "Used for new collections and large attachments.",
        &p.embed_model,
        "nomic-embed-text",
        false,
        |v| prefs::update(|p| p.embed_model = v.trim().to_string()),
    )
    .last_child()
    .inspect(|w| w.add_css_class("mono"));
    let (r, _) = widgets::choice_row(
        "Excerpt size",
        "How documents are split. Smaller pieces are more precise, larger keep more context.",
        widgets::opts(&[("600", "Small"), ("1200", "Medium"), ("2400", "Large")]),
        &p.chunk_size.to_string(),
        |v| prefs::update(|p| p.chunk_size = v.parse().unwrap_or(1200)),
    );
    g.add(&r);
    let (r, _) = widgets::choice_row(
        "Excerpts per question",
        "How many pieces of your documents go with each question.",
        widgets::opts(&[("3", "3"), ("5", "5"), ("8", "8"), ("12", "12")]),
        &p.rag_top_k.to_string(),
        |v| prefs::update(|p| p.rag_top_k = v.parse().unwrap_or(5)),
    );
    g.add(&r);
}

// ---------- Voice ----------

fn file_row(g: &Group, title: &str, desc: &str, value: &str, set: fn(&mut prefs::Prefs, String)) {
    let b = gtk::Button::with_label(if value.is_empty() { "Choose…" } else { value.rsplit('/').next().unwrap_or(value) });
    b.set_tooltip_text(Some(value));
    b.connect_clicked(move |b| {
        let b2 = b.clone();
        widgets::open_files("Choose a file", &[], move |files| {
            if let Some(f) = files.first() {
                let s = paths::pretty(f);
                b2.set_label(f.file_name().map(|n| n.to_string_lossy().to_string()).as_deref().unwrap_or("Chosen"));
                b2.set_tooltip_text(Some(&s));
                prefs::update(|p| set(p, s.clone()));
                hub::emit(&hub::LIBRARY);
            }
        });
    });
    g.add(&widgets::row(title, desc, Some(b.upcast_ref())));
}

fn voice_group(page: &Page) {
    let g = page.group("Voice");
    g.note("Speech stays on this computer: whisper.cpp listens and Piper speaks.");
    let status = widgets::vbox(6);
    g.add(&status);
    let refresh = {
        let status = status.clone();
        move || {
            widgets::clear(&status);
            let d = voice::dictation_problem();
            let s = voice::speech_problem();
            status.append(&widgets::row(
                "Dictation",
                &util::esc(d.as_deref().unwrap_or("Ready. Press the microphone or Ctrl+Space in a chat.")),
                Some(
                    widgets::tag(if d.is_none() { "Ready" } else { "Not set up" }, if d.is_none() { "accent" } else { "" })
                        .upcast_ref(),
                ),
            ));
            status.append(&widgets::row(
                "Reading aloud",
                &util::esc(s.as_deref().unwrap_or("Ready. Press the speaker under any reply.")),
                Some(
                    widgets::tag(if s.is_none() { "Ready" } else { "Not set up" }, if s.is_none() { "accent" } else { "" })
                        .upcast_ref(),
                ),
            ));
        }
    };
    refresh();
    hub::on(&hub::LIBRARY, &status, refresh);
    let p = prefs::get();
    entry_row(&g, "whisper.cpp program", "Found on your PATH when left as is.", &p.whisper_bin, "whisper-cli", false, |v| {
        prefs::update(|p| p.whisper_bin = v.trim().to_string());
        hub::emit(&hub::LIBRARY);
    });
    file_row(&g, "Whisper model", "A ggml model file, such as <tt>ggml-base.en.bin</tt>.", &p.whisper_model, |p, v| {
        p.whisper_model = v
    });
    entry_row(&g, "Piper program", "Found on your PATH when left as is.", &p.piper_bin, "piper", false, |v| {
        prefs::update(|p| p.piper_bin = v.trim().to_string());
        hub::emit(&hub::LIBRARY);
    });
    file_row(
        &g,
        "Piper voice",
        "A voice model file (<tt>.onnx</tt>), with its <tt>.onnx.json</tt> beside it.",
        &p.piper_voice,
        |p, v| p.piper_voice = v,
    );
    let (r, _) = widgets::switch_row("Read replies aloud", "Speak each new reply as soon as it's finished.", p.auto_read, |on| {
        prefs::update(|p| p.auto_read = on)
    });
    g.add(&r);
    let (r, _) = widgets::switch_row(
        "Hands-free",
        "Send as soon as dictation ends. With reading aloud, you can talk back and forth.",
        p.hands_free,
        |on| prefs::update(|p| p.hands_free = on),
    );
    g.add(&r);
}

// ---------- Quick ask ----------

fn quick_ask(page: &Page) {
    let g = page.group("Quick ask");
    g.note("A small window for one-off questions, from anywhere.");
    let p = prefs::get();
    let mut opts = vec![(String::new(), "Same as new chats".to_string())];
    opts.extend(models::chat_models().into_iter().map(|m| (m.clone(), util::model_label(&m))));
    let (r, _) = widgets::choice_row("Model", "Small, fast models suit quick questions.", opts, &p.quick_model, |v| {
        prefs::update(|p| p.quick_model = v)
    });
    g.add(&r);
    let code = widgets::label(
        "o.bind(\"SUPER + ALT + SPACE\", \"Quick ask\", \"chat --quick --selection\")\no.window({ title = \"^Quick ask$\" }, { float = true, center = true, size = { 720, 480 } })",
        "code-block",
    );
    code.set_selectable(true);
    code.set_wrap(true);
    code.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    let v = widgets::vbox(8);
    v.append(&code);
    let copy = gtk::Button::with_label("Copy");
    copy.set_halign(gtk::Align::Start);
    let text = code.text().to_string();
    copy.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
        window::toast("Copied. Paste it into ~/.config/hypr/bindings.lua");
    });
    v.append(&copy);
    stacked(
        &g,
        "Keyboard shortcut",
        "Add this to <tt>~/.config/hypr/bindings.lua</tt>. <tt>--selection</tt> asks about the text you've highlighted; <tt>--clipboard</tt> uses what you copied.",
        &v,
    );
    let shot = widgets::keys(&["Ctrl", "Shift", "S"]);
    g.add(&widgets::row(
        "Screenshot to chat",
        "In this window, drag out an area to attach it. <tt>chat --screenshot</tt> does the same from a shortcut.",
        Some(shot.upcast_ref()),
    ));
}

// ---------- MCP ----------

fn mcp(page: &Page) {
    let g = page.group("MCP servers");
    g.note("Tools from Model Context Protocol servers, started on this computer. Each tool shows up for models that use tools.");
    let list = widgets::vbox(6);
    g.add(&list);
    let fill: Rc<dyn Fn()> = {
        let list = list.clone();
        Rc::new(move || {
            widgets::clear(&list);
            let servers = prefs::get().mcp_servers;
            if servers.is_empty() {
                list.append(&widgets::label("No servers yet.", "dim"));
            }
            for (i, s) in servers.iter().enumerate() {
                let controls = widgets::hbox(6);
                if let Some(status) = tools::mcp::status(&s.name).filter(|_| s.enabled) {
                    let failed = status.starts_with("Failed");
                    let t = widgets::tag(if failed { "Failed" } else { &status }, if failed { "danger" } else { "accent" });
                    t.set_tooltip_text(Some(&status));
                    controls.append(&t);
                }
                let sw = gtk::Switch::new();
                sw.set_active(s.enabled);
                sw.set_valign(gtk::Align::Center);
                sw.connect_active_notify(move |sw| {
                    prefs::update(|p| {
                        if let Some(s) = p.mcp_servers.get_mut(i) {
                            s.enabled = sw.is_active();
                        }
                    });
                    restart_mcp();
                });
                controls.append(&sw);
                controls.append(&widgets::two_click("Remove", "Click again", move || {
                    prefs::update(|p| {
                        if i < p.mcp_servers.len() {
                            p.mcp_servers.remove(i);
                        }
                    });
                    restart_mcp();
                }));
                list.append(&widgets::row(&s.name, &format!("<tt>{}</tt>", util::esc(&s.command)), Some(controls.upcast_ref())));
            }
        })
    };
    fill();
    MCP_FILL.with(|m| *m.borrow_mut() = Some(fill.clone()));
    let (r, _) = widgets::button_row(
        "Add a server",
        "The command that starts it, for example <tt>npx -y @modelcontextprotocol/server-filesystem ~/Documents</tt>.",
        "Add…",
        |_| {
            let (dialog, card) = widgets::dialog("Add an MCP server", 520);
            card.append(&widgets::label("Name", "field-label"));
            let name = gtk::Entry::new();
            name.set_placeholder_text(Some("Files"));
            card.append(&name);
            card.append(&widgets::label("Command", "field-label"));
            let command = gtk::Entry::new();
            command.add_css_class("mono");
            command.set_placeholder_text(Some("npx -y @modelcontextprotocol/server-filesystem ~/Documents"));
            card.append(&command);
            let buttons = widgets::hbox(8);
            buttons.set_halign(gtk::Align::End);
            let cancel = gtk::Button::with_label("Cancel");
            let add = gtk::Button::with_label("Add");
            add.add_css_class("suggested-action");
            buttons.append(&cancel);
            buttons.append(&add);
            card.append(&buttons);
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let d = dialog.clone();
            add.connect_clicked(move |_| {
                let (n, c) = (name.text().trim().to_string(), command.text().trim().to_string());
                if n.is_empty() || c.is_empty() {
                    return;
                }
                prefs::update(|p| p.mcp_servers.push(McpServer { name: n, command: c, enabled: true }));
                restart_mcp();
                d.close();
            });
            dialog.present();
        },
    );
    g.add(&r);
}

thread_local! {
    static MCP_FILL: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

fn restart_mcp() {
    let servers = prefs::get().mcp_servers;
    if let Some(f) = MCP_FILL.with(|m| m.borrow().clone()) {
        f();
    }
    crate::rt::spawn(tools::mcp::restart(servers), |_| {
        if let Some(f) = MCP_FILL.with(|m| m.borrow().clone()) {
            f();
        }
    });
}

// ---------- Data ----------

fn data(page: &Page) {
    let g = page.group("Data");
    let (r, _) = widgets::button_row("Export every chat", "One JSON file you can import here again.", "Export…", |_| {
        match transfer::export_all() {
            Ok(text) => widgets::save_text("chats.json", text),
            Err(e) => window::toast(&format!("{e:#}")),
        }
    });
    g.add(&r);
    let (r, _) =
        widgets::button_row("Import chats", "From an export of this app, or an Open WebUI chat export.", "Import…", |_| {
            widgets::open_files("Import chats", &[("JSON", &["*.json"])], |files| {
                let mut total = 0;
                for f in files {
                    match std::fs::read_to_string(&f).map_err(anyhow::Error::from).and_then(|t| transfer::import(&t)) {
                        Ok(n) => total += n,
                        Err(e) => window::toast(&format!("{}: {e:#}", f.display())),
                    }
                }
                if total > 0 {
                    window::toast(&format!("Imported {total} chat{}", if total == 1 { "" } else { "s" }));
                    hub::emit(&hub::CHATS);
                }
            })
        });
    g.add(&r);
    let (r, _) = widgets::button_row(
        "Data folder",
        &format!("<tt>{}</tt>", util::esc(&paths::pretty(&paths::data_dir()))),
        "Open",
        |_| {
            let _ = std::fs::create_dir_all(paths::data_dir());
            crate::cmd::spawn(&["xdg-open", &paths::data_dir().to_string_lossy()]);
        },
    );
    g.add(&r);
    let clear = widgets::two_click("Delete all chats", "Click again to delete every chat", || {
        let _ = store::delete_all_chats();
        crate::sections::chat::new_chat(None);
        hub::emit(&hub::CHATS);
        window::toast("All chats deleted");
    });
    g.add(&widgets::row("Delete history", "Removes every chat. Prompts, memories and knowledge stay.", Some(clear.upcast_ref())));
}
