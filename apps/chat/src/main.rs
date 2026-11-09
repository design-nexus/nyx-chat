//! Chat — a desktop chat client for local models in Nyx AI Core, remote llama.cpp servers and Ollama.

mod chatlist;
mod clamp;
mod cmd;
mod control;
mod hf;
mod highlight;
mod hub;
mod hw;
mod jsonview;
mod llama;
mod markdown;
mod models;
mod ollama;
mod paths;
mod prefs;
mod pulls;
mod quick;
mod rag;
mod rt;
mod sections;
mod settings_dialog;
mod store;
mod theme;
mod tools;
mod transfer;
mod util;
mod voice;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.design_nexus.nyx.Chat";

const USAGE: &str = "Usage: nyx-chat [OPTION…]\n\
\n\
  --section ID     open a page: chat, models, marketplace, arena, knowledge, prompts, stats, settings\n\
  --new            start a new chat\n\
  --quick          open the small Quick ask window\n\
  --selection      with --quick: ask about the highlighted text\n\
  --clipboard      with --quick: ask about the copied text\n\
  --screenshot     drag out an area of the screen and attach it to a chat\n\
  --attach FILE…   attach files to a new message\n\
  --version        print the version\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-chat", name: "Chat" });
    nyx_core::migrate::from(&["nexus-chat"]);
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version") {
        println!("chat {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }

    // GTK's Vulkan renderer enumerates every GPU at startup, which wakes a
    // sleeping discrete GPU on hybrid laptops. GL renders only on the one in use.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    // Developer aid: a different id gives a separate instance (a test copy on a virtual
    // monitor), so it doesn't hand its arguments to the copy that's open.
    let id = std::env::var("NYX_CHAT_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = gtk::Application::builder().application_id(id).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    // Nyx Assistant reads and sends chats through this (primary instance only).
    app.connect_startup(|app| nyx_core::control::serve(app, control::tools(), control::handle));
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let has = |f: &str| argv.iter().any(|a| a == f);
        let section = argv.iter().position(|a| a == "--section").and_then(|i| argv.get(i + 1)).cloned();
        let cwd = cl.cwd().unwrap_or_default();
        let attach: Vec<std::path::PathBuf> = argv
            .iter()
            .skip_while(|a| *a != "--attach")
            .skip(1)
            .take_while(|a| !a.starts_with("--"))
            .map(|a| if std::path::Path::new(a).is_absolute() { a.into() } else { cwd.join(a) })
            .collect();

        if has("--quick") {
            let context = if has("--selection") {
                cmd::output(&["wl-paste", "--primary", "--no-newline"])
            } else if has("--clipboard") {
                cmd::output(&["wl-paste", "--no-newline"])
            } else {
                None
            };
            quick::open(app, context);
            return glib::ExitCode::SUCCESS;
        }

        let wants_chat = has("--new") || has("--screenshot") || !attach.is_empty();
        window::present(app, section.as_deref().or(wants_chat.then_some("chat")));
        if has("--new") {
            sections::chat::new_chat(None);
        }
        if has("--screenshot") {
            // Let the window settle before the selection overlay appears.
            glib::timeout_add_local_once(std::time::Duration::from_millis(250), sections::chat::screenshot);
        }
        if !attach.is_empty() {
            sections::chat::attach(attach);
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| prefs::flush());
    app.run()
}
