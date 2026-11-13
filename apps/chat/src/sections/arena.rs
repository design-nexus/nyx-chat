//! Arena: two anonymous models answer the same prompt; the user votes, and
//! an Elo leaderboard keeps score.

use gtk::glib;
use gtk::prelude::*;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::markdown::Markdown;
use crate::ollama::api::Conn;
use crate::ollama::chat::{self, Event, Msg, Request};
use crate::{hub, models, prefs, rt, store, tools, util, widgets, window};

const RANDOM: &str = "Random";

/// Elo ratings from votes, best first, with wins, losses and ties. A tie
/// counts half a win each; "both bad" is a tie that also costs each a few points.
pub fn elo(votes: &[(String, String, String)]) -> Vec<(String, f64, u32, u32, u32)> {
    const K: f64 = 32.0;
    let mut r: HashMap<String, (f64, u32, u32, u32)> = HashMap::new();
    for (a, b, winner) in votes {
        let ra = r.entry(a.clone()).or_insert((1000.0, 0, 0, 0)).0;
        let rb = r.entry(b.clone()).or_insert((1000.0, 0, 0, 0)).0;
        let ea = 1.0 / (1.0 + 10f64.powf((rb - ra) / 400.0));
        let sa = match winner.as_str() {
            "a" => 1.0,
            "b" => 0.0,
            _ => 0.5,
        };
        let penalty = if winner == "bad" { 8.0 } else { 0.0 };
        let na = ra + K * (sa - ea) - penalty;
        let nb = rb + K * ((1.0 - sa) - (1.0 - ea)) - penalty;
        let ea_ = r.get_mut(a).expect("inserted");
        ea_.0 = na;
        match winner.as_str() {
            "a" => ea_.1 += 1,
            "b" => ea_.2 += 1,
            _ => ea_.3 += 1,
        }
        let eb = r.get_mut(b).expect("inserted");
        eb.0 = nb;
        match winner.as_str() {
            "b" => eb.1 += 1,
            "a" => eb.2 += 1,
            _ => eb.3 += 1,
        }
    }
    let mut out: Vec<(String, f64, u32, u32, u32)> = r.into_iter().map(|(m, (e, w, l, t))| (m, e, w, l, t)).collect();
    out.sort_by(|x, y| y.1.total_cmp(&x.1));
    out
}

struct Round {
    prompt: String,
    a: String,
    b: String,
    done: Cell<u8>,
    voted: Cell<bool>,
}

/// The leaderboard and the last rounds, redrawn after every vote.
struct Boards {
    board_count: gtk::Label,
    board: gtk::Box,
    rounds_head: gtk::Label,
    rounds: gtk::Box,
}

impl Boards {
    fn fill(&self) {
        draw_board(&self.board, &self.board_count);
        draw_rounds(&self.rounds, &self.rounds_head);
    }
}

pub fn build() -> gtk::Widget {
    let root = widgets::vbox(0);
    root.add_css_class("arena-page");
    let (bar, _, end) = widgets::page_bar("Arena", "Two hidden models answer; you pick the better one");
    let new_round = widgets::icon_label_button("view-refresh-symbolic", "New round");
    new_round.set_tooltip_text(Some("Ask a new question"));
    end.append(&new_round);
    root.append(&bar);

    let cols = widgets::hbox(0);
    cols.set_vexpand(true);
    root.append(&cols);
    let body = widgets::vbox(14);
    body.add_css_class("arena-body");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .hexpand(true)
        .child(&body)
        .build();
    cols.append(&scroll);
    let line = nyx_ui::frame::pane_line(gtk::Orientation::Vertical);
    cols.append(&line);
    let side = widgets::vbox(0);
    side.add_css_class("arena-side");
    cols.append(&side);

    // ----- A new round: the question and, if wanted, the two models -----
    let ask = widgets::vbox(10);
    ask.add_css_class("arena-ask");
    let (sw, prompt) = widgets::text_area("", false, 64);
    let prompt_buf = prompt.buffer();
    let ph = widgets::label("Ask both models the same question…", "arena-placeholder");
    ph.set_can_target(false);
    ph.set_halign(gtk::Align::Start);
    ph.set_valign(gtk::Align::Start);
    let tover = gtk::Overlay::new();
    tover.set_child(Some(&sw));
    tover.add_overlay(&ph);
    let ph2 = ph.clone();
    prompt_buf.connect_changed(move |b| ph2.set_visible(b.char_count() == 0));
    ask.append(&tover);
    let pickers = widgets::hbox(8);
    pickers.add_css_class("arena-pickers");
    let pick_a = gtk::DropDown::from_strings(&[]);
    let pick_b = gtk::DropDown::from_strings(&[]);
    widgets::ellipsize_to(&pick_a, 18);
    widgets::ellipsize_to(&pick_b, 18);
    for (label, dd) in [("Model A", &pick_a), ("Model B", &pick_b)] {
        pickers.append(&widgets::label(label, "field-label"));
        pickers.append(dd);
    }
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    pickers.append(&spacer);
    let start = gtk::Button::with_label("Start round");
    start.add_css_class("suggested-action");
    pickers.append(&start);
    ask.append(&pickers);
    let note = widgets::label("Leave both on Random to keep it blind. Names show after you vote.", "arena-note");
    note.set_xalign(0.0);
    note.set_wrap(true);
    ask.append(&note);
    body.append(&ask);
    let fill = {
        let (a, b) = (pick_a.clone(), pick_b.clone());
        move || {
            let mut names = vec![RANDOM.to_string()];
            names.extend(models::chat_models());
            let labels: Vec<String> = names
                .iter()
                .map(|m| if m == RANDOM { m.clone() } else { util::clip_middle(&util::model_label(m), 26) })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            a.set_model(Some(&gtk::StringList::new(&refs)));
            b.set_model(Some(&gtk::StringList::new(&refs)));
        }
    };
    fill();
    hub::on(&hub::MODELS, &pickers, fill);

    // ----- The round: the question, two hidden answers, the vote -----
    let question = widgets::hbox(10);
    question.add_css_class("arena-question");
    question.append(&gtk::Image::from_icon_name("chat-message-new-symbolic"));
    let question_text = widgets::label("", "");
    question_text.set_wrap(true);
    question_text.set_xalign(0.0);
    question_text.set_selectable(true);
    question.append(&question_text);
    question.set_visible(false);
    body.append(&question);
    let arena = widgets::hbox(12);
    arena.set_homogeneous(true);
    arena.add_css_class("arena-row");
    arena.set_visible(false);
    body.append(&arena);
    let votes = widgets::hbox(8);
    votes.set_halign(gtk::Align::Center);
    votes.add_css_class("arena-votes");
    votes.set_visible(false);
    body.append(&votes);

    let rounds_head = widgets::label("Your last rounds", "arena-sechead");
    rounds_head.set_xalign(0.0);
    body.append(&rounds_head);
    let rounds = widgets::vbox(0);
    rounds.add_css_class("arena-rounds");
    body.append(&rounds);

    // ----- The leaderboard, beside the round (under it in narrow windows) -----
    let board_box = widgets::vbox(0);
    board_box.add_css_class("arena-board");
    let head = widgets::hbox(8);
    head.add_css_class("arena-board-head");
    head.append(&widgets::label("Leaderboard", "arena-board-title"));
    let board_count = widgets::label("", "arena-board-count");
    board_count.set_hexpand(true);
    board_count.set_xalign(0.0);
    head.append(&board_count);
    board_box.append(&head);
    let board = widgets::vbox(0);
    board_box.append(&board);
    let boards = Rc::new(Boards { board_count, board, rounds_head, rounds });
    let b2 = boards.clone();
    let clear = widgets::two_click("Reset leaderboard", "Click again to clear every vote", move || {
        let _ = store::clear_votes();
        b2.fill();
    });
    clear.remove_css_class("destructive-action");
    clear.add_css_class("flat");
    clear.add_css_class("danger-text");
    clear.add_css_class("arena-reset");
    clear.set_halign(gtk::Align::Start);
    board_box.append(&clear);
    side.append(&board_box);
    boards.fill();
    crate::window::on_narrow({
        let arena = arena.clone();
        move |n| arena.set_orientation(if n { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal })
    });
    // The leaderboard needs 280 px beside the round; under 1200 px it goes under it.
    crate::window::on_roomy({
        let (side, line, body, board_box) = (side.clone(), line.clone(), body.clone(), board_box.clone());
        move |roomy| {
            let n = !roomy;
            let target = if n { &body } else { &side };
            if board_box.parent().as_ref() != Some(target.upcast_ref::<gtk::Widget>()) {
                if let Some(p) = board_box.parent().and_downcast::<gtk::Box>() {
                    p.remove(&board_box);
                }
                target.append(&board_box);
            }
            side.set_visible(!n);
            line.set_visible(!n);
        }
    });

    let round: Rc<RefCell<Option<Rc<Round>>>> = Rc::new(RefCell::new(None));
    {
        let (ask, question, arena, votes, prompt, round) =
            (ask.clone(), question.clone(), arena.clone(), votes.clone(), prompt.clone(), round.clone());
        new_round.connect_clicked(move |_| {
            *round.borrow_mut() = None;
            widgets::clear(&arena);
            widgets::clear(&votes);
            for w in [question.upcast_ref::<gtk::Widget>(), arena.upcast_ref(), votes.upcast_ref()] {
                w.set_visible(false);
            }
            prompt.buffer().set_text("");
            ask.set_visible(true);
            prompt.grab_focus();
        });
    }
    let (pa, pb, arena2, votes2, round2, fb) = (pick_a.clone(), pick_b.clone(), arena.clone(), votes.clone(), round.clone(), boards.clone());
    let (ask2, question2, question_text2) = (ask.clone(), question.clone(), question_text.clone());
    start.connect_clicked(move |_| {
        let text = widgets::buffer_text(&prompt).trim().to_string();
        if text.is_empty() {
            prompt.grab_focus();
            return;
        }
        let names = models::chat_models();
        if names.len() < 2 {
            window::toast("The arena needs at least two chat models");
            return;
        }
        let chosen = |dd: &gtk::DropDown| -> Option<String> {
            let i = dd.selected() as usize;
            if i == 0 { None } else { names.get(i - 1).cloned() }
        };
        let (mut a, mut b) = (chosen(&pa), chosen(&pb));
        let seed = util::now() as usize ^ (text.len() * 7919);
        if a.is_none() {
            let pool: Vec<&String> = names.iter().filter(|n| Some(*n) != b.as_ref()).collect();
            a = pool.get(seed % pool.len()).map(|s| s.to_string());
        }
        if b.is_none() {
            let pool: Vec<&String> = names.iter().filter(|n| Some(*n) != a.as_ref()).collect();
            b = pool.get((seed / 3 + 1) % pool.len()).map(|s| s.to_string());
        }
        let (Some(a), Some(b)) = (a, b) else { return };
        if a == b {
            window::toast("Pick two different models");
            return;
        }
        let r = Rc::new(Round { prompt: text.clone(), a: a.clone(), b: b.clone(), done: Cell::new(0), voted: Cell::new(false) });
        *round2.borrow_mut() = Some(r.clone());
        widgets::clear(&arena2);
        widgets::clear(&votes2);
        votes2.set_visible(false);
        ask2.set_visible(false);
        question_text2.set_text(&text);
        question2.set_visible(true);
        arena2.set_visible(true);
        let cols = [("Model A", a), ("Model B", b)].map(|(label, model)| {
            let col = widgets::vbox(10);
            col.add_css_class("arena-answer");
            let head = widgets::hbox(8);
            head.append(&widgets::label(label, "arena-answer-name"));
            let name = widgets::short_label("hidden until you vote", "arena-answer-model", gtk::pango::EllipsizeMode::End);
            name.set_tooltip_text(None);
            head.append(&name);
            col.append(&head);
            let md = Markdown::new();
            col.append(&md.root);
            arena2.append(&col);
            (model, name, md)
        });
        let names_labels: Vec<gtk::Label> = cols.iter().map(|c| c.1.clone()).collect();
        if prefs::get().host().backend == prefs::Backend::LlamaCpp {
            let [(a, _, a_md), (b, _, b_md)] = cols;
            let (r2, votes3, fb2, labels, prompt) = (r.clone(), votes2.clone(), fb.clone(), names_labels.clone(), text.clone());
            run_one(&a, &text, a_md, move || {
                r2.done.set(1);
                let (r3, votes4, fb3, labels2) = (r2.clone(), votes3.clone(), fb2.clone(), labels.clone());
                run_one(&b, &prompt, b_md.clone(), move || {
                    r3.done.set(2);
                    show_votes(&votes4, &r3, &labels2, fb3.clone());
                });
            });
        } else {
            for (model, _, md) in cols {
                let (r2, votes3, fb2, labels) = (r.clone(), votes2.clone(), fb.clone(), names_labels.clone());
                run_one(&model, &text, md, move || {
                    r2.done.set(r2.done.get() + 1);
                    if r2.done.get() == 2 { show_votes(&votes3, &r2, &labels, fb2.clone()); }
                });
            }
        }
    });
    // Developer aid: CHAT_SNAPSHOT_ARENA="prompt" plays a round on its own.
    if let Ok(text) = std::env::var("CHAT_SNAPSHOT_ARENA") {
        let s2 = start.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            prompt_buf.set_text(&text);
            s2.emit_clicked();
        });
    }
    root.upcast()
}

fn run_one(model: &str, prompt: &str, md: Rc<Markdown>, done: impl Fn() + 'static) {
    let pr = prefs::get();
    let settings = prefs::for_model(model);
    let local_llama = crate::llama::api::local_service();
    let think = models::has(model, "thinking").then_some(false);
    let req = Request {
        conn: Conn::current(),
        model: model.to_string(),
        messages: vec![Msg::new("user", prompt)],
        tools: vec![],
        think,
        format: None,
        options: if local_llama { json!({ "temperature": settings.temperature, "top_p": settings.top_p,
            "top_k": settings.top_k, "min_p": settings.min_p, "repeat_penalty": settings.repeat_penalty,
            "num_ctx": settings.num_ctx }) } else { json!({ "temperature": pr.temperature, "num_ctx": pr.num_ctx }) },
        keep_alive: models::keep_alive_for(model),
        max_rounds: 1,
        tool_ctx: tools::Ctx { prefs: pr },
    };
    let text = Rc::new(RefCell::new(String::new()));
    let cancel = Arc::new(AtomicBool::new(false));
    rt::stream(
        move |tx| chat::run(req, tx, cancel),
        move |ev| match ev {
            Event::Delta { content, .. } => {
                text.borrow_mut().push_str(&content);
                md.set(&text.borrow());
            }
            Event::Error(e) => md.set(&format!("{}\n\n*Error: {e}*", text.borrow())),
            Event::Done => done(),
            _ => {}
        },
    );
}

fn show_votes(votes: &gtk::Box, r: &Rc<Round>, labels: &[gtk::Label], boards: Rc<Boards>) {
    votes.set_visible(true);
    let choices = [
        ("a", "A is better", Some("go-previous-symbolic")),
        ("tie", "It's a tie", None),
        ("bad", "Both are bad", None),
        ("b", "B is better", Some("go-next-symbolic")),
    ];
    for (id, text, icon) in choices {
        let b = match icon {
            Some(i) => widgets::icon_label_button(i, text),
            None => gtk::Button::with_label(text),
        };
        b.add_css_class("arena-vote");
        if id == "bad" {
            b.add_css_class("danger-text");
        }
        let (r2, labels2, votes2, boards2) = (r.clone(), labels.to_vec(), votes.clone(), boards.clone());
        b.connect_clicked(move |_| {
            if r2.voted.replace(true) {
                return;
            }
            let _ = store::add_vote(&r2.prompt, &r2.a, &r2.b, id);
            for (l, m) in labels2.iter().zip([&r2.a, &r2.b]) {
                l.set_text(&util::model_label(m));
                l.set_tooltip_text(Some(m));
                l.add_css_class("revealed");
            }
            widgets::clear(&votes2);
            votes2.append(&widgets::label("Thanks. The leaderboard is updated.", "dim"));
            boards2.fill();
        });
        votes.append(&b);
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn draw_board(b: &gtk::Box, count: &gtk::Label) {
    widgets::clear(b);
    let votes = store::votes().unwrap_or_default();
    count.set_text(&if votes.is_empty() { String::new() } else { plural(votes.len(), "vote") });
    if votes.is_empty() {
        let e = widgets::empty("No votes yet. Play a round to start the board.");
        e.add_css_class("arena-board-empty");
        b.append(&e);
        return;
    }
    for (i, (model, rating, w, l, t)) in elo(&votes).into_iter().enumerate() {
        let row = widgets::hbox(10);
        row.add_css_class("board-row");
        let rank = widgets::label(&format!("{}", i + 1), "board-rank");
        rank.add_css_class("tnum");
        if i == 0 {
            rank.add_css_class("first");
        }
        rank.set_valign(gtk::Align::Center);
        row.append(&rank);
        let tx = widgets::vbox(1);
        tx.set_hexpand(true);
        let name = widgets::label(&util::model_label(&model), "board-model");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_tooltip_text(Some(&model));
        tx.append(&name);
        let rec = widgets::label(&format!("{w}–{l}–{t} wins, losses, ties"), "board-record");
        rec.set_xalign(0.0);
        rec.add_css_class("tnum");
        tx.append(&rec);
        row.append(&tx);
        let r = widgets::label(&format!("{rating:.0}"), "board-score");
        r.add_css_class("tnum");
        r.set_tooltip_text(Some("Rating: everyone starts at 1000"));
        r.set_valign(gtk::Align::Center);
        row.append(&r);
        b.append(&row);
    }
}

/// The newest rounds: the question and who won.
fn draw_rounds(b: &gtk::Box, head: &gtk::Label) {
    widgets::clear(b);
    let rounds = store::recent_votes(5).unwrap_or_default();
    head.set_visible(!rounds.is_empty());
    b.set_visible(!rounds.is_empty());
    for (prompt, a, m_b, winner) in rounds {
        let row = widgets::hbox(10);
        row.add_css_class("round-row");
        let decided = matches!(winner.as_str(), "a" | "b");
        let icon = gtk::Image::from_icon_name(if decided { "object-select-symbolic" } else { "view-dual-symbolic" });
        icon.add_css_class(if decided { "round-won" } else { "round-even" });
        row.append(&icon);
        let q = widgets::label(prompt.lines().next().unwrap_or(""), "round-question");
        q.set_hexpand(true);
        q.set_xalign(0.0);
        q.set_ellipsize(gtk::pango::EllipsizeMode::End);
        q.set_tooltip_text(Some(&prompt));
        row.append(&q);
        let (win, lose) = if winner == "b" { (&m_b, &a) } else { (&a, &m_b) };
        let short = |m: &str| util::esc(&util::clip_middle(&util::model_label(m), 22));
        let result = gtk::Label::new(None);
        result.add_css_class("round-result");
        match winner.as_str() {
            "a" | "b" => result.set_markup(&format!("<b>{}</b> beat {}", short(win), short(lose))),
            "bad" => result.set_text("Both were bad"),
            _ => result.set_text("It's a tie"),
        }
        result.set_tooltip_text(Some(&format!("{} and {}", a, m_b)));
        result.set_ellipsize(gtk::pango::EllipsizeMode::End);
        result.set_max_width_chars(48);
        row.append(&result);
        b.append(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &str, b: &str, w: &str) -> (String, String, String) {
        (a.into(), b.into(), w.into())
    }

    #[test]
    fn winners_rise() {
        let board = elo(&[v("x", "y", "a"), v("x", "y", "a"), v("y", "z", "tie")]);
        assert_eq!(board[0].0, "x");
        assert!(board[0].1 > 1000.0);
        let x = board.iter().find(|r| r.0 == "x").unwrap();
        assert_eq!((x.2, x.3, x.4), (2, 0, 0));
        let y = board.iter().find(|r| r.0 == "y").unwrap();
        assert_eq!((y.2, y.3, y.4), (0, 2, 1));
    }

    #[test]
    fn ties_between_equals_change_nothing() {
        let board = elo(&[v("x", "y", "tie")]);
        assert!(board.iter().all(|r| (r.1 - 1000.0).abs() < 1e-9));
    }
}
