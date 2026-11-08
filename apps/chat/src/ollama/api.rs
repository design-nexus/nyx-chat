//! Ollama's HTTP API: models, pulls, embeddings and the local service.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

use crate::prefs::{Backend, Host, normalize_host};

/// Where to reach a server. Cloned into background tasks.
#[derive(Clone, Debug)]
pub struct Conn {
    pub url: String,
    pub key: String,
    pub backend: Backend,
}

impl From<&Host> for Conn {
    fn from(h: &Host) -> Self {
        Conn { url: normalize_host(&h.url, h.backend), key: h.key.clone(), backend: h.backend }
    }
}

impl Conn {
    pub fn current() -> Conn {
        Conn::from(&crate::prefs::get().host())
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    pub(crate) fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(client().get(self.url(path)))
    }

    pub(crate) fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(client().post(self.url(path)))
    }

    pub(crate) fn auth(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.key.is_empty() { r } else { r.bearer_auth(&self.key) }
    }
}

pub fn client() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("nyx-chat/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

pub(crate) async fn check(r: reqwest::Response) -> Result<reqwest::Response> {
    if r.status().is_success() {
        return Ok(r);
    }
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().or(v["error"]["message"].as_str()).map(String::from)).unwrap_or(text);
    bail!("{} {}", status.as_u16(), msg.trim())
}

#[derive(Clone, Debug, Default)]
pub struct Installed {
    pub name: String,
    pub size: u64,
    pub family: String,
    pub parameter_size: String,
    pub quantization: String,
    pub modified: String,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub struct Running {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
    pub expires: String,
}

#[derive(Clone, Debug, Default)]
pub struct ModelInfo {
    pub capabilities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
    pub context_length: Option<u64>,
    pub parameters: String,
    pub template: String,
    pub license: String,
    pub system: String,
}

impl ModelInfo {
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

pub async fn version(conn: &Conn) -> Result<String> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::version(conn).await; }
    let r = check(conn.get("/api/version").timeout(Duration::from_secs(3)).send().await?).await?;
    let v: Value = r.json().await?;
    Ok(v["version"].as_str().unwrap_or("").to_string())
}

pub async fn tags(conn: &Conn) -> Result<Vec<Installed>> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::tags(conn).await; }
    let r = check(conn.get("/api/tags").timeout(Duration::from_secs(5)).send().await?).await?;
    let body: Value = r.json().await?;
    let mut models: Vec<Installed> = body["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let d = &item["details"];
            Installed {
                name: item["name"].as_str().unwrap_or("").to_string(),
                size: item["size"].as_u64().unwrap_or(0),
                family: d["family"].as_str().unwrap_or("").to_string(),
                parameter_size: d["parameter_size"].as_str().unwrap_or("").to_string(),
                quantization: d["quantization_level"].as_str().unwrap_or("").to_string(),
                modified: item["modified_at"].as_str().unwrap_or("").to_string(),
                digest: item["digest"].as_str().unwrap_or("").to_string(),
            }
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

pub async fn running(conn: &Conn) -> Result<Vec<Running>> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::running(conn).await; }
    let r = check(conn.get("/api/ps").timeout(Duration::from_secs(5)).send().await?).await?;
    let body: Value = r.json().await?;
    Ok(body["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| Running {
            name: item["name"].as_str().unwrap_or("").to_string(),
            size: item["size"].as_u64().unwrap_or(0),
            size_vram: item["size_vram"].as_u64().unwrap_or(0),
            expires: item["expires_at"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

pub async fn show(conn: &Conn, model: &str) -> Result<ModelInfo> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::show(conn, model).await; }
    let r = check(conn.post("/api/show").json(&json!({ "model": model })).timeout(Duration::from_secs(10)).send().await?).await?;
    let body: Value = r.json().await?;
    Ok(parse_show(&body))
}

fn parse_show(body: &Value) -> ModelInfo {
    let context_length = body["model_info"]
        .as_object()
        .and_then(|info| info.iter().filter(|(k, _)| k.ends_with("context_length")).filter_map(|(_, v)| v.as_u64()).max());
    let s = |k: &str| body[k].as_str().unwrap_or("").to_string();
    ModelInfo {
        capabilities: body["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().map(String::from))
            .collect(),
        reasoning_efforts: Vec::new(),
        context_length,
        parameters: s("parameters"),
        template: s("template"),
        license: s("license"),
        system: s("system"),
    }
}

pub async fn delete(conn: &Conn, model: &str) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::delete(conn, model).await; }
    check(conn.auth(client().delete(conn.url("/api/delete"))).json(&json!({ "model": model })).send().await?).await?;
    Ok(())
}

pub async fn copy(conn: &Conn, source: &str, destination: &str) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { bail!("Copy is only available with Ollama"); }
    check(conn.post("/api/copy").json(&json!({ "source": source, "destination": destination })).send().await?).await?;
    Ok(())
}

/// `keep_alive` as Ollama wants it: bare numbers ("-1", "0") are seconds and
/// must be sent as numbers; anything else ("5m", "1h") is a duration string.
pub fn keep_alive_value(s: &str) -> Value {
    match s.trim().parse::<i64>() {
        Ok(n) => json!(n),
        Err(_) => json!(s.trim()),
    }
}

/// Drop a model from memory: an empty generate with `keep_alive: 0`.
pub async fn unload(conn: &Conn, model: &str) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::unload(conn, model).await; }
    check(conn.post("/api/generate").json(&json!({ "model": model, "keep_alive": 0 })).send().await?).await?;
    Ok(())
}

/// Load a model ahead of time so the first reply is quick.
pub async fn preload(conn: &Conn, model: &str, keep_alive: &str) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::preload(conn, model, keep_alive.trim() == "-1").await; }
    check(conn.post("/api/generate").json(&json!({ "model": model, "keep_alive": keep_alive_value(keep_alive) })).send().await?)
        .await?;
    Ok(())
}

pub async fn embed(conn: &Conn, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::embed(conn, model, inputs).await; }
    let r = check(
        conn.post("/api/embed")
            .json(&json!({ "model": model, "input": inputs }))
            .timeout(Duration::from_secs(300))
            .send()
            .await?,
    )
    .await?;
    let body: Value = r.json().await?;
    let out: Vec<Vec<f32>> = body["embeddings"]
        .as_array()
        .context("no embeddings in reply")?
        .iter()
        .map(|e| e.as_array().into_iter().flatten().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
        .collect();
    if out.len() != inputs.len() {
        bail!("expected {} embeddings, got {}", inputs.len(), out.len());
    }
    Ok(out)
}

/// One-shot, non-streaming chat (titles, summaries).
pub async fn complete(conn: &Conn, model: &str, messages: Value) -> Result<String> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::complete(conn, model, messages).await; }
    let body = json!({ "model": model, "messages": messages, "stream": false, "think": false });
    let r = check(conn.post("/api/chat").json(&body).timeout(Duration::from_secs(120)).send().await?).await;
    // Models without thinking support reject `think`; retry without it.
    let r = match r {
        Ok(r) => r,
        Err(e) if e.to_string().contains("think") => {
            let mut body = body;
            body.as_object_mut().map(|o| o.remove("think"));
            check(conn.post("/api/chat").json(&body).timeout(Duration::from_secs(120)).send().await?).await?
        }
        Err(e) => return Err(e),
    };
    let v: Value = r.json().await?;
    Ok(super::chat::strip_thinking(v["message"]["content"].as_str().unwrap_or("")).trim().to_string())
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub status: String,
    pub completed: u64,
    pub total: u64,
}

/// Pull a model, reporting progress. Ends when the stream does.
pub async fn pull(conn: &Conn, model: &str, tx: async_channel::Sender<Progress>) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { return crate::llama::api::pull(conn, model, tx).await; }
    let r = check(conn.post("/api/pull").json(&json!({ "model": model, "stream": true })).send().await?).await?;
    read_ndjson(r, |v| {
        let _ = tx.try_send(Progress {
            status: v["status"].as_str().unwrap_or("").to_string(),
            completed: v["completed"].as_u64().unwrap_or(0),
            total: v["total"].as_u64().unwrap_or(0),
        });
        true
    })
    .await
}

/// Create a model from a base with a system prompt and parameters.
pub async fn create(
    conn: &Conn,
    name: &str,
    from: &str,
    system: &str,
    parameters: serde_json::Map<String, Value>,
    tx: async_channel::Sender<Progress>,
) -> Result<()> {
    if conn.backend == Backend::LlamaCpp { bail!("Create a model is only available with Ollama"); }
    let mut body = json!({ "model": name, "from": from, "stream": true });
    if !system.trim().is_empty() {
        body["system"] = json!(system);
    }
    if !parameters.is_empty() {
        body["parameters"] = Value::Object(parameters);
    }
    let r = check(conn.post("/api/create").json(&body).send().await?).await?;
    read_ndjson(r, |v| {
        let _ = tx.try_send(Progress { status: v["status"].as_str().unwrap_or("").to_string(), completed: 0, total: 0 });
        true
    })
    .await
}

/// Read a newline-delimited JSON stream. `on_value` returns false to stop early.
pub async fn read_ndjson(response: reqwest::Response, mut on_value: impl FnMut(Value) -> bool) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.context("reading response")?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line).with_context(|| format!("bad json: {line}"))?;
            if let Some(error) = value["error"].as_str() {
                bail!("{error}");
            }
            if !on_value(value) {
                return Ok(());
            }
        }
    }
    Ok(())
}

// ---------- Update checks ----------

/// Where a model's manifest lives in its registry, from its local name.
pub fn manifest_url(name: &str) -> String {
    let (path, tag) = match name.rsplit_once(':') {
        Some((p, t)) if !t.contains('/') => (p, t),
        _ => (name, "latest"),
    };
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [host, rest @ ..] if host.contains('.') && !rest.is_empty() => {
            format!("https://{host}/v2/{}/manifests/{tag}", rest.join("/"))
        }
        [model] => format!("https://registry.ollama.ai/v2/library/{model}/manifests/{tag}"),
        _ => format!("https://registry.ollama.ai/v2/{path}/manifests/{tag}"),
    }
}

/// True when the registry has a manifest whose digest differs from the local one.
pub async fn update_available(name: &str, local_digest: &str) -> Result<bool> {
    use sha2::Digest;
    let r = client()
        .get(manifest_url(name))
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let r = check(r).await?;
    let bytes = r.bytes().await?;
    let remote = format!("{:x}", sha2::Sha256::digest(&bytes));
    let local = local_digest.trim_start_matches("sha256:");
    Ok(!local.is_empty() && remote != local)
}

/// Ollama's error for a Hugging Face CDN redirect quotes a whole URL; say it plainly.
pub fn short_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("redirect") && (lower.contains("realm") || lower.contains("blocked")) {
        return "Hugging Face redirected the download and Ollama blocked it. Update Ollama and try again.".into();
    }
    if lower.contains("connection refused") || lower.contains("error sending request") || lower.contains("tcp connect") {
        return "The model server isn't reachable. Check its service and address in Settings.".into();
    }
    if message.chars().count() > 200 { format!("{}…", message.chars().take(200).collect::<String>()) } else { message.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_urls() {
        assert_eq!(manifest_url("llama3.2"), "https://registry.ollama.ai/v2/library/llama3.2/manifests/latest");
        assert_eq!(manifest_url("qwen3:8b"), "https://registry.ollama.ai/v2/library/qwen3/manifests/8b");
        assert_eq!(manifest_url("someone/model:q4"), "https://registry.ollama.ai/v2/someone/model/manifests/q4");
        assert_eq!(manifest_url("hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M"), "https://hf.co/v2/unsloth/Qwen3-8B-GGUF/manifests/Q4_K_M");
    }

    #[test]
    fn parses_show() {
        let body = json!({
            "capabilities": ["completion", "tools", "thinking"],
            "model_info": { "qwen3.context_length": 40960, "qwen3.block_count": 36 },
            "parameters": "temperature 0.6",
        });
        let info = parse_show(&body);
        assert!(info.has("tools"));
        assert!(!info.has("vision"));
        assert_eq!(info.context_length, Some(40960));
        assert_eq!(info.parameters, "temperature 0.6");
    }

    #[test]
    fn keep_alive_numbers_are_numbers() {
        assert_eq!(keep_alive_value("-1"), json!(-1));
        assert_eq!(keep_alive_value("0"), json!(0));
        assert_eq!(keep_alive_value("5m"), json!("5m"));
    }

    #[test]
    fn short_errors() {
        assert!(short_error("error sending request for url (http://127.0.0.1:11434/api/tags)").contains("isn't reachable"));
        assert_eq!(short_error("model not found"), "model not found");
    }
}

#[cfg(test)]
mod network_tests {
    /// `cargo test -- --ignored`: needs the internet and a local server with models.
    #[test]
    #[ignore]
    fn installed_models_match_their_registry() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let conn =
                super::Conn { url: std::env::var("OLLAMA_URL").unwrap_or("http://127.0.0.1:11434".into()), key: String::new(), backend: crate::prefs::Backend::Ollama };
            for m in super::tags(&conn).await.unwrap() {
                let newer = super::update_available(&m.name, &m.digest).await;
                println!("{} -> {newer:?}", m.name);
                assert!(newer.is_ok());
            }
        });
    }
}
