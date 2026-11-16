//! Speech: dictation and reading aloud through Nyx Voice (`nyx-voiced`), NyxOS's local
//! speech service, when it's there. Without it, whisper.cpp and Piper, if set up in
//! Settings → Voice, with PipeWire recording and playing the audio.

use anyhow::{Context, Result, bail};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::{cmd, paths, prefs, window};
use nyx_voice_client::proto::{Mode, Until};

/// A dictation take in progress.
enum Take {
    /// pw-record writing a WAV for whisper.cpp.
    Local(Child, PathBuf),
    /// Nyx Voice listening until told to stop; its transcript arrives here.
    Voice(std::sync::mpsc::Receiver<Result<String>>),
}

/// What's reading aloud.
#[derive(Clone, Copy, PartialEq)]
enum Speech {
    /// Piper and pw-play's process group.
    Local(u32),
    /// A Nyx Voice `speak`, by its turn.
    Voice(u64),
}

thread_local! {
    static RECORDER: RefCell<Option<Take>> = const { RefCell::new(None) };
    static SPEAKER: RefCell<Option<Speech>> = const { RefCell::new(None) };
    static TURN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Nyx Voice, when its daemon is listening on this session.
fn nyx_voice() -> Option<nyx_voice_client::Client> {
    let client = nyx_voice_client::Client::new("nyx-chat");
    client.available().then_some(client)
}

/// Run one Nyx Voice call to completion on this (worker) thread.
fn voice_call<T>(f: impl std::future::Future<Output = nyx_voice_client::Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(f).map_err(|e| anyhow::anyhow!("{e}"))
}

fn bin(configured: &str, fallbacks: &[&str]) -> Option<String> {
    let c = configured.trim();
    if !c.is_empty() && cmd::present(&paths::expand(c).to_string_lossy()) {
        return Some(paths::expand(c).to_string_lossy().into());
    }
    fallbacks.iter().find(|b| cmd::present(b)).map(|b| b.to_string())
}

/// Why dictation can't run, if it can't.
pub fn dictation_problem() -> Option<String> {
    if nyx_voice().is_some() {
        return None;
    }
    let p = prefs::get();
    let none = "Dictation uses Nyx Voice, which isn't running (it comes with Nyx Assistant), or whisper.cpp with a model chosen in Settings → Voice.";
    if bin(&p.whisper_bin, &["whisper-cli", "whisper-cpp", "whisper"]).is_none() && p.whisper_model.trim().is_empty() {
        return Some(none.into());
    }
    if !cmd::present("pw-record") {
        return Some("Recording needs PipeWire's pw-record.".into());
    }
    if bin(&p.whisper_bin, &["whisper-cli", "whisper-cpp", "whisper"]).is_none() {
        return Some("Dictation needs whisper.cpp (the whisper-cli program).".into());
    }
    if p.whisper_model.trim().is_empty() || !paths::expand(&p.whisper_model).exists() {
        return Some("Choose a whisper model file (ggml-*.bin) in Settings → Voice.".into());
    }
    None
}

/// Why reading aloud can't run, if it can't.
pub fn speech_problem() -> Option<String> {
    if nyx_voice().is_some() {
        return None;
    }
    let p = prefs::get();
    if bin(&p.piper_bin, &["piper", "piper-tts"]).is_none() && p.piper_voice.trim().is_empty() {
        return Some("Reading aloud uses Nyx Voice, which isn't running (it comes with Nyx Assistant), or Piper with a voice chosen in Settings → Voice.".into());
    }
    if !cmd::present("pw-play") {
        return Some("Playback needs PipeWire's pw-play.".into());
    }
    if bin(&p.piper_bin, &["piper", "piper-tts"]).is_none() {
        return Some("Reading aloud needs Piper (the piper program).".into());
    }
    if p.piper_voice.trim().is_empty() || !paths::expand(&p.piper_voice).exists() {
        return Some("Choose a Piper voice (.onnx) in Settings → Voice.".into());
    }
    None
}

pub fn recording() -> bool {
    RECORDER.with(|r| r.borrow().is_some())
}

pub fn start_recording() -> Result<()> {
    if let Some(problem) = dictation_problem() {
        bail!("{problem}");
    }
    if let Some(client) = nyx_voice() {
        // Push to talk: the take runs until stop_and_transcribe says stop.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("dictation".into()).spawn(move || {
            let _ = tx.send(voice_call(client.listen_text(Mode::Dictation, Until::Stop)));
        })?;
        RECORDER.with(|r| *r.borrow_mut() = Some(Take::Voice(rx)));
        return Ok(());
    }
    let dir = paths::cache_dir().join("voice");
    std::fs::create_dir_all(&dir)?;
    let wav = dir.join("dictation.wav");
    let child = Command::new("pw-record")
        .args(["--rate", "16000", "--channels", "1", "--format", "s16"])
        .arg(&wav)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't start pw-record")?;
    RECORDER.with(|r| *r.borrow_mut() = Some(Take::Local(child, wav)));
    Ok(())
}

pub fn stop_and_transcribe(done: impl FnOnce(Result<String>) + 'static) {
    let (mut child, wav) = match RECORDER.with(|r| r.borrow_mut().take()) {
        Some(Take::Local(child, wav)) => (child, wav),
        Some(Take::Voice(transcript)) => {
            cmd::background(
                move || -> Result<String> {
                    if let Some(client) = nyx_voice() {
                        voice_call(client.stop_listening())?;
                    }
                    let text = transcript
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .map_err(|_| anyhow::anyhow!("Nyx Voice didn't send the text"))??;
                    Ok(text.trim().to_string())
                },
                done,
            );
            return;
        }
        None => {
            done(Err(anyhow::anyhow!("not recording")));
            return;
        }
    };
    let p = prefs::get();
    let whisper = bin(&p.whisper_bin, &["whisper-cli", "whisper-cpp", "whisper"]).unwrap_or_default();
    let model = paths::expand(&p.whisper_model);
    cmd::background(
        move || -> Result<String> {
            // SIGINT lets pw-record finish the WAV header.
            // SAFETY: plain syscall on our own child's pid.
            unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
            let _ = child.wait();
            let out = Command::new(&whisper)
                .args(["-m"])
                .arg(&model)
                .args(["-f"])
                .arg(&wav)
                .args(["-nt", "-np", "-l", "auto"])
                .stdin(Stdio::null())
                .output()
                .context("couldn't run whisper")?;
            if !out.status.success() {
                bail!("whisper failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
            }
            Ok(clean_transcript(&String::from_utf8_lossy(&out.stdout)))
        },
        done,
    );
}

/// whisper marks silence and noise as `[BLANK_AUDIO]`, `(music)` and so on.
fn clean_transcript(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn speaking() -> bool {
    SPEAKER.with(|s| s.borrow().is_some())
}

pub fn stop_speaking() {
    match SPEAKER.with(|s| s.borrow_mut().take()) {
        Some(Speech::Local(pid)) => {
            // SAFETY: plain syscall; the group was created for this playback only.
            unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
        }
        Some(Speech::Voice(_)) => cmd::background(|| nyx_voice().map(|client| voice_call(client.stop_speaking())), |_| {}),
        None => {}
    }
}

/// Read `markdown` aloud (code blocks are skipped). Speaking again stops the last.
pub fn speak(markdown: &str) {
    if speaking() {
        stop_speaking();
        return;
    }
    if let Some(problem) = speech_problem() {
        window::toast(&problem);
        return;
    }
    let text = crate::markdown::plain(markdown);
    if text.is_empty() {
        return;
    }
    if let Some(client) = nyx_voice() {
        let turn = TURN.with(|t| {
            t.set(t.get() + 1);
            t.get()
        });
        SPEAKER.with(|s| *s.borrow_mut() = Some(Speech::Voice(turn)));
        let text: String = text.chars().take(nyx_voice_client::proto::MAX_SPEAK).collect();
        cmd::background(
            move || voice_call(client.speak(&text)),
            move |result| {
                SPEAKER.with(|s| {
                    if *s.borrow() == Some(Speech::Voice(turn)) {
                        *s.borrow_mut() = None;
                    }
                });
                if let Err(e) = result {
                    window::toast(&format!("Couldn't read it aloud: {e}"));
                }
            },
        );
        return;
    }
    let p = prefs::get();
    let piper = bin(&p.piper_bin, &["piper", "piper-tts"]).unwrap_or_default();
    let voice = paths::expand(&p.piper_voice);
    let dir = paths::cache_dir().join("voice");
    let _ = std::fs::create_dir_all(&dir);
    let wav = dir.join("speech.wav");
    // One process group runs Piper then pw-play, so Stop ends both.
    let script = format!(
        "{} --model {} --output_file {} && pw-play {}",
        shell_quote(&piper),
        shell_quote(&voice.to_string_lossy()),
        shell_quote(&wav.to_string_lossy()),
        shell_quote(&wav.to_string_lossy())
    );
    use std::os::unix::process::CommandExt;
    let child = Command::new("bash")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = child else {
        window::toast("Couldn't start Piper");
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let pid = child.id();
    SPEAKER.with(|s| *s.borrow_mut() = Some(Speech::Local(pid)));
    cmd::background(
        move || child.wait().map(|s| s.success()).unwrap_or(false),
        move |_| {
            SPEAKER.with(|s| {
                if *s.borrow() == Some(Speech::Local(pid)) {
                    *s.borrow_mut() = None;
                }
            });
        },
    );
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcripts_drop_noise_markers() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO]\n Hello there. (music) How are you?\n"), "Hello there. How are you?");
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
