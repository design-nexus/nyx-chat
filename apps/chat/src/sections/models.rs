//! Models: what's installed, what's loaded, what's downloading, and making
//! new models from existing ones.

use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::rc::Rc;

use crate::ollama::api::{self, Conn, Installed};
use crate::pulls::{self, State};
use crate::{hub, models, prefs, rt, util, widgets, window};

/// Keep a long model name on one line, shortened in the middle, with the
/// full name on hover.
fn one_line_title(row: &gtk::Box, full: &str) {
    let title = row
        .first_child()
        .and_downcast::<gtk::Box>()
        .and_then(|text| text.first_child())
        .and_then(|w| if w.is::<gtk::Label>() { Some(w) } else { w.first_child() })
        .and_downcast::<gtk::Label>();
    if let Some(t) = title {
        t.set_text(&util::model_label(full));
        t.set_wrap(false);
        t.set_ellipsize(pango::EllipsizeMode::Middle);
        t.set_tooltip_text(Some(full));
    }
}

/// A download's id and state, with the widgets updated in place.
type DownloadRow = (u64, State, gtk::LevelBar, gtk::Label);

pub fn build() -> gtk::Widget {
    let page = widgets::page("models");
    let llama = prefs::get().host().backend == prefs::Backend::LlamaCpp;

    let banner = widgets::vbox(0);
    page.body.append(&banner);

    // ----- Download -----
    let g = page.group("Download a model");
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(if llama { "user/repo-GGUF:Q4_K_M" } else { "llama3.2, qwen3:8b, hf.co/user/repo:Q4_K_M" }));
    entry.add_css_class("mono");
    entry.set_hexpand(true);
    let go = gtk::Button::with_label("Download");
    go.add_css_class("suggested-action");
    let row = widgets::row(
        "By name",
        if llama { "A Hugging Face GGUF as <tt>user/repo:quant</tt>." } else { "Any model from <a href=\"https://ollama.com/library\">ollama.com/library</a>, or a GGUF on Hugging Face as <tt>hf.co/user/repo:quant</tt>." },
        None,
    );
    row.add_css_class("tall");
    row.set_orientation(gtk::Orientation::Vertical);
    let line = widgets::hbox(8);
    line.append(&entry);
    line.append(&go);
    row.append(&line);
    g.add(&row);
    let start = {
        let entry = entry.clone();
        move || {
            let name = entry.text().trim().to_string();
            if !name.is_empty() {
                pulls::start(&name);
                entry.set_text("");
            }
        }
    };
    let s2 = start.clone();
    go.connect_clicked(move |_| s2());
    entry.connect_activate(move |_| start());
    let (create_row, _) = widgets::button_row(
        "Create a model",
        "Give an installed model a new name, system prompt and settings, such as a coding helper.",
        "Create…",
        |_| create_dialog(),
    );
    if !llama { g.add(&create_row); }

    // ----- Downloads -----
    let dl = page.group("Downloads");
    let dl_list = widgets::vbox(6);
    dl.add(&dl_list);
    let dl_rows: Rc<RefCell<Vec<DownloadRow>>> = Rc::new(RefCell::new(Vec::new()));
    let refresh_dl = {
        let (dl, dl_list, dl_rows) = (dl.clone(), dl_list.clone(), dl_rows.clone());
        move || {
            let all = pulls::all();
            dl.wrapper.set_visible(!all.is_empty());
            let same = {
                let rows = dl_rows.borrow();
                rows.len() == all.len() && rows.iter().zip(&all).all(|(r, p)| r.0 == p.id && r.1 == p.state)
            };
            if same {
                for ((_, _, bar, desc), p) in dl_rows.borrow().iter().zip(&all) {
                    bar.set_value(p.fraction());
                    desc.set_text(&pulls::describe(p));
                }
                return;
            }
            widgets::clear(&dl_list);
            dl_rows.borrow_mut().clear();
            for p in &all {
                let bar = widgets::usage_bar();
                bar.set_value(p.fraction());
                let desc = widgets::label(&pulls::describe(p), "settings-option-description");
                desc.add_css_class("tnum");
                desc.set_ellipsize(pango::EllipsizeMode::End);
                let card = widgets::vbox(6);
                card.add_css_class("settings-option");
                card.add_css_class("tall");
                let top = widgets::hbox(10);
                let t = widgets::label(&p.name, "settings-option-title");
                t.set_hexpand(true);
                t.set_ellipsize(pango::EllipsizeMode::Middle);
                top.append(&t);
                let id = p.id;
                if p.active() {
                    let c = gtk::Button::with_label("Cancel");
                    c.connect_clicked(move |_| pulls::cancel(id));
                    top.append(&c);
                } else {
                    let tag = match &p.state {
                        State::Done => widgets::tag("Done", "accent"),
                        State::Failed(_) => widgets::tag("Failed", "danger"),
                        _ => widgets::tag("Cancelled", ""),
                    };
                    top.append(&tag);
                    if matches!(p.state, State::Failed(_) | State::Cancelled) {
                        let name = p.name.clone();
                        let retry = gtk::Button::with_label("Retry");
                        retry.connect_clicked(move |_| pulls::start(&name));
                        top.append(&retry);
                    }
                }
                card.append(&top);
                if p.active() {
                    card.append(&bar);
                }
                card.append(&desc);
                dl_list.append(&card);
                dl_rows.borrow_mut().push((p.id, p.state.clone(), bar, desc));
            }
            if all.iter().any(|p| !p.active()) {
                let clear = gtk::Button::with_label("Clear finished");
                clear.add_css_class("flat");
                clear.set_halign(gtk::Align::Start);
                clear.connect_clicked(|_| pulls::clear_finished());
                dl_list.append(&clear);
            }
        }
    };
    refresh_dl();
    hub::on(&hub::PULLS, &dl_list, refresh_dl);

    // ----- Running -----
    let run = page.group("Loaded now");
    let run_list = widgets::vbox(6);
    run.add(&run_list);

    // ----- Installed -----
    let inst = page.group("Installed");
    let inst_list = widgets::vbox(6);
    inst.add(&inst_list);

    let refresh = {
        let (banner, run, run_list, inst, inst_list) =
            (banner.clone(), run.clone(), run_list.clone(), inst.clone(), inst_list.clone());
        move || {
            widgets::clear(&banner);
            if models::online() == Some(false) {
                let b = widgets::banner(&crate::sections::chat::unreachable_markup(), true);
                if prefs::get().host().backend == prefs::Backend::LlamaCpp
                    && prefs::normalize_host(&prefs::get().host().url, prefs::Backend::LlamaCpp) == "http://127.0.0.1:8080"
                    && crate::llama::api::installed() {
                    for action in ["start", "restart"] {
                        let btn = gtk::Button::with_label(if action == "start" { "Start" } else { "Restart" });
                        btn.set_valign(gtk::Align::Center);
                        btn.connect_clicked(move |b| {
                            b.set_sensitive(false);
                            crate::cmd::background(
                                move || crate::llama::api::control(action),
                                |r| {
                                    match r {
                                        Ok(m) => window::toast(&m),
                                        Err(e) => window::toast(&format!("{e:#}")),
                                    }
                                    models::refresh();
                                },
                            );
                        });
                        b.append(&btn);
                    }
                }
                banner.append(&b);
            }
            fill_running(&run, &run_list);
            fill_installed(&inst, &inst_list);
        }
    };
    refresh();
    hub::on(&hub::MODELS, &inst_list, refresh);

    // While the page is on screen, keep "Loaded now" current.
    let weak = run_list.downgrade();
    glib::timeout_add_seconds_local(5, move || {
        let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
        if w.is_mapped() {
            models::refresh_running();
        }
        glib::ControlFlow::Continue
    });
    page.root.upcast()
}

fn fill_running(g: &widgets::Group, list: &gtk::Box) {
    widgets::clear(list);
    let running = models::running();
    g.wrapper.set_visible(!running.is_empty());
    for m in running {
        let gpu = if m.size > 0 { (m.size_vram as f64 / m.size as f64 * 100.0).round() as u64 } else { 0 };
        let place = match gpu {
            100 => "all on the GPU".to_string(),
            0 => "on the CPU".to_string(),
            n => format!("{n}% on the GPU"),
        };
        let until = util::parse_time(&m.expires).map(|t| t - util::now()).filter(|s| *s > 0);
        let desc = match until {
            Some(s) if s < 86_400 * 365 => {
                format!("{} · {place} · unloads in {}", util::size(m.size), crate::pulls::duration(s as u64))
            }
            _ => format!("{} · {place} · stays loaded", util::size(m.size)),
        };
        let name = m.name.clone();
        let full = m.name.clone();
        let (row, _) = widgets::button_row(&m.name, &desc, "Unload", move |b| {
            b.set_sensitive(false);
            let (conn, n) = (Conn::current(), name.clone());
            rt::spawn(async move { api::unload(&conn, &n).await }, |r| {
                if let Err(e) = r {
                    window::toast(&format!("{e:#}"));
                }
                models::refresh_running();
            });
        });
        one_line_title(&row, &full);
        list.append(&row);
    }
}

fn fill_installed(g: &widgets::Group, list: &gtk::Box) {
    widgets::clear(list);
    let installed = models::installed();
    if models::online() != Some(true) {
        g.wrapper.set_visible(false);
        return;
    }
    g.wrapper.set_visible(true);
    let total: u64 = installed.iter().map(|m| m.size).sum();
    let updates = models::updates();
    let summary = format!("{} model{} · {}", installed.len(), if installed.len() == 1 { "" } else { "s" }, util::size(total));
    let head = widgets::hbox(8);
    let sl = widgets::label(&summary, "dim");
    sl.add_css_class("tnum");
    sl.set_hexpand(true);
    head.append(&sl);
    if !updates.is_empty() {
        let all = gtk::Button::with_label(&format!("Update all ({})", updates.len()));
        all.add_css_class("suggested-action");
        let u2 = updates.clone();
        all.connect_clicked(move |_| {
            for m in &u2 {
                pulls::start(m);
            }
        });
        head.append(&all);
    }
    let check = gtk::Button::with_label("Check for updates");
    check.connect_clicked(|b| {
        b.set_sensitive(false);
        b.set_label("Checking…");
        let b2 = b.clone();
        models::check_updates(move |n| {
            b2.set_sensitive(true);
            b2.set_label("Check for updates");
            window::toast(&match n {
                0 => "Every model is up to date".to_string(),
                1 => "1 model can be updated".to_string(),
                n => format!("{n} models can be updated"),
            });
        });
    });
    if prefs::get().host().backend == prefs::Backend::Ollama || crate::llama::api::local_service() { head.append(&check); }
    list.append(&head);

    if installed.is_empty() {
        let e = widgets::empty("Nothing installed yet. Download a model above, or browse the Marketplace.");
        list.append(&e);
        return;
    }
    let default = models::default_model();
    for m in installed {
        list.append(&model_row(&m, m.name == default));
    }
}

fn model_row(m: &Installed, is_default: bool) -> gtk::Widget {
    let info = models::info(&m.name);
    let drafter = info.as_ref().is_some_and(|i| i.has("draft"));
    let mut bits = vec![util::size(m.size)];
    if drafter { bits.push("Companion drafter · requires a main model".into()); }
    for s in [&m.parameter_size, &m.quantization, &m.family] {
        if !s.is_empty() {
            bits.push(s.clone());
        }
    }
    if crate::llama::api::local_service() {
        let settings = prefs::for_model(&m.name);
        bits.push(format!("{} context · KV {}", util::count(settings.num_ctx as u64), settings.kv_cache.to_uppercase()));
        if crate::llama::presets::pending(&m.name) { bits.push("Settings pending next load".into()); }
    }
    if let Some(t) = util::parse_time(&m.modified) {
        bits.push(format!("updated {}", util::ago(t)));
    }
    let row = widgets::row(&m.name, &util::esc(&bits.join(" · ")), None);
    one_line_title(&row, &m.name);
    // Tags after the title.
    if let Some(text) = row.first_child().and_downcast::<gtk::Box>() {
        let title_row = widgets::hbox(6);
        if let Some(t) = text.first_child() {
            text.remove(&t);
            t.set_hexpand(false);
            title_row.append(&t);
        }
        if is_default {
            title_row.append(&widgets::tag("Default", "accent"));
        }
        if models::has_update(&m.name) {
            title_row.append(&widgets::tag("Update available", "accent"));
        }
        if let Some(i) = &info {
            if drafter { title_row.append(&widgets::tag("Drafter", "")); }
            for (cap, label) in [("vision", "Vision"), ("tools", "Tools"), ("thinking", "Thinking"), ("embedding", "Embedding")] {
                if i.has(cap) {
                    title_row.append(&widgets::tag(label, ""));
                }
            }
        }
        text.prepend(&title_row);
    }
    let controls = widgets::hbox(6);
    controls.set_valign(gtk::Align::Center);
    if models::has_update(&m.name) {
        let up = gtk::Button::with_label("Update");
        let n = m.name.clone();
        up.connect_clicked(move |_| pulls::start(&n));
        controls.append(&up);
    }
    if info.as_ref().is_none_or(|i| i.capabilities.is_empty() || i.has("completion")) {
        let chat = gtk::Button::with_label("Chat");
        let n = m.name.clone();
        chat.connect_clicked(move |_| {
            crate::sections::chat::new_chat(None);
            models::pick(&n);
            if let Some(p) = crate::sections::chat::page() {
                p.st.borrow_mut().chat_mut().model = n.clone();
                crate::sections::chat::refresh_header(&p);
                crate::sections::chat::refresh_banners(&p);
            }
        });
        controls.append(&chat);
    }
    if !drafter {
        let loaded = models::is_loaded(&m.name);
        let load = gtk::Button::with_label(if loaded { "Reload" } else { "Load" });
        load.set_tooltip_text(Some(if loaded { "Unload and load this model again" } else { "Load this model into memory" }));
        let name = m.name.clone();
        load.connect_clicked(move |button| {
            button.set_sensitive(false);
            if loaded {
                let (conn, n, keep_alive) = (Conn::current(), name.clone(), prefs::get().keep_alive);
                rt::spawn(async move {
                    api::unload(&conn, &n).await?;
                    api::preload(&conn, &n, &keep_alive).await
                }, |result| {
                    if let Err(error) = result { window::toast(&format!("Couldn't reload model: {error:#}")); }
                    models::refresh();
                });
            } else {
                models::load(&name, None);
            }
        });
        controls.append(&load);
    }
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("view-more-symbolic");
    menu.add_css_class("flat");
    let m2 = m.clone();
    menu.set_create_popup_func(move |mb| mb.set_popover(Some(&model_menu(&m2))));
    controls.append(&menu);
    row.append(&controls);
    row.upcast()
}

fn menu_item(pop: &gtk::Popover, label: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("flat");
    b.add_css_class("menu-row");
    if let Some(l) = b.child().and_downcast::<gtk::Label>() {
        l.set_xalign(0.0);
    }
    let p = pop.clone();
    b.connect_clicked(move |_| {
        p.popdown();
        f();
    });
    b
}

fn model_menu(m: &Installed) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let v = widgets::vbox(1);
    let name = m.name.clone();
    v.append(&menu_item(&pop, "Details…", move || details(&name)));
    if crate::llama::api::local_service() {
        let name = m.name.clone();
        v.append(&menu_item(&pop, "Settings…", move || model_settings_dialog(&name)));
    }
    let name = m.name.clone();
    v.append(&menu_item(&pop, "Use for new chats", move || {
        prefs::update(|p| p.new_chat_model = name.clone());
        hub::emit(&hub::MODELS);
        window::toast(&format!("New chats start with {name}"));
    }));
    let name = m.name.clone();
    if prefs::get().host().backend == prefs::Backend::Ollama { v.append(&menu_item(&pop, "Copy as…", move || {
        let src = name.clone();
        widgets::prompt_text(
            "Copy model",
            "The copy shares its files with the original, so it takes no extra space.",
            &format!("{src}-copy"),
            "Copy",
            move |dst| {
                let (conn, s, d) = (Conn::current(), src.clone(), dst.trim().to_string());
                if d.is_empty() {
                    return;
                }
                rt::spawn(async move { api::copy(&conn, &s, &d).await }, move |r| {
                    match r {
                        Ok(()) => window::toast("Copied"),
                        Err(e) => window::toast(&format!("{e:#}")),
                    }
                    models::refresh();
                });
            },
        );
    })); }
    let name = m.name.clone();
    v.append(&menu_item(&pop, "Download again", move || pulls::start(&name)));
    let name = m.name.clone();
    let p2 = pop.clone();
    let del = widgets::two_click("Delete", "Click again to delete", move || {
        p2.popdown();
        let (conn, n) = (Conn::current(), name.clone());
        let deleted = name.clone();
        rt::spawn(async move { api::delete(&conn, &n).await }, move |r| {
            match r {
                Ok(()) => {
                    if crate::llama::api::local_service() {
                        prefs::update(|p| { p.model_settings.remove(&deleted); });
                    }
                    window::toast("Model deleted");
                },
                Err(e) => window::toast(&format!("{e:#}")),
            }
            models::refresh();
        });
    });
    del.add_css_class("menu-row");
    v.append(&del);
    pop.set_child(Some(&v));
    pop
}

/// Opens with what Nyx AI Core loads the model with now.
fn model_settings_dialog(name: &str) {
    let (model, n) = (name.to_string(), name.to_string());
    rt::spawn(async move { crate::llama::core::settings_of(&n).await }, move |r| match r {
        Ok(current) => {
            if !crate::llama::presets::pending(&model) {
                prefs::update(|p| { p.model_settings.insert(model.clone(), current.clone()); });
            }
            settings_dialog(&model, if crate::llama::presets::pending(&model) { prefs::for_model(&model) } else { current });
        }
        Err(e) => window::toast(&format!("Couldn't read the model's settings: {}", api::short_error(&format!("{e:#}")))),
    });
}

fn settings_dialog(name: &str, current: prefs::ModelSettings) {
    let (dialog, card) = widgets::dialog(&format!("Model settings · {}", util::model_label(name)), 520);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(480).max_content_height(640).propagate_natural_height(true).build();
    let body = widgets::vbox(8);
    scroller.set_child(Some(&body));
    card.append(&scroller);

    let context_opts = widgets::opts(&[("2048", "2K"), ("4096", "4K"), ("8192", "8K"),
        ("16384", "16K"), ("32768", "32K"), ("65536", "64K"), ("131072", "128K"), ("262144", "256K")]);
    let context = widgets::dropdown(&context_opts, &current.num_ctx.to_string());
    body.append(&widgets::row("Context length", "Reserved when this model loads. Longer contexts use more memory.", Some(context.upcast_ref())));

    let gpu_opts = widgets::opts(&[("auto", "Automatic"), ("all", "All layers"), ("0", "CPU only")]);
    let gpu = widgets::dropdown(&gpu_opts, &current.gpu_layers);
    body.append(&widgets::row("GPU offload", "Automatic usually gives the best fit.", Some(gpu.upcast_ref())));

    let flash_opts = widgets::opts(&[("auto", "Automatic"), ("on", "On"), ("off", "Off")]);
    let flash = widgets::dropdown(&flash_opts, &current.flash_attention);
    body.append(&widgets::row("Flash Attention", "Quantized KV cache requires this to be on.", Some(flash.upcast_ref())));

    let kv_opts = widgets::opts(&[("f16", "F16 · full precision"), ("q8_0", "Q8_0 · smaller"), ("q4_0", "Q4_0 · smallest")]);
    let kv = widgets::dropdown(&kv_opts, &current.kv_cache);
    body.append(&widgets::row("KV cache", "Applies the same type to keys and values. Q4_0 saves the most memory but may affect answers.", Some(kv.upcast_ref())));
    let flash2 = flash.clone();
    kv.connect_selected_notify(move |d| {
        let quantized = d.selected() != 0;
        if quantized { flash2.set_selected(1); }
        flash2.set_sensitive(!quantized);
    });
    if current.kv_cache != "f16" { flash.set_selected(1); flash.set_sensitive(false); }

    body.append(&widgets::label("SAMPLING", "group-title"));
    let number = |title: &str, description: &str, min: f64, max: f64, step: f64, value: f64, digits: u32| {
        let control = gtk::SpinButton::with_range(min, max, step);
        control.set_digits(digits);
        control.set_value(value);
        body.append(&widgets::row(title, description, Some(control.upcast_ref())));
        control
    };
    let temp = number("Temperature", "Lower is more focused.", 0.0, 2.0, 0.05, current.temperature, 2);
    let top_p = number("Top P", "Probability mass considered during sampling.", 0.05, 1.0, 0.05, current.top_p, 2);
    let top_k = number("Top K", "Number of likely tokens considered.", 0.0, 100.0, 1.0, current.top_k as f64, 0);
    let min_p = number("Min P", "Filter unlikely tokens relative to the best one.", 0.0, 1.0, 0.01, current.min_p, 2);
    let repeat = number("Repetition penalty", "Values above 1 reduce repetition.", 1.0, 2.0, 0.05, current.repeat_penalty, 2);

    let detected = models::info(name).map(|i| i.reasoning_efforts).unwrap_or_default();
    let hidden = prefs::get().reasoning_hidden.get(name).cloned().unwrap_or_default();
    let mut effort_checks = Vec::new();
    if !detected.is_empty() {
        body.append(&widgets::label("REASONING LEVELS", "group-title"));
        let help = widgets::label("The model's template accepts these levels. Hide any that do not work well with this model.", "dim");
        help.set_wrap(true);
        body.append(&help);
        for &(label, level) in crate::llama::reasoning::CANDIDATES {
            if !detected.iter().any(|s| s == level) { continue; }
            let check = gtk::CheckButton::with_label(label);
            check.set_active(!hidden.iter().any(|s| s == level));
            body.append(&check);
            effort_checks.push((level.to_string(), check));
        }
    }

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
    let model = name.to_string();
    save.connect_clicked(move |_| {
        let setting = prefs::ModelSettings {
            num_ctx: context_opts[context.selected() as usize].0.parse().unwrap_or(8192),
            gpu_layers: gpu_opts[gpu.selected() as usize].0.clone(),
            flash_attention: if kv.selected() == 0 { flash_opts[flash.selected() as usize].0.clone() } else { "on".into() },
            kv_cache: kv_opts[kv.selected() as usize].0.clone(),
            temperature: temp.value(), top_p: top_p.value(), top_k: top_k.value() as i64,
            min_p: min_p.value(), repeat_penalty: repeat.value(),
        };
        let runtime_changed = setting != current;
        let hidden: Vec<String> = effort_checks.iter().filter(|(_, check)| !check.is_active())
            .map(|(level, _)| level.clone()).collect();
        prefs::update(|p| {
            p.model_settings.insert(model.clone(), setting);
            if !effort_checks.is_empty() {
                if hidden.is_empty() { p.reasoning_hidden.remove(&model); }
                else { p.reasoning_hidden.insert(model.clone(), hidden); }
            }
        });
        if runtime_changed { crate::llama::presets::mark_with_context(model.clone(), current.num_ctx); }
        hub::emit(&hub::MODELS);
        window::toast(if crate::llama::presets::pending(&model) { "Saved; runtime changes apply on the next model load" } else { "Model settings saved" });
        d.close();
    });
    dialog.present();
}

fn details(name: &str) {
    let (dialog, card) = widgets::dialog(name, 640);
    let body = widgets::vbox(10);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(420)
        .max_content_height(620)
        .propagate_natural_height(true)
        .child(&body)
        .build();
    card.append(&scroll);
    let sp = gtk::Spinner::new();
    sp.start();
    body.append(&sp);
    let (conn, n) = (Conn::current(), name.to_string());
    let b2 = body.clone();
    rt::spawn(async move { api::show(&conn, &n).await }, move |r| {
        widgets::clear(&b2);
        let info = match r {
            Ok(i) => i,
            Err(e) => {
                b2.append(&widgets::banner(&util::esc(&format!("{e:#}")), true));
                return;
            }
        };
        let tags = widgets::hbox(6);
        for c in &info.capabilities {
            tags.append(&widgets::tag(c, ""));
        }
        if let Some(ctx) = info.context_length {
            tags.append(&widgets::tag(&format!("{} context", util::count(ctx)), "accent"));
        }
        b2.append(&tags);
        for (title, text) in [
            ("Parameters", &info.parameters),
            ("System prompt", &info.system),
            ("Template", &info.template),
            ("Licence", &info.license),
        ] {
            if text.trim().is_empty() {
                continue;
            }
            b2.append(&widgets::label(&title.to_uppercase(), "group-title"));
            let l = widgets::label(&util::clip(text.trim(), 6000), "code-block");
            l.set_wrap(true);
            l.set_wrap_mode(pango::WrapMode::WordChar);
            l.set_selectable(true);
            b2.append(&l);
        }
    });
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

fn create_dialog() {
    let (dialog, card) = widgets::dialog("Create a model", 520);
    let explain = widgets::label("Starts from a model you have. Nothing is downloaded or copied.", "dim");
    explain.set_wrap(true);
    card.append(&explain);
    card.append(&widgets::label("Name", "field-label"));
    let name = gtk::Entry::new();
    name.set_placeholder_text(Some("my-helper"));
    name.add_css_class("mono");
    card.append(&name);
    card.append(&widgets::label("Based on", "field-label"));
    let bases = models::chat_models();
    let opts: Vec<(String, String)> = bases.iter().map(|m| (m.clone(), util::model_label(m))).collect();
    let base = widgets::dropdown(&opts, &models::default_model());
    card.append(&base);
    card.append(&widgets::label("System prompt", "field-label"));
    let (sw, sys) = widgets::text_area("", false, 100);
    card.append(&sw);
    card.append(&widgets::label("Temperature", "field-label"));
    let temp = gtk::SpinButton::with_range(0.0, 2.0, 0.05);
    temp.set_value(0.7);
    temp.set_digits(2);
    card.append(&temp);
    card.append(&widgets::label("Context length", "field-label"));
    let ctx_opts = widgets::opts(&[
        ("4096", "4K tokens"),
        ("8192", "8K tokens"),
        ("16384", "16K tokens"),
        ("32768", "32K tokens"),
        ("65536", "64K tokens"),
    ]);
    let ctx = widgets::dropdown(&ctx_opts, "8192");
    card.append(&ctx);
    let status = widgets::label("", "dim");
    card.append(&status);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let create = gtk::Button::with_label("Create");
    create.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&create);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    create.connect_clicked(move |b| {
        let n = name.text().trim().to_string();
        let Some(from) = bases.get(base.selected() as usize).cloned() else { return };
        if n.is_empty() {
            name.grab_focus();
            return;
        }
        b.set_sensitive(false);
        let mut params = serde_json::Map::new();
        params.insert("temperature".into(), serde_json::json!(temp.value()));
        let c: i64 = ctx_opts.get(ctx.selected() as usize).and_then(|o| o.0.parse().ok()).unwrap_or(8192);
        params.insert("num_ctx".into(), serde_json::json!(c));
        let system = widgets::buffer_text(&sys);
        let conn = Conn::current();
        let (d2, st2, b2) = (d.clone(), status.clone(), b.clone());
        let (done_tx, done_rx) = async_channel::bounded::<Result<(), String>>(1);
        let n2 = n.clone();
        rt::stream(
            move |tx| async move {
                let r = api::create(&conn, &n2, &from, &system, params, tx).await.map_err(|e| format!("{e:#}"));
                let _ = done_tx.send(r).await;
            },
            move |p: api::Progress| st2.set_text(&p.status),
        );
        glib::spawn_future_local(async move {
            match done_rx.recv().await.unwrap_or(Err("stopped".into())) {
                Ok(()) => {
                    window::toast(&format!("Created {n}"));
                    models::refresh();
                    d2.close();
                }
                Err(e) => {
                    window::toast(&e);
                    b2.set_sensitive(true);
                }
            }
        });
    });
    dialog.present();
}
