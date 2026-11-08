//! The download queue, shared by Models and Marketplace. Pulls run one after
//! another so each gets the full connection.

use std::cell::RefCell;
use std::time::Instant;

use crate::ollama::api::{self, Conn, Progress};
use crate::{hub, models, rt, window};

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Pull {
    pub id: u64,
    pub name: String,
    conn: Conn,
    pub state: State,
    pub status: String,
    pub completed: u64,
    pub total: u64,
    /// Bytes per second, smoothed.
    pub speed: f64,
    last: Option<(Instant, u64)>,
}

impl Pull {
    pub fn fraction(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.completed as f64 / self.total as f64 }
    }

    pub fn eta_secs(&self) -> Option<u64> {
        (self.speed > 1.0 && self.total > self.completed).then(|| ((self.total - self.completed) as f64 / self.speed) as u64)
    }

    pub fn active(&self) -> bool {
        matches!(self.state, State::Queued | State::Running)
    }
}

thread_local! {
    static PULLS: RefCell<Vec<Pull>> = const { RefCell::new(Vec::new()) };
    static CURRENT: RefCell<Option<(u64, tokio::task::AbortHandle)>> = const { RefCell::new(None) };
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

pub fn all() -> Vec<Pull> {
    PULLS.with(|p| p.borrow().clone())
}

pub fn active_count() -> usize {
    PULLS.with(|p| p.borrow().iter().filter(|x| x.active()).count())
}

pub fn is_pulling(name: &str) -> bool {
    PULLS.with(|p| p.borrow().iter().any(|x| x.active() && x.name == name))
}

pub fn start(name: &str) {
    let name = name.trim().to_string();
    if name.is_empty() || is_pulling(&name) {
        return;
    }
    let id = NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    });
    PULLS.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|x| x.active() || x.name != name);
        p.push(Pull {
            id,
            name: name.clone(),
            conn: Conn::current(),
            state: State::Queued,
            status: "Waiting".into(),
            completed: 0,
            total: 0,
            speed: 0.0,
            last: None,
        });
    });
    hub::emit(&hub::PULLS);
    pump();
}

pub fn cancel(id: u64) {
    let target = PULLS.with(|p| p.borrow().iter().find(|x| x.id == id && x.active()).map(|x| (x.name.clone(), x.conn.clone())));
    if let Some((name, conn)) = target {
        if crate::llama::core::local(&conn) {
            rt::spawn(async move { crate::llama::core::cancel_pull(&name).await }, |_| models::refresh());
        } else if conn.backend == crate::prefs::Backend::LlamaCpp {
            rt::spawn(async move { api::unload(&conn, &name).await }, |_| models::refresh());
        }
    }
    let running = CURRENT.with(|c| c.borrow().as_ref().filter(|(cid, _)| *cid == id).map(|(_, h)| h.clone()));
    if let Some(h) = running {
        h.abort();
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
    set(id, |p| p.state = State::Cancelled);
    hub::emit(&hub::PULLS);
    pump();
}

/// Forget finished, failed and cancelled downloads.
pub fn clear_finished() {
    PULLS.with(|p| p.borrow_mut().retain(|x| x.active()));
    hub::emit(&hub::PULLS);
}

fn set(id: u64, f: impl FnOnce(&mut Pull)) {
    PULLS.with(|p| {
        if let Some(x) = p.borrow_mut().iter_mut().find(|x| x.id == id) {
            f(x);
        }
    });
}

fn pump() {
    if CURRENT.with(|c| c.borrow().is_some()) {
        return;
    }
    let Some((id, name, conn)) = PULLS.with(|p| p.borrow().iter().find(|x| x.state == State::Queued).map(|x| (x.id, x.name.clone(), x.conn.clone())))
    else {
        return;
    };
    set(id, |p| {
        p.state = State::Running;
        p.status = "Starting".into();
    });
    hub::emit(&hub::PULLS);
    let (done_tx, done_rx) = async_channel::bounded::<Result<(), String>>(1);
    let handle = rt::stream(
        move |tx: async_channel::Sender<Progress>| async move {
            let r = api::pull(&conn, &name, tx).await.map_err(|e| api::short_error(&format!("{e:#}")));
            let _ = done_tx.send(r).await;
        },
        move |tick: Progress| {
            set(id, |p| {
                p.status = tick.status.clone();
                if tick.total > 0 {
                    let now = Instant::now();
                    if let Some((t, c)) = p.last
                        && tick.completed >= c
                    {
                        let dt = now.duration_since(t).as_secs_f64();
                        if dt > 0.3 {
                            let inst = (tick.completed - c) as f64 / dt;
                            p.speed = if p.speed == 0.0 { inst } else { p.speed * 0.7 + inst * 0.3 };
                            p.last = Some((now, tick.completed));
                        }
                    } else {
                        p.last = Some((now, tick.completed));
                    }
                    if tick.total != p.total {
                        p.speed = 0.0;
                        p.last = Some((now, tick.completed));
                    }
                    p.completed = tick.completed;
                    p.total = tick.total;
                }
            });
            hub::emit(&hub::PULLS);
        },
    );
    CURRENT.with(|c| *c.borrow_mut() = Some((id, handle)));
    gtk::glib::spawn_future_local(async move {
        let result = done_rx.recv().await.unwrap_or(Err("cancelled".into()));
        let still_current = CURRENT.with(|c| c.borrow().as_ref().is_some_and(|(cid, _)| *cid == id));
        if !still_current {
            return;
        }
        CURRENT.with(|c| *c.borrow_mut() = None);
        let name = PULLS.with(|p| p.borrow().iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
        match result {
            Ok(()) => {
                set(id, |p| {
                    p.state = State::Done;
                    p.status = "Done".into();
                    p.completed = p.total;
                });
                models::clear_update(&name);
                window::toast(&format!("Downloaded {name}"));
            }
            Err(e) => {
                set(id, |p| p.state = State::Failed(e.clone()));
                window::toast(&format!("Couldn't download {name}: {e}"));
            }
        }
        models::refresh();
        hub::emit(&hub::PULLS);
        pump();
    });
}

/// A compact label for marketplace download buttons.
pub fn brief(p: &Pull) -> String {
    if p.total == 0 { return if p.state == State::Queued { "Queued".into() } else { "Starting…".into() }; }
    let mut s = format!("{}%", (p.fraction() * 100.0).clamp(0.0, 100.0).floor() as u32);
    if p.speed > 0.0 { s.push_str(&format!(" · {}/s", crate::util::size(p.speed as u64))); }
    s
}

/// "26% · 1.2 GB of 4.7 GB · 38 MB/s · 2 min left"
pub fn describe(p: &Pull) -> String {
    use crate::util::size;
    match &p.state {
        State::Queued => "Waiting".into(),
        State::Done => "Done".into(),
        State::Cancelled => "Cancelled".into(),
        State::Failed(e) => e.clone(),
        State::Running if p.total == 0 => capitalize(&p.status),
        State::Running => {
            let mut s = format!("{}% · {} of {}", (p.fraction() * 100.0).clamp(0.0, 100.0).floor() as u32, size(p.completed), size(p.total));
            if p.speed > 0.0 {
                s.push_str(&format!(" · {}/s", size(p.speed as u64)));
            }
            if let Some(eta) = p.eta_secs() {
                s.push_str(&format!(" · {} left", duration(eta)));
            }
            s
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, (secs % 3600) / 60),
    }
}
