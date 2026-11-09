//! Sending: store the question, start one stream per model, write each
//! reply to the store as it completes, and tidy up when every stream ends.

use base64::Engine;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::composer::Attachment;
use super::live::Live;
use super::{Page, render};
use crate::ollama::api::{self, Conn};
use crate::ollama::chat::{self, Cancel, Event, Msg, Request, ToolCall};
use crate::prefs::Prefs;
use crate::store::{self, Chat, FileRef, Message, NewMessage};
use crate::{hub, models, prefs, rag, rt, tools, util, voice, window};

/// Files longer than this are searched (when an embedding model is around)
/// or cut, instead of being sent whole.
const INLINE_CHARS: usize = 24_000;

pub struct Stream {
    pub chat: i64,
    pub model: String,
    pub live: Rc<Live>,
    cancel: Cancel,
    abort: RefCell<Option<tokio::task::AbortHandle>>,
    /// The assistant/tool rows this stream has written.
    rows: RefCell<Vec<i64>>,
    current: Cell<i64>,
    /// The stream whose answer the chat continues from.
    primary: bool,
    /// Continuing a reply: its text and token count so far.
    prefix: String,
    prefix_tokens: i64,
}

impl Stream {
    pub fn ids(&self) -> Vec<i64> {
        self.rows.borrow().clone()
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Give the loop a moment to save what it has, then cut the connection.
        let abort = self.abort.borrow().clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            if let Some(a) = abort {
                a.abort();
            }
        });
    }

    fn last(&self) -> i64 {
        self.rows.borrow().last().copied().unwrap_or(0)
    }
}

// ---------- Entry points ----------

/// Send what's in the composer.
pub(super) fn submit(p: &Rc<Page>, text: String, attachments: Vec<Attachment>) {
    if super::streaming(p.st.borrow().id()) {
        return;
    }
    let (model, compare) = {
        let st = p.st.borrow();
        (st.chat().model.clone(), st.chat().compare.clone())
    };
    if model.is_empty() {
        window::toast("Pick a model first");
        return;
    }
    let images: Vec<String> = attachments.iter().filter_map(|a| a.image.clone()).collect();
    let files: Vec<FileRef> = attachments.iter().filter_map(|a| a.file.clone()).collect();
    if !images.is_empty() && models::info(&model).is_some_and(|i| !i.has("vision")) {
        window::toast(&format!("{model} can't see images, so they'll be left out"));
    }
    let Some(chat_id) = ensure_chat(p) else { return };
    let parent = {
        let st = p.st.borrow();
        let leaf = st.chat().leaf;
        (leaf > 0).then_some(leaf)
    };
    let uid = match store::add_message(NewMessage {
        chat: chat_id,
        parent,
        role: "user",
        content: &text,
        model: "",
        images: &images,
        files: &files,
        extra: Value::Null,
    }) {
        Ok(id) => id,
        Err(e) => {
            window::toast(&format!("Couldn't save the message: {e}"));
            return;
        }
    };
    reload(p);
    let mut all = vec![model];
    all.extend(compare);
    generate(p, uid, all);
    hub::emit(&hub::CHATS);
}

/// Make sure the chat exists in the store (it's a draft until the first message).
fn ensure_chat(p: &Rc<Page>) -> Option<i64> {
    if let Some(id) = p.st.borrow().id() {
        return Some(id);
    }
    let draft = p.st.borrow().draft.clone();
    let host = prefs::get().host().name;
    let id = match store::create_chat(&draft.model, draft.folder, &host) {
        Ok(id) => id,
        Err(e) => {
            window::toast(&format!("Couldn't create the chat: {e}"));
            return None;
        }
    };
    let chat = Chat { id, created: util::now(), updated: util::now(), host, ..draft };
    let _ = store::save_chat(&chat);
    p.st.borrow_mut().chat = Some(chat);
    prefs::update(|pr| pr.last_chat = id);
    Some(id)
}

fn reload(p: &Page) {
    let Some(id) = p.st.borrow().id() else { return };
    let all = store::messages(id).unwrap_or_default();
    let chat = store::chat(id).ok().flatten();
    let mut st = p.st.borrow_mut();
    st.all = all;
    if let Some(c) = chat {
        st.chat = Some(c);
    }
}

/// Have the model keep writing a reply from where it ended. The reply goes in
/// as the last message, which Ollama continues instead of answering.
pub(super) fn continue_reply(p: &Rc<Page>, msg_id: i64) {
    let Some(chat_id) = p.st.borrow().id() else { return };
    if super::streaming(Some(chat_id)) {
        window::toast("A reply is already being written");
        return;
    }
    let (m, question) = {
        let st = p.st.borrow();
        let Some(m) = st.all.iter().find(|m| m.id == msg_id).cloned() else { return };
        let path = store::path_to(&st.all, msg_id);
        let q = path.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()).unwrap_or_default();
        (m, q)
    };
    let mut prepared = prepare(p, &m.model, msg_id);
    // Just more text: no tools, no fresh search, no new reasoning.
    prepared.request.tools.clear();
    prepared.request.max_rounds = 1;
    prepared.request.think = prepared.request.think.map(|_| false);
    prepared.pre_search = None;
    prepared.rag_query = question;
    let stream = start_prepared(chat_id, &m.model, prepared, msg_id, true, (m.content.clone(), m.tokens));
    super::add_stream(stream);
    p.follow.set(true);
    render(p);
    super::scroll_to_bottom_now(&p.scroller);
    p.composer.set_streaming(true);
}

/// Replace a reply's text with the user's correction; later turns build on it.
pub(super) fn save_edited_reply(p: &Rc<Page>, msg_id: i64, text: String) {
    if let Ok(Some(mut m)) = store::message(msg_id) {
        m.content = text;
        set_extra(&mut m.extra, "edited", json!(true));
        let _ = store::finish_message(&m);
    }
    reload(p);
    render(p);
}

/// Start a new chat from the conversation up to `msg_id`.
pub(super) fn branch(p: &Rc<Page>, msg_id: i64) {
    let Some(chat_id) = p.st.borrow().id() else { return };
    match store::branch_chat(chat_id, msg_id) {
        Ok(id) => {
            hub::emit(&hub::CHATS);
            super::open(id);
            window::toast("Branched into a new chat");
        }
        Err(e) => window::toast(&format!("Couldn't branch: {e}")),
    }
}

/// Edit a question: it becomes a new branch, answered again.
pub(super) fn resend_edited(p: &Rc<Page>, original: &Message, text: String) {
    let Some(chat_id) = p.st.borrow().id() else { return };
    if super::streaming(Some(chat_id)) {
        window::toast("Wait for the reply to finish, or stop it first");
        return;
    }
    let Ok(uid) = store::add_message(NewMessage {
        chat: chat_id,
        parent: original.parent,
        role: "user",
        content: &text,
        model: "",
        images: &original.images,
        files: &original.files,
        extra: Value::Null,
    }) else {
        return;
    };
    reload(p);
    let (model, compare) = {
        let st = p.st.borrow();
        (st.chat().model.clone(), st.chat().compare.clone())
    };
    let mut all = vec![model];
    all.extend(compare);
    generate(p, uid, all);
}

pub(super) fn delete_answer(p: &Rc<Page>, first: i64) {
    let Some(chat_id) = p.st.borrow().id() else { return };
    if super::streaming(Some(chat_id)) {
        return;
    }
    let parent = p.st.borrow().all.iter().find(|m| m.id == first).and_then(|m| m.parent);
    let _ = store::delete_branch(first);
    let all = store::messages(chat_id).unwrap_or_default();
    let leaf = parent.map(|pid| store::deepest(&all, pid)).unwrap_or(0);
    let _ = store::set_leaf(chat_id, leaf);
    reload(p);
    render(p);
    window::toast("Answer deleted");
}

/// Answer the user message `user_id` with each of `models`.
pub(super) fn generate(p: &Rc<Page>, user_id: i64, models_list: Vec<String>) {
    let Some(chat_id) = p.st.borrow().id() else { return };
    if super::streaming(Some(chat_id)) {
        window::toast("A reply is already being written");
        return;
    }
    let group = (models_list.len() > 1).then(util::now);
    let mut first_rows = Vec::new();
    for (i, model) in models_list.iter().enumerate() {
        let extra = match group {
            Some(g) => json!({ "group": g }),
            None => Value::Null,
        };
        let Ok(row) = store::add_message(NewMessage {
            chat: chat_id,
            parent: Some(user_id),
            role: "assistant",
            content: "",
            model,
            images: &[],
            files: &[],
            extra,
        }) else {
            continue;
        };
        first_rows.push((i == 0, model.clone(), row));
    }
    reload(p);
    // Show the primary answer's branch while the others stream beside it.
    if let Some((_, _, row)) = first_rows.first() {
        let _ = store::set_leaf(chat_id, *row);
        p.st.borrow_mut().chat_mut().leaf = *row;
    }
    let mut started = Vec::new();
    for (primary, model, row) in first_rows {
        started.push(start(p, chat_id, &model, user_id, row, primary));
    }
    for s in started {
        super::add_stream(s);
    }
    p.follow.set(true);
    render(p);
    super::scroll_to_bottom_now(&p.scroller);
    p.composer.set_streaming(true);
}

// ---------- One stream ----------

/// A message as the async side sees it (files and images are read there).
struct Hist {
    role: String,
    content: String,
    images: Vec<PathBuf>,
    files: Vec<FileRef>,
    tool_calls: Vec<ToolCall>,
    tool_name: String,
}

struct Prepared {
    request: Request,
    history: Vec<Hist>,
    system: String,
    memories: Vec<String>,
    location_in_prompt: bool,
    knowledge: Vec<(i64, String)>,
    rag_query: String,
    rag_top_k: usize,
    embed_model: String,
    chunk_size: usize,
    vision: bool,
    /// Search first and add the results (for models that can't call tools).
    pre_search: Option<String>,
    clipboard: Option<String>,
    /// Tokens the model can take in, and what to do past that.
    ctx_limit: usize,
    overflow: String,
    chars_per_token: f64,
}

fn start(p: &Rc<Page>, chat_id: i64, model: &str, user_id: i64, first_row: i64, primary: bool) -> Rc<Stream> {
    let prepared = prepare(p, model, user_id);
    start_prepared(chat_id, model, prepared, first_row, primary, (String::new(), 0))
}

/// Run `prepared`, writing into `first_row`. `prefix` is the reply so far
/// (text and tokens) when continuing one.
fn start_prepared(
    chat_id: i64,
    model: &str,
    prepared: Prepared,
    first_row: i64,
    primary: bool,
    prefix: (String, i64),
) -> Rc<Stream> {
    let live = Live::new(model);
    if !prefix.0.is_empty() {
        live.delta(&prefix.0, "");
    }
    let stream = Rc::new(Stream {
        chat: chat_id,
        model: model.to_string(),
        live: live.clone(),
        cancel: Arc::new(AtomicBool::new(false)),
        abort: RefCell::new(None),
        rows: RefCell::new(vec![first_row]),
        current: Cell::new(first_row),
        primary,
        prefix: prefix.0,
        prefix_tokens: prefix.1,
    });
    let cancel = stream.cancel.clone();
    let s2 = stream.clone();
    let mut round = 0usize;
    // Ollama loads the model with the first request; show that on the picker.
    let mut waiting_for_model = !models::is_loaded(model);
    if waiting_for_model {
        models::set_loading(Some(model));
    }
    let handle = rt::stream(
        move |tx| async move { run(prepared, tx, cancel).await },
        move |ev: Event| {
            if waiting_for_model && matches!(ev, Event::Delta { .. } | Event::RoundEnd { .. } | Event::Error(_) | Event::Done) {
                waiting_for_model = false;
                if models::loading().as_deref() == Some(s2.model.as_str()) {
                    models::set_loading(None);
                }
                models::refresh_running();
            }
            on_event(&s2, ev, &mut round);
        },
    );
    *stream.abort.borrow_mut() = Some(handle);
    stream
}

fn prepare(p: &Page, model: &str, user_id: i64) -> Prepared {
    let pr = prefs::get();
    let (chat, all) = {
        let st = p.st.borrow();
        (st.chat().clone(), st.all.clone())
    };
    let folder = chat.folder.and_then(store::folder);
    let info = models::info(model);
    let can = |c: &str| info.as_ref().is_some_and(|i| i.has(c));

    let path = store::path_to(&all, user_id);
    let history: Vec<Hist> = path
        .iter()
        .filter(|m| !(m.role == "assistant" && m.content.is_empty() && m.tool_calls.is_empty()))
        .map(|m| Hist {
            role: m.role.clone(),
            content: m.content.clone(),
            images: m.images.iter().map(PathBuf::from).collect(),
            files: m.files.clone(),
            tool_calls: m.tool_calls.clone(),
            tool_name: m.tool_name.clone(),
        })
        .collect();

    let system = [chat.system.clone(), folder.as_ref().map(|f| f.system.clone()).unwrap_or_default(), pr.system_prompt.clone()]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .unwrap_or_default();

    let tool_names: Vec<String> = chat
        .params
        .tools
        .clone()
        .or_else(|| folder.as_ref().map(|f| f.tools.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| pr.tools_enabled.clone());
    let location_on = pr.location_mode != "off";
    let tools_defs = if can("tools") { tools::definitions(&tool_names, location_on) } else { vec![] };
    // Models without tools can't decide when to search, so searching before
    // every message happens only when the Web chip is on in this chat.
    let wants_search = chat.params.tools.as_ref().is_some_and(|t| t.iter().any(|x| x == "web_search"));
    let question = path.last().map(|m| m.content.clone()).unwrap_or_default();
    let pre_search = (!can("tools") && wants_search && !question.trim().is_empty()).then(|| util::clip(&question, 200));

    let mut kb_ids = chat.knowledge.clone();
    if let Some(f) = &folder {
        for k in &f.knowledge {
            if !kb_ids.contains(k) {
                kb_ids.push(*k);
            }
        }
    }
    let kbs = store::kbases().unwrap_or_default();
    let knowledge: Vec<(i64, String)> = kb_ids
        .iter()
        .filter_map(|id| {
            kbs.iter()
                .find(|k| k.id == *id)
                .map(|k| {
                    let available = models::embedding_models();
                    let want = if k.model.is_empty() { &pr.embed_model } else { &k.model };
                    (k.id, models::stand_in(want, &available, &format!("the knowledge base “{}”", k.name)))
                })
        })
        .collect();

    let pa = &chat.params;
    let model_defaults = prefs::for_model(model);
    let local_llama = crate::llama::api::local_service();
    let mut options = json!({
        "temperature": pa.temperature.unwrap_or(if local_llama { model_defaults.temperature } else { pr.temperature }),
        "top_p": pa.top_p.unwrap_or(if local_llama { model_defaults.top_p } else { pr.top_p }),
        "top_k": pa.top_k.unwrap_or(if local_llama { model_defaults.top_k } else { pr.top_k }),
        "num_ctx": context_limit(&chat),
    });
    if local_llama {
        options["min_p"] = json!(pa.min_p.unwrap_or(model_defaults.min_p));
        options["repeat_penalty"] = json!(pa.repeat_penalty.unwrap_or(model_defaults.repeat_penalty));
    }
    if let Some(seed) = pa.seed {
        options["seed"] = json!(seed);
    }
    let format =
        pa.json.then(|| serde_json::from_str::<Value>(&chat.schema).ok().filter(|v| v.is_object()).unwrap_or(json!("json")));
    let think = can("thinking").then(|| pa.think.unwrap_or(pr.think));
    let reasoning_choices = crate::llama::reasoning::options(
        info.as_ref().map(|i| i.reasoning_efforts.as_slice()).unwrap_or(&[]),
        pr.reasoning_hidden.get(model).map(Vec::as_slice).unwrap_or(&[]),
    );
    if pr.host().backend == prefs::Backend::LlamaCpp && think == Some(true) && can("reasoning_effort")
        && let Some(level) = crate::llama::reasoning::effective(&reasoning_choices, pa.reasoning_effort.as_deref())
    {
        options["reasoning_effort"] = json!(level);
    }

    let clipboard = system.contains("{{CLIPBOARD}}").then(|| crate::cmd::output(&["wl-paste", "-n"]).unwrap_or_default());
    let embed_available = models::embedding_models();
    let embed_model = if embed_available.contains(&models::with_tag(&pr.embed_model)) || embed_available.contains(&pr.embed_model)
    {
        pr.embed_model.clone()
    } else {
        models::stand_in(&pr.embed_model, &embed_available, "searching long attachments")
    };

    Prepared {
        request: Request {
            conn: Conn::current(),
            model: model.to_string(),
            messages: Vec::new(),
            tools: tools_defs,
            think,
            format,
            options,
            keep_alive: models::keep_alive_for(model),
            max_rounds: pr.max_tool_rounds,
            tool_ctx: tools::Ctx { prefs: pr.clone() },
        },
        history,
        system,
        memories: store::memories().unwrap_or_default().into_iter().map(|(_, m)| m).collect(),
        location_in_prompt: pr.location_in_prompt && location_on,
        knowledge,
        rag_query: question,
        rag_top_k: pr.rag_top_k,
        embed_model,
        chunk_size: pr.chunk_size,
        vision: can("vision"),
        pre_search,
        clipboard,
        ctx_limit: context_limit(&chat) as usize,
        overflow: chat.params.overflow.clone().unwrap_or_else(|| pr.overflow.clone()),
        chars_per_token: chars_per_token(&path),
    }
}

/// How many tokens this chat's model takes in: the chat's context length (or
/// the default), never more than the model supports.
pub(crate) fn context_limit(chat: &Chat) -> i64 {
    let default = if crate::llama::api::local_service() {
        crate::llama::presets::runtime_context(&chat.model).unwrap_or_else(|| prefs::for_model(&chat.model).num_ctx)
    } else { prefs::get().num_ctx };
    let asked = chat.params.num_ctx.unwrap_or(default);
    let asked = if crate::llama::api::local_service() { asked.min(default) } else { asked };
    match models::info(&chat.model).and_then(|i| i.context_length) {
        Some(max) if max > 0 => asked.min(max as i64),
        _ => asked,
    }
}

/// Tokens the last reply on `path` used: what it read plus what it wrote.
pub(crate) fn context_used(path: &[Message]) -> Option<u64> {
    let m = path.iter().rev().find(|m| m.role == "assistant" && m.extra["stats"]["prompt_tokens"].as_u64().unwrap_or(0) > 0)?;
    Some(m.extra["stats"]["prompt_tokens"].as_u64().unwrap_or(0) + m.tokens.max(0) as u64)
}

/// Characters per token for this conversation, learned from the last reply's
/// prompt size; about 3.5 when there's nothing to learn from.
fn chars_per_token(path: &[Message]) -> f64 {
    let Some(i) = path.iter().rposition(|m| m.role == "assistant" && m.extra["stats"]["prompt_tokens"].as_u64().unwrap_or(0) > 0)
    else {
        return 3.5;
    };
    let tokens = path[i].extra["stats"]["prompt_tokens"].as_u64().unwrap_or(0) as f64;
    let chars: usize = path[..i].iter().map(|m| m.content.len() + m.files.iter().map(|f| f.chars).sum::<usize>()).sum();
    if chars < 2000 {
        return 3.5;
    }
    (chars as f64 / tokens).clamp(2.0, 6.0)
}

/// Rough token count of a message as sent.
fn estimate(m: &Msg, chars_per_token: f64) -> usize {
    (m.content.len() as f64 / chars_per_token) as usize + m.images.len() * 768 + 8
}

/// Fit `messages` (system prompt first, newest question last) into `budget`
/// tokens. "rolling" drops the oldest turns, "keep_first" keeps the first
/// exchange and drops the ones after it, "stop" changes nothing. Returns how
/// many messages were left out, or the estimated size when it can't fit.
fn fit_context(messages: &mut Vec<Msg>, budget: usize, policy: &str, chars_per_token: f64) -> Result<usize, usize> {
    let total = |ms: &[Msg]| ms.iter().map(|m| estimate(m, chars_per_token)).sum::<usize>();
    let size = total(messages);
    if size <= budget {
        return Ok(0);
    }
    if policy == "stop" {
        return Err(size);
    }
    // Turns start at each user message; the system prompt and the last turn stay.
    let starts: Vec<usize> = messages.iter().enumerate().filter(|(_, m)| m.role == "user").map(|(i, _)| i).collect();
    if starts.len() < 2 {
        return Err(size);
    }
    let keep_from = if policy == "keep_first" { 1 } else { 0 };
    let mut dropped = 0;
    // Drop whole turns, oldest first (after the kept first exchange).
    for t in keep_from..starts.len() - 1 {
        if total(messages) <= budget {
            break;
        }
        let (a, b) = (starts[t] - dropped, starts[t + 1] - dropped);
        messages.drain(a..b);
        dropped += b - a;
    }
    if total(messages) <= budget { Ok(dropped) } else { Err(total(messages)) }
}

/// Build the final messages (system prompt, memories, location, documents,
/// search results), then run the chat loop.
async fn run(mut p: Prepared, tx: async_channel::Sender<Event>, cancel: Cancel) {
    let prefs: Prefs = p.request.tool_ctx.prefs.clone();
    let conn = p.request.conn.clone();
    let mut extra_context: Vec<String> = Vec::new();

    let location = if p.location_in_prompt || p.system.contains("{{USER_LOCATION}}") {
        tools::location::lookup(&prefs).await.ok().map(|l| l.describe())
    } else {
        None
    };
    let mut system = tools::now_line();
    let base = tools::expand_variables(&p.system, &prefs, location.as_deref(), p.clipboard.as_deref());
    if !base.trim().is_empty() {
        system.push_str("\n\n");
        system.push_str(base.trim());
    }
    if !p.memories.is_empty() {
        system.push_str("\n\nWhat you know about the user (they asked you to remember):\n");
        for m in &p.memories {
            system.push_str(&format!("- {m}\n"));
        }
    }
    if p.location_in_prompt
        && let Some(l) = &location
    {
        system.push_str(&format!("\n\nThe user's location: {l}"));
    }

    // Knowledge bases.
    if !p.knowledge.is_empty() && !p.rag_query.trim().is_empty() {
        let call = ToolCall { name: "knowledge".into(), arguments: json!({ "query": util::clip(&p.rag_query, 80) }) };
        let _ = tx.send(Event::ToolStart(call.clone())).await;
        let mut found = Vec::new();
        let mut error = None;
        for (kb, model) in &p.knowledge {
            if model.is_empty() {
                error = Some("Download an embedding model to use Knowledge with this server".into());
                continue;
            }
            let backend = conn.backend.as_str();
            match store::missing_vectors(*kb, backend, model) {
                Ok(missing) if !missing.is_empty() => {
                    let texts: Vec<String> = missing.iter().map(|(_, text)| text.clone()).collect();
                    match rag::embed_all(&conn, model, &texts).await {
                        Ok(vectors) => {
                            let pairs: Vec<_> = missing.into_iter().map(|(id, _)| id).zip(vectors).collect();
                            if let Err(e) = store::save_vectors(backend, model, &pairs) { error = Some(format!("{e:#}")); }
                        }
                        Err(e) => { error = Some(format!("{e:#}")); continue; }
                    }
                }
                Err(e) => { error = Some(format!("{e:#}")); continue; }
                _ => {}
            }
            let pool = store::kchunks_for(*kb, backend, model).unwrap_or_default();
            match rag::retrieve(&conn, model, &p.rag_query, pool, p.rag_top_k).await {
                Ok(f) => found.extend(f),
                Err(e) => error = Some(format!("{e:#}")),
            }
        }
        found.sort_by(|a, b| b.score.total_cmp(&a.score));
        found.truncate(p.rag_top_k.max(1));
        let output = match (&error, found.is_empty()) {
            (Some(e), true) => tools::Output::error(api::short_error(e)),
            (_, true) => tools::Output::text("Nothing relevant found."),
            _ => {
                extra_context.push(rag::context_block(&found));
                tools::Output::text(found.iter().map(|f| format!("{} ({:.2})", f.doc, f.score)).collect::<Vec<_>>().join("\n"))
            }
        };
        let _ = tx.send(Event::ToolResult { call, output }).await;
    }

    // Web search for models that can't call tools themselves.
    if let Some(q) = p.pre_search.take() {
        let call = ToolCall { name: "web_search".into(), arguments: json!({ "query": q }) };
        let _ = tx.send(Event::ToolStart(call.clone())).await;
        let output = tools::web::search(&q, &prefs).await.unwrap_or_else(|e| tools::Output::error(format!("{e:#}")));
        if !output.failed {
            extra_context.push(output.text.clone());
        }
        let _ = tx.send(Event::ToolResult { call, output }).await;
    }

    // History, with attachments read now.
    let last_user = p.history.iter().rposition(|h| h.role == "user");
    let mut messages = Vec::new();
    for (i, h) in p.history.iter().enumerate() {
        let mut content = h.content.clone();
        for f in &h.files {
            let text = std::fs::read_to_string(&f.path).unwrap_or_default();
            let big = text.chars().count() > INLINE_CHARS;
            if big && Some(i) == last_user && !p.embed_model.is_empty() {
                let chunks = rag::chunk(&text, p.chunk_size);
                let excerpt = match rag::embed_all(&conn, &p.embed_model, &chunks).await {
                    Ok(vecs) => {
                        let pool = chunks.into_iter().zip(vecs).map(|(t, v)| (f.name.clone(), t, v)).collect();
                        match rag::retrieve(&conn, &p.embed_model, &p.rag_query, pool, 6).await {
                            Ok(found) => found.iter().map(|x| x.text.clone()).collect::<Vec<_>>().join("\n…\n"),
                            Err(_) => util::clip(&text, INLINE_CHARS),
                        }
                    }
                    Err(_) => util::clip(&text, INLINE_CHARS),
                };
                content.push_str(&format!(
                    "\n\n<file name=\"{}\" note=\"excerpts most relevant to the question\">\n{}\n</file>",
                    f.name, excerpt
                ));
            } else {
                content.push_str(&format!("\n\n<file name=\"{}\">\n{}\n</file>", f.name, util::clip(&text, INLINE_CHARS)));
            }
        }
        let images = if p.vision {
            h.images
                .iter()
                .filter_map(|path| std::fs::read(path).ok())
                .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
                .collect()
        } else {
            vec![]
        };
        messages.push(Msg {
            role: h.role.clone(),
            content,
            images,
            tool_calls: h.tool_calls.clone(),
            tool_name: h.tool_name.clone(),
        });
    }

    for c in extra_context {
        system.push_str("\n\n");
        system.push_str(&c);
    }
    if !system.trim().is_empty() {
        messages.insert(0, Msg::new("system", system.trim()));
    }
    // Leave room for the reply.
    let budget = p.ctx_limit.saturating_sub((p.ctx_limit / 4).clamp(512, 2048));
    match fit_context(&mut messages, budget, &p.overflow, p.chars_per_token) {
        Ok(0) => {}
        Ok(n) => {
            let _ = tx.send(Event::Trimmed(n)).await;
        }
        Err(size) => {
            let msg = format!(
                "This chat (about {} tokens) is longer than its context of {} tokens. Branch a new chat from a recent message, raise the context length, or let older messages drop in this chat's settings.",
                util::count(size as u64),
                util::count(p.ctx_limit as u64)
            );
            let _ = tx.send(Event::Error(msg)).await;
            let _ = tx.send(Event::Done).await;
            return;
        }
    }
    p.request.messages = messages;
    chat::run(p.request, tx, cancel).await;
}

// ---------- Events ----------

fn on_event(s: &Rc<Stream>, ev: Event, round: &mut usize) {
    match ev {
        Event::RoundStart => {
            if *round > 0 {
                // Each round after tool results is its own assistant message.
                let parent = s.last();
                if let Ok(id) = store::add_message(NewMessage {
                    chat: s.chat,
                    parent: Some(parent),
                    role: "assistant",
                    content: "",
                    model: &s.model,
                    images: &[],
                    files: &[],
                    extra: Value::Null,
                }) {
                    s.rows.borrow_mut().push(id);
                    s.current.set(id);
                }
                s.live.round_start();
            }
            *round += 1;
        }
        Event::Delta { content, thinking } => s.live.delta(&content, &thinking),
        Event::RoundEnd { content, thinking, calls, stats } => {
            s.live.round_end(&stats);
            if let Ok(Some(mut m)) = store::message(s.current.get()) {
                let continued = !s.prefix.is_empty() && s.current.get() == s.rows.borrow()[0];
                m.content = if continued { format!("{}{content}", s.prefix) } else { content };
                if !continued {
                    m.thinking = thinking;
                }
                m.tool_calls = calls;
                m.tokens = stats.tokens as i64 + if continued { s.prefix_tokens } else { 0 };
                m.tps = stats.tokens_per_sec;
                set_extra(&mut m.extra, "stats", stats.to_json());
                let secs = s.live.think_secs.get();
                if secs > 0.0 {
                    set_extra(&mut m.extra, "think_secs", json!(secs));
                }
                let _ = store::finish_message(&m);
            }
        }
        Event::Trimmed(n) => {
            if let Ok(Some(mut m)) = store::message(s.current.get()) {
                set_extra(&mut m.extra, "trimmed", json!(n));
                let _ = store::finish_message(&m);
            }
            if s.primary {
                window::toast(&format!("Left out {n} earlier message{} to fit the context", if n == 1 { "" } else { "s" }));
            }
        }
        Event::ToolStart(call) => s.live.tool_start(&call),
        Event::ToolResult { call, output } => {
            s.live.tool_result(&call, &output);
            // Knowledge and pre-search cards aren't real tool calls; the reply
            // row is still empty, so they're recorded on it instead.
            let is_real = store::message(s.current.get()).ok().flatten().is_some_and(|m| m.tool_calls.contains(&call));
            if is_real {
                if let Ok(id) = store::add_message(NewMessage {
                    chat: s.chat,
                    parent: Some(s.last()),
                    role: "tool",
                    content: &output.text,
                    model: "",
                    images: &[],
                    files: &[],
                    extra: json!({ "sources": output.sources, "failed": output.failed }),
                }) {
                    if let Ok(Some(mut m)) = store::message(id) {
                        m.tool_name = call.name.clone();
                        let _ = store::finish_message(&m);
                    }
                    s.rows.borrow_mut().push(id);
                }
            } else if let Ok(Some(mut m)) = store::message(s.current.get()) {
                // Keep the sources so citations still link after a reload.
                if !output.sources.is_empty() {
                    set_extra(&mut m.extra, "sources", json!(output.sources));
                }
                let mut pre: Vec<Value> = m.extra["pre"].as_array().cloned().unwrap_or_default();
                pre.push(json!({ "name": call.name, "arguments": call.arguments, "text": util::clip(&output.text, 4000), "failed": output.failed }));
                set_extra(&mut m.extra, "pre", json!(pre));
                let _ = store::finish_message(&m);
            }
        }
        Event::Approval { title, detail, reply } => {
            s.live.approval(&title, &detail, reply);
            crate::window::toast(&title);
        }
        Event::Error(e) => {
            s.live.error(&e);
            if let Ok(Some(mut m)) = store::message(s.current.get()) {
                set_extra(&mut m.extra, "error", json!(e));
                let _ = store::finish_message(&m);
            }
        }
        Event::Done => finished(s),
    }
}

fn set_extra(extra: &mut Value, key: &str, v: Value) {
    if !extra.is_object() {
        *extra = json!({});
    }
    extra[key] = v;
}

fn finished(s: &Rc<Stream>) {
    s.live.finish();
    super::remove_stream(s);
    if s.primary {
        let _ = store::set_leaf(s.chat, s.last());
    }
    let others_running = super::streaming(Some(s.chat));
    let Some(p) = super::page() else { return };
    let on_screen = p.st.borrow().id() == Some(s.chat);
    if on_screen && !others_running {
        reload(&p);
        render(&p);
        p.composer.set_streaming(false);
    }
    if !others_running {
        if crate::llama::api::local_service() { models::refresh_running(); }
        let _ = store::touch_chat(s.chat);
        maybe_title(s.chat);
        hub::emit(&hub::CHATS);
        if s.primary {
            let text = s.live.text();
            let pr = prefs::get();
            if pr.auto_read && !text.trim().is_empty() && !s.cancel.load(Ordering::Relaxed) {
                voice::speak(&text);
            }
            if pr.followups && !text.trim().is_empty() && !s.cancel.load(Ordering::Relaxed) {
                suggest_followups(s.chat, s.last(), &s.model);
            }
        }
    }
}

/// Ask the model for a few follow-up questions and keep them on the reply.
fn suggest_followups(chat_id: i64, reply: i64, model: &str) {
    let all = store::messages(chat_id).unwrap_or_default();
    let path = store::path_to(&all, reply);
    if path.last().is_some_and(|m| m.error().is_some()) {
        return;
    }
    let convo: String = path
        .iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && !m.content.is_empty())
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|m| format!("{}: {}\n\n", if m.role == "user" { "User" } else { "Assistant" }, util::clip(&m.content, 1500)))
        .collect();
    let prompt = format!(
        "{convo}Suggest three short follow-up questions the user might ask next, in the user's voice. Reply with the three questions only, one per line, with no numbering."
    );
    let (conn, model) = (Conn::current(), model.to_string());
    rt::spawn(async move { api::complete(&conn, &model, json!([{ "role": "user", "content": prompt }])).await }, move |r| {
        let Ok(text) = r else { return };
        let list = parse_followups(&text);
        if list.is_empty() {
            return;
        }
        if let Ok(Some(mut m)) = store::message(reply) {
            set_extra(&mut m.extra, "followups", json!(list));
            let _ = store::finish_message(&m);
        }
        if let Some(p) = super::page()
            && p.st.borrow().id() == Some(chat_id)
            && !super::streaming(Some(chat_id))
        {
            reload(&p);
            render(&p);
        }
    });
}

/// Up to three questions from the model's reply: one per line, bullets and
/// numbers removed.
fn parse_followups(text: &str) -> Vec<String> {
    let text = text.rsplit("</think>").next().unwrap_or(text);
    text.lines()
        .map(|l| l.trim().trim_start_matches(|c: char| c.is_ascii_digit() || "-*•.) ".contains(c)).trim().trim_matches('"'))
        .filter(|l| l.len() > 3 && l.len() <= 140)
        .take(3)
        .map(String::from)
        .collect()
}

/// Name a new chat after its first exchange.
fn maybe_title(chat_id: i64) {
    if !prefs::get().auto_title {
        return;
    }
    let Ok(Some(chat)) = store::chat(chat_id) else { return };
    if chat.title != "New chat" || chat.temporary {
        return;
    }
    let all = store::messages(chat_id).unwrap_or_default();
    let path = store::path_to(&all, chat.leaf);
    let Some(q) = path.iter().find(|m| m.role == "user") else { return };
    let a = path.iter().find(|m| m.role == "assistant" && !m.content.is_empty()).map(|m| m.content.clone()).unwrap_or_default();
    let fallback = util::clip(q.content.lines().next().unwrap_or("Chat"), 48);
    let conn = Conn::current();
    let model = chat.model.clone();
    let prompt = format!(
        "Write a short title (3 to 6 words) for this conversation. Reply with the title only, no quotes or punctuation at the end.\n\nUser: {}\n\nAssistant: {}",
        util::clip(&q.content, 1000),
        util::clip(&a, 1000)
    );
    rt::spawn(async move { api::complete(&conn, &model, json!([{ "role": "user", "content": prompt }])).await }, move |r| {
        let title = r.ok().map(|t| clean_title(&t)).filter(|t| !t.is_empty()).unwrap_or(fallback);
        if let Ok(Some(mut c)) = store::chat(chat_id)
            && c.title == "New chat"
        {
            c.title = title;
            let _ = store::save_chat(&c);
            hub::emit(&hub::CHATS);
            super::reload_if_current(chat_id);
        }
    });
}

fn clean_title(t: &str) -> String {
    // Thinking models sometimes leak their reasoning; keep the last line.
    let t = t.rsplit("</think>").next().unwrap_or(t);
    let line = t.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_matches(|c: char| c == '"' || c == '\'' || c == '*' || c == '#' || c == '.').trim();
    let line = line.strip_prefix("Title:").unwrap_or(line).trim();
    util::clip(line, 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_the_context() {
        let m = |role: &str, n: usize| Msg::new(role, "x".repeat(n));
        let chat = || {
            vec![m("system", 35), m("user", 350), m("assistant", 350), m("user", 350), m("assistant", 350), m("user", 35)]
        };
        // Everything fits: nothing changes.
        let mut ms = chat();
        assert_eq!(fit_context(&mut ms, 10_000, "rolling", 3.5), Ok(0));
        assert_eq!(ms.len(), 6);
        // Rolling drops the first exchange.
        let mut ms = chat();
        assert_eq!(fit_context(&mut ms, 260, "rolling", 3.5), Ok(2));
        assert_eq!(ms.iter().map(|x| x.role.as_str()).collect::<Vec<_>>(), ["system", "user", "assistant", "user"]);
        // Keep-first drops the middle instead.
        let mut ms = chat();
        assert_eq!(fit_context(&mut ms, 260, "keep_first", 3.5), Ok(2));
        assert_eq!(ms[1].content.len(), 350);
        assert_eq!(ms.len(), 4);
        // Stop changes nothing and reports the size.
        let mut ms = chat();
        assert!(fit_context(&mut ms, 250, "stop", 3.5).is_err());
        assert_eq!(ms.len(), 6);
        // Too big even with one turn left.
        let mut ms = chat();
        assert!(fit_context(&mut ms, 10, "rolling", 3.5).is_err());
    }

    #[test]
    fn parses_followups() {
        let q = parse_followups("<think>hm</think>\n1. How fast is it?\n- What does it cost?\n\n\"Can I try it?\"\nAnd one more?");
        assert_eq!(q, ["How fast is it?", "What does it cost?", "Can I try it?"]);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("\"Baking Sourdough Bread.\""), "Baking Sourdough Bread");
        assert_eq!(clean_title("<think>hmm</think>\nTitle: Rust Lifetimes"), "Rust Lifetimes");
        assert_eq!(clean_title("**Weekend Plans**"), "Weekend Plans");
    }
}
