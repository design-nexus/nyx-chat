//! Web search (DuckDuckGo, SearXNG, Brave, Tavily) and page reading.

use anyhow::{Context, Result, bail};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::time::Duration;

use super::Output;
use crate::prefs::Prefs;
use crate::store::Source;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36")
        .build()
        .unwrap_or_default()
}

pub async fn search(query: &str, prefs: &Prefs) -> Result<Output> {
    if query.is_empty() {
        bail!("empty search query");
    }
    let n = prefs.search_results.clamp(1, 10) as usize;
    let mut hits = match prefs.search_backend.as_str() {
        "searxng" => searxng(query, &prefs.searxng_url).await?,
        "brave" => brave(query, &crate::secrets::get("brave")).await?,
        "tavily" => tavily(query, &crate::secrets::get("tavily")).await?,
        _ => duckduckgo(query).await?,
    };
    hits.truncate(n);
    if hits.is_empty() {
        return Ok(Output::text(format!("No results for \"{query}\".")));
    }
    let mut pages: Vec<String> = vec![String::new(); hits.len()];
    if prefs.search_fetch_pages {
        let fetches = hits.iter().take(3).map(|h| page_text(&h.url, 3000));
        for (i, r) in futures_util::future::join_all(fetches).await.into_iter().enumerate() {
            if let Ok(t) = r {
                pages[i] = t;
            }
        }
    }
    Ok(format_hits(query, &hits, &pages))
}

fn format_hits(query: &str, hits: &[Hit], pages: &[String]) -> Output {
    let mut text = format!("Search results for \"{query}\":\n");
    for (i, h) in hits.iter().enumerate() {
        text.push_str(&format!("\n[{}] {}\n{}\n{}\n", i + 1, h.title, h.url, h.snippet));
        if let Some(p) = pages.get(i).filter(|p| !p.is_empty()) {
            text.push_str(&format!("Page text: {p}\n"));
        }
    }
    text.push_str("\nCite sources inline as [1], [2] … matching the numbers above.");
    Output { text, sources: hits.iter().map(|h| Source { title: h.title.clone(), url: h.url.clone() }).collect(), failed: false }
}

async fn duckduckgo(query: &str) -> Result<Vec<Hit>> {
    let html = client()
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query), ("kl", "wt-wt")])
        .send()
        .await?
        .error_for_status()
        .context("DuckDuckGo")?
        .text()
        .await?;
    let hits = parse_duckduckgo(&html);
    if hits.is_empty() && html.contains("anomaly") {
        bail!("DuckDuckGo asked for a captcha. Try again in a minute, or pick another search engine in Settings.");
    }
    Ok(hits)
}

pub fn parse_duckduckgo(html: &str) -> Vec<Hit> {
    let doc = Html::parse_document(html);
    let result = Selector::parse("div.result").expect("selector");
    let link = Selector::parse("a.result__a").expect("selector");
    let snippet = Selector::parse(".result__snippet").expect("selector");
    doc.select(&result)
        .filter(|r| !r.value().classes().any(|c| c == "result--ad"))
        .filter_map(|r| {
            let a = r.select(&link).next()?;
            let url = unwrap_ddg_link(a.value().attr("href")?);
            if !url.starts_with("http") {
                return None;
            }
            Some(Hit {
                title: a.text().collect::<String>().trim().to_string(),
                url,
                snippet: r.select(&snippet).next().map(|s| s.text().collect::<String>()).unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

/// DuckDuckGo sometimes wraps links as `//duckduckgo.com/l/?uddg=<encoded>`.
fn unwrap_ddg_link(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let enc = href[pos + 5..].split('&').next().unwrap_or("");
        return percent_decode(enc);
    }
    href.to_string()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into()
}

async fn searxng(query: &str, base: &str) -> Result<Vec<Hit>> {
    if base.trim().is_empty() {
        bail!("Set your SearXNG address in Settings → Web search.");
    }
    let url = format!("{}/search?format=json&q={}", base.trim().trim_end_matches('/'), crate::util::encode_query(query));
    let v: Value =
        client().get(url).send().await?.error_for_status().context("SearXNG (is the JSON format enabled?)")?.json().await?;
    Ok(v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Hit {
            title: r["title"].as_str().unwrap_or("").into(),
            url: r["url"].as_str().unwrap_or("").into(),
            snippet: r["content"].as_str().unwrap_or("").into(),
        })
        .collect())
}

async fn brave(query: &str, key: &str) -> Result<Vec<Hit>> {
    if key.trim().is_empty() {
        bail!("Add a Brave Search API key in Settings → Web search.");
    }
    let url = format!("https://api.search.brave.com/res/v1/web/search?q={}", crate::util::encode_query(query));
    let v: Value = client()
        .get(url)
        .header("X-Subscription-Token", key.trim())
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()
        .context("Brave Search")?
        .json()
        .await?;
    Ok(v["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Hit {
            title: r["title"].as_str().unwrap_or("").into(),
            url: r["url"].as_str().unwrap_or("").into(),
            snippet: strip_tags(r["description"].as_str().unwrap_or("")),
        })
        .collect())
}

async fn tavily(query: &str, key: &str) -> Result<Vec<Hit>> {
    if key.trim().is_empty() {
        bail!("Add a Tavily API key in Settings → Web search.");
    }
    let v: Value = client()
        .post("https://api.tavily.com/search")
        .bearer_auth(key.trim())
        .json(&json!({ "query": query, "max_results": 10 }))
        .send()
        .await?
        .error_for_status()
        .context("Tavily")?
        .json()
        .await?;
    Ok(v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Hit {
            title: r["title"].as_str().unwrap_or("").into(),
            url: r["url"].as_str().unwrap_or("").into(),
            snippet: r["content"].as_str().unwrap_or("").into(),
        })
        .collect())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

pub async fn fetch(url: &str) -> Result<Output> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("only http and https links can be read");
    }
    let text = page_text(url, 12_000).await?;
    Ok(Output { text, sources: vec![Source { title: url.into(), url: url.into() }], failed: false })
}

/// Download a page and keep its readable text, up to `max` characters.
pub async fn page_text(url: &str, max: usize) -> Result<String> {
    let r = client().get(url).send().await?.error_for_status()?;
    let is_html = r.headers().get("content-type").and_then(|v| v.to_str().ok()).is_none_or(|t| t.contains("html"));
    let body = r.text().await?;
    let text = if is_html { readable_text(&body) } else { body };
    Ok(crate::util::clip(&text, max))
}

/// The text of headings, paragraphs, list items and table cells, preferring
/// `<article>` or `<main>` when the page has one.
pub fn readable_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let root_sel = Selector::parse("article, main, [role=main]").expect("selector");
    let blocks = Selector::parse("h1, h2, h3, h4, p, li, pre, td, th, blockquote").expect("selector");
    let title_sel = Selector::parse("title").expect("selector");
    let mut out = String::new();
    if let Some(t) = doc.select(&title_sel).next() {
        out.push_str(t.text().collect::<String>().trim());
        out.push_str("\n\n");
    }
    let roots: Vec<_> = doc.select(&root_sel).collect();
    let mut push = |el: scraper::ElementRef| {
        let t: String = el.text().collect::<Vec<_>>().join(" ");
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.len() > 1 {
            out.push_str(&t);
            out.push('\n');
        }
    };
    if let Some(root) = roots.first() {
        root.select(&blocks).for_each(&mut push);
    } else {
        doc.select(&blocks).for_each(&mut push);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: &str = r#"<div id="links">
      <div class="result results_links result--ad"><h2 class="result__title">
        <a class="result__a" href="https://ads.example/">Ad</a></h2></div>
      <div class="result results_links web-result"><div class="result__body">
        <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://rust-lang.org/">Rust Programming Language</a></h2>
        <a class="result__snippet" href="https://rust-lang.org/"><b>Rust</b> is fast.</a></div></div>
      <div class="result results_links web-result"><div class="result__body">
        <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=x">The Book</a></h2>
        </div></div></div>"#;

    #[test]
    fn parses_duckduckgo() {
        let hits = parse_duckduckgo(DDG);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].snippet, "Rust is fast.");
        assert_eq!(hits[1].url, "https://doc.rust-lang.org/book/");
    }

    #[test]
    fn formats_numbered_results_with_sources() {
        let hits = vec![Hit { title: "A".into(), url: "https://a".into(), snippet: "s".into() }];
        let out = format_hits("q", &hits, &[]);
        assert!(out.text.contains("[1] A"));
        assert_eq!(out.sources[0].url, "https://a");
    }

    #[test]
    fn readable_text_prefers_article() {
        let html =
            "<html><title>T</title><body><nav><li>menu</li></nav><article><h1>Head</h1><p>Body  text</p></article></body></html>";
        let t = readable_text(html);
        assert!(t.starts_with("T\n"));
        assert!(t.contains("Head\nBody text"));
        assert!(!t.contains("menu"));
    }
}
