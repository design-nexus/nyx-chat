//! OpenAI chat-completion wire format for the llama.cpp router.

use anyhow::Result;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::ollama::api;
use crate::ollama::chat::{Cancel, Event, Msg, Request, Stats, ToolCall};

fn messages(items: &[Msg]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    for (i, m) in items.iter().enumerate() {
        if m.role == "tool" {
            let id = pending.iter().position(|(_, name)| *name == m.tool_name)
                .map(|pos| pending.remove(pos).0).unwrap_or_else(|| format!("call_{i}"));
            out.push(json!({"role": "tool", "tool_call_id": id, "content": m.content}));
            continue;
        }
        let mut item = json!({"role": m.role, "content": m.content});
        if !m.images.is_empty() {
            let mut content = vec![json!({"type": "text", "text": m.content})];
            for image in &m.images {
                let mime = if image.starts_with("/9j/") { "image/jpeg" }
                    else if image.starts_with("UklGR") { "image/webp" }
                    else if image.starts_with("R0lGOD") { "image/gif" }
                    else { "image/png" };
                content.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{image}")}}));
            }
            item["content"] = json!(content);
        }
        if !m.tool_calls.is_empty() {
            let calls: Vec<Value> = m.tool_calls.iter().enumerate().map(|(n, c)| {
                let id = format!("call_{i}_{n}");
                pending.push((id.clone(), c.name.clone()));
                json!({"id": id, "type": "function", "function": {"name": c.name, "arguments": c.arguments.to_string()}})
            }).collect();
            item["tool_calls"] = json!(calls);
        }
        out.push(item);
    }
    out
}

fn body(req: &Request) -> Value {
    let mut body = json!({"model": req.model, "messages": messages(&req.messages), "stream": true,
        "stream_options": {"include_usage": true}});
    if !req.tools.is_empty() { body["tools"] = json!(req.tools); }
    if let Some(options) = req.options.as_object() {
        for key in ["temperature", "top_p", "top_k", "min_p", "repeat_penalty", "seed", "reasoning_effort"] {
            if key == "reasoning_effort" && req.think == Some(false) { continue; }
            if let Some(value) = options.get(key) { body[key] = value.clone(); }
        }
    }
    if let Some(format) = &req.format {
        body["response_format"] = if format.as_str() == Some("json") { json!({"type": "json_object"}) }
            else { json!({"type": "json_schema", "schema": format}) };
    }
    if let Some(think) = req.think { body["chat_template_kwargs"] = json!({"enable_thinking": think}); }
    body
}

pub async fn stream_once(req: &Request, tx: &async_channel::Sender<Event>, cancel: &Cancel)
    -> Result<(String, String, Vec<ToolCall>, Stats)> {
    if super::core::local(&req.conn) {
        return super::core::stream_once(req, tx, cancel).await;
    }
    let started = Instant::now();
    let response = api::check(req.conn.post("/v1/chat/completions").json(&body(req)).send().await?).await?;
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let (mut content, mut thinking, mut finish) = (String::new(), String::new(), String::new());
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut stats = Stats::default();
    let mut first = None;
    while let Some(chunk) = stream.next().await {
        buffer.extend_from_slice(&chunk?);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data: ") else { continue };
            if data == "[DONE]" { break; }
            let value: Value = serde_json::from_str(data)?;
            let delta = &value["choices"][0]["delta"];
            let c = delta["content"].as_str().unwrap_or("");
            let t = delta["reasoning_content"].as_str().unwrap_or("");
            if !c.is_empty() || !t.is_empty() {
                first.get_or_insert_with(|| started.elapsed().as_millis() as u64);
                content.push_str(c);
                thinking.push_str(t);
                let _ = tx.try_send(Event::Delta { content: c.into(), thinking: t.into() });
            }
            if let Some(parts) = delta["tool_calls"].as_array() {
                for part in parts {
                    let index = part["index"].as_u64().unwrap_or(0) as usize;
                    while calls.len() <= index { calls.push((String::new(), String::new())); }
                    if let Some(name) = part["function"]["name"].as_str() { calls[index].0.push_str(name); }
                    if let Some(args) = part["function"]["arguments"].as_str() { calls[index].1.push_str(args); }
                }
            }
            if let Some(reason) = value["choices"][0]["finish_reason"].as_str() { finish = reason.into(); }
            if let Some(usage) = value["usage"].as_object() {
                stats.prompt_tokens = usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
                stats.tokens = usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
            }
            if cancel.load(Ordering::Relaxed) { break; }
        }
        if cancel.load(Ordering::Relaxed) { break; }
    }
    stats.total_ms = started.elapsed().as_millis() as u64;
    stats.prompt_ms = first.unwrap_or(0);
    stats.tokens_per_sec = if stats.total_ms > stats.prompt_ms && stats.tokens > 0 {
        stats.tokens as f64 * 1000.0 / (stats.total_ms - stats.prompt_ms) as f64
    } else { 0.0 };
    stats.done_reason = finish;
    let calls = calls.into_iter().filter(|(name, _)| !name.is_empty()).map(|(name, args)| ToolCall {
        name, arguments: serde_json::from_str(&args).unwrap_or(json!({})) }).collect();
    Ok((content, thinking, calls, stats))
}
