//! Hugging Face: search GGUF repositories, list their quantisations, read model cards.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use pulldown_cmark::{Event, Parser};
use scraper::Html;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use crate::util::{encode_path, encode_query};


/// Projectors and speculative drafters are companion files, not chat models.
pub fn auxiliary_gguf(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.contains("mmproj") || name.contains("dflash") || name.contains("drafter")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Trending,
    Downloads,
    Likes,
    Updated,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Trending, Sort::Downloads, Sort::Likes, Sort::Updated];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Trending => "Trending",
            Sort::Downloads => "Downloads",
            Sort::Likes => "Likes",
            Sort::Updated => "Updated",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Sort::Trending => "trendingScore",
            Sort::Downloads => "downloads",
            Sort::Likes => "likes",
            Sort::Updated => "lastModified",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Vision,
    Embedding,
    All,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Text, Kind::Vision, Kind::Embedding, Kind::All];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Vision => "Vision",
            Kind::Embedding => "Embedding",
            Kind::All => "All",
        }
    }

    fn pipeline(self) -> Option<&'static str> {
        match self {
            Kind::Text => Some("text-generation"),
            Kind::Vision => Some("image-text-to-text"),
            Kind::Embedding => Some("feature-extraction"),
            Kind::All => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
    pub last_modified: String,
    pub tags: Vec<String>,
    pub pipeline: String,
}

#[derive(Clone, Debug, Default)]
pub struct GgufFile {
    pub path: String,
    pub size: u64,
    pub quant: String,
}

#[derive(Clone, Debug, Default)]
pub struct Detail {
    /// Parameter count.
    pub params: u64,
    pub architecture: String,
    pub context_length: u64,
    /// The chat template mentions tools.
    pub tools: bool,
    pub license: String,
    pub base_model: String,
    pub files: Vec<GgufFile>,
}

pub fn search_url(query: &str, sort: Sort, kind: Kind) -> String {
    let mut url = format!(
        "https://huggingface.co/api/models?filter=gguf&full=false&sort={}&direction=-1&limit=40&search={}",
        sort.api(),
        encode_query(query)
    );
    if let Some(p) = kind.pipeline() {
        url.push_str("&pipeline_tag=");
        url.push_str(p);
    }
    url
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("nyx-chat/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

fn token_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"))
        .join("nyx-chat/huggingface.token")
}

fn shared_token_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("HF_HOME").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".cache"))
            .join("huggingface")
    }).join("token")
}

pub fn has_app_token() -> bool { fs::read_to_string(token_path()).is_ok_and(|s| !s.trim().is_empty()) }
pub fn has_shared_token() -> bool { fs::read_to_string(shared_token_path()).is_ok_and(|s| !s.trim().is_empty()) }

fn token() -> Option<String> {
    for path in [token_path(), shared_token_path()] {
        if let Ok(s) = fs::read_to_string(path) && !s.trim().is_empty() { return Some(s.trim().into()); }
    }
    None
}

pub fn save_token(value: &str) -> Result<()> {
    let path = token_path();
    fs::create_dir_all(path.parent().unwrap())?;
    use std::io::Write;
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(value.trim().as_bytes())?;
    Ok(())
}

pub fn remove_app_token() -> Result<()> {
    match fs::remove_file(token_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub async fn whoami(value: &str) -> Result<String> {
    let response: Value = client().get("https://huggingface.co/api/whoami-v2")
        .bearer_auth(value.trim()).send().await?.error_for_status().context("Hugging Face login")?.json().await?;
    Ok(response["name"].as_str().unwrap_or("Hugging Face account").to_string())
}

fn auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(t) = token() { req.bearer_auth(t) } else { req }
}

#[derive(Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(rename = "lastModified", default)]
    last_modified: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    pipeline_tag: String,
}

pub async fn search(query: &str, sort: Sort, kind: Kind) -> Result<Vec<Model>> {
    let raw: Vec<ApiModel> = auth(client().get(search_url(query, sort, kind)))
        .send()
        .await?
        .error_for_status()
        .context("Hugging Face search")?
        .json()
        .await?;
    Ok(raw
        .into_iter()
        .map(|m| Model {
            id: m.id,
            downloads: m.downloads,
            likes: m.likes,
            last_modified: m.last_modified,
            tags: m.tags.into_iter().filter(|t| !t.contains(':') && t != "gguf").take(6).collect(),
            pipeline: m.pipeline_tag,
        })
        .collect())
}

pub async fn detail(repo: &str) -> Result<Detail> {
    let url = format!("https://huggingface.co/api/models/{}", encode_path(repo));
    let v: Value = auth(client().get(url)).send().await?.error_for_status().context("Hugging Face model")?.json().await?;
    let mut d = Detail {
        params: v["gguf"]["total"].as_u64().unwrap_or(0),
        architecture: v["gguf"]["architecture"].as_str().unwrap_or("").into(),
        context_length: v["gguf"]["context_length"].as_u64().unwrap_or(0),
        tools: v["gguf"]["chat_template"].as_str().is_some_and(|t| t.contains("tools")),
        license: v["cardData"]["license"].as_str().unwrap_or("").into(),
        base_model: match &v["cardData"]["base_model"] {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.first().and_then(|x| x.as_str()).unwrap_or("").into(),
            _ => String::new(),
        },
        files: Vec::new(),
    };
    d.files = gguf_files(repo).await?;
    Ok(d)
}

#[derive(Deserialize)]
struct TreeItem {
    path: String,
    #[serde(default)]
    size: u64,
}

pub async fn gguf_files(repo: &str) -> Result<Vec<GgufFile>> {
    let url = format!("https://huggingface.co/api/models/{}/tree/main?recursive=true", encode_path(repo));
    let raw: Vec<TreeItem> = auth(client().get(url)).send().await?.error_for_status().context("Hugging Face file list")?.json().await?;
    Ok(collect_files(raw))
}

fn collect_files(raw: Vec<TreeItem>) -> Vec<GgufFile> {
    // Split files ("…-00001-of-00003.gguf") count once, with their total size.
    let mut files: Vec<GgufFile> = Vec::new();
    for item in raw {
        let lower = item.path.to_ascii_lowercase();
        if !lower.ends_with(".gguf") || auxiliary_gguf(&item.path) {
            continue;
        }
        let quant = quant_tag(&item.path);
        if let Some(existing) = files.iter_mut().find(|f| f.quant == quant && is_split(&f.path) && is_split(&item.path)) {
            existing.size += item.size;
            continue;
        }
        files.push(GgufFile { quant, size: item.size, path: item.path });
    }
    files.sort_by_key(|a| a.size);
    // Repos often carry the same quant more than once (in folders, or as
    // variants); a pull by quant name gets one of them, so list each once.
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.quant.clone()));
    files
}

fn is_split(path: &str) -> bool {
    path.contains("-of-0")
}

/// The model card, without its YAML front matter.
pub async fn readme(repo: &str) -> Result<String> {
    let url = format!("https://huggingface.co/{}/raw/main/README.md", encode_path(repo));
    let text = auth(client().get(url)).send().await?.error_for_status()?.text().await?;
    Ok(plain_html(strip_front_matter(&text)))
}

/// Keep Markdown syntax, but turn a model card's raw HTML into readable text.
fn plain_html(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut end = 0;
    for (event, range) in Parser::new(markdown).into_offset_iter() {
        if !matches!(event, Event::Html(_) | Event::InlineHtml(_)) || range.start < end { continue; }
        out.push_str(&markdown[end..range.start]);
        let html = &markdown[range.clone()];
        let fragment = Html::parse_fragment(html);
        let value = fragment.root_element().text().collect::<String>();
        if html.trim_start().starts_with("<br") { out.push('\n'); }
        else { out.push_str(&value); }
        end = range.end;
    }
    out.push_str(&markdown[end..]);
    out
}

pub fn strip_front_matter(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        return rest[end + 4..].trim_start();
    }
    text
}

/// What to pass to `ollama pull`: the quant tag picks one file out of the repo.
pub fn ollama_ref(repo: &str, quant: &str) -> String {
    format!("hf.co/{repo}:{quant}")
}

pub fn quant_tag(filename: &str) -> String {
    let base = filename.rsplit('/').next().unwrap_or(filename);
    let upper = base.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < bytes.len() {
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        if let Some(len) = match_quant(&upper[i..]) {
            best = Some((i, len));
            i += len;
        } else {
            i += 1;
        }
    }
    match best {
        Some((start, len)) => upper[start..start + len].to_string(),
        None => base.trim_end_matches(".gguf").trim_end_matches(".GGUF").to_string(),
    }
}

fn match_quant(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if s.starts_with("BF16") {
        return Some(4);
    }
    if s.starts_with("MXFP") {
        return take_body(s, 4, 1);
    }
    if s.starts_with("PQ") {
        return take_body(s, 2, 2);
    }
    if s.starts_with("PTQ") {
        return take_body(s, 3, 2);
    }
    if s.starts_with("IQ") {
        return take_body(s, 2, 2);
    }
    if s.starts_with("F16") || s.starts_with("F32") {
        return Some(3);
    }
    if b.first() == Some(&b'Q') {
        return take_body(s, 1, 2);
    }
    None
}

/// Digits after the prefix, then up to `segments` `_XX` parts (Q4_K_M, IQ4_XS).
fn take_body(s: &str, prefix: usize, segments: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = prefix;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == prefix {
        return None;
    }
    for _ in 0..segments {
        if i < b.len() && b[i] == b'_' {
            let mut k = i + 1;
            while k < b.len() && b[k].is_ascii_alphanumeric() {
                k += 1;
            }
            if k == i + 1 {
                break;
            }
            i = k;
        }
    }
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_urls() {
        let url = search_url("qwen 3", Sort::Trending, Kind::Text);
        assert!(url.contains("filter=gguf"));
        assert!(url.contains("sort=trendingScore"));
        assert!(url.contains("search=qwen+3"));
        assert!(url.contains("pipeline_tag=text-generation"));
        assert!(!search_url("", Sort::Likes, Kind::All).contains("pipeline_tag"));
    }

    #[test]
    fn quant_tags() {
        assert_eq!(quant_tag("Llama-3.2-3B-Instruct-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_tag("model-IQ3_M.gguf"), "IQ3_M");
        assert_eq!(quant_tag("foo-BF16.gguf"), "BF16");
        assert_eq!(quant_tag("sub/bar-IQ4_XS.gguf"), "IQ4_XS");
        assert_eq!(quant_tag("Ternary-Bonsai-2-27B-PQ2_0.gguf"), "PQ2_0");
        assert_eq!(quant_tag("Ternary-Bonsai-2-27B-PTQ1_0.gguf"), "PTQ1_0");
        assert!(auxiliary_gguf("Ternary-Bonsai-2-9B-DFlash2-Q8_0.gguf"));
        assert_eq!(quant_tag("Q8_0/big-Q8_0-00001-of-00002.gguf"), "Q8_0");
        assert_eq!(ollama_ref("unsloth/Qwen3-8B-GGUF", "Q4_K_M"), "hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M");
    }

    #[test]
    fn merges_split_files_and_skips_projectors() {
        let raw = vec![
            TreeItem { path: "a-Q8_0-00001-of-00002.gguf".into(), size: 10 },
            TreeItem { path: "a-Q8_0-00002-of-00002.gguf".into(), size: 5 },
            TreeItem { path: "a-Q4_K_M.gguf".into(), size: 8 },
            TreeItem { path: "mmproj-F16.gguf".into(), size: 1 },
            TreeItem { path: "README.md".into(), size: 1 },
        ];
        let f = collect_files(raw);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].quant.as_str(), f[0].size), ("Q4_K_M", 8));
        assert_eq!((f[1].quant.as_str(), f[1].size), ("Q8_0", 15));
    }

    #[test]
    fn front_matter() {
        assert_eq!(strip_front_matter("---\nlicense: mit\n---\n# Hi"), "# Hi");
        assert_eq!(strip_front_matter("# Hi"), "# Hi");
    }

    #[test]
    fn model_card_html() {
        assert_eq!(plain_html("<div>Hello</div>\n\n**world**"), "Hello\n\n**world**");
        assert_eq!(plain_html("Model <span>name</span><br>next"), "Model name\nnext");
    }
}
