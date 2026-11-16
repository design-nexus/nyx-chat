//! The side panel: this chat's settings, or a preview of an HTML, SVG or
//! Mermaid block.

use gtk::prelude::*;
use std::rc::Rc;

use super::Page;
use crate::store::Params;
use crate::{models, paths, prefs, store, util, widgets, window};

fn heading(text: &str) -> gtk::Label {
    widgets::label(&text.to_uppercase(), "panel-heading")
}

fn slider(min: f64, max: f64, step: f64, value: f64, digits: usize, on: impl Fn(f64) + 'static) -> gtk::Box {
    let row = widgets::hbox(8);
    let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    s.set_value(value);
    s.set_hexpand(true);
    s.set_draw_value(false);
    let readout = widgets::label(&format!("{value:.digits$}"), "value-readout");
    readout.add_css_class("tnum");
    readout.set_xalign(1.0);
    readout.set_width_chars(5);
    let r2 = readout.clone();
    s.connect_value_changed(move |s| {
        let v = (s.value() / step).round() * step;
        r2.set_text(&format!("{v:.digits$}"));
        on(v);
    });
    row.append(&s);
    row.append(&readout);
    row
}

fn with_params(p: &Page, f: impl FnOnce(&mut Params)) {
    f(&mut p.st.borrow_mut().chat_mut().params);
    super::save_chat(p);
}

pub(super) fn fill(p: &Rc<Page>) {
    let b = &p.settings_box;
    widgets::clear(b);
    b.add_css_class("panel-body");
    let chat = p.st.borrow().chat().clone();
    let pr = prefs::get();
    let pa = chat.params.clone();

    // No title: the header already names the chat.
    let head = widgets::hbox(8);
    head.set_halign(gtk::Align::End);
    let reset = gtk::Button::with_label("Reset");
    reset.add_css_class("flat");
    reset.set_tooltip_text(Some("Use the defaults from Settings"));
    let p2 = p.clone();
    reset.connect_clicked(move |_| {
        {
            let mut st = p2.st.borrow_mut();
            let c = st.chat_mut();
            c.params = Params::default();
            c.system.clear();
            c.schema.clear();
        }
        super::save_chat(&p2);
        fill(&p2);
        p2.composer.refresh();
    });
    head.append(&reset);
    b.append(&head);

    b.append(&heading("System prompt"));
    let hint = if pr.system_prompt.is_empty() {
        "How the model should behave in this chat."
    } else {
        "Leave empty to use the default from Settings."
    };
    let (sw, tv) = widgets::text_area(&chat.system, false, 90);
    b.append(&sw);
    let h = widgets::label(hint, "panel-note");
    h.set_wrap(true);
    b.append(&h);
    let vars = widgets::label("{{CURRENT_DATE}} {{USER_NAME}} {{USER_LOCATION}} {{CLIPBOARD}}", "panel-note");
    vars.add_css_class("mono");
    vars.set_wrap(true);
    b.append(&vars);
    let p2 = p.clone();
    tv.buffer().connect_changed(move |buf| {
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        p2.st.borrow_mut().chat_mut().system = text;
        super::save_chat(&p2);
    });

    b.append(&heading("Temperature"));
    let model_defaults = prefs::for_model(&chat.model);
    let local_llama = crate::llama::api::local_service();
    let p2 = p.clone();
    b.append(&slider(0.0, 2.0, 0.05, pa.temperature.unwrap_or(if local_llama { model_defaults.temperature } else { pr.temperature }), 2, move |v| {
        with_params(&p2, |pa| pa.temperature = Some(v))
    }));
    b.append(&widgets::label("Lower is focused, higher is creative.", "panel-note"));

    b.append(&heading("Top P"));
    let p2 = p.clone();
    b.append(&slider(0.05, 1.0, 0.05, pa.top_p.unwrap_or(if local_llama { model_defaults.top_p } else { pr.top_p }), 2, move |v| with_params(&p2, |pa| pa.top_p = Some(v))));

    b.append(&heading("Top K"));
    let p2 = p.clone();
    b.append(&slider(0.0, 100.0, 1.0, pa.top_k.unwrap_or(if local_llama { model_defaults.top_k } else { pr.top_k }) as f64, 0, move |v| {
        with_params(&p2, |pa| pa.top_k = Some(v as i64))
    }));
    if local_llama {
        b.append(&heading("Min P"));
        let p2 = p.clone();
        b.append(&slider(0.0, 1.0, 0.01, pa.min_p.unwrap_or(model_defaults.min_p), 2,
            move |v| with_params(&p2, |pa| pa.min_p = Some(v))));
        b.append(&heading("Repetition penalty"));
        let p2 = p.clone();
        b.append(&slider(1.0, 2.0, 0.05, pa.repeat_penalty.unwrap_or(model_defaults.repeat_penalty), 2,
            move |v| with_params(&p2, |pa| pa.repeat_penalty = Some(v))));
    }

    b.append(&heading("Context length"));
    let max_ctx = models::info(&chat.model).and_then(|i| i.context_length).unwrap_or(131_072) as i64;
    let max_ctx = if local_llama { max_ctx.min(crate::llama::presets::runtime_context(&chat.model).unwrap_or(model_defaults.num_ctx)) } else { max_ctx };
    let sizes: Vec<i64> =
        [2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144].into_iter().filter(|s| *s <= max_ctx.max(2048)).collect();
    let opts: Vec<(String, String)> =
        sizes.iter().map(|s| (s.to_string(), format!("{} tokens", util::count(*s as u64)))).collect();
    let (_, dd) = {
        let p2 = p.clone();
        let opts2 = opts.clone();
        let dd = widgets::dropdown(&opts, &pa.num_ctx.unwrap_or(if local_llama { model_defaults.num_ctx } else { pr.num_ctx }).to_string());
        dd.connect_selected_notify(move |d| {
            if let Some((v, _)) = opts2.get(d.selected() as usize) {
                let n: i64 = v.parse().unwrap_or(8192);
                with_params(&p2, |pa| pa.num_ctx = Some(n));
            }
        });
        ((), dd)
    };
    b.append(&dd);
    b.append(&widgets::label(if crate::llama::api::local_service() {
        "Limits this chat within the model's context length. Change the model limit on the Models page."
    } else { "More context remembers more, and uses more memory." }, "panel-note"));

    b.append(&heading("When the context is full"));
    let opts = widgets::opts(crate::sections::settings::OVERFLOW);
    let dd = widgets::dropdown(&opts, pa.overflow.as_deref().unwrap_or(&pr.overflow));
    let p2 = p.clone();
    dd.connect_selected_notify(move |d| {
        if let Some((v, _)) = opts.get(d.selected() as usize) {
            with_params(&p2, |pa| pa.overflow = Some(v.clone()));
        }
    });
    b.append(&dd);

    b.append(&heading("Seed"));
    let seed = gtk::Entry::new();
    seed.add_css_class("mono");
    seed.set_placeholder_text(Some("Random"));
    if let Some(s) = pa.seed {
        seed.set_text(&s.to_string());
    }
    let p2 = p.clone();
    seed.connect_changed(move |e| {
        let v = e.text().trim().parse::<i64>().ok();
        with_params(&p2, |pa| pa.seed = v);
    });
    b.append(&seed);

    if models::has(&chat.model, "thinking") {
        let row = widgets::hbox(8);
        let l = widgets::label("Think before answering", "panel-label");
        l.set_hexpand(true);
        row.append(&l);
        let sw = gtk::Switch::new();
        sw.set_active(pa.think.unwrap_or(pr.think));
        let p2 = p.clone();
        sw.connect_active_notify(move |s| {
            with_params(&p2, |pa| pa.think = Some(s.is_active()));
            p2.composer.refresh();
        });
        row.append(&sw);
        b.append(&row);
    }

    // ----- Structured output -----
    b.append(&heading("Structured output"));
    let row = widgets::hbox(8);
    let l = widgets::label("Reply in JSON", "panel-label");
    l.set_hexpand(true);
    row.append(&l);
    let json_sw = gtk::Switch::new();
    json_sw.set_active(pa.json);
    row.append(&json_sw);
    b.append(&row);
    let (schema_sw, schema) = widgets::text_area(&chat.schema, true, 110);
    schema_sw.set_visible(pa.json);
    b.append(&schema_sw);
    let schema_note = widgets::label("Optional JSON schema the reply must follow.", "panel-note");
    schema_note.set_wrap(true);
    schema_note.set_visible(pa.json);
    b.append(&schema_note);
    let (p2, s2, n2) = (p.clone(), schema_sw.clone(), schema_note.clone());
    json_sw.connect_active_notify(move |s| {
        with_params(&p2, |pa| pa.json = s.is_active());
        s2.set_visible(s.is_active());
        n2.set_visible(s.is_active());
        p2.composer.refresh();
        super::render(&p2);
    });
    let (p2, n2) = (p.clone(), schema_note.clone());
    schema.buffer().connect_changed(move |buf| {
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        let valid = text.trim().is_empty() || serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v.is_object());
        n2.set_text(if valid { "Optional JSON schema the reply must follow." } else { "This isn't a valid JSON object yet." });
        if valid {
            n2.remove_css_class("danger-text");
        } else {
            n2.add_css_class("danger-text");
        }
        p2.st.borrow_mut().chat_mut().schema = text;
        super::save_chat(&p2);
    });

    // ----- Knowledge -----
    let kbs = store::kbases().unwrap_or_default();
    if !kbs.is_empty() {
        b.append(&heading("Knowledge"));
        for kb in kbs {
            let c = gtk::CheckButton::with_label(&kb.name);
            c.set_active(chat.knowledge.contains(&kb.id));
            let (p2, id) = (p.clone(), kb.id);
            c.connect_toggled(move |c| {
                {
                    let mut st = p2.st.borrow_mut();
                    let k = &mut st.chat_mut().knowledge;
                    k.retain(|x| *x != id);
                    if c.is_active() {
                        k.push(id);
                    }
                }
                super::save_chat(&p2);
                p2.composer.refresh();
            });
            b.append(&c);
        }
    }
}

// ---------- Previews ----------

pub(super) fn show_artifact(p: &Rc<Page>, lang: &str, code: &str) {
    let b = &p.artifact_box;
    widgets::clear(b);
    let head = widgets::hbox(8);
    let t = widgets::label(
        match lang {
            "svg" | "xml" => "SVG preview",
            "mermaid" => "Diagram",
            _ => "HTML preview",
        },
        "panel-title",
    );
    t.set_hexpand(true);
    head.append(&t);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.set_tooltip_text(Some("Close the preview"));
    let p2 = p.clone();
    close.connect_clicked(move |_| {
        p2.panel.set_visible(false);
        window::set_needs_room(false);
    });
    head.append(&close);
    b.append(&head);

    let is_svg = lang == "svg" || (lang == "xml" && code.contains("<svg"));
    if is_svg {
        match gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(code.as_bytes())) {
            Ok(tex) => {
                let pic = gtk::Picture::for_paintable(&tex);
                pic.set_content_fit(gtk::ContentFit::Contain);
                pic.set_vexpand(true);
                pic.add_css_class("artifact-picture");
                b.append(&pic);
            }
            Err(e) => b.append(&widgets::banner(&format!("This SVG couldn't be drawn: {}", util::esc(&e.to_string())), true)),
        }
    } else {
        let note = widgets::label(
            "Pages and diagrams run in your web browser, kept apart from this app. Nothing is sent anywhere.",
            "panel-note",
        );
        note.set_wrap(true);
        b.append(&note);
        let (sw, _) = widgets::text_area(code, true, 240);
        sw.set_vexpand(true);
        b.append(&sw);
    }

    let row = widgets::hbox(8);
    row.set_halign(gtk::Align::End);
    let save = gtk::Button::with_label("Save…");
    let (c, ext) = (code.to_string(), if is_svg { "svg" } else { "html" });
    let wrapped = if lang == "mermaid" { mermaid_page(code) } else { code.to_string() };
    let w2 = wrapped.clone();
    save.connect_clicked(move |_| {
        widgets::save_text(&format!("artifact.{ext}"), if ext == "svg" { c.clone() } else { w2.clone() })
    });
    row.append(&save);
    let open = gtk::Button::with_label("Open in browser");
    open.add_css_class("suggested-action");
    let ext2 = ext.to_string();
    open.connect_clicked(move |_| open_in_browser(&wrapped, &ext2));
    row.append(&open);
    b.append(&row);

    p.panel_stack.set_visible_child_name("artifact");
    p.panel.set_visible(true);
    window::set_needs_room(true);
}

fn mermaid_page(code: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Diagram</title>\
         <script src=\"https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js\"></script></head>\
         <body style=\"margin:24px;font-family:sans-serif\"><pre class=\"mermaid\">{}</pre>\
         <script>mermaid.initialize({{startOnLoad:true}});</script></body></html>",
        util::esc(code)
    )
}

fn open_in_browser(contents: &str, ext: &str) {
    let dir = paths::cache_dir().join("artifacts");
    let path = dir.join(format!("artifact-{}.{ext}", util::now()));
    if let Err(e) = crate::cmd::atomic_write(&path, contents) {
        window::toast(&format!("Couldn't write the preview: {e}"));
        return;
    }
    let uri = gtk::gio::File::for_path(&path).uri();
    if let Err(e) = gtk::gio::AppInfo::launch_default_for_uri(&uri, gtk::gio::AppLaunchContext::NONE) {
        window::toast(&format!("Couldn't open a browser: {e}"));
    }
}
