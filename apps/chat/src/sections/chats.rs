//! Chats: every chat in one place, with its folders, pinned chats and the archive,
//! and a search through every word.

use gtk::prelude::*;
use std::cell::RefCell;

use crate::store::Folder;
use crate::{chatlist, clamp, hub, store, widgets};

thread_local! {
    static SEARCH: RefCell<Option<gtk::SearchEntry>> = const { RefCell::new(None) };
}

/// Put the cursor in the search.
pub fn focus_search() {
    SEARCH.with(|s| {
        if let Some(e) = s.borrow().as_ref() {
            e.grab_focus();
        }
    });
}

pub fn build() -> gtk::Widget {
    let root = widgets::vbox(0);
    root.add_css_class("chats-page");

    let (bar, sub, end) = widgets::page_bar("Chats", "");
    let folder = widgets::icon_label_button("folder-new-symbolic", "New folder");
    folder.add_css_class("flat");
    folder.set_tooltip_text(Some("A folder gives its chats a model, instructions and tools"));
    folder.connect_clicked(|_| chatlist::edit_folder(Folder::default()));
    end.append(&folder);
    let count = move || {
        let n = store::chats(false).map(|c| c.len()).unwrap_or(0);
        sub.set_text(&format!("{n} chat{}", if n == 1 { "" } else { "s" }));
    };
    count();
    hub::on(&hub::CHATS, &bar, count);
    root.append(&bar);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search every word in your chats  (Ctrl+F)"));
    search.add_css_class("chats-search");
    search.update_property(&[gtk::accessible::Property::Label("Search chats")]);
    search.connect_stop_search(|e| e.set_text(""));
    SEARCH.with(|s| *s.borrow_mut() = Some(search.clone()));

    let body = widgets::vbox(0);
    body.add_css_class("chats-body");
    body.append(&search);
    body.append(&chatlist::build(&search));
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&clamp::Clamp::new(&body, 760))
        .build();
    root.append(&scroll);
    root.upcast()
}
