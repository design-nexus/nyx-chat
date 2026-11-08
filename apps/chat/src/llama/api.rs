//! llama.cpp hosts: this computer's is Nyx AI Core (`core`); other machines
//! are reached through the router's OpenAI-compatible API.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

use super::core;
use crate::ollama::api::{self, Conn, Installed, ModelInfo, Progress, Running};

async fn list(conn: &Conn, reload: bool) -> Result<Vec<Value>> {
    let path = if reload { "/models?reload=1" } else { "/models" };
    let response = api::check(conn.get(path).timeout(Duration::from_secs(10)).send().await?).await?;
    let body: Value = response.json().await?;
    Ok(body["data"].as_array().context("no model list in llama.cpp reply")?.clone())
}

fn named<'a>(list: &'a [Value], name: &str) -> Option<&'a Value> {
    list.iter().find(|item| item["id"].as_str() == Some(name))
}

pub async fn version(conn: &Conn) -> Result<String> {
    if core::local(conn) {
        return core::version().await;
    }
    api::check(conn.get("/health").timeout(Duration::from_secs(3)).send().await?).await?;
    Ok("llama.cpp".into())
}

pub async fn tags(conn: &Conn) -> Result<Vec<Installed>> {
    if core::local(conn) {
        return core::tags().await;
    }
    let mut models: Vec<Installed> = list(conn, false).await?.iter().filter_map(|item| {
        let name = item["id"].as_str()?.to_string();
        if item["status"]["value"].as_str() == Some("downloading") { return None; }
        if !matches!(item["source"].as_str(), Some("cache" | "preset"))
            && item["path"].as_str().filter(|p| !p.is_empty()).is_none() { return None; }
        let size = item["size"].as_u64().or_else(|| item["path"].as_str().and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()))).unwrap_or(0);
        Some(Installed { name, size, family: item["meta"]["general.architecture"].as_str().unwrap_or("").into(),
            parameter_size: String::new(), quantization: item["meta"]["general.file_type"].as_str().unwrap_or("").into(),
            modified: String::new(), digest: String::new() })
    }).collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

pub async fn running(conn: &Conn) -> Result<Vec<Running>> {
    if core::local(conn) {
        return core::running().await;
    }
    Ok(list(conn, false).await?.iter().filter(|item| item["status"]["value"].as_str() == Some("loaded"))
        .filter_map(|item| Some(Running { name: item["id"].as_str()?.into(),
            size: item["size"].as_u64().unwrap_or(0), size_vram: 0, expires: String::new() })).collect())
}

fn info(item: &Value) -> ModelInfo {
    let mut capabilities = vec![];
    let name = item["id"].as_str().unwrap_or("").to_ascii_lowercase();
    let embedding = name.contains("embed") || name.contains("nomic") || name.contains("bge-");
    let audio_codec = name.contains("codec");
    let input = item["architecture"]["input_modalities"].as_array();
    let output = item["architecture"]["output_modalities"].as_array();
    if input.is_some_and(|a| a.iter().any(|x| x.as_str() == Some("image"))) { capabilities.push("vision".into()); }
    if embedding { capabilities.push("embedding".into()); }
    if audio_codec { capabilities.push("audio".into()); }
    if !embedding && !audio_codec && output.is_some_and(|a| a.iter().any(|x| x.as_str() == Some("text"))) {
        capabilities.extend(["completion".into(), "tools".into(), "thinking".into()]);
    } else if !embedding && !audio_codec { capabilities.push("embedding".into()); }
    ModelInfo { capabilities, reasoning_efforts: Vec::new(), context_length: item["meta"]["n_ctx_train"].as_u64()
        .or(item["meta"]["context_length"].as_u64()), parameters: String::new(),
        template: item["meta"]["tokenizer.chat_template"].as_str().unwrap_or("").into(),
        license: String::new(), system: String::new() }
}

pub async fn show(conn: &Conn, model: &str) -> Result<ModelInfo> {
    if core::local(conn) {
        return core::show(model).await;
    }
    let entries = list(conn, false).await?;
    let item = named(&entries, model).context("model isn't installed")?;
    let mut result = info(item);
    if item["status"]["value"].as_str() == Some("loaded") {
        let url = format!("/props?model={}", crate::util::encode_query(model));
        if let Ok(raw) = conn.get(&url).timeout(Duration::from_secs(5)).send().await
            && let Ok(response) = api::check(raw).await
            && let Ok(props) = response.json::<Value>().await
            && props["chat_template_caps"]["supports_reasoning_effort"].as_bool() == Some(true)
        {
            let checks = crate::llama::reasoning::CANDIDATES.iter().map(|(_, level)| async move {
                let body = json!({"model": model, "messages": [{"role": "user", "content": "hello"}],
                    "chat_template_kwargs": {"enable_thinking": true}, "reasoning_effort": level});
                let response = conn.post("/apply-template").json(&body).timeout(Duration::from_secs(5)).send().await;
                (level, response.is_ok_and(|r| r.status().is_success()))
            });
            result.reasoning_efforts = futures_util::future::join_all(checks).await.into_iter()
                .filter_map(|(level, accepted)| accepted.then_some((*level).to_string())).collect();
            if !result.reasoning_efforts.is_empty() {
                result.capabilities.push("reasoning_effort".into());
            }
        }
    }
    Ok(result)
}

pub async fn delete(conn: &Conn, model: &str) -> Result<()> {
    if core::local(conn) {
        return core::delete(model).await;
    }
    let url = format!("/models?model={}", crate::util::encode_query(model));
    api::check(conn.auth(api::client().delete(conn.url(&url))).send().await?).await?;
    Ok(())
}

pub async fn unload(conn: &Conn, model: &str) -> Result<()> {
    if core::local(conn) {
        return core::unload(model).await;
    }
    api::check(conn.post("/models/unload").json(&json!({"model": model})).send().await?).await?;
    Ok(())
}

/// Load a model; `keep` holds it loaded on this computer until it's unloaded.
pub async fn preload(conn: &Conn, model: &str, keep: bool) -> Result<()> {
    if core::local(conn) {
        return core::preload(model, keep).await;
    }
    api::check(conn.post("/models/load").json(&json!({"model": model})).timeout(Duration::from_secs(300)).send().await?).await?;
    Ok(())
}

pub async fn embed(conn: &Conn, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
    if core::local(conn) {
        return core::embed(model, inputs).await;
    }
    let response = api::check(conn.post("/v1/embeddings").json(&json!({"model": model, "input": inputs}))
        .timeout(Duration::from_secs(300)).send().await?).await?;
    let body: Value = response.json().await?;
    let mut data = body["data"].as_array().context("no embeddings in reply")?.clone();
    data.sort_by_key(|v| v["index"].as_u64().unwrap_or(0));
    let vectors: Vec<Vec<f32>> = data.iter().map(|v| v["embedding"].as_array().into_iter().flatten()
        .filter_map(|n| n.as_f64()).map(|n| n as f32).collect()).collect();
    if vectors.len() != inputs.len() { bail!("expected {} embeddings, got {}", inputs.len(), vectors.len()); }
    Ok(vectors)
}

pub async fn complete(conn: &Conn, model: &str, messages: Value) -> Result<String> {
    if core::local(conn) {
        return core::complete(model, messages).await;
    }
    let response = api::check(conn.post("/v1/chat/completions")
        .json(&json!({"model": model, "messages": messages, "stream": false}))
        .timeout(Duration::from_secs(120)).send().await?).await?;
    let body: Value = response.json().await?;
    Ok(crate::ollama::chat::strip_thinking(body["choices"][0]["message"]["content"].as_str().unwrap_or("")).trim().into())
}

pub async fn pull(conn: &Conn, model: &str, tx: async_channel::Sender<Progress>) -> Result<()> {
    let canonical = model.rsplit_once(':').map(|(repo, tag)| format!("{repo}:{}", crate::hf::quant_tag(tag))).unwrap_or_else(|| model.to_string());
    if core::local(conn) {
        return core::pull(&canonical, tx).await;
    }
    let reply: Value = api::check(conn.post("/models").json(&json!({"model": canonical})).send().await?).await?.json().await?;
    if reply["success"].as_bool() == Some(false) {
        bail!("{}", reply["error"]["message"].as_str().unwrap_or("llama.cpp could not start the download"));
    }
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(700)).await;
        if started.elapsed() > Duration::from_secs(4 * 3600) { bail!("model download timed out"); }
        let entries = list(conn, false).await?;
        let Some(item) = named(&entries, &canonical) else {
            if started.elapsed() > Duration::from_secs(30) { bail!("llama.cpp did not list the requested download: {canonical}"); }
            continue;
        };
        let status = item["status"]["value"].as_str().unwrap_or("");
        if item["status"]["failed"].as_bool() == Some(true) || status == "error" {
            bail!("{}", item["status"]["error"].as_str().unwrap_or("model download failed"));
        }
        if status == "downloading" {
            let progress = item["status"]["progress"].as_object();
            let (completed, total) = progress.map(|p| p.values().fold((0, 0), |(done, all), f| {
                (done + f["done"].as_u64().unwrap_or(0), all + f["total"].as_u64().unwrap_or(0))
            })).unwrap_or((0, 0));
            let _ = tx.send(Progress { status: "Downloading".into(), completed, total }).await;
        } else if item["source"].as_str() == Some("cache") || matches!(status, "loaded" | "sleeping")
            || (status == "unloaded" && item["path"].as_str().is_some_and(|p| !p.is_empty())) {
            return Ok(());
        }
    }
}

/// `running`, `on demand` (the socket waits for the first app) or `stopped`.
pub fn service_state() -> String {
    let active = |unit: &str| crate::cmd::output(&["systemctl", "--user", "is-active", unit]).is_some_and(|s| s.trim() == "active");
    if active("nyx-aid.service") {
        "running".into()
    } else if active("nyx-aid.socket") {
        "on demand".into()
    } else {
        "stopped".into()
    }
}

pub fn local_service() -> bool {
    let host = crate::prefs::get().host();
    host.backend == crate::prefs::Backend::LlamaCpp
        && crate::prefs::normalize_host(&host.url, host.backend) == core::URL
}

pub fn service_enabled() -> bool {
    crate::cmd::output(&["systemctl", "--user", "is-enabled", "nyx-aid.socket"])
        .is_some_and(|s| s.trim() == "enabled")
}

pub fn set_service_enabled(enabled: bool) -> Result<()> {
    if enabled {
        crate::cmd::run(&["systemctl", "--user", "enable", "--now", "nyx-aid.socket"]).map(|_| ())
    } else {
        crate::cmd::run(&["systemctl", "--user", "disable", "--now", "nyx-aid.socket"])?;
        crate::cmd::run(&["systemctl", "--user", "stop", "nyx-aid.service"]).map(|_| ())
    }
}

pub fn installed() -> bool {
    core::installed()
}

pub fn control(action: &str) -> Result<String> {
    match action {
        "start" => crate::cmd::run(&["systemctl", "--user", "start", "nyx-aid.socket", "nyx-aid.service"])?,
        "stop" => crate::cmd::run(&["systemctl", "--user", "stop", "nyx-aid.service", "nyx-aid.socket"])?,
        "restart" => crate::cmd::run(&["systemctl", "--user", "restart", "nyx-aid.service"])?,
        _ => bail!("unknown service action {action}"),
    };
    Ok(format!("{} Nyx AI Core", if action == "start" { "Started" } else if action == "stop" { "Stopped" } else { "Restarted" }))
}
