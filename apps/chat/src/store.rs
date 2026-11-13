//! Chat history and everything else the app keeps, in one SQLite file.
//!
//! Messages form a tree (`parent`), so editing a question or regenerating an
//! answer adds a branch instead of overwriting. A chat's `leaf` is the message
//! at the end of the branch being shown.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;

use crate::{paths, util};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL, model TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '',
    tools TEXT NOT NULL DEFAULT '', knowledge TEXT NOT NULL DEFAULT '', collapsed INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS chats (
    id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '', folder INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    model TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '', params TEXT NOT NULL DEFAULT '{}',
    schema TEXT NOT NULL DEFAULT '', compare TEXT NOT NULL DEFAULT '', knowledge TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '', pinned INTEGER NOT NULL DEFAULT 0, archived INTEGER NOT NULL DEFAULT 0,
    host TEXT NOT NULL DEFAULT '', leaf INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
    temporary INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY, chat INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE, parent INTEGER,
    role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', thinking TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '', images TEXT NOT NULL DEFAULT '[]', files TEXT NOT NULL DEFAULT '[]',
    tool_calls TEXT NOT NULL DEFAULT '[]', tool_name TEXT NOT NULL DEFAULT '', extra TEXT NOT NULL DEFAULT '{}',
    tokens INTEGER NOT NULL DEFAULT 0, tps REAL NOT NULL DEFAULT 0, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat);
CREATE INDEX IF NOT EXISTS messages_parent ON messages(parent);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(body, chat UNINDEXED, msg UNINDEXED);
CREATE TABLE IF NOT EXISTS prompts (
    id INTEGER PRIMARY KEY, command TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', content TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS memories (id INTEGER PRIMARY KEY, content TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kbases (id INTEGER PRIMARY KEY, name TEXT NOT NULL, model TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kdocs (
    id INTEGER PRIMARY KEY, kb INTEGER NOT NULL REFERENCES kbases(id) ON DELETE CASCADE, name TEXT NOT NULL,
    chars INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kchunks (
    id INTEGER PRIMARY KEY, kb INTEGER NOT NULL REFERENCES kbases(id) ON DELETE CASCADE,
    doc INTEGER NOT NULL REFERENCES kdocs(id) ON DELETE CASCADE, text TEXT NOT NULL, vec BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS kchunks_kb ON kchunks(kb);
CREATE TABLE IF NOT EXISTS kchunk_vectors (
    chunk INTEGER NOT NULL REFERENCES kchunks(id) ON DELETE CASCADE,
    backend TEXT NOT NULL, model TEXT NOT NULL, vec BLOB NOT NULL,
    PRIMARY KEY (chunk, backend, model));
CREATE TABLE IF NOT EXISTS arena (
    id INTEGER PRIMARY KEY, prompt TEXT NOT NULL, model_a TEXT NOT NULL, model_b TEXT NOT NULL,
    winner TEXT NOT NULL, created INTEGER NOT NULL);
";

thread_local! {
    static DB: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn open() -> Result<Connection> {
    let path = std::env::var_os("NYX_CHAT_DB").map(std::path::PathBuf::from).unwrap_or_else(paths::database);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(SCHEMA)?;
    migrate(&db)?;
    Ok(db)
}

/// Bring a database from an older version up to date.
fn migrate(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        // Temporary chats (databases made before them lack the column).
        if db.prepare("SELECT temporary FROM chats LIMIT 0").is_err() {
            db.execute_batch("ALTER TABLE chats ADD COLUMN temporary INTEGER NOT NULL DEFAULT 0")?;
        }
        db.execute_batch("PRAGMA user_version = 1")?;
    }
    if version < 2 {
        db.execute_batch("ALTER TABLE kdocs ADD COLUMN backend TEXT NOT NULL DEFAULT 'ollama';
                          ALTER TABLE kdocs ADD COLUMN model TEXT NOT NULL DEFAULT '';
                          PRAGMA user_version = 2")?;
    }
    Ok(())
}

/// Run `f` with this thread's connection (each thread opens its own).
pub fn with<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    DB.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(open()?);
        }
        f(slot.as_ref().expect("just opened"))
    })
}

// ---------- Types ----------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub min_p: Option<f64>,
    pub repeat_penalty: Option<f64>,
    pub num_ctx: Option<i64>,
    pub seed: Option<i64>,
    pub think: Option<bool>,
    /// llama.cpp chat template reasoning level for this chat.
    pub reasoning_effort: Option<String>,
    /// What to do when the chat outgrows its context; `None` uses the default.
    pub overflow: Option<String>,
    /// Tools for this chat; `None` uses the defaults from Settings.
    pub tools: Option<Vec<String>>,
    /// Ask for JSON (with `schema` when set).
    pub json: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Chat {
    pub id: i64,
    pub title: String,
    pub folder: Option<i64>,
    pub model: String,
    pub system: String,
    pub params: Params,
    pub schema: String,
    /// Extra models answering alongside `model`, comma-separated.
    pub compare: Vec<String>,
    pub knowledge: Vec<i64>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
    pub host: String,
    pub leaf: i64,
    pub created: i64,
    pub updated: i64,
    /// Not kept: deleted when you leave it.
    pub temporary: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FileRef {
    pub name: String,
    /// Where the extracted text is kept.
    pub path: String,
    pub chars: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub id: i64,
    pub chat: i64,
    pub parent: Option<i64>,
    pub role: String,
    pub content: String,
    pub thinking: String,
    pub model: String,
    /// Image files in the attachments folder.
    pub images: Vec<String>,
    pub files: Vec<FileRef>,
    pub tool_calls: Vec<crate::ollama::chat::ToolCall>,
    pub tool_name: String,
    /// sources, error, compare flag, tool arguments…
    pub extra: Value,
    pub tokens: i64,
    pub tps: f64,
    pub created: i64,
}

impl Message {
    pub fn sources(&self) -> Vec<Source> {
        serde_json::from_value(self.extra["sources"].clone()).unwrap_or_default()
    }

    pub fn error(&self) -> Option<String> {
        self.extra["error"].as_str().map(String::from)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Folder {
    pub id: i64,
    pub name: String,
    pub model: String,
    pub system: String,
    pub tools: Vec<String>,
    pub knowledge: Vec<i64>,
    pub collapsed: bool,
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn split_ids(s: &str) -> Vec<i64> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

fn join_ids(ids: &[i64]) -> String {
    ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

const CHAT_COLS: &str =
    "id, title, folder, model, system, params, schema, compare, knowledge, tags, pinned, archived, host, leaf, created, updated, temporary";

fn chat_row(r: &rusqlite::Row) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: r.get(0)?,
        title: r.get(1)?,
        folder: r.get(2)?,
        model: r.get(3)?,
        system: r.get(4)?,
        params: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
        schema: r.get(6)?,
        compare: split_list(&r.get::<_, String>(7)?),
        knowledge: split_ids(&r.get::<_, String>(8)?),
        tags: split_list(&r.get::<_, String>(9)?),
        pinned: r.get(10)?,
        archived: r.get(11)?,
        host: r.get(12)?,
        leaf: r.get(13)?,
        created: r.get(14)?,
        updated: r.get(15)?,
        temporary: r.get(16)?,
    })
}

const MSG_COLS: &str =
    "id, chat, parent, role, content, thinking, model, images, files, tool_calls, tool_name, extra, tokens, tps, created";

fn msg_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        chat: r.get(1)?,
        parent: r.get(2)?,
        role: r.get(3)?,
        content: r.get(4)?,
        thinking: r.get(5)?,
        model: r.get(6)?,
        images: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        files: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        tool_calls: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
        tool_name: r.get(10)?,
        extra: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or(Value::Null),
        tokens: r.get(12)?,
        tps: r.get(13)?,
        created: r.get(14)?,
    })
}

// ---------- Chats ----------

pub fn create_chat(model: &str, folder: Option<i64>, host: &str) -> Result<i64> {
    with(|db| {
        let now = util::now();
        db.execute(
            "INSERT INTO chats (title, folder, model, host, created, updated) VALUES ('New chat', ?1, ?2, ?3, ?4, ?4)",
            params![folder, model, host, now],
        )?;
        Ok(db.last_insert_rowid())
    })
}

pub fn chat(id: i64) -> Result<Option<Chat>> {
    with(|db| Ok(db.query_row(&format!("SELECT {CHAT_COLS} FROM chats WHERE id = ?1"), [id], chat_row).optional()?))
}

/// Every chat that isn't archived (or only archived ones), newest first.
pub fn chats(archived: bool) -> Result<Vec<Chat>> {
    with(|db| {
        let mut st = db.prepare(&format!("SELECT {CHAT_COLS} FROM chats WHERE archived = ?1 AND temporary = 0 ORDER BY updated DESC"))?;
        Ok(st.query_map([archived], chat_row)?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn save_chat(c: &Chat) -> Result<()> {
    with(|db| {
        db.execute(
            "UPDATE chats SET title=?2, folder=?3, model=?4, system=?5, params=?6, schema=?7, compare=?8, knowledge=?9,
             tags=?10, pinned=?11, archived=?12, host=?13, leaf=?14, temporary=?15 WHERE id=?1",
            params![
                c.id,
                c.title,
                c.folder,
                c.model,
                c.system,
                serde_json::to_string(&c.params)?,
                c.schema,
                c.compare.join(","),
                join_ids(&c.knowledge),
                c.tags.join(","),
                c.pinned,
                c.archived,
                c.host,
                c.leaf,
                c.temporary
            ],
        )?;
        Ok(())
    })
}

pub fn touch_chat(id: i64) -> Result<()> {
    with(|db| {
        db.execute("UPDATE chats SET updated = ?2 WHERE id = ?1", params![id, util::now()])?;
        Ok(())
    })
}

pub fn set_leaf(chat: i64, leaf: i64) -> Result<()> {
    with(|db| {
        db.execute("UPDATE chats SET leaf = ?2 WHERE id = ?1", params![chat, leaf])?;
        Ok(())
    })
}

/// A new chat holding `chat`'s conversation up to and including `msg`, with
/// the same settings. Attachments are shared by path. Returns the new chat.
pub fn branch_chat(chat: i64, msg: i64) -> Result<i64> {
    let Some(src) = self::chat(chat)? else { anyhow::bail!("no such chat") };
    let all = messages(chat)?;
    let path = path_to(&all, msg);
    let id = create_chat(&src.model, src.folder, &src.host)?;
    let mut copy = src.clone();
    copy.id = id;
    copy.title = format!("{} (branch)", src.title);
    copy.pinned = false;
    copy.archived = false;
    // A branch is kept, even from a temporary chat.
    copy.temporary = false;
    let mut parent: Option<i64> = None;
    for m in &path {
        let new_id = add_message(NewMessage {
            chat: id,
            parent,
            role: &m.role,
            content: &m.content,
            model: &m.model,
            images: &m.images,
            files: &m.files,
            extra: m.extra.clone(),
        })?;
        let mut done = m.clone();
        done.id = new_id;
        done.chat = id;
        finish_message(&done)?;
        parent = Some(new_id);
    }
    copy.leaf = parent.unwrap_or(0);
    save_chat(&copy)?;
    Ok(id)
}

/// Delete every temporary chat (left over from a crash, or on quitting).
pub fn purge_temporary() -> Result<()> {
    let ids: Vec<i64> = with(|db| {
        let mut st = db.prepare("SELECT id FROM chats WHERE temporary = 1")?;
        Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    })?;
    for id in ids {
        delete_chat(id)?;
    }
    Ok(())
}

pub fn delete_chat(id: i64) -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM fts WHERE chat = ?1", [id])?;
        db.execute("DELETE FROM chats WHERE id = ?1", [id])?;
        Ok(())
    })
}

pub fn delete_all_chats() -> Result<()> {
    with(|db| {
        db.execute_batch("DELETE FROM fts; DELETE FROM messages; DELETE FROM chats;")?;
        Ok(())
    })
}

// ---------- Messages ----------

pub struct NewMessage<'a> {
    pub chat: i64,
    pub parent: Option<i64>,
    pub role: &'a str,
    pub content: &'a str,
    pub model: &'a str,
    pub images: &'a [String],
    pub files: &'a [FileRef],
    pub extra: Value,
}

pub fn add_message(m: NewMessage) -> Result<i64> {
    with(|db| {
        db.execute(
            "INSERT INTO messages (chat, parent, role, content, model, images, files, extra, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                m.chat,
                m.parent,
                m.role,
                m.content,
                m.model,
                serde_json::to_string(m.images)?,
                serde_json::to_string(m.files)?,
                m.extra.to_string(),
                util::now()
            ],
        )?;
        let id = db.last_insert_rowid();
        db.execute("UPDATE chats SET leaf = ?2, updated = ?3 WHERE id = ?1", params![m.chat, id, util::now()])?;
        if !m.content.is_empty() {
            db.execute("INSERT INTO fts (body, chat, msg) VALUES (?1, ?2, ?3)", params![m.content, m.chat, id])?;
        }
        Ok(id)
    })
}

/// Store the finished text of an assistant or tool message.
pub fn finish_message(m: &Message) -> Result<()> {
    with(|db| {
        db.execute(
            "UPDATE messages SET content=?2, thinking=?3, tool_calls=?4, tool_name=?5, extra=?6, tokens=?7, tps=?8 WHERE id=?1",
            params![
                m.id,
                m.content,
                m.thinking,
                serde_json::to_string(&m.tool_calls)?,
                m.tool_name,
                m.extra.to_string(),
                m.tokens,
                m.tps
            ],
        )?;
        db.execute("DELETE FROM fts WHERE msg = ?1", [m.id])?;
        if !m.content.is_empty() && m.role != "tool" {
            db.execute("INSERT INTO fts (body, chat, msg) VALUES (?1, ?2, ?3)", params![m.content, m.chat, m.id])?;
        }
        Ok(())
    })
}

pub fn message(id: i64) -> Result<Option<Message>> {
    with(|db| Ok(db.query_row(&format!("SELECT {MSG_COLS} FROM messages WHERE id = ?1"), [id], msg_row).optional()?))
}

pub fn messages(chat: i64) -> Result<Vec<Message>> {
    with(|db| {
        let mut st = db.prepare(&format!("SELECT {MSG_COLS} FROM messages WHERE chat = ?1 ORDER BY id"))?;
        Ok(st.query_map([chat], msg_row)?.collect::<rusqlite::Result<_>>()?)
    })
}

/// Remove a message and everything below it.
pub fn delete_branch(id: i64) -> Result<()> {
    with(|db| {
        db.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            let mut stack = vec![id];
            while let Some(m) = stack.pop() {
                let mut st = db.prepare("SELECT id FROM messages WHERE parent = ?1")?;
                stack.extend(st.query_map([m], |r| r.get::<_, i64>(0))?.flatten());
                db.execute("DELETE FROM fts WHERE msg = ?1", [m])?;
                db.execute("DELETE FROM messages WHERE id = ?1", [m])?;
            }
            Ok(())
        })();
        db.execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
        r
    })
}

/// The messages from the root down to `leaf`, from an in-memory list.
pub fn path_to(all: &[Message], leaf: i64) -> Vec<Message> {
    let mut out = Vec::new();
    let mut cur = all.iter().find(|m| m.id == leaf);
    while let Some(m) = cur {
        out.push(m.clone());
        cur = m.parent.and_then(|p| all.iter().find(|x| x.id == p));
    }
    out.reverse();
    out
}

/// Messages sharing `m`'s parent (including `m`), oldest first.
pub fn siblings(all: &[Message], m: &Message) -> Vec<i64> {
    all.iter().filter(|x| x.parent == m.parent && x.chat == m.chat).map(|x| x.id).collect()
}

/// Follow the newest child down to the end of the branch.
pub fn deepest(all: &[Message], from: i64) -> i64 {
    let mut cur = from;
    while let Some(child) = all.iter().filter(|m| m.parent == Some(cur)).max_by_key(|m| m.id) {
        cur = child.id;
    }
    cur
}

// ---------- Search ----------

pub struct Hit {
    pub chat: i64,
    pub title: String,
    pub snippet: String,
}

fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect::<String>())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chats whose title or messages match every word.
pub fn search(q: &str) -> Result<Vec<Hit>> {
    let fq = fts_query(q);
    if fq.is_empty() {
        return Ok(vec![]);
    }
    with(|db| {
        let mut hits: Vec<Hit> = Vec::new();
        let like = format!("%{}%", q.trim());
        let mut st = db.prepare("SELECT id, title FROM chats WHERE title LIKE ?1 AND temporary = 0 ORDER BY updated DESC LIMIT 50")?;
        for r in st.query_map([&like], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.flatten() {
            hits.push(Hit { chat: r.0, title: r.1, snippet: String::new() });
        }
        let mut st = db.prepare(
            "SELECT fts.chat, chats.title, snippet(fts, 0, '\u{1}', '\u{2}', '…', 12) FROM fts
             JOIN chats ON chats.id = fts.chat WHERE fts MATCH ?1 AND chats.temporary = 0 ORDER BY rank LIMIT 100",
        )?;
        for r in st.query_map([&fq], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.flatten() {
            if let Some(h) = hits.iter_mut().find(|h| h.chat == r.0) {
                if h.snippet.is_empty() {
                    h.snippet = r.2;
                }
            } else {
                hits.push(Hit { chat: r.0, title: r.1, snippet: r.2 });
            }
        }
        Ok(hits)
    })
}

// ---------- Folders ----------

pub fn folders() -> Result<Vec<Folder>> {
    with(|db| {
        let mut st = db.prepare("SELECT id, name, model, system, tools, knowledge, collapsed FROM folders ORDER BY name")?;
        Ok(st
            .query_map([], |r| {
                Ok(Folder {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    model: r.get(2)?,
                    system: r.get(3)?,
                    tools: split_list(&r.get::<_, String>(4)?),
                    knowledge: split_ids(&r.get::<_, String>(5)?),
                    collapsed: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn folder(id: i64) -> Option<Folder> {
    folders().ok()?.into_iter().find(|f| f.id == id)
}

pub fn save_folder(f: &Folder) -> Result<i64> {
    with(|db| {
        if f.id == 0 {
            db.execute(
                "INSERT INTO folders (name, model, system, tools, knowledge, collapsed, created) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![f.name, f.model, f.system, f.tools.join(","), join_ids(&f.knowledge), f.collapsed, util::now()],
            )?;
            return Ok(db.last_insert_rowid());
        }
        db.execute(
            "UPDATE folders SET name=?2, model=?3, system=?4, tools=?5, knowledge=?6, collapsed=?7 WHERE id=?1",
            params![f.id, f.name, f.model, f.system, f.tools.join(","), join_ids(&f.knowledge), f.collapsed],
        )?;
        Ok(f.id)
    })
}

pub fn delete_folder(id: i64) -> Result<()> {
    with(|db| {
        db.execute("UPDATE chats SET folder = NULL WHERE folder = ?1", [id])?;
        db.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(())
    })
}

// ---------- Prompts ----------

#[derive(Clone, Debug, Default)]
pub struct Prompt {
    pub id: i64,
    pub command: String,
    pub title: String,
    pub content: String,
}

pub fn prompts() -> Result<Vec<Prompt>> {
    with(|db| {
        let mut st = db.prepare("SELECT id, command, title, content FROM prompts ORDER BY command")?;
        Ok(st
            .query_map([], |r| Ok(Prompt { id: r.get(0)?, command: r.get(1)?, title: r.get(2)?, content: r.get(3)? }))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn save_prompt(p: &Prompt) -> Result<()> {
    with(|db| {
        if p.id == 0 {
            db.execute(
                "INSERT INTO prompts (command, title, content) VALUES (?1, ?2, ?3)",
                params![p.command, p.title, p.content],
            )?;
        } else {
            db.execute(
                "UPDATE prompts SET command=?2, title=?3, content=?4 WHERE id=?1",
                params![p.id, p.command, p.title, p.content],
            )?;
        }
        Ok(())
    })
}

pub fn delete_prompt(id: i64) -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM prompts WHERE id = ?1", [id])?;
        Ok(())
    })
}

// ---------- Memories ----------

pub fn memories() -> Result<Vec<(i64, String)>> {
    with(|db| {
        let mut st = db.prepare("SELECT id, content FROM memories ORDER BY id")?;
        Ok(st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn add_memory(content: &str) -> Result<()> {
    with(|db| {
        db.execute("INSERT INTO memories (content, created) VALUES (?1, ?2)", params![content.trim(), util::now()])?;
        Ok(())
    })
}

pub fn update_memory(id: i64, content: &str) -> Result<()> {
    with(|db| {
        db.execute("UPDATE memories SET content = ?2 WHERE id = ?1", params![id, content.trim()])?;
        Ok(())
    })
}

pub fn delete_memory(id: i64) -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM memories WHERE id = ?1", [id])?;
        Ok(())
    })
}

// ---------- Knowledge ----------

#[derive(Clone, Debug, Default)]
pub struct KBase {
    pub id: i64,
    pub name: String,
    pub model: String,
    pub docs: Vec<(i64, String, i64)>,
}

pub fn kbases() -> Result<Vec<KBase>> {
    with(|db| {
        let mut st = db.prepare("SELECT id, name, model FROM kbases ORDER BY name")?;
        let mut out: Vec<KBase> = st
            .query_map([], |r| Ok(KBase { id: r.get(0)?, name: r.get(1)?, model: r.get(2)?, docs: vec![] }))?
            .collect::<rusqlite::Result<_>>()?;
        let mut st = db.prepare("SELECT id, name, chars FROM kdocs WHERE kb = ?1 ORDER BY name")?;
        for kb in &mut out {
            kb.docs = st.query_map([kb.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        }
        Ok(out)
    })
}

pub fn add_kbase(name: &str, model: &str) -> Result<i64> {
    with(|db| {
        db.execute("INSERT INTO kbases (name, model, created) VALUES (?1, ?2, ?3)", params![name, model, util::now()])?;
        Ok(db.last_insert_rowid())
    })
}

pub fn delete_kbase(id: i64) -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM kbases WHERE id = ?1", [id])?;
        Ok(())
    })
}

pub fn delete_kdoc(id: i64) -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM kdocs WHERE id = ?1", [id])?;
        Ok(())
    })
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn add_kdoc(kb: i64, name: &str, backend: &str, model: &str, chunks: &[(String, Vec<f32>)]) -> Result<()> {
    with(|db| {
        db.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            let chars: usize = chunks.iter().map(|c| c.0.chars().count()).sum();
            db.execute(
                "INSERT INTO kdocs (kb, name, chars, created, backend, model) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![kb, name, chars as i64, util::now(), backend, model],
            )?;
            let doc = db.last_insert_rowid();
            let mut st = db.prepare("INSERT INTO kchunks (kb, doc, text, vec) VALUES (?1, ?2, ?3, ?4)")?;
            for (text, v) in chunks {
                st.execute(params![kb, doc, text, vec_to_blob(v)])?;
            }
            Ok(())
        })();
        db.execute_batch(if r.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
        r
    })
}

/// Chunks that still need vectors for a selected backend and embedding model.
pub fn missing_vectors(kb: i64, backend: &str, model: &str) -> Result<Vec<(i64, String)>> {
    with(|db| {
        let mut st = db.prepare("SELECT c.id, c.text FROM kchunks c JOIN kdocs d ON d.id=c.doc
            LEFT JOIN kchunk_vectors v ON v.chunk=c.id AND v.backend=?2 AND v.model=?3
            WHERE c.kb=?1 AND NOT (d.backend=?2 AND COALESCE(NULLIF(d.model,''), (SELECT model FROM kbases WHERE id=?1))=?3)
            AND v.chunk IS NULL")?;
        Ok(st.query_map(params![kb, backend, model], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn save_vectors(backend: &str, model: &str, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
    with(|db| {
        let mut st = db.prepare("INSERT OR REPLACE INTO kchunk_vectors (chunk, backend, model, vec) VALUES (?1,?2,?3,?4)")?;
        for (id, vec) in vectors { st.execute(params![id, backend, model, vec_to_blob(vec)])?; }
        Ok(())
    })
}

pub fn kchunks_for(kb: i64, backend: &str, model: &str) -> Result<Vec<(String, String, Vec<f32>)>> {
    with(|db| {
        let mut st = db.prepare("SELECT d.name, c.text, CASE WHEN d.backend=?2 AND
            COALESCE(NULLIF(d.model,''), (SELECT model FROM kbases WHERE id=?1))=?3 THEN c.vec ELSE v.vec END
            FROM kchunks c JOIN kdocs d ON d.id=c.doc
            LEFT JOIN kchunk_vectors v ON v.chunk=c.id AND v.backend=?2 AND v.model=?3
            WHERE c.kb=?1")?;
        let mut out = Vec::new();
        for row in st.query_map(params![kb, backend, model], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<Vec<u8>>>(2)?)))? {
            let (name, text, vec) = row?;
            if let Some(vec) = vec { out.push((name, text, blob_to_vec(&vec))); }
        }
        Ok(out)
    })
}

// ---------- Arena ----------

pub fn add_vote(prompt: &str, a: &str, b: &str, winner: &str) -> Result<()> {
    with(|db| {
        db.execute(
            "INSERT INTO arena (prompt, model_a, model_b, winner, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![prompt, a, b, winner, util::now()],
        )?;
        Ok(())
    })
}

pub fn votes() -> Result<Vec<(String, String, String)>> {
    with(|db| {
        let mut st = db.prepare("SELECT model_a, model_b, winner FROM arena ORDER BY id")?;
        Ok(st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?)
    })
}

/// The newest rounds first: (question, model A, model B, winner).
pub fn recent_votes(n: usize) -> Result<Vec<(String, String, String, String)>> {
    with(|db| {
        let mut st = db.prepare("SELECT prompt, model_a, model_b, winner FROM arena ORDER BY id DESC LIMIT ?1")?;
        Ok(st.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn clear_votes() -> Result<()> {
    with(|db| {
        db.execute("DELETE FROM arena", [])?;
        Ok(())
    })
}

// ---------- Stats ----------

#[derive(Clone, Debug, Default)]
pub struct ModelStats {
    pub model: String,
    pub replies: i64,
    pub tokens: i64,
    pub avg_tps: f64,
}

pub struct Totals {
    pub chats: i64,
    pub messages: i64,
    pub tokens: i64,
}

pub fn totals() -> Result<Totals> {
    with(|db| {
        Ok(Totals {
            chats: db.query_row("SELECT COUNT(*) FROM chats", [], |r| r.get(0))?,
            messages: db.query_row("SELECT COUNT(*) FROM messages WHERE role IN ('user','assistant')", [], |r| r.get(0))?,
            tokens: db.query_row("SELECT COALESCE(SUM(tokens),0) FROM messages", [], |r| r.get(0))?,
        })
    })
}

/// Per local day for the last `days` days: (day start, tokens, chats started).
pub fn daily(days: i64) -> Result<Vec<(i64, i64, i64)>> {
    use chrono::{Duration, Local, TimeZone};
    let today = Local::now().date_naive();
    let start_of = |d: chrono::NaiveDate| {
        Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).expect("midnight")).earliest().map(|t| t.timestamp()).unwrap_or(0)
    };
    with(|db| {
        let mut out = Vec::new();
        for i in (0..days).rev() {
            let day = today - Duration::days(i);
            let (a, b) = (start_of(day), start_of(day + Duration::days(1)));
            let tokens: i64 =
                db.query_row("SELECT COALESCE(SUM(tokens),0) FROM messages WHERE created >= ?1 AND created < ?2", [a, b], |r| {
                    r.get(0)
                })?;
            let chats: i64 =
                db.query_row("SELECT COUNT(*) FROM chats WHERE created >= ?1 AND created < ?2", [a, b], |r| r.get(0))?;
            out.push((a, tokens, chats));
        }
        Ok(out)
    })
}

/// The last seven days: chats started and words the models wrote.
pub struct Week {
    pub chats: i64,
    pub words: u64,
}

pub fn week() -> Result<Week> {
    let since = util::now() - 7 * 86_400;
    with(|db| {
        let chats = db.query_row("SELECT COUNT(*) FROM chats WHERE created >= ?1 AND temporary = 0", [since], |r| r.get(0))?;
        let mut st = db.prepare("SELECT content FROM messages WHERE role = 'assistant' AND created >= ?1")?;
        let mut words = 0u64;
        for content in st.query_map([since], |r| r.get::<_, String>(0))? {
            words += content?.split_whitespace().count() as u64;
        }
        Ok(Week { chats, words })
    })
}

/// The model that writes fastest, in words a second: its average speed in tokens,
/// turned into words by how many words its recent replies have per token.
pub fn fastest() -> Result<Option<(String, f64)>> {
    let best = per_model()?.into_iter().filter(|m| m.avg_tps > 0.0).max_by(|a, b| a.avg_tps.total_cmp(&b.avg_tps));
    let Some(best) = best else { return Ok(None) };
    with(|db| {
        let mut st = db.prepare(
            "SELECT content, tokens FROM messages WHERE role = 'assistant' AND model = ?1 AND tokens > 0 ORDER BY id DESC LIMIT 200",
        )?;
        let (mut words, mut tokens) = (0u64, 0i64);
        for row in st.query_map([&best.model], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (content, t) = row?;
            words += content.split_whitespace().count() as u64;
            tokens += t;
        }
        let ratio = if tokens > 0 { words as f64 / tokens as f64 } else { 0.0 };
        Ok((ratio > 0.0).then(|| (best.model.clone(), best.avg_tps * ratio)))
    })
}

/// The newest reply in a chat, for a preview line.
pub fn last_reply(chat: i64) -> Result<Option<String>> {
    with(|db| {
        Ok(db
            .query_row(
                "SELECT content FROM messages WHERE chat = ?1 AND role = 'assistant' AND content != '' ORDER BY id DESC LIMIT 1",
                [chat],
                |r| r.get(0),
            )
            .optional()?)
    })
}

pub fn per_model() -> Result<Vec<ModelStats>> {
    with(|db| {
        let mut st = db.prepare(
            "SELECT model, COUNT(*), COALESCE(SUM(tokens),0), COALESCE(AVG(CASE WHEN tokens >= 2 AND tps > 0 THEN tps END),0) FROM messages
             WHERE role = 'assistant' AND model != '' GROUP BY model ORDER BY COUNT(*) DESC",
        )?;
        Ok(st
            .query_map([], |r| Ok(ModelStats { model: r.get(0)?, replies: r.get(1)?, tokens: r.get(2)?, avg_tps: r.get(3)? }))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Point this thread at a fresh database.
    pub fn scratch() {
        DB.with(|cell| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(SCHEMA).unwrap();
            migrate(&db).unwrap();
            *cell.borrow_mut() = Some(db);
        });
    }

    fn msg(chat: i64, parent: Option<i64>, role: &str, content: &str) -> i64 {
        add_message(NewMessage { chat, parent, role, content, model: "m", images: &[], files: &[], extra: Value::Null }).unwrap()
    }

    #[test]
    fn migrates_old_databases() {
        let db = Connection::open_in_memory().unwrap();
        // A chats table from before temporary chats.
        db.execute_batch(
            "CREATE TABLE chats (id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '', folder INTEGER, model TEXT NOT NULL DEFAULT '',
             system TEXT NOT NULL DEFAULT '', params TEXT NOT NULL DEFAULT '{}', schema TEXT NOT NULL DEFAULT '',
             compare TEXT NOT NULL DEFAULT '', knowledge TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT '',
             pinned INTEGER NOT NULL DEFAULT 0, archived INTEGER NOT NULL DEFAULT 0, host TEXT NOT NULL DEFAULT '',
             leaf INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL);
             INSERT INTO chats (title, created, updated) VALUES ('old', 1, 1);",
        )
        .unwrap();
        db.execute_batch(SCHEMA).unwrap();
        migrate(&db).unwrap();
        migrate(&db).unwrap();
        let t: i64 = db.query_row("SELECT temporary FROM chats", [], |r| r.get(0)).unwrap();
        assert_eq!(t, 0);
    }

    #[test]
    fn temporary_chats_stay_out_of_lists() {
        scratch();
        let keep = create_chat("m", None, "").unwrap();
        let temp = create_chat("m", None, "").unwrap();
        let mut c = chat(temp).unwrap().unwrap();
        c.temporary = true;
        c.title = "secret plans".into();
        save_chat(&c).unwrap();
        msg(temp, None, "user", "secret plans");
        assert_eq!(chats(false).unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [keep]);
        assert!(search("secret").unwrap().is_empty());
        purge_temporary().unwrap();
        assert!(chat(temp).unwrap().is_none());
        assert!(chat(keep).unwrap().is_some());
    }

    #[test]
    fn branches_a_chat() {
        scratch();
        let c = create_chat("m", None, "").unwrap();
        let q1 = msg(c, None, "user", "one");
        let a1 = msg(c, Some(q1), "assistant", "two");
        let q2 = msg(c, Some(a1), "user", "three");
        msg(c, Some(q2), "assistant", "four");
        let b = branch_chat(c, a1).unwrap();
        let copied = messages(b).unwrap();
        assert_eq!(copied.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(copied[1].parent, Some(copied[0].id));
        let bc = chat(b).unwrap().unwrap();
        assert_eq!(bc.leaf, copied[1].id);
        assert!(bc.title.ends_with("(branch)"));
    }

    #[test]
    fn branches() {
        scratch();
        let c = create_chat("m", None, "local").unwrap();
        let u1 = msg(c, None, "user", "hello");
        let a1 = msg(c, Some(u1), "assistant", "first");
        let a2 = msg(c, Some(u1), "assistant", "second");
        let u2 = msg(c, Some(a1), "user", "more");
        let all = messages(c).unwrap();
        assert_eq!(siblings(&all, all.iter().find(|m| m.id == a1).unwrap()), vec![a1, a2]);
        assert_eq!(deepest(&all, a1), u2);
        let path: Vec<i64> = path_to(&all, u2).iter().map(|m| m.id).collect();
        assert_eq!(path, vec![u1, a1, u2]);
        assert_eq!(chat(c).unwrap().unwrap().leaf, u2);
        delete_branch(a1).unwrap();
        let ids: Vec<i64> = messages(c).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![u1, a2]);
    }

    #[test]
    fn full_text_search() {
        scratch();
        let c = create_chat("m", None, "local").unwrap();
        let u = msg(c, None, "user", "how do I bake sourdough bread");
        let a = msg(c, Some(u), "assistant", "");
        let mut m = message(a).unwrap().unwrap();
        m.content = "Start with an active starter.".into();
        finish_message(&m).unwrap();
        assert_eq!(search("sourdough").unwrap().len(), 1);
        assert_eq!(search("starter").unwrap().len(), 1);
        assert!(search("pizza").unwrap().is_empty());
        assert!(search("\"').*").unwrap().is_empty());
    }

    #[test]
    fn chat_settings_round_trip() {
        scratch();
        let id = create_chat("llama", None, "local").unwrap();
        let mut c = chat(id).unwrap().unwrap();
        c.compare = vec!["qwen3".into()];
        c.knowledge = vec![3, 4];
        c.params.tools = Some(vec!["web_search".into()]);
        c.tags = vec!["work".into()];
        save_chat(&c).unwrap();
        let back = chat(id).unwrap().unwrap();
        assert_eq!(back.compare, vec!["qwen3"]);
        assert_eq!(back.knowledge, vec![3, 4]);
        assert_eq!(back.params.tools, Some(vec!["web_search".to_string()]));
        assert_eq!(back.tags, vec!["work"]);
    }

    #[test]
    fn knowledge_vectors_round_trip() {
        scratch();
        let kb = add_kbase("docs", "nomic").unwrap();
        add_kdoc(kb, "a.md", "ollama", "nomic", &[("hello".into(), vec![0.5, -1.0])]).unwrap();
        let chunks = kchunks_for(kb, "ollama", "nomic").unwrap();
        assert_eq!(chunks[0].2, vec![0.5, -1.0]);
        let missing = missing_vectors(kb, "llama_cpp", "embed").unwrap();
        assert_eq!(missing.len(), 1);
        save_vectors("llama_cpp", "embed", &[(missing[0].0, vec![0.25, 0.75])]).unwrap();
        assert_eq!(kchunks_for(kb, "llama_cpp", "embed").unwrap()[0].2, vec![0.25, 0.75]);
        assert_eq!(kchunks_for(kb, "ollama", "nomic").unwrap()[0].2, vec![0.5, -1.0]);
        assert_eq!(kbases().unwrap()[0].docs.len(), 1);
    }
}
