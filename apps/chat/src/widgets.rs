//! Nyx-style building blocks: pages, groups and option rows.

pub use nyx_ui::widgets::*;

use crate::paths;
use gtk::pango;
use gtk::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

// ---------- Search registry ----------

pub struct SearchItem {
    pub text: String,
    /// Weak, so rows rebuilt on refresh don't pile up here.
    pub row: gtk::glib::WeakRef<gtk::Widget>,
}

thread_local! {
    pub static SEARCH: RefCell<Vec<SearchItem>> = const { RefCell::new(Vec::new()) };
}

fn register(row: &impl IsA<gtk::Widget>, title: &str, desc: &str, keywords: &str) {
    SEARCH.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|i| i.row.upgrade().is_some());
        s.push(SearchItem {
            text: format!("{title} {desc} {keywords}").to_lowercase(),
            row: row.upcast_ref::<gtk::Widget>().downgrade(),
        })
    });
}

/// The words a row is found by in the settings search.
pub fn search_text(row: &gtk::Widget) -> Option<String> {
    SEARCH.with(|s| s.borrow().iter().find(|i| i.row.upgrade().as_ref() == Some(row)).map(|i| i.text.clone()))
}

// ---------- Page / group ----------

pub struct Page {
    pub root: gtk::ScrolledWindow,
    pub body: gtk::Box,
}

/// A page of the main window. Its name shows in the top bar, so the page itself
/// starts straight with its content.
pub fn page(section: &str) -> Page {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("settings-page");
    body.add_css_class(&format!("page-{section}"));
    body.set_hexpand(true);

    let root = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&body)
        .vexpand(true)
        .build();
    Page { root, body }
}

impl Page {
    pub fn group(&self, title: &str) -> Group {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.add_css_class("settings-group");
        // The settings card lists groups by this name.
        wrapper.set_widget_name(title);
        if !title.is_empty() {
            let l = gtk::Label::new(Some(&title.to_uppercase()));
            l.add_css_class("group-title");
            l.set_xalign(0.0);
            wrapper.append(&l);
        }
        // Rows join into one card, split by hairlines.
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("group-list");
        wrapper.append(&list);
        self.body.append(&wrapper);
        Group { wrapper, list }
    }
}

/// A page's own title bar: its name, a line saying what it is, and its actions at the end.
pub fn page_bar(title: &str, subtitle: &str) -> (gtk::Box, gtk::Label, gtk::Box) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("page-bar");
    bar.append(&label(title, "page-bar-title"));
    let sub = label(subtitle, "page-bar-subtitle");
    sub.set_ellipsize(pango::EllipsizeMode::End);
    sub.set_hexpand(true);
    sub.set_xalign(0.0);
    bar.append(&sub);
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.append(&end);
    (bar, sub, end)
}

/// A button with an icon before its label.
pub fn icon_label_button(icon: &str, text: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let row = hbox(6);
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&label(text, ""));
    b.set_child(Some(&row));
    b.set_valign(gtk::Align::Center);
    b
}

#[derive(Clone)]
pub struct Group {
    pub wrapper: gtk::Box,
    pub list: gtk::Box,
}

impl Group {
    pub fn add(&self, w: &impl IsA<gtk::Widget>) {
        self.list.append(w);
    }

    pub fn note(&self, text: &str) {
        let l = gtk::Label::new(None);
        l.set_markup(text);
        l.add_css_class("group-note");
        l.set_xalign(0.0);
        l.set_wrap(true);
        // Notes sit just under the group title.
        self.wrapper.insert_child_after(&l, self.wrapper.first_child().as_ref());
    }
}

// ---------- Rows ----------

/// An option card: title and description on the left, control on the right.
pub fn row(title: &str, desc: &str, control: Option<&gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.add_css_class("settings-option");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("settings-option-title");
    t.set_xalign(0.0);
    t.set_wrap(true);
    text.append(&t);
    if !desc.is_empty() {
        let d = gtk::Label::new(None);
        d.set_markup(desc);
        d.add_css_class("settings-option-description");
        d.set_xalign(0.0);
        d.set_wrap(true);
        d.set_wrap_mode(pango::WrapMode::WordChar);
        text.append(&d);
    }
    row.append(&text);
    if let Some(c) = control {
        c.set_valign(gtk::Align::Center);
        row.append(c);
    }
    register(&row, title, desc, "");
    row
}

pub fn switch_row(title: &str, desc: &str, active: bool, on_change: impl Fn(bool) + 'static) -> (gtk::Box, gtk::Switch) {
    let sw = gtk::Switch::new();
    sw.set_active(active);
    sw.connect_active_notify(move |s| on_change(s.is_active()));
    let r = row(title, desc, Some(sw.upcast_ref()));
    (r, sw)
}

pub fn dropdown(options: &[(String, String)], current: &str) -> gtk::DropDown {
    let labels: Vec<&str> = options.iter().map(|(_, l)| l.as_str()).collect();
    let dd = gtk::DropDown::from_strings(&labels);
    ellipsize(&dd);
    if let Some(i) = options.iter().position(|(id, _)| id == current) {
        dd.set_selected(i as u32);
    } else {
        dd.set_selected(gtk::INVALID_LIST_POSITION);
    }
    dd
}

/// A closed drop-down shortens its text with an ellipsis instead of growing to
/// its longest option; the open list shows every option in full.
pub fn ellipsize(dd: &gtk::DropDown) {
    ellipsize_to(dd, 30);
}

/// As [`ellipsize`], with the closed text at most `max_chars` wide.
pub fn ellipsize_to(dd: &gtk::DropDown, max_chars: i32) {
    // Typing to search matches the text. Set first: setting it can bring back the
    // default factory.
    dd.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
    dd.set_factory(Some(&string_factory(Some(max_chars))));
    dd.set_list_factory(Some(&string_factory(None)));
}

/// Labels for a drop-down of strings; with a `max_chars`, they ellipsize there. The
/// open list's rows (no `max_chars`) also check the current choice.
fn string_factory(max_chars: Option<i32>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let l = gtk::Label::new(None);
        l.set_xalign(0.0);
        if let Some(max) = max_chars {
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.set_width_chars(6);
            l.set_max_width_chars(max);
            item.set_child(Some(&l));
            return;
        }
        l.set_hexpand(true);
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&l);
        row.append(&check);
        item.set_child(Some(&row));
        item.connect_selected_notify(move |it| check.set_opacity(if it.is_selected() { 1.0 } else { 0.0 }));
    });
    f.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(s) = item.item().and_downcast::<gtk::StringObject>() else { return };
        let child = item.child();
        let label = child.as_ref().and_then(|c| c.downcast_ref::<gtk::Label>().cloned().or_else(|| c.first_child().and_downcast::<gtk::Label>()));
        if let Some(l) = label {
            l.set_text(&s.string());
        }
        if let Some(check) = child.and_then(|c| c.last_child()).and_downcast::<gtk::Image>() {
            check.set_opacity(if item.is_selected() { 1.0 } else { 0.0 });
        }
    });
    f
}

pub fn choice_row(
    title: &str,
    desc: &str,
    options: Vec<(String, String)>,
    current: &str,
    on_change: impl Fn(String) + 'static,
) -> (gtk::Box, gtk::DropDown) {
    let dd = dropdown(&options, current);
    dd.connect_selected_notify(move |d| {
        if let Some((id, _)) = options.get(d.selected() as usize) {
            on_change(id.clone());
        }
    });
    let r = row(title, desc, Some(dd.upcast_ref()));
    (r, dd)
}

pub fn button_row(title: &str, desc: &str, label: &str, on_click: impl Fn(&gtk::Button) + 'static) -> (gtk::Box, gtk::Button) {
    let b = gtk::Button::with_label(label);
    b.connect_clicked(on_click);
    let r = row(title, desc, Some(b.upcast_ref()));
    (r, b)
}

/// A flat icon button with a tooltip.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

/// A one-line label for text that can be long (model names, titles, paths):
/// it shortens with an ellipsis instead of widening the page, and shows the
/// whole text on hover.
pub fn short_label(text: &str, class: &str, mode: pango::EllipsizeMode) -> gtk::Label {
    let l = label(text, class);
    l.set_ellipsize(mode);
    l.set_tooltip_text(Some(text));
    l
}

// ---------- Monitoring pieces ----------

/// A modal card dialog in the app's style. Returns the window and its content box.
pub fn dialog(title: &str, width: i32) -> (gtk::Window, gtk::Box) {
    let dialog = gtk::Window::builder().modal(true).title(title).default_width(width).build();
    if let Some(parent) = crate::window::window() {
        dialog.set_transient_for(Some(&parent));
    }
    dialog.add_css_class("chat-window");
    dialog.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    let card = vbox(12);
    card.add_css_class("dialog-card");
    card.append(&label(title, "dialog-title"));
    dialog.set_child(Some(&card));
    let keys = gtk::EventControllerKey::new();
    let d = dialog.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            d.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    dialog.add_controller(keys);
    (dialog, card)
}

// ---------- Files ----------

/// Ask where to save `contents`, suggesting `name`.
pub fn save_text(name: &str, contents: String) {
    let dialog = gtk::FileDialog::builder().title("Save").initial_name(name).modal(true).build();
    let parent = crate::window::window();
    dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
        if let Ok(file) = r
            && let Some(path) = file.path()
        {
            match crate::cmd::atomic_write(&path, &contents) {
                Ok(()) => crate::window::toast(&format!("Saved {}", paths::pretty(&path))),
                Err(e) => crate::window::toast(&format!("Couldn't save: {e}")),
            }
        }
    });
}

/// Pick one or more files. `filter` is a list of (name, patterns).
pub fn open_files(title: &str, filters: &[(&str, &[&str])], done: impl Fn(Vec<PathBuf>) + 'static) {
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    if !filters.is_empty() {
        let store = gtk::gio::ListStore::new::<gtk::FileFilter>();
        for (name, patterns) in filters {
            let f = gtk::FileFilter::new();
            f.set_name(Some(name));
            for p in *patterns {
                f.add_pattern(p);
            }
            store.append(&f);
        }
        dialog.set_filters(Some(&store));
    }
    let parent = crate::window::window();
    dialog.open_multiple(parent.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
        if let Ok(list) = r {
            let files: Vec<PathBuf> =
                (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gtk::gio::File>()?.path()).collect();
            if !files.is_empty() {
                done(files);
            }
        }
    });
}

pub fn pick_folder(done: impl Fn(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder().title("Choose a folder").modal(true).build();
    let parent = crate::window::window();
    dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
        if let Ok(f) = r
            && let Some(p) = f.path()
        {
            done(p);
        }
    });
}

// ---------- Small helpers ----------

/// A text entry dialog: title, one-line explanation, initial text, button label.
pub fn prompt_text(title: &str, explain: &str, initial: &str, action: &str, done: impl Fn(String) + 'static) {
    let (dialog, card) = dialog(title, 420);
    if !explain.is_empty() {
        let l = label(explain, "dim");
        l.set_wrap(true);
        card.append(&l);
    }
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    card.append(&entry);
    let buttons = hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label(action);
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let done = Rc::new(done);
    let (d, e, f) = (dialog.clone(), entry.clone(), done.clone());
    ok.connect_clicked(move |_| {
        f(e.text().to_string());
        d.close();
    });
    let (d, f) = (dialog.clone(), done);
    entry.connect_activate(move |e| {
        f(e.text().to_string());
        d.close();
    });
    dialog.present();
    entry.grab_focus();
}

/// A multi-line text box with the input look.
pub fn text_area(text: &str, mono: bool, min_height: i32) -> (gtk::ScrolledWindow, gtk::TextView) {
    let tv = gtk::TextView::new();
    tv.buffer().set_text(text);
    tv.set_wrap_mode(gtk::WrapMode::WordChar);
    tv.add_css_class("text-area");
    tv.set_top_margin(8);
    tv.set_bottom_margin(8);
    tv.set_left_margin(10);
    tv.set_right_margin(10);
    if mono {
        tv.add_css_class("mono");
    }
    let sw = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(min_height)
        .max_content_height(min_height * 3)
        .propagate_natural_height(true)
        .child(&tv)
        .build();
    sw.add_css_class("text-area-frame");
    (sw, tv)
}

pub fn buffer_text(tv: &gtk::TextView) -> String {
    let b = tv.buffer();
    b.text(&b.start_iter(), &b.end_iter(), false).to_string()
}

/// A segmented control: 2–5 short, mutually exclusive choices.
pub fn segmented(options: &[(&str, &str)], current: &str, on_change: impl Fn(String) + 'static) -> gtk::Box {
    let track = hbox(0);
    track.add_css_class("segmented");
    let on_change = Rc::new(on_change);
    let buttons: Rc<RefCell<Vec<(String, gtk::Button)>>> = Rc::new(RefCell::new(Vec::new()));
    for (id, text) in options {
        let b = gtk::Button::with_label(text);
        if *id == current {
            b.add_css_class("selected");
        }
        let (idc, all, f) = (id.to_string(), buttons.clone(), on_change.clone());
        b.connect_clicked(move |_| {
            for (bid, bb) in all.borrow().iter() {
                if *bid == idc {
                    bb.add_css_class("selected");
                } else {
                    bb.remove_css_class("selected");
                }
            }
            f(idc.clone());
        });
        track.append(&b);
        buttons.borrow_mut().push((id.to_string(), b));
    }
    track
}

/// Remove every child of a box.
pub fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

/// A key cap, for keyboard shortcuts.
pub fn keys(combo: &[&str]) -> gtk::Box {
    let b = hbox(4);
    for (i, k) in combo.iter().enumerate() {
        if i > 0 {
            b.append(&label("+", "dim"));
        }
        b.append(&label(k, "key-cap"));
    }
    b
}

/// A labelled empty state for lists with nothing in them.
pub fn empty(text: &str) -> gtk::Label {
    let l = label(text, "empty-state");
    l.set_wrap(true);
    l.set_xalign(0.5);
    l.set_justify(gtk::Justification::Center);
    l
}
