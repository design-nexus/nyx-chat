//! What's installed on the active server, and what each model can do. Pages
//! read this cache and subscribe to `hub::MODELS` for changes.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ollama::api::{self, Conn, Installed, ModelInfo, Running};
use crate::{hub, prefs, rt};

#[derive(Default)]
struct State {
    installed: Vec<Installed>,
    running: Vec<Running>,
    info: HashMap<String, ModelInfo>,
    /// None until the first check finishes.
    online: Option<bool>,
    error: String,
    updates: HashSet<String>,
    refreshing: bool,
    /// Being loaded into memory right now.
    loading: Option<String>,
    /// The picked model was loaded after the first successful refresh.
    restored: bool,
    /// Nyx AI Core's default chat model, for when Chat has no pick of its own.
    shared_default: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

pub fn installed() -> Vec<Installed> {
    STATE.with(|s| s.borrow().installed.clone())
}

/// Installed models that can chat (not embedding-only).
pub fn chat_models() -> Vec<String> {
    STATE.with(|s| {
        let s = s.borrow();
        s.installed
            .iter()
            .filter(|m| s.info.get(&m.name).is_none_or(|i| i.capabilities.is_empty() || i.has("completion")))
            .map(|m| m.name.clone())
            .collect()
    })
}

pub fn embedding_models() -> Vec<String> {
    STATE.with(|s| {
        let s = s.borrow();
        s.installed.iter().filter(|m| s.info.get(&m.name).is_some_and(|i| i.has("embedding"))).map(|m| m.name.clone()).collect()
    })
}

pub fn running() -> Vec<Running> {
    STATE.with(|s| s.borrow().running.clone())
}

pub fn info(name: &str) -> Option<ModelInfo> {
    STATE.with(|s| s.borrow().info.get(name).cloned())
}

pub fn has(name: &str, cap: &str) -> bool {
    info(name).is_some_and(|i| i.has(cap))
}

pub fn online() -> Option<bool> {
    STATE.with(|s| s.borrow().online)
}

pub fn error() -> String {
    STATE.with(|s| s.borrow().error.clone())
}

pub fn is_installed(name: &str) -> bool {
    let want = with_tag(name);
    STATE.with(|s| s.borrow().installed.iter().any(|m| m.name == want || m.name.eq_ignore_ascii_case(&want)))
}

/// `llama3` → `llama3:latest`, the way Ollama lists it.
pub fn with_tag(name: &str) -> String {
    if prefs::get().host().backend == prefs::Backend::LlamaCpp { return name.to_string(); }
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') { name.to_string() } else { format!("{name}:latest") }
}

pub fn has_update(name: &str) -> bool {
    STATE.with(|s| s.borrow().updates.contains(name))
}

pub fn updates() -> Vec<String> {
    STATE.with(|s| s.borrow().updates.iter().cloned().collect())
}

/// The model a new chat starts with: the chosen one (or the last picked) if
/// it's installed, else the first installed.
pub fn default_model() -> String {
    let p = prefs::get();
    let backend = p.host().backend;
    let shared = STATE.with(|s| s.borrow().shared_default.clone()).unwrap_or_default();
    let want = [p.new_chat_model, p.last_model, shared].into_iter().find(|m| !m.is_empty()).unwrap_or_default();
    let chat = chat_models();
    if !want.is_empty() && chat.contains(&want) {
        return want;
    }
    chat.first().cloned().unwrap_or_else(|| {
        if backend == prefs::Backend::LlamaCpp { String::new() } else { want }
    })
}

pub fn is_loaded(name: &str) -> bool {
    STATE.with(|s| s.borrow().running.iter().any(|r| r.name == name))
}

pub fn loading() -> Option<String> {
    STATE.with(|s| s.borrow().loading.clone())
}

/// Mark a model as loading (or not) for the load indicator.
pub fn set_loading(name: Option<&str>) {
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let new = name.map(String::from);
        (s.loading != new).then(|| s.loading = new).is_some()
    });
    if changed {
        hub::emit(&hub::MODELS);
    }
}

/// How long a request should ask Ollama to keep `model` loaded: forever for
/// the picked model when "keep loaded" is on, else the setting.
pub fn keep_alive_for(model: &str) -> String {
    let p = prefs::get();
    if p.keep_loaded && p.last_model == model { "-1".into() } else { p.keep_alive }
}

/// The user picked a model in a chat: new chats follow it, and it's loaded
/// (and the previously picked one unloaded) when "keep loaded" is on.
pub fn pick(name: &str) {
    let prev = prefs::get().last_model;
    if prev == name {
        return;
    }
    prefs::update(|p| p.last_model = name.to_string());
    if prefs::get().keep_loaded {
        let p = prefs::get();
        let unload = if p.host().backend == prefs::Backend::LlamaCpp && p.llama_models_max > 1 { None }
            else { Some(prev).filter(|p| !p.is_empty() && p != name) };
        load(name, unload);
    }
}

/// Load `name` to stay in memory, unloading `unload` first.
pub fn load(name: &str, unload: Option<String>) {
    if name.is_empty() || loading().as_deref() == Some(name) {
        return;
    }
    set_loading(Some(name));
    let (conn, n) = (Conn::current(), name.to_string());
    let unload = unload.filter(|u| is_loaded(u));
    rt::spawn(
        async move {
            if let Some(u) = unload {
                let _ = api::unload(&conn, &u).await;
            }
            api::preload(&conn, &n, "-1").await
        },
        move |r| {
            if let Err(e) = r {
                crate::window::toast(&format!("Couldn't load the model: {}", api::short_error(&format!("{e:#}"))));
            }
            STATE.with(|s| s.borrow_mut().loading = None);
            refresh_running_then_emit();
        },
    );
}

/// Turning "keep loaded" off lets the picked model expire like any other.
pub fn release_pinned() {
    let p = prefs::get();
    if p.last_model.is_empty() || !is_loaded(&p.last_model) {
        return;
    }
    if p.host().backend == prefs::Backend::LlamaCpp {
        let (conn, name) = (Conn::current(), p.last_model);
        rt::spawn(async move { api::unload(&conn, &name).await }, |_| refresh_running_then_emit());
        return;
    }
    let (conn, n, ka) = (Conn::current(), p.last_model.clone(), p.keep_alive.clone());
    rt::spawn(async move { api::preload(&conn, &n, &ka).await }, |_| refresh_running_then_emit());
}

fn refresh_running_then_emit() {
    let conn = Conn::current();
    rt::spawn(async move {
        let running = api::running(&conn).await?;
        let infos = futures_util::future::join_all(running.iter().map(|m| api::show(&conn, &m.name))).await;
        Ok::<_, anyhow::Error>((running, infos))
    }, |r| {
        if let Ok((list, infos)) = r {
            STATE.with(|s| {
                let mut state = s.borrow_mut();
                for (model, info) in list.iter().zip(infos) {
                    if let Ok(info) = info { state.info.insert(model.name.clone(), info); }
                }
                state.running = list;
            });
        }
        hub::emit(&hub::MODELS);
        crate::llama::presets::apply_pending();
    });
}

/// Re-read the model list and each model's capabilities.
pub fn refresh() {
    if STATE.with(|s| std::mem::replace(&mut s.borrow_mut().refreshing, true)) {
        return;
    }
    let conn = Conn::current();
    let source = (conn.url.clone(), conn.backend);
    rt::spawn(
        async move {
            let tags = api::tags(&conn).await;
            let running = api::running(&conn).await.unwrap_or_default();
            let vram = if crate::llama::core::local(&conn) { crate::llama::core::vram().await.ok() } else { None };
            let updates = if crate::llama::core::local(&conn) { crate::llama::core::known_updates().await.ok() } else { None };
            let shared_default =
                if crate::llama::core::local(&conn) { crate::llama::core::default_chat_model().await.ok().flatten() } else { None };
            let mut infos = Vec::new();
            if let Ok(list) = &tags {
                let futs = list.iter().map(|m| {
                    let conn = conn.clone();
                    let name = m.name.clone();
                    async move { (name.clone(), api::show(&conn, &name).await) }
                });
                infos = futures_util::future::join_all(futs).await;
            }
            (tags, running, infos, vram, updates, shared_default)
        },
        move |(tags, running, infos, vram, updates, shared_default)| {
            STATE.with(|s| s.borrow_mut().shared_default = shared_default);
            if let Some(vram) = vram.filter(|v| *v > 0 && *v != prefs::get().vram_bytes) {
                prefs::update(|p| p.vram_bytes = vram);
            }
            let current = Conn::current();
            if (current.url, current.backend) != source {
                STATE.with(|s| s.borrow_mut().refreshing = false);
                refresh();
                return;
            }
            if crate::llama::api::local_service() && let Ok(list) = &tags {
                initialize_model_presets(list);
            }
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.refreshing = false;
                match tags {
                    Ok(list) => {
                        s.online = Some(true);
                        s.error.clear();
                        s.installed = list;
                        s.info.clear();
                    }
                    Err(e) => {
                        s.online = Some(false);
                        s.error = api::short_error(&format!("{e:#}"));
                        s.installed.clear();
                    }
                }
                s.running = running;
                for (name, info) in infos {
                    if let Ok(i) = info {
                        s.info.insert(name, i);
                    }
                }
                if let Some(found) = updates {
                    s.updates = found.into_iter().collect();
                }
                let names: HashSet<String> = s.installed.iter().map(|m| m.name.clone()).collect();
                s.updates.retain(|n| names.contains(n));
            });
            hub::emit(&hub::MODELS);
            // Put the picked model back in memory when the app starts.
            let first = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let first = s.online == Some(true) && !s.restored;
                if first {
                    s.restored = true;
                }
                first
            });
            let p = prefs::get();
            if first && p.keep_loaded && is_installed(&p.last_model) && !is_loaded(&p.last_model) {
                load(&p.last_model, None);
            }
        },
    );
}

fn initialize_model_presets(list: &[Installed]) {
    thread_local! { static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    if READY.with(|ready| ready.get()) { return; }
    if !prefs::get().model_settings_migrated {
        prefs::update(|p| {
            let old = prefs::ModelSettings::from_legacy(p);
            for model in list { p.model_settings.entry(model.name.clone()).or_insert_with(|| old.clone()); }
            p.model_settings_migrated = true;
        });
    }
    match crate::llama::presets::sync() {
        Ok(()) => {
            READY.with(|ready| ready.set(true));
            crate::llama::presets::mark(prefs::get().model_settings.keys().cloned());
        },
        Err(e) => crate::window::toast(&format!("Couldn't send model settings to Nyx AI Core: {e:#}")),
    }
}

pub fn switch_host() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.installed.clear();
        s.running.clear();
        s.info.clear();
        s.online = None;
        s.error.clear();
        s.updates.clear();
        s.restored = false;
    });
    hub::emit(&hub::MODELS);
    refresh();
}

/// Just the running list (the Models page polls this).
pub fn refresh_running() {
    let conn = Conn::current();
    let known: HashSet<String> = STATE.with(|s| s.borrow().running.iter().map(|m| m.name.clone()).collect());
    rt::spawn(async move {
        let list = api::running(&conn).await?;
        let new: Vec<String> = list.iter().filter(|m| !known.contains(&m.name)).map(|m| m.name.clone()).collect();
        let infos = futures_util::future::join_all(new.iter().map(|name| api::show(&conn, name))).await;
        Ok::<_, anyhow::Error>((list, new, infos))
    }, |r| {
        if let Ok((list, new, infos)) = r {
            let changed = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let changed = s
                    .running
                    .iter()
                    .map(|m| (&m.name, m.expires.as_str()))
                    .ne(list.iter().map(|m| (&m.name, m.expires.as_str())));
                s.running = list;
                for (name, info) in new.into_iter().zip(infos) {
                    if let Ok(info) = info { s.info.insert(name, info); }
                }
                changed
            });
            if changed {
                hub::emit(&hub::MODELS);
            }
            crate::llama::presets::apply_pending();
        }
    });
}

/// Compare every installed model with its registry. `done` gets how many can update.
pub fn check_updates(done: impl FnOnce(usize) + 'static) {
    if crate::llama::api::local_service() {
        rt::spawn(crate::llama::core::check_updates(), move |r| {
            let found: HashSet<String> = r.unwrap_or_default().into_iter().collect();
            let n = found.len();
            STATE.with(|s| s.borrow_mut().updates = found);
            hub::emit(&hub::MODELS);
            done(n);
        });
        return;
    }
    if prefs::get().host().backend == prefs::Backend::LlamaCpp {
        done(0);
        return;
    }
    let list: Vec<(String, String)> = installed().into_iter().map(|m| (m.name, m.digest)).collect();
    rt::spawn(
        async move {
            let futs = list.into_iter().map(|(name, digest)| async move {
                let r = api::update_available(&name, &digest).await;
                (name, r)
            });
            futures_util::future::join_all(futs).await
        },
        move |results| {
            let found: HashSet<String> = results.into_iter().filter_map(|(n, r)| r.ok().filter(|b| *b).map(|_| n)).collect();
            let n = found.len();
            STATE.with(|s| s.borrow_mut().updates = found);
            prefs::update(|p| p.update_check_day = chrono::Local::now().format("%Y-%m-%d").to_string());
            hub::emit(&hub::MODELS);
            done(n);
        },
    );
}

/// Once a day, quietly. Nyx AI Core checks its own models.
pub fn daily_update_check() {
    if crate::llama::api::local_service() {
        return;
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if prefs::get().update_check_day != today && online() == Some(true) && !installed().is_empty() {
        check_updates(|n| {
            if n > 0 {
                crate::window::toast(&format!("{n} model update{} available", if n == 1 { "" } else { "s" }));
            }
        });
    }
}

pub fn clear_update(name: &str) {
    STATE.with(|s| s.borrow_mut().updates.remove(name));
}
