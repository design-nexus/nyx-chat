//! Model-specific load-time settings for this computer's models, kept by Nyx AI
//! Core. Chat sends what differs; AI Core applies a loaded model's changes when
//! it's next idle and says which are still waiting.

use anyhow::Result;
use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::prefs::{self, ModelSettings};
use crate::{hub, rt};

thread_local! {
    static PENDING: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static CURRENT_CONTEXT: RefCell<BTreeMap<String, i64>> = const { RefCell::new(BTreeMap::new()) };
    static APPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Send every model's settings (and the defaults) to Nyx AI Core.
pub fn sync() -> Result<()> {
    if !crate::llama::api::local_service() {
        return Ok(());
    }
    let models = prefs::get().model_settings;
    rt::spawn(
        async move {
            crate::llama::core::configure("*", &ModelSettings::default()).await?;
            for (name, settings) in &models {
                crate::llama::core::configure(name, settings).await?;
            }
            Ok::<_, anyhow::Error>(())
        },
        |r| {
            if let Err(e) = r {
                crate::window::toast(&format!("Couldn't send model settings to Nyx AI Core: {}", crate::ollama::api::short_error(&format!("{e:#}"))));
            }
        },
    );
    Ok(())
}

pub fn pending(name: &str) -> bool { PENDING.with(|p| p.borrow().contains_key(name)) }
pub fn runtime_context(name: &str) -> Option<i64> { CURRENT_CONTEXT.with(|c| c.borrow().get(name).copied()) }

pub fn mark_with_context(name: String, current_context: i64) {
    CURRENT_CONTEXT.with(|c| { c.borrow_mut().entry(name.clone()).or_insert(current_context); });
    mark([name]);
}

pub fn mark(names: impl IntoIterator<Item = String>) {
    PENDING.with(|p| for name in names {
        let version = NEXT.with(|next| { let v = next.get(); next.set(v + 1); v });
        p.borrow_mut().insert(name, version);
    });
    apply_pending();
    if !WATCHING.with(|w| w.replace(true)) {
        gtk::glib::timeout_add_seconds_local(5, || {
            if PENDING.with(|p| p.borrow().is_empty()) {
                WATCHING.with(|w| w.set(false));
                gtk::glib::ControlFlow::Break
            } else {
                apply_pending();
                gtk::glib::ControlFlow::Continue
            }
        });
    }
}

/// Send pending models' settings and forget the ones AI Core has applied.
pub fn apply_pending() {
    if !crate::llama::api::local_service() { return; }
    if APPLYING.with(|a| a.replace(true)) { return; }
    let versioned: Vec<(String, u64)> = PENDING.with(|p| p.borrow().iter().map(|(n, v)| (n.clone(), *v)).collect());
    if versioned.is_empty() { APPLYING.with(|a| a.set(false)); return; }
    let work: Vec<(String, ModelSettings)> = versioned.iter().map(|(n, _)| (n.clone(), prefs::for_model(n))).collect();
    rt::spawn(async move {
        let mut applied = Vec::new();
        for (name, settings) in work {
            if !crate::llama::core::configure(&name, &settings).await? {
                applied.push(name);
            }
        }
        Ok::<_, anyhow::Error>(applied)
    }, move |result| {
        APPLYING.with(|a| a.set(false));
        let Ok(applied) = result else { return };
        let mut changed = false;
        PENDING.with(|p| {
            for (name, version) in &versioned {
                if applied.contains(name) && p.borrow().get(name) == Some(version) {
                    p.borrow_mut().remove(name);
                    CURRENT_CONTEXT.with(|c| { c.borrow_mut().remove(name); });
                    changed = true;
                }
            }
        });
        if changed {
            hub::emit(&hub::MODELS);
            crate::models::refresh_running();
        }
    });
}
