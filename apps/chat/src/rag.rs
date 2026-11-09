//! Retrieval: split documents into overlapping chunks, embed them with an
//! embedding model on the selected server, and find the chunks closest to a question.

use anyhow::Result;

use crate::ollama::api::{self, Conn};

/// Split on paragraph boundaries into chunks of about `size` characters, each
/// sharing a little text with the one before so nothing falls between them.
pub fn chunk(text: &str, size: usize) -> Vec<String> {
    let size = size.max(200);
    let overlap = size / 6;
    let mut out = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        // Very long paragraphs are cut into pieces first.
        let pieces: Vec<String> = if para.chars().count() > size {
            para.chars().collect::<Vec<_>>().chunks(size).map(|c| c.iter().collect()).collect()
        } else {
            vec![para.to_string()]
        };
        for piece in pieces {
            if !cur.is_empty() && cur.chars().count() + piece.chars().count() > size {
                out.push(cur.clone());
                let tail: String = cur.chars().rev().take(overlap).collect::<Vec<_>>().into_iter().rev().collect();
                cur = tail;
                cur.push_str("\n\n");
            }
            cur.push_str(&piece);
            cur.push_str("\n\n");
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.iter().map(|c| c.trim().to_string()).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

/// Embed chunks in batches.
pub async fn embed_all(conn: &Conn, model: &str, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut out = Vec::with_capacity(chunks.len());
    for batch in chunks.chunks(32) {
        out.extend(api::embed(conn, model, batch).await?);
    }
    Ok(out)
}

pub struct Found {
    pub doc: String,
    pub text: String,
    pub score: f32,
}

/// The `k` chunks most like `query`.
pub async fn retrieve(
    conn: &Conn,
    model: &str,
    query: &str,
    pool: Vec<(String, String, Vec<f32>)>,
    k: usize,
) -> Result<Vec<Found>> {
    if pool.is_empty() {
        return Ok(vec![]);
    }
    let q = api::embed(conn, model, &[query.to_string()]).await?.pop().unwrap_or_default();
    let mut scored: Vec<Found> = pool.into_iter().map(|(doc, text, v)| Found { score: cosine(&q, &v), doc, text }).collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(k);
    Ok(scored)
}

/// The context block added to the system prompt.
pub fn context_block(found: &[Found]) -> String {
    let mut s = String::from(
        "Use the following excerpts from the user's documents when they help answer. \
         Mention the document name when you use one.\n",
    );
    for f in found {
        s.push_str(&format!("\n<document name=\"{}\">\n{}\n</document>\n", f.doc, f.text));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_overlap_and_cover_everything() {
        let text = (0..30).map(|i| format!("Paragraph number {i} with some words in it.")).collect::<Vec<_>>().join("\n\n");
        let chunks = chunk(&text, 300);
        assert!(chunks.len() > 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 400));
        for i in 0..30 {
            assert!(chunks.iter().any(|c| c.contains(&format!("number {i} "))));
        }
    }

    #[test]
    fn long_paragraphs_are_split() {
        let text = "x".repeat(2500);
        assert!(chunk(&text, 1000).len() >= 3);
    }

    #[test]
    fn cosine_similarity() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// `cargo test -- --ignored`: needs a server with the all-minilm model at OLLAMA_URL.
    #[test]
    #[ignore]
    fn finds_the_relevant_chunk() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let conn = Conn { url: std::env::var("OLLAMA_URL").unwrap_or("http://127.0.0.1:11434".into()), key: String::new(), backend: crate::prefs::Backend::Ollama };
            let chunks = vec![
                "The office cat is called Biscuit and likes sunny windows.".to_string(),
                "Quarterly revenue grew twelve percent on hardware sales.".to_string(),
                "The wifi password is on the fridge.".to_string(),
            ];
            let vecs = embed_all(&conn, "all-minilm", &chunks).await.unwrap();
            let pool = chunks.into_iter().zip(vecs).map(|(t, v)| ("doc".to_string(), t, v)).collect();
            let found = retrieve(&conn, "all-minilm", "What is the name of our pet?", pool, 1).await.unwrap();
            assert!(found[0].text.contains("Biscuit"));
        });
    }
}
