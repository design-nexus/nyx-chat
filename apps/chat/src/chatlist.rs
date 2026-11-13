//! The chat list on the Chats page: pinned chats, folders and chats by date, or
//! search results while the search field has text.

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::sections::chat;
use crate::store::{self, Chat, Folder};
use crate::{hub, models, transfer, util, widgets, window};

thread_local! {
    static SHOW_ARCHIVED: Cell<bool> = const { Cell::new(false) };
    /// Chats deleted a moment ago, hidden until the Undo toast has gone.
    static DELETING: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
}

/// How long Undo is offered before a deleted chat is really gone.
const UNDO_SECONDS: u32 = 7;

/// Delete a chat with an Undo: it leaves the list now and the database once
/// the toast has gone. If the app quits first, the chat stays.
fn delete_with_undo(id: i64) {
    DELETING.with(|d| d.borrow_mut().push(id));
    chat::closed(id);
    hub::emit(&hub::CHATS);
    window::toast_action("Chat deleted", "Undo", move || {
        DELETING.with(|d| d.borrow_mut().retain(|x| *x != id));
        hub::emit(&hub::CHATS);
    });
    glib::timeout_add_seconds_local_once(UNDO_SECONDS, move || {
        let still = DELETING.with(|d| {
            let mut d = d.borrow_mut();
            let had = d.contains(&id);
            d.retain(|x| *x != id);
            had
        });
        if still {
            let _ = store::delete_chat(id);
            hub::emit(&hub::CHATS);
        }
    });
}

/// The list, driven by `search`. It rebuilds itself when chats change.
pub fn build(search: &gtk::SearchEntry) -> gtk::Widget {
    let outer = widgets::vbox(0);
    let list = widgets::vbox(1);
    list.add_css_class("chat-list");
    outer.append(&list);

    let query = Rc::new(RefCell::new(String::new()));
    let rebuild = {
        let (list, query) = (list.clone(), query.clone());
        Rc::new(move || fill(&list, &query.borrow()))
    };
    rebuild();
    let r = rebuild.clone();
    hub::on(&hub::CHATS, &list, move || r());

    let pending: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
    let (r, q) = (rebuild.clone(), query.clone());
    search.connect_search_changed(move |e| {
        *q.borrow_mut() = e.text().trim().to_string();
        if let Some(id) = pending.take() {
            id.remove();
        }
        let (r, p2) = (r.clone(), pending.clone());
        pending.set(Some(glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
            p2.set(None);
            r();
        })));
    });
    search.connect_activate(|e| {
        let q = e.text().to_string();
        if let Ok(hits) = store::search(&q)
            && let Some(first) = hits.first()
        {
            chat::open(first.chat);
        }
    });
    outer.upcast()
}

/// A sidebar group label; dropping a chat on it takes it out of its folder.
fn group_label(title: &str, add: window::NavAdd<'_>) -> gtk::Box {
    let head = window::nav_group(title, false, add);
    head.add_controller(drop_target(None));
    head
}

/// `head` in its section's colour from the theme's [sections.chat].
fn sectioned(head: gtk::Box, section: &str) -> gtk::Box {
    nyx_ui::sidebar::set_section(&head, section);
    head
}

fn fill(list: &gtk::Box, query: &str) {
    widgets::clear(list);
    if !query.is_empty() {
        fill_search(list, query);
        return;
    }
    let archived = SHOW_ARCHIVED.with(|a| a.get());
    let mut chats = store::chats(archived).unwrap_or_default();
    DELETING.with(|d| chats.retain(|c| !d.borrow().contains(&c.id)));

    if archived {
        list.append(&group_label("Archived", None));
        if chats.is_empty() {
            list.append(&empty_note("Nothing archived."));
        }
        for c in &chats {
            list.append(&chat_row(c));
        }
    } else {
        let folders = store::folders().unwrap_or_default();
        let pinned: Vec<&Chat> = chats.iter().filter(|c| c.pinned && c.folder.is_none()).collect();
        if !pinned.is_empty() {
            list.append(&sectioned(group_label("Pinned", None), "pinned"));
            for c in pinned {
                list.append(&chat_row(c));
            }
        }

        list.append(&sectioned(group_label("Folders", Some(("New folder", Box::new(|| edit_folder(Folder::default()))))), "folders"));
        if folders.is_empty() {
            list.append(&empty_note("Folders give their chats a model, prompt and tools."));
        }
        for f in &folders {
            list.append(&folder_header(f));
            if !f.collapsed {
                for c in chats.iter().filter(|c| c.folder == Some(f.id)) {
                    let row = chat_row(c);
                    row.add_css_class("in-folder");
                    list.append(&row);
                }
            }
        }

        list.append(&sectioned(group_label("Chats", Some(("New chat (Ctrl+N)", Box::new(|| chat::new_chat(None))))), "chats"));
        let loose: Vec<&Chat> = chats.iter().filter(|c| !c.pinned && c.folder.is_none()).collect();
        if loose.is_empty() {
            list.append(&empty_note("Your chats will appear here."));
        }
        let mut last_group = String::new();
        for c in loose {
            let group = date_group(c.updated);
            if group != last_group {
                list.append(&widgets::label(&group, "chat-date-group"));
                last_group = group;
            }
            list.append(&chat_row(c));
        }
    }

    let n_archived = if archived { 0 } else { store::chats(true).map(|c| c.len()).unwrap_or(0) };
    if archived || n_archived > 0 {
        let b = gtk::Button::with_label(&if archived {
            "Back to chats".to_string()
        } else {
            format!("Archived chats ({n_archived})")
        });
        b.add_css_class("flat");
        b.add_css_class("archive-toggle");
        b.connect_clicked(|_| {
            SHOW_ARCHIVED.with(|a| a.set(!a.get()));
            hub::emit(&hub::CHATS);
        });
        list.append(&b);
    }
}

fn empty_note(text: &str) -> gtk::Label {
    let l = widgets::label(text, "chat-list-empty");
    l.set_wrap(true);
    // A wrapping label asks for its whole line; this keeps the sidebar narrow.
    l.set_max_width_chars(24);
    l
}

fn date_group(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return "Older".into() };
    let days = (Local::now().date_naive() - t.date_naive()).num_days();
    match days {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=7 => "Previous 7 days".into(),
        8..=30 => "Previous 30 days".into(),
        _ => t.format("%B %Y").to_string(),
    }
}

fn fill_search(list: &gtk::Box, query: &str) {
    let hits = store::search(query).unwrap_or_default();
    list.append(&window::nav_group(&format!("{} result{}", hits.len(), if hits.len() == 1 { "" } else { "s" }), false, None));
    for h in hits {
        let b = gtk::Button::new();
        b.add_css_class("chat-item");
        b.add_css_class("search-result");
        let v = widgets::vbox(2);
        let t = widgets::label(&h.title, "chat-title");
        t.set_ellipsize(pango::EllipsizeMode::End);
        v.append(&t);
        if !h.snippet.is_empty() {
            let s = gtk::Label::new(None);
            let markup = util::esc(&h.snippet).replace('\u{1}', "<b>").replace('\u{2}', "</b>");
            s.set_markup(&markup);
            s.add_css_class("chat-snippet");
            s.set_xalign(0.0);
            s.set_wrap(true);
            s.set_lines(2);
            s.set_ellipsize(pango::EllipsizeMode::End);
            v.append(&s);
        }
        b.set_child(Some(&v));
        let id = h.chat;
        b.connect_clicked(move |_| chat::open(id));
        list.append(&b);
    }
}

fn chat_row(c: &Chat) -> gtk::Widget {
    // The ⋯ button floats over the row's right end, so the pill spans the width.
    let row = gtk::Overlay::new();
    row.add_css_class("chat-row");
    let b = gtk::Button::new();
    b.add_css_class("chat-item");
    b.set_hexpand(true);
    if chat::current_id() == Some(c.id) {
        b.add_css_class("active");
    }
    let content = widgets::hbox(6);
    if c.pinned {
        let pin = gtk::Image::from_icon_name("view-pin-symbolic");
        pin.add_css_class("dim");
        content.append(&pin);
    }
    let t = widgets::label(&c.title, "chat-title");
    t.set_ellipsize(pango::EllipsizeMode::End);
    t.set_hexpand(true);
    t.set_xalign(0.0);
    content.append(&t);
    let meta = if c.model.is_empty() { util::ago(c.updated) } else { format!("{} · {}", util::model_label(&c.model), util::ago(c.updated)) };
    let m = widgets::label(&meta, "chat-meta");
    m.set_ellipsize(pango::EllipsizeMode::Middle);
    m.set_max_width_chars(36);
    content.append(&m);
    b.set_child(Some(&content));
    b.set_tooltip_text(Some(&format!("{} · {}", c.title, util::ago(c.updated))));
    let id = c.id;
    b.connect_clicked(move |_| chat::open(id));

    // Drag onto a folder to move it.
    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::MOVE);
    drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&format!("chat:{id}").to_value())));
    b.add_controller(drag);

    // Right-click opens the same menu as the ⋯ button.
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("view-more-symbolic");
    menu.add_css_class("flat");
    menu.add_css_class("row-menu");
    menu.set_tooltip_text(Some("More"));
    let c2 = c.clone();
    menu.set_create_popup_func(move |m| {
        m.set_popover(Some(&chat_menu(&c2)));
    });
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let m2 = menu.clone();
    click.connect_pressed(move |_, _, _, _| m2.popup());
    b.add_controller(click);

    menu.set_halign(gtk::Align::End);
    menu.set_valign(gtk::Align::Center);
    row.set_child(Some(&b));
    row.add_overlay(&menu);
    row.upcast()
}

fn menu_button(label: &str, pop: &gtk::Popover, f: impl Fn() + 'static) -> gtk::Button {
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

fn update_chat(id: i64, f: impl FnOnce(&mut Chat)) {
    if let Ok(Some(mut c)) = store::chat(id) {
        f(&mut c);
        let _ = store::save_chat(&c);
        hub::emit(&hub::CHATS);
        chat::reload_if_current(id);
    }
}

pub fn chat_menu(c: &Chat) -> gtk::Popover {
    let pop = gtk::Popover::new();
    pop.add_css_class("menu");
    let v = widgets::vbox(1);
    let id = c.id;
    let title = c.title.clone();
    v.append(&menu_button("Rename…", &pop, move || rename(id, &title)));
    let pinned = c.pinned;
    v.append(&menu_button(if pinned { "Unpin" } else { "Pin" }, &pop, move || update_chat(id, |c| c.pinned = !pinned)));
    let tags = c.tags.join(", ");
    v.append(&menu_button("Tags…", &pop, move || {
        widgets::prompt_text("Tags", "Separate tags with commas.", &tags, "Save", move |t| {
            update_chat(id, |c| c.tags = t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        })
    }));

    let folders = store::folders().unwrap_or_default();
    if !folders.is_empty() || c.folder.is_some() {
        v.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        v.append(&widgets::label("MOVE TO", "menu-heading-small"));
        if c.folder.is_some() {
            v.append(&menu_button("No folder", &pop, move || update_chat(id, |c| c.folder = None)));
        }
        for f in folders.into_iter().filter(|f| Some(f.id) != c.folder) {
            let fid = f.id;
            v.append(&menu_button(&f.name, &pop, move || update_chat(id, |c| c.folder = Some(fid))));
        }
    }

    v.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let title = c.title.clone();
    v.append(&menu_button("Export as Markdown", &pop, move || match transfer::markdown(id) {
        Ok(text) => widgets::save_text(&format!("{}.md", file_name(&title)), text),
        Err(e) => window::toast(&format!("Couldn't export: {e}")),
    }));
    let title = c.title.clone();
    v.append(&menu_button("Export as JSON", &pop, move || match transfer::export_one(id) {
        Ok(text) => widgets::save_text(&format!("{}.json", file_name(&title)), text),
        Err(e) => window::toast(&format!("Couldn't export: {e}")),
    }));
    let archived = c.archived;
    v.append(&menu_button(if archived { "Unarchive" } else { "Archive" }, &pop, move || {
        update_chat(id, |c| c.archived = !archived);
        window::toast(if archived { "Chat restored" } else { "Chat archived" });
    }));
    let p2 = pop.clone();
    let del = gtk::Button::with_label("Delete");
    del.connect_clicked(move |_| {
        p2.popdown();
        delete_with_undo(id);
    });
    del.add_css_class("menu-row");
    v.append(&del);
    pop.set_child(Some(&v));
    pop
}

fn file_name(title: &str) -> String {
    let s: String = title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "chat".into() } else { s }
}

pub fn rename(id: i64, current: &str) {
    widgets::prompt_text("Rename chat", "", current, "Rename", move |t| {
        let t = t.trim().to_string();
        if !t.is_empty() {
            update_chat(id, |c| c.title = t);
        }
    });
}

fn drop_target(folder: Option<i64>) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    target.connect_drop(move |_, value, _, _| {
        let Ok(s) = value.get::<String>() else { return false };
        let Some(id) = s.strip_prefix("chat:").and_then(|x| x.parse::<i64>().ok()) else { return false };
        update_chat(id, |c| c.folder = folder);
        true
    });
    target
}

fn folder_header(f: &Folder) -> gtk::Widget {
    let row = gtk::Overlay::new();
    row.add_css_class("chat-row");
    let b = gtk::Button::new();
    b.add_css_class("chat-item");
    b.add_css_class("folder-item");
    b.set_hexpand(true);
    let content = widgets::hbox(8);
    content.append(&gtk::Image::from_icon_name(if f.collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" }));
    content.append(&gtk::Image::from_icon_name("folder-symbolic"));
    let t = widgets::label(&f.name, "chat-title");
    t.set_ellipsize(pango::EllipsizeMode::End);
    t.set_hexpand(true);
    content.append(&t);
    b.set_child(Some(&content));
    let f2 = f.clone();
    b.connect_clicked(move |_| {
        let mut f = f2.clone();
        f.collapsed = !f.collapsed;
        let _ = store::save_folder(&f);
        hub::emit(&hub::CHATS);
    });
    b.add_controller(drop_target(Some(f.id)));

    let menu = gtk::MenuButton::new();
    menu.set_icon_name("view-more-symbolic");
    menu.add_css_class("flat");
    menu.add_css_class("row-menu");
    let f2 = f.clone();
    menu.set_create_popup_func(move |m| {
        let pop = gtk::Popover::new();
        pop.add_css_class("menu");
        let v = widgets::vbox(1);
        let id = f2.id;
        v.append(&menu_button("New chat here", &pop, move || chat::new_chat(Some(id))));
        let f3 = f2.clone();
        v.append(&menu_button("Folder settings…", &pop, move || edit_folder(f3.clone())));
        let p2 = pop.clone();
        let del = widgets::two_click("Delete folder", "Click again: chats stay", move || {
            p2.popdown();
            let _ = store::delete_folder(id);
            hub::emit(&hub::CHATS);
        });
        del.add_css_class("menu-row");
        v.append(&del);
        pop.set_child(Some(&v));
        m.set_popover(Some(&pop));
    });
    menu.set_halign(gtk::Align::End);
    menu.set_valign(gtk::Align::Center);
    row.set_child(Some(&b));
    row.add_overlay(&menu);
    row.upcast()
}

/// Create or edit a folder: its name and the defaults chats inside it use.
pub fn edit_folder(f: Folder) {
    let new = f.id == 0;
    let (dialog, card) = widgets::dialog(if new { "New folder" } else { "Folder settings" }, 520);
    let explain = widgets::label("Chats in this folder start with these settings, like a project.", "dim");
    explain.set_wrap(true);
    card.append(&explain);

    card.append(&widgets::label("Name", "field-label"));
    let name = gtk::Entry::new();
    name.set_text(&f.name);
    name.set_placeholder_text(Some("Folder name"));
    card.append(&name);

    card.append(&widgets::label("Model", "field-label"));
    let mut options = vec![(String::new(), "Use the default".to_string())];
    options.extend(models::chat_models().into_iter().map(|m| (m.clone(), util::model_label(&m))));
    let model = widgets::dropdown(&options, &f.model);
    if model.selected() == gtk::INVALID_LIST_POSITION {
        model.set_selected(0);
    }
    card.append(&model);

    card.append(&widgets::label("System prompt", "field-label"));
    let (sys_sw, sys) = widgets::text_area(&f.system, false, 90);
    card.append(&sys_sw);

    card.append(&widgets::label("Tools", "field-label"));
    let tools_box = gtk::FlowBox::new();
    tools_box.set_selection_mode(gtk::SelectionMode::None);
    tools_box.set_max_children_per_line(3);
    let mut tool_checks = Vec::new();
    for t in crate::tools::BUILTIN {
        let c = gtk::CheckButton::with_label(t.title);
        c.set_active(f.tools.iter().any(|x| x == t.name));
        tools_box.insert(&c, -1);
        tool_checks.push((t.name, c));
    }
    card.append(&tools_box);
    let note = widgets::label("Leave all unticked to use the tools from Settings.", "dim");
    note.add_css_class("small-text");
    card.append(&note);

    let kbs = store::kbases().unwrap_or_default();
    let mut kb_checks = Vec::new();
    if !kbs.is_empty() {
        card.append(&widgets::label("Knowledge", "field-label"));
        let kb_box = gtk::FlowBox::new();
        kb_box.set_selection_mode(gtk::SelectionMode::None);
        kb_box.set_max_children_per_line(3);
        for kb in kbs {
            let c = gtk::CheckButton::with_label(&kb.name);
            c.set_active(f.knowledge.contains(&kb.id));
            kb_box.insert(&c, -1);
            kb_checks.push((kb.id, c));
        }
        card.append(&kb_box);
    }

    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_top(8);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label(if new { "Create" } else { "Save" });
    save.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&save);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    let name2 = name.clone();
    save.connect_clicked(move |_| {
        let name = &name2;
        let n = name.text().trim().to_string();
        if n.is_empty() {
            name.grab_focus();
            return;
        }
        let folder = Folder {
            id: f.id,
            name: n,
            model: options.get(model.selected() as usize).map(|o| o.0.clone()).unwrap_or_default(),
            system: widgets::buffer_text(&sys),
            tools: tool_checks.iter().filter(|(_, c)| c.is_active()).map(|(n, _)| n.to_string()).collect(),
            knowledge: kb_checks.iter().filter(|(_, c)| c.is_active()).map(|(id, _)| *id).collect(),
            collapsed: f.collapsed,
        };
        if let Err(e) = store::save_folder(&folder) {
            window::toast(&format!("Couldn't save the folder: {e}"));
        }
        hub::emit(&hub::CHATS);
        d.close();
    });
    dialog.present();
    name.grab_focus();
}
