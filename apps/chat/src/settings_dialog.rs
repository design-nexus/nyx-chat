//! Settings as a card over the window: the groups listed on the left, the
//! chosen group's options on the right, and a search across all of them.

use crate::sections::settings;
use crate::{widgets, window};
use gtk::prelude::*;
use nyx_ui::settings_card::{self, About, Spec, Standard};

pub use nyx_ui::settings_card::{escape, fit, focus_search, is_open};

/// Each settings group's icon and one-line summary in the dialog's list.
const GROUPS: &[(&str, &str, &str)] = &[
    ("Connection", "network-server-symbolic", "Model servers"),
    ("Chat defaults", "user-available-symbolic", "Model, prompt, sending"),
    ("Tools", "applications-utilities-symbolic", "What models can use"),
    ("Web search", "system-search-symbolic", "Search engine, results"),
    ("Location", "find-location-symbolic", "Where you are"),
    ("Memories", "user-bookmarks-symbolic", "Facts every chat knows"),
    ("Knowledge", "accessories-dictionary-symbolic", "Embedding, chunks"),
    ("Voice", "audio-input-microphone-symbolic", "Dictation, reading aloud"),
    ("Quick ask", "edit-find-symbolic", "The small window"),
    ("MCP servers", "network-workgroup-symbolic", "Tools from other programs"),
    ("Data", "drive-harddisk-symbolic", "Import, export, delete"),
];

const SPEC: Spec = Spec {
    groups: GROUPS,
    search_text: Some(crate::widgets::search_text),
    standard: Some(Standard {
        app: "chat",
        about: About {
            name: "Chat",
            icon: "io.github.design_nexus.nyx.Chat",
            version: env!("CARGO_PKG_VERSION"),
            description: "Chat with local models through Nyx AI Core or Ollama, with tools and a model marketplace.",
            licence: "MIT",
            website: "https://github.com/design-nexus/nyx-chat",
            issues: "https://github.com/design-nexus/nyx-chat/issues",
            credits: &[],
        },
        shortcuts: window::SHORTCUTS,
        reduce_motion: || crate::prefs::get().reduce_motion,
        set_reduce_motion: |on| crate::prefs::update(|p| p.reduce_motion = on),
    }),
};

pub fn open() {
    let (Some(overlay), Some(window)) = (window::overlay(), window::window()) else { return };
    settings_card::open(&overlay, &window, SPEC, || {
        let page = widgets::page("settings");
        settings::build(&page);
        window::mark_page(page.root.upcast_ref(), window::narrow());
        (page.root.upcast(), page.body)
    });
}
