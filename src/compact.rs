//! Lets a chat go on past the model's context window (as the web chat's chat-compact.js).
//!
//! The earlier conversation is replaced, in what's sent only, by a summary (`Conv::compaction`,
//! see chat::api_history); nothing leaves the screen and the latest user message always goes
//! as it is. The summary is built in chunks that each fit well inside the context, carrying the
//! summary so far into the next, so a chat of any length compacts: one summary request over the
//! whole history overflows the same way the chat did.
//!
//! Plain chat compacts before sending once the history is near the context, and once more if
//! the model still refuses it as too long (chat::run). Code Mode keeps its own trigger and uses
//! `summarize` when its history is too big for one request (code::compact).

use crate::{App, api};
use dioxus::prelude::*;
use serde_json::{Value, json};

/// Of the context, estimated: plain chat compacts before sending past this.
pub const COMPACT_AT: f64 = 0.75;
/// Of the context, per summary request.
const CHUNK_SHARE: f64 = 0.4;
/// A cautious estimate; real text runs 3.3 to 4 characters a token.
const CHARS_PER_TOKEN: usize = 3;
const SUMMARY_MAX_TOKENS: u32 = 8000;
/// Tool output kept per result, in the transcript.
const TOOL_CHARS: usize = 4000;
const MIN_SUMMARY_CHARS: usize = 100;

const PROMPT: &str = "You are compacting a long conversation so it can continue from your summary alone: the conversation will be replaced by it. \
Below is the summary so far (it may be empty) and the next part of the conversation. Write the updated summary. \
Keep: who the user is and what they want; every request and its outcome; decisions and why; facts, names, numbers, IDs, URLs, file names and code that matter; \
the tone and any instructions the user gave about style or format; and what is still open. Drop small talk and repetition. \
Write it as notes in the same language as the conversation, at most about 1,500 words. Reply with the summary only.";

/// Estimated tokens of wire messages.
pub fn tokens(msgs: &[Value]) -> u64 {
    (msgs.iter().map(|m| m.to_string().len()).sum::<usize>() / CHARS_PER_TOKEN) as u64
}

fn clip(t: &str, max: usize) -> String {
    let n = t.chars().count();
    if n <= max {
        return t.to_string();
    }
    let head: String = t.chars().take(max / 2).collect();
    let tail: String = t.chars().skip(n - max / 2).collect();
    format!("{head}\n[…]\n{tail}")
}

/// The conversation as readable lines, roles named, images noted, tool output clipped.
fn transcript(msgs: &[Value]) -> Vec<String> {
    let mut lines = vec![];
    for m in msgs {
        let role = m["role"].as_str().unwrap_or("");
        if role == "system" {
            continue;
        }
        let mut text = match &m["content"] {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .map(|p| match p["type"].as_str() {
                    Some("text") => p["text"].as_str().unwrap_or("").to_string(),
                    Some("image_url") => "[image]".into(),
                    other => format!("[{}]", other.unwrap_or("attachment")),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if role == "tool" {
            lines.push(format!("Tool output: {}", clip(&text, TOOL_CHARS)));
            continue;
        }
        if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
            let code: Vec<String> = calls.iter().map(|c| clip(c["function"]["arguments"].as_str().unwrap_or(""), TOOL_CHARS)).collect();
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("Ran code:\n{}", code.join("\n")));
        }
        if text.trim().is_empty() {
            continue;
        }
        lines.push(format!("{}: {text}", if role == "user" { "User" } else { "Assistant" }));
    }
    lines
}

/// Lines grouped into chunks of at most `max` characters; a longer line is split.
fn chunks(lines: &[String], max: usize) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        for piece in chars.chunks(max) {
            let piece: String = piece.iter().collect();
            if !current.is_empty() && current.chars().count() + piece.chars().count() + 2 > max {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(&piece);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// One summary request. A burst of them meets the chat's per-minute limit, so a 429 is waited
/// out (for up to about two minutes) and a lost connection or 502/503/504 tried again.
async fn request(model: &str, prompt: &str, stop: &impl Fn() -> bool) -> Result<String, String> {
    let body = json!({"model": model, "stream": false, "max_tokens": SUMMARY_MAX_TOKENS, "messages": [{"role": "user", "content": prompt}]});
    let started = crate::store::now();
    let mut attempt = 0u32;
    loop {
        let r = if api::has_session() {
            api::web_post("/web/chat/completions", &body, 300).await
        } else {
            api::guest_post("/anon/chat/completions", &body, 300).await
        };
        match r {
            Ok(v) => {
                let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
                if text.chars().count() < MIN_SUMMARY_CHARS {
                    return Err("The summary came back empty.".into());
                }
                return Ok(text);
            }
            Err(e) => {
                let status = api::status_of(&e).0;
                let busy = status == 429 && crate::store::now() - started < 120_000;
                let flaky = matches!(status, 0 | 502 | 503 | 504) && attempt < 2;
                if (busy || flaky) && !stop() {
                    attempt += 1;
                    let mut left = if busy { 15_000 } else { 2000 * attempt };
                    while left > 0 && !stop() {
                        crate::sleep(left.min(500)).await;
                        left = left.saturating_sub(500);
                    }
                    continue;
                }
                return Err(api::msg(e));
            }
        }
    }
}

/// A summary of `msgs`, built chunk by chunk so no request is near the context.
/// `progress(done, total)` is called before each chunk.
pub async fn summarize(model: &str, msgs: &[Value], context: u64, stop: &impl Fn() -> bool, progress: &mut impl FnMut(usize, usize)) -> Result<String, String> {
    let max = ((context as f64 * CHUNK_SHARE) as usize * CHARS_PER_TOKEN).max(4000);
    let parts = chunks(&transcript(msgs), max);
    let mut summary = String::new();
    for (i, part) in parts.iter().enumerate() {
        if stop() {
            return Err("Stopped by the user.".into());
        }
        progress(i, parts.len());
        let prompt = format!("{PROMPT}\n\n<summary_so_far>\n{summary}\n</summary_so_far>\n\n<next_part>\n{part}\n</next_part>");
        summary = match request(model, &prompt, stop).await {
            Ok(s) => s,
            Err(_) if !stop() => request(model, &prompt, stop).await?,
            Err(e) => return Err(e),
        };
    }
    Ok(summary)
}

/// Plain chat: compact the conversation before the latest user message when the history is
/// near the context (`force`: the model already refused it as too long). Returns whether it did.
pub async fn if_needed(mut app: App, model: &str, context: u64, force: bool, stop: &impl Fn() -> bool, note: &mut impl FnMut(String)) -> Result<bool, String> {
    let conv = app.conv.peek().clone();
    if !force && (tokens(&crate::chat::api_history(&conv, model, false)) as f64) < COMPACT_AT * context as f64 {
        return Ok(false);
    }
    let through = match conv.messages.last() {
        Some(m) if m.role == "user" => conv.messages.len() - 1,
        _ => conv.messages.len(),
    };
    if through < 1 {
        return Ok(false);
    }
    let mut before = conv.clone();
    before.messages.truncate(through);
    let prefix = crate::chat::api_history(&before, model, false);
    note("Summarizing earlier messages to fit the model’s context…".into());
    let summary = summarize(model, &prefix, context, stop, &mut |done, total| {
        if total > 1 {
            note(format!("Summarizing earlier messages to fit the model’s context… ({} of {total})", done + 1));
        }
    })
    .await?;
    app.conv.write().compaction = Some(crate::chat::Compaction { summary, through });
    note("Earlier messages were summarized to fit this model’s context.".into());
    Ok(true)
}
