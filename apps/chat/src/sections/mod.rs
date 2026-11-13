//! Every page, in the sidebar's order.

pub mod arena;
pub mod chat;
pub mod chats;
pub mod knowledge;
pub mod marketplace;
pub mod models;
pub mod prompts;
pub mod settings;
pub mod stats;

pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    /// The sidebar group it's listed under; empty for the places above the groups.
    pub group: &'static str,
    pub icon: &'static str,
    /// One line for the status bar.
    pub description: &'static str,
    /// The page it shows; `None` for Home, which is the chat page with nothing sent yet.
    pub build: Option<fn() -> gtk::Widget>,
    /// Listed in the sidebar. A conversation is opened from Home or Chats instead.
    pub nav: bool,
}

/// The sidebar's groups and the theme section that colours each.
pub const GROUPS: &[(&str, &str)] = &[("Models", "swatch-blue"), ("Library", "library"), ("Play", "swatch-pink")];

/// The pages. Settings isn't one: it opens as a card over the window.
pub fn all() -> Vec<Section> {
    vec![
        Section {
            id: "home",
            title: "Home",
            group: "",
            icon: "go-home-symbolic",
            description: "Ask something new",
            build: None,
            nav: true,
        },
        Section {
            id: "chats",
            title: "Chats",
            group: "",
            icon: "document-open-recent-symbolic",
            description: "Every chat, its folders and the archive (Ctrl+F searches them)",
            build: Some(chats::build),
            nav: true,
        },
        Section {
            id: "chat",
            title: "Chat",
            group: "",
            icon: "user-available-symbolic",
            description: "",
            build: Some(chat::build),
            nav: false,
        },
        Section {
            id: "models",
            title: "Models",
            group: "Models",
            icon: "chat-models-symbolic",
            description: "What's installed on this server, what's loaded, and what's downloading",
            build: Some(models::build),
            nav: true,
        },
        Section {
            id: "marketplace",
            title: "Marketplace",
            group: "Models",
            icon: "chat-store-symbolic",
            description: "GGUF models on Hugging Face and which fit this computer",
            build: Some(marketplace::build),
            nav: true,
        },
        Section {
            id: "knowledge",
            title: "Knowledge",
            group: "Library",
            icon: "accessories-dictionary-symbolic",
            description: "Collections of your documents; attach one to a chat, or type # in the message box",
            build: Some(knowledge::build),
            nav: true,
        },
        Section {
            id: "prompts",
            title: "Prompts",
            group: "Library",
            icon: "utilities-terminal-symbolic",
            description: "Text you use often; type / and its command in any chat to insert it",
            build: Some(prompts::build),
            nav: true,
        },
        Section {
            id: "arena",
            title: "Arena",
            group: "Play",
            icon: "applications-games-symbolic",
            description: "Two hidden models answer the same question; vote, and the leaderboard keeps score",
            build: Some(arena::build),
            nav: true,
        },
        Section {
            id: "stats",
            title: "Stats",
            group: "Play",
            icon: "chat-stats-symbolic",
            description: "Your chats in numbers, counted from this computer's history",
            build: Some(stats::build),
            nav: true,
        },
    ]
}
