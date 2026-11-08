//! The llama.cpp host on this computer is Nyx AI Core, the suite's shared model
//! runtime. It owns loading, downloads and load-time settings; Chat keeps its
//! chats and sends each request through `nyx-ai-client`.

use anyhow::{Result, anyhow, bail};
use nyx_ai_client::proto::{
    self, Capability, ChatRequest, Content, Event as CoreEvent, FunctionCall, ImageUrl, Message, ModelState, Part, Priority,
    ResponseFormat, Role, Sampling, Tool, ToolCall as CoreCall, ToolFunction,
};
use nyx_ai_client::{ChatOutcome, Client, Item};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::ollama::api::{Conn, Installed, ModelInfo, Progress, Running};
use crate::ollama::chat::{Cancel, Event, Msg, Request, Stats, ToolCall};
use crate::prefs::{Backend, ModelSettings};

/// The address the local host has always had in Chat's settings.
pub const URL: &str = "http://127.0.0.1:8080";

pub fn local(conn: &Conn) -> bool {
    conn.backend == Backend::LlamaCpp && conn.url == URL
}

fn client() -> Client {
    Client::new("nyx-chat")
}

fn fail(e: proto::Error) -> anyhow::Error {
    if e.kind == proto::ErrorKind::Unavailable {
        anyhow!("The model server isn't reachable. Nyx AI Core isn't running; start it in Settings.")
    } else {
        anyhow!("{}", e.message)
    }
}

pub async fn version() -> Result<String> {
    let caps = client().capabilities().await.map_err(fail)?;
    Ok(match caps.worker_release {
        Some(release) => format!("Nyx AI Core {} (llama.cpp {release})", caps.server_version),
        None => format!("Nyx AI Core {}", caps.server_version),
    })
}

fn usable(m: &proto::Model) -> bool {
    !matches!(m.state, ModelState::Missing | ModelState::Downloading)
}

pub async fn tags() -> Result<Vec<Installed>> {
    let models = client().list_models().await.map_err(fail)?;
    Ok(models
        .into_iter()
        .filter(usable)
        .map(|m| Installed {
            name: m.id,
            size: m.size,
            family: m.architecture.unwrap_or_default(),
            parameter_size: m.parameters.unwrap_or_default(),
            quantization: m.quantization.unwrap_or_default(),
            modified: String::new(),
            digest: String::new(),
        })
        .collect())
}

pub async fn running() -> Result<Vec<Running>> {
    let models = client().list_models().await.map_err(fail)?;
    Ok(models
        .into_iter()
        .filter(|m| matches!(m.state, ModelState::Ready | ModelState::Busy))
        .map(|m| Running { name: m.id, size: m.size, size_vram: 0, expires: String::new() })
        .collect())
}

/// AI Core's capabilities by the names the rest of Chat uses.
fn capability_names(caps: &[Capability], efforts: &[String]) -> Vec<String> {
    let mut names: Vec<String> = caps
        .iter()
        .map(|c| match c {
            Capability::Chat => "completion",
            Capability::Tools => "tools",
            Capability::Thinking => "thinking",
            Capability::Vision => "vision",
            Capability::Audio => "audio",
            Capability::Embedding => "embedding",
            Capability::Draft => "draft",
        })
        .map(String::from)
        .collect();
    if !efforts.is_empty() {
        names.push("reasoning_effort".into());
    }
    names
}

pub async fn show(model: &str) -> Result<ModelInfo> {
    let d = client().model_info(model).await.map_err(fail)?;
    Ok(ModelInfo {
        capabilities: capability_names(&d.model.capabilities, &d.reasoning_efforts),
        reasoning_efforts: d.reasoning_efforts,
        context_length: d.model.context_length,
        parameters: String::new(),
        template: d.template.unwrap_or_default(),
        license: d.license.unwrap_or_default(),
        system: String::new(),
    })
}

pub async fn delete(model: &str) -> Result<()> {
    client().remove_model(model).await.map_err(fail)
}

pub async fn unload(model: &str) -> Result<()> {
    client().unload_model(model).await.map(|_| ()).map_err(fail)
}

/// Load a model; `keep` holds it loaded for Chat until it's unloaded.
pub async fn preload(model: &str, keep: bool) -> Result<()> {
    client().load_model(model, keep).await.map_err(fail)
}

pub async fn embed(model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
    let e = client().embed(model, inputs.to_vec(), Priority::Normal).await.map_err(fail)?;
    if e.vectors.len() != inputs.len() {
        bail!("expected {} embeddings, got {}", inputs.len(), e.vectors.len());
    }
    Ok(e.vectors)
}

pub async fn complete(model: &str, messages: Value) -> Result<String> {
    let messages = messages
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| Ok(Message::text(role(m["role"].as_str().unwrap_or("user"))?, m["content"].as_str().unwrap_or(""))))
        .collect::<Result<Vec<_>>>()?;
    let request = ChatRequest { model: model.into(), messages, thinking: Some(false), ..Default::default() };
    let out = client().chat(request).await.map_err(fail)?.collect_chat().await.map_err(fail)?;
    Ok(crate::ollama::chat::strip_thinking(&out.content).trim().into())
}

pub async fn pull(model: &str, tx: async_channel::Sender<Progress>) -> Result<()> {
    let mut stream = client().download_model(model).await.map_err(fail)?;
    while let Some(item) = stream.next().await {
        match item.map_err(fail)? {
            Item::Event(CoreEvent::Progress { status, completed, total }) => {
                let _ = tx.send(Progress { status, completed, total }).await;
            }
            Item::Done(_) => return Ok(()),
            Item::Event(_) => {}
        }
    }
    bail!("Nyx AI Core stopped reporting the download")
}

/// Ask Hugging Face now; the models with updates.
pub async fn check_updates() -> Result<Vec<String>> {
    Ok(client().check_updates().await.map_err(fail)?.into_iter().map(|m| m.id).collect())
}

/// The model Nyx Settings set as the default for new chats.
pub async fn default_chat_model() -> Result<Option<String>> {
    Ok(client().service_settings().await.map_err(fail)?.defaults.get("chat").cloned())
}

/// Models AI Core already knows have updates (it checks daily).
pub async fn known_updates() -> Result<Vec<String>> {
    Ok(client().list_models().await.map_err(fail)?.into_iter().filter(|m| m.update_available).map(|m| m.id).collect())
}

pub async fn cancel_pull(model: &str) -> Result<()> {
    client().cancel_download(model).await.map_err(fail)
}

fn role(name: &str) -> Result<Role> {
    Ok(match name {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        other => bail!("unknown message role {other}"),
    })
}

fn image_url(image: &str) -> String {
    let mime = if image.starts_with("/9j/") {
        "image/jpeg"
    } else if image.starts_with("UklGR") {
        "image/webp"
    } else if image.starts_with("R0lGOD") {
        "image/gif"
    } else {
        "image/png"
    };
    format!("data:{mime};base64,{image}")
}

/// Chat's messages as AI Core takes them: tool results answer the call with
/// the same tool name, as the OpenAI format wants.
fn messages(items: &[Msg]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    for (i, m) in items.iter().enumerate() {
        if m.role == "tool" {
            let id = pending.iter().position(|(_, name)| *name == m.tool_name).map(|pos| pending.remove(pos).0).unwrap_or_else(|| format!("call_{i}"));
            out.push(Message { role: Role::Tool, content: Content::Text(m.content.clone()), tool_calls: Vec::new(), tool_call_id: Some(id) });
            continue;
        }
        let content = if m.images.is_empty() {
            Content::Text(m.content.clone())
        } else {
            let mut parts = vec![Part::Text { text: m.content.clone() }];
            parts.extend(m.images.iter().map(|image| Part::ImageUrl { image_url: ImageUrl { url: image_url(image) } }));
            Content::Parts(parts)
        };
        let tool_calls = m
            .tool_calls
            .iter()
            .enumerate()
            .map(|(n, c)| {
                let id = format!("call_{i}_{n}");
                pending.push((id.clone(), c.name.clone()));
                CoreCall { id, kind: "function".into(), function: FunctionCall { name: c.name.clone(), arguments: c.arguments.to_string() } }
            })
            .collect();
        out.push(Message { role: role(&m.role)?, content, tool_calls, tool_call_id: None });
    }
    Ok(out)
}

fn tools(defs: &[Value]) -> Vec<Tool> {
    defs.iter()
        .filter_map(|d| {
            let f = &d["function"];
            Some(Tool {
                kind: "function".into(),
                function: ToolFunction {
                    name: f["name"].as_str()?.to_string(),
                    description: f["description"].as_str().map(String::from),
                    parameters: f.get("parameters").filter(|p| p.is_object()).cloned(),
                    strict: None,
                },
            })
        })
        .collect()
}

pub fn request(req: &Request) -> Result<ChatRequest> {
    let o = &req.options;
    let sampling = Sampling {
        temperature: o["temperature"].as_f64(),
        top_p: o["top_p"].as_f64(),
        top_k: o["top_k"].as_i64(),
        min_p: o["min_p"].as_f64(),
        repeat_penalty: o["repeat_penalty"].as_f64(),
        seed: o["seed"].as_i64(),
        ..Default::default()
    };
    let response_format = req.format.as_ref().map(|f| {
        if f.as_str() == Some("json") { ResponseFormat::JsonObject } else { ResponseFormat::JsonSchema { schema: f.clone() } }
    });
    let reasoning_effort =
        o["reasoning_effort"].as_str().filter(|_| req.think != Some(false)).filter(|l| proto::REASONING_EFFORTS.contains(l)).map(String::from);
    Ok(ChatRequest {
        model: req.model.clone(),
        messages: messages(&req.messages)?,
        tools: tools(&req.tools),
        tool_choice: None,
        response_format,
        sampling,
        reasoning_effort,
        thinking: req.think,
        priority: Priority::Interactive,
    })
}

pub async fn stream_once(req: &Request, tx: &async_channel::Sender<Event>, cancel: &Cancel) -> Result<(String, String, Vec<ToolCall>, Stats)> {
    let mut stream = client().chat(request(req)?).await.map_err(fail)?;
    let mut out = ChatOutcome::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let item = tokio::select! {
            item = stream.next() => item,
            _ = tokio::time::sleep(Duration::from_millis(150)) => continue,
        };
        let Some(item) = item else { break };
        match item.map_err(fail)? {
            Item::Event(CoreEvent::Delta { content, reasoning, tool_calls }) => {
                out.content.push_str(&content);
                out.reasoning.push_str(&reasoning);
                out.add_tool_calls(&tool_calls);
                if !content.is_empty() || !reasoning.is_empty() {
                    let _ = tx.try_send(Event::Delta { content, thinking: reasoning });
                }
            }
            Item::Event(_) => {}
            Item::Done(proto::Reply::Finished(f)) => {
                out.finished = f;
                break;
            }
            Item::Done(_) => bail!("Nyx AI Core ended the chat unexpectedly"),
        }
    }
    let f = &out.finished;
    let stats = Stats {
        tokens: f.usage.completion_tokens,
        tokens_per_sec: f.timings.tokens_per_second,
        prompt_tokens: f.usage.prompt_tokens,
        load_ms: f.timings.load_ms,
        prompt_ms: f.timings.first_token_ms,
        total_ms: f.timings.total_ms,
        done_reason: f.finish_reason.clone(),
        stopped: false,
    };
    let calls = out
        .tool_calls
        .into_iter()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(_, name, args)| ToolCall { name, arguments: serde_json::from_str(&args).unwrap_or(json!({})) })
        .collect();
    Ok((out.content, out.reasoning, calls, stats))
}

/// The load-time settings Chat manages for a model, in AI Core's keys.
pub fn values(s: &ModelSettings) -> BTreeMap<String, String> {
    let flash = if s.kv_cache == "f16" { s.flash_attention.as_str() } else { "on" };
    [
        ("ctx-size", s.num_ctx.to_string()),
        ("n-gpu-layers", s.gpu_layers.clone()),
        ("flash-attn", flash.to_string()),
        ("cache-type-k", s.kv_cache.clone()),
        ("cache-type-v", s.kv_cache.clone()),
        ("temp", s.temperature.to_string()),
        ("top-p", s.top_p.to_string()),
        ("top-k", s.top_k.to_string()),
        ("min-p", s.min_p.to_string()),
        ("repeat-penalty", s.repeat_penalty.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Send what differs from AI Core's settings for `model` (`*` for the
/// defaults). Returns whether they wait for the model's next load.
pub async fn configure(model: &str, settings: &ModelSettings) -> Result<bool> {
    let client = client();
    let current = client.model_settings(model).await.map_err(fail)?;
    let changes: BTreeMap<String, String> = values(settings).into_iter().filter(|(k, v)| current.values.get(k) != Some(v)).collect();
    if changes.is_empty() {
        return Ok(current.pending);
    }
    Ok(client.configure_model(model, changes).await.map_err(fail)?.pending)
}

pub async fn configure_service(max_loaded: u32, idle_seconds: i64) -> Result<()> {
    client().configure_service(Some(max_loaded), Some(idle_seconds)).await.map(|_| ()).map_err(fail)
}

/// The GPU memory AI Core sees, for fit estimates.
pub async fn vram() -> Result<u64> {
    Ok(client().resource_status().await.map_err(fail)?.vram_total)
}

pub fn installed() -> bool {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    crate::cmd::present("nyx-aid") || home.join(".local/bin/nyx-aid").exists() || client().available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_answer_their_calls() {
        let items = vec![
            Msg::new("user", "weather?"),
            Msg { role: "assistant".into(), tool_calls: vec![ToolCall { name: "weather".into(), arguments: json!({"city": "Paris"}) }], ..Default::default() },
            Msg { role: "tool".into(), content: "sunny".into(), tool_name: "weather".into(), ..Default::default() },
            Msg { role: "user".into(), content: "and this?".into(), images: vec!["/9j/abc".into()], ..Default::default() },
        ];
        let out = messages(&items).unwrap();
        assert_eq!(out[1].tool_calls[0].id, "call_1_0");
        assert_eq!(out[1].tool_calls[0].function.arguments, "{\"city\":\"Paris\"}");
        assert_eq!(out[2].tool_call_id.as_deref(), Some("call_1_0"));
        let Content::Parts(parts) = &out[3].content else { panic!() };
        assert!(matches!(&parts[1], Part::ImageUrl { image_url } if image_url.url.starts_with("data:image/jpeg;base64,")));
        let req = ChatRequest { model: "m".into(), messages: out, ..Default::default() };
        req.validate().unwrap();
    }

    #[test]
    fn settings_map_to_ai_core_keys() {
        let v = values(&ModelSettings { kv_cache: "q8_0".into(), ..Default::default() });
        assert_eq!(v["flash-attn"], "on");
        assert_eq!(v["ctx-size"], "8192");
        assert_eq!(v["repeat-penalty"], "1");
        assert_eq!(capability_names(&[Capability::Chat, Capability::Vision], &["low".into()]), ["completion", "vision", "reasoning_effort"]);
    }
}
