//! Code Mode: the model gets one tool, `run_code`, and its Python or JavaScript runs on the phone
//! in a sandbox WebView of its own (see MainActivity.kt), never in the app's WebView. Code reaches
//! the outside only through `tools.*`, which comes back here: web search, and paid tools billed to
//! the balance. The server says what's on, the per-call cap and the system
//! prompt (`GET /code_mode`), so the app and the web tell the model the same thing.
//!
//! A reply is an agent loop: stream a completion; if it asks to run code, run it, send the result
//! back, and repeat until the model answers in words. The tool turns are kept on the message
//! (`trace`) so the next request can replay them.
//!
//! Requests are append-only so the provider's prompt cache keeps hitting: nothing already sent is
//! ever changed, and each step's messages are a prefix of the next step's. When a conversation
//! nears the model's context, it's compacted once: the summary replaces the older messages, which
//! costs one cache miss, and then it's append-only again. Old tool results are never trimmed.

use crate::chat::{self, Msg};
use crate::{App, api, bg, js, sleep};
use dioxus::prelude::*;
use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::{StreamExt, future::{self, Either}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_STEPS: usize = 100;
/// Times one failed step is asked again before the answer stops and offers Continue.
const MAX_RETRIES: u32 = 2;
const RUN_TIMEOUT_MS: u32 = 30 * 60 * 1000;
const MAX_TOOL_CALLS: usize = 200;
/// Compact when a step has used this share of the model's context.
const COMPACT_AT: f64 = 0.70;
/// The recent tool work kept word for word through a compaction, as a share of the context.
const TAIL_SHARE: f64 = 0.15;
pub(crate) const DEFAULT_CONTEXT: u64 = 128_000;
const MIN_SUMMARY_CHARS: usize = 200;
const MAX_COMPACT_TRIES: u32 = 3;
/// Times one reply tells the model its request failed, then stops and offers Continue.
const MAX_FED_BACK: u32 = 2;
/// Times one reply asks the model to redo a tool call it wrote out unreadably.
const MAX_REPAIRS: u32 = 3;
const SUMMARY_PROMPT: &str = "Summarize this conversation so the work can continue from your summary alone: everything above will be replaced by it. Reply with text only and do not call any tools. Keep all of the following: the user's goals, and their latest request quoted verbatim; the decisions made and why; what was tried and the results, including failures; exact values, IDs, URLs and file names; what paid tools returned; what Python state exists (variables, imports, functions); and what is left to do. If there is an earlier summary above, carry over everything in it that still matters.";

/// The server's Code Mode settings for this account.
#[derive(Clone, PartialEq, Default)]
pub struct Config {
    pub enabled: bool,
    /// Where the sandbox pages are (this server, or a separate origin).
    pub base: String,
    pub paid_tools: bool,
    pub web_search: bool,
    /// Tools can be looked up (guests can too; running a paid one takes an account).
    pub catalog: bool,
    pub per_call: f64,
    pub result_chars: usize,
    pub system_prompt: String,
    pub run_code_tool: Value,
}

pub async fn load(mut app: App) {
    let Ok(v) = api::get("/code_mode").await else { return };
    let enabled = v["enabled"] == true && v["system_prompt"].is_string();
    let origin = v["sandbox_origin"].as_str().unwrap_or("self");
    app.code.set(Config {
        enabled,
        base: if origin == "self" { api::BASE.to_string() } else { origin.trim_end_matches('/').to_string() },
        paid_tools: v["paid_tools"] == true,
        web_search: v["web_search"] == true,
        catalog: v["tool_catalog"] == true,
        per_call: v["per_call_usd"].as_f64().unwrap_or(0.0),
        result_chars: v["result_chars"].as_u64().unwrap_or(12_000) as usize,
        system_prompt: v["system_prompt"].as_str().unwrap_or("").to_string(),
        run_code_tool: v["run_code_tool"].clone(),
    });
}

/// Code can run for this model: Code Mode is on and the model can call tools.
/// What kind of file a media URL is, from its extension. `save`-able media are only ever shown,
/// never parsed.
fn media_ext(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

pub fn is_video(url: &str) -> bool { matches!(media_ext(url).as_str(), "mp4" | "webm" | "mov" | "m4v") }
pub fn is_audio(url: &str) -> bool { matches!(media_ext(url).as_str(), "mp3" | "wav" | "m4a" | "ogg") }

/// The file's name as the button shows it, shortened.
pub fn media_filename(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("file");
    let name = name.replace('%', "");
    if name.chars().count() > 28 { format!("{}…", name.chars().take(27).collect::<String>()) } else { name }
}

pub fn capable(app: &App, model: &str) -> bool {
    app.code.peek().enabled && app.models.peek().iter().find(|m| m.id == model).is_none_or(|m| m.tools)
}

/// Code runs for this model: it can, and the Tools switch is on.
pub fn active(app: &App, model: &str) -> bool {
    capable(app, model) && app.settings.peek().tools
}

// ---- what a run looks like, kept on the message ----

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Page {
    pub title: String,
    pub html: String,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct ToolUse {
    pub name: String,
    pub detail: String,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub ok: Option<bool>,
}

/// Media URLs named anywhere in a tool's answer, deduplicated, the first few only. A generated
/// video or image comes back as a CDN link (MiniMax, Alibaba, Replicate), and it expires.
fn media_urls(value: &Value) -> Vec<String> {
    fn ext_ok(url: &str) -> bool {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let marker = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        matches!(marker.as_str(), "mp4" | "webm" | "mov" | "m4v" | "png" | "jpg" | "jpeg" | "webp" | "gif" | "mp3" | "wav" | "m4a" | "ogg")
    }
    fn walk(v: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 6 || out.len() >= 4 { return }
        match v {
            Value::String(s) => {
                for raw in s.split_whitespace() {
                    let url: String = raw.trim_matches(|c: char| matches!(c, '"' | ',' | ')' | ']' | ';' | '\'')).to_string();
                    if url.starts_with("https://") && url.len() < 2048 && ext_ok(&url) && !out.contains(&url) {
                        out.push(url);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out, depth + 1)),
            Value::Object(map) => map.values().for_each(|i| walk(i, out, depth + 1)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut out, 0);
    out
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Run {
    /// What the model said before asking for this run.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intro: String,
    pub language: String,
    pub code: String,
    #[serde(default)]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Video, audio or a remote image the code or a paid tool produced, as https URLs the
    /// WebView loads with the user's own connection (the sandbox has no network).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<Page>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolUse>,
    #[serde(default)]
    pub cost: f64,
}

// ---- the sandbox ----

/// Where MainActivity's sandbox messages go (`obsSandbox` in the bridge): the running reply.
static EVENTS: Mutex<Option<futures_channel::mpsc::UnboundedSender<Value>>> = Mutex::new(None);
static READY: AtomicBool = AtomicBool::new(false);

/// A message from the sandbox (JSON text), from the bridge.
pub fn on_sandbox(text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    if let Some(tx) = EVENTS.lock().unwrap().as_ref() {
        let _ = tx.unbounded_send(v);
    }
}

fn post(v: &Value) {
    let text = serde_json::to_string(&v.to_string()).unwrap_or_default();
    js(&format!("window.android && android.sandboxPost && android.sandboxPost({text})"));
}

async fn next(rx: &mut UnboundedReceiver<Value>, ms: u32) -> Option<Value> {
    match future::select(rx.next(), Box::pin(sleep(ms))).await {
        Either::Left((v, _)) => v,
        Either::Right(_) => None,
    }
}

/// Starts the sandbox if it isn't running and waits for it to be ready.
async fn ensure(rx: &mut UnboundedReceiver<Value>, base: &str, mut app: App, stop: &impl Fn() -> bool) -> Result<(), String> {
    if READY.load(Ordering::SeqCst) {
        return Ok(());
    }
    js(&format!("window.android && android.sandboxStart && android.sandboxStart({})", serde_json::to_string(base).unwrap_or_default()));
    // The sandbox's pages come from the server, so with no connection it keeps trying. With the
    // server reachable, MAX_LOST_WAITS restarts (~2 min) and it stops with an error instead: a
    // sandbox that won't start used to hold the chat on "Waiting for Obscurify" for good.
    let mut asked = crate::store::now();
    let mut restarts = 0u32;
    while !stop() {
        if let Some(v) = next(rx, 1000).await
            && v["type"] == "ready"
        {
            READY.store(true, Ordering::SeqCst);
            app.waiting.set(false);
            return Ok(());
        }
        if crate::store::now() - asked > 20_000 {
            if api::reachable().await {
                restarts += 1;
                if restarts > MAX_LOST_WAITS {
                    app.waiting.set(false);
                    return Err("The code sandbox would not start. Closing and reopening the app restarts it.".into());
                }
            }
            app.waiting.set(true);
            asked = crate::store::now();
            js(&format!("window.android && android.sandboxStart && android.sandboxStart({})", serde_json::to_string(base).unwrap_or_default()));
        }
    }
    app.waiting.set(false);
    Err("Stopped by the user.".into())
}

/// The reply's spending and tools.
struct Turn {
    paid: bool,
    catalog: bool,
    web: bool,
    per_call: f64,
    spent: f64,
    result_chars: usize,
    app: App,
}

/// Where a tool call in DeepSeek's own markup starts: `<｜DSML｜invoke name="run_code">`.
pub(crate) fn dsml_start(t: &str) -> Option<usize> {
    let at = t.find("DSML")?;
    t[..at].rfind('<').filter(|open| at - open <= 8)
}

/// The arguments of a tool call in that markup. Each parameter runs to its closing tag; code can
/// have `</` in it, but not this bar.
fn dsml_call(t: &str) -> Option<Value> {
    let at = dsml_start(t)?;
    let t = &t[at..];
    if !t.contains("invoke name=\"run_code\"") {
        return None;
    }
    let param = |name: &str| {
        let rest = &t[t.find(&format!("parameter name=\"{name}\""))?..];
        let body = &rest[rest.find('>')? + 1..];
        let end = body.find("</\u{ff5c}").or_else(|| body.find("</|")).unwrap_or(body.len());
        Some(body[..end].trim_matches('\n').to_string())
    };
    Some(json!({"language": param("language").unwrap_or_else(|| "python".into()), "code": param("code")?}))
}

/// Text that is a tool call, or an attempt at one, and not an answer.
fn written_call(content: &str) -> bool {
    let t = content.trim_start();
    dsml_start(content).is_some() || t.starts_with("<function") || t.starts_with("<tool_call") || t.starts_with("<invoke")
}

/// Some models (Llama 3.3 among them) often write a tool call as text instead of making it:
/// `<function=run_code>{"code": …}</function>`, `<function-run_code{…}}`, with no braces, or the bare JSON. Read
/// as an answer it runs nothing and shows gibberish, so it becomes the call it meant to be.
fn text_tool_call(content: &str, step: usize) -> Option<Value> {
    if let Some(args) = dsml_call(content) {
        return Some(json!({"id": format!("text_call_{step}"), "type": "function", "function": {"name": "run_code", "arguments": args.to_string()}}));
    }
    let t = content.trim();
    if !(t.starts_with("<function") || t.starts_with("<tool_call") || t.starts_with('{')) {
        return None;
    }
    if !t.chars().take(60).collect::<String>().contains("run_code") && !t.starts_with('{') {
        return None;
    }
    let first = t.find('{').and_then(|at| serde_json::Deserializer::from_str(&t[at..]).into_iter::<Value>().next()?.ok());
    // Or with no braces at all: <function(run_code)>"code": "…", "language": "python"</function>
    let v = first.or_else(|| {
        let start = t.find("\"code\"").or_else(|| t.find("\"language\""))?;
        let end = t.rfind("</").unwrap_or(t.len());
        serde_json::from_str(&format!("{{{}}}", t.get(start..end)?.trim().trim_end_matches(','))).ok()
    })?;
    let args = ["parameters", "arguments"].iter().map(|k| &v[*k]).find(|a| a.is_object()).unwrap_or(&v);
    args["code"].as_str()?;
    if t.starts_with('{') && !(v["name"] == "run_code" || v["language"].is_string()) {
        return None;
    }
    Some(json!({"id": format!("text_call_{step}"), "type": "function", "function": {"name": "run_code", "arguments": args.to_string()}}))
}

/// A lost connection is waited out for as long as it takes, until Stop. A rate limit isn't: it may
/// be a plan's limit that lasts for hours, so after this long the answer stops and offers Continue.
const RATE_WAIT_MS: i64 = 5 * 60 * 1000;
/// A busy paid tool (a 429) is waited out this long, not longer: the model is better off hearing
/// it and trying something else than the chat sitting on "Waiting" (customer report 2026-11-10).
const TOOL_BUSY_MS: i64 = 2 * 60 * 1000;

/// Failures from a reachable server (a 502/503/504, a stream cut short, a tool's 503) waited on
/// before giving up: ~45 s with wait_a_bit's 1, 2, 4, 8, 15, 15 s. Only a lost connection (no
/// answer at all) is waited out for as long as it takes. A web search answering 503 used to hold
/// a run, and the chat, on "Waiting for Obscurify" for good (customer report 2026-11-09).
pub const MAX_LOST_WAITS: u32 = 6;

/// Whether to wait and try again, and waits if so: always while `offline`, otherwise up to
/// MAX_LOST_WAITS times (counted in `waits`).
pub async fn wait_again(mut app: App, delay: &mut u32, waits: &mut u32, offline: bool) -> bool {
    if !offline {
        if *waits >= MAX_LOST_WAITS {
            app.waiting.set(false);
            return false;
        }
        *waits += 1;
    }
    app.waiting.set(true);
    wait_a_bit(app, delay).await;
    true
}

/// Waits out a pause before trying again, without holding up Stop. Grows up to 15 seconds.
pub async fn wait_a_bit(app: App, delay: &mut u32) {
    let mut left = *delay;
    while left > 0 && !*app.stop.peek() {
        crate::sleep(left.min(500)).await;
        left = left.saturating_sub(500);
    }
    *delay = (*delay * 2).min(15_000);
}

/// A tool call to the server: the signed-in route, or the guest one. A lost connection doesn't fail
/// it: it waits and asks again until the answer comes. A paid call carries a request id, so the
/// server runs it once however many times it's asked, and it's safe to ask again.
async fn tool_post(mut app: App, path: &str, body: &Value, timeout: u64) -> Result<Value, String> {
    let mut delay = 1000;
    let mut waits = 0;
    let mut limited_since = None;
    loop {
        let r = if api::has_session() {
            api::web_post(&format!("/web/tools{path}"), body, timeout).await
        } else {
            api::guest_post(&format!("/anon/tools{path}"), body, timeout).await
        };
        // Busy: nothing ran. It's waited out for two minutes, since it may not pass by itself.
        if let Err(e) = &r && api::status_of(e).0 == 429 {
            let since = *limited_since.get_or_insert_with(crate::store::now);
            if crate::store::now() - since > TOOL_BUSY_MS {
                app.waiting.set(false);
                return Err("Tools are busy right now (asked for 2 minutes). Use another way, or carry on without it.".into());
            }
        }
        match r {
            // A paid call's 503 means the tool itself timed out and may have charged: not repeated.
            Err(e) if api::transient(&e) && !(path == "/call" && api::status_of(&e).0 == 503) && !*app.stop.peek() => {
                // A rate limit has its own limit above; no answer at all is a lost connection.
                let status = api::status_of(&e).0;
                if status == 429 {
                    app.waiting.set(true);
                    wait_a_bit(app, &mut delay).await;
                } else if !wait_again(app, &mut delay, &mut waits, status == 0).await {
                    return Err(format!("The tool request kept failing ({}). Try again later, or carry on without it.", api::msg(e)));
                }
            }
            other => {
                app.waiting.set(false);
                return other.map_err(api::msg);
            }
        }
    }
}

async fn tool(name: &str, args: &Value, t: &mut Turn) -> Result<Value, String> {
    let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
    match name {
        "budget" => Ok(json!({
            "spent_usd": t.spent,
            "suggested_max_usd": if t.paid { t.per_call } else { 0.0 },
            "per_call_max_usd": if t.paid { t.per_call } else { 0.0 },
        })),
        "web_search" if t.web => tool_post(t.app, "/web_search", &json!({"query": s("query")}), 120).await,
        "web_search" => Err("Web search is not available.".into()),
        "search" | "inspect" if !t.catalog => Err("Tools are not available right now.".into()),
        "call" if !t.paid => Err("Running this tool needs a signed-in Obscurify account.".into()),
        "search" => tool_post(t.app, "/search", &json!({"query": s("query"), "limit": args["limit"].as_u64().unwrap_or(8)}), 60).await.map(|v| v["tools"].clone()),
        "inspect" => tool_post(t.app, "/inspect", &json!({"id": s("id")}), 60).await,
        "call" => {
            // The balance is the limit; `per_call` is only what an app of this vintage sends when
            // the model names no cap, and the server holds calls inside its own ceiling anyway.
            let asked = args["max_cost_usd"].as_f64().filter(|a| *a > 0.0);
            let max = match asked {
                Some(a) => Some(a),
                None if t.per_call > 0.0 => Some(t.per_call),
                None => None,
            };
            let request_id = format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
            let cap = max.map(|m| json!((m * 1e6).round() / 1e6)).unwrap_or(Value::Null);
            let v = tool_post(t.app, "/call", &json!({"id": s("id"), "input": args["input"].clone(), "max_cost_usd": cap, "request_id": request_id}), 170).await?;
            t.spent += v["cost_usd"].as_f64().unwrap_or(0.0);
            Ok(v)
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn detail(name: &str, args: &Value) -> String {
    match name {
        "web_search" | "search" => format!("“{}”", args["query"].as_str().unwrap_or("").chars().take(80).collect::<String>()),
        "inspect" | "call" => args["id"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Runs one piece of code, answering its tool calls; `show` redraws the reply.
async fn execute(rx: &mut UnboundedReceiver<Value>, msg: &mut Msg, t: &mut Turn, base: &str, stop: impl Fn() -> bool, show: &mut impl FnMut(&Msg)) -> String {
    let started = crate::store::now();
    let run_id = started;
    let (language, code) = {
        let r = msg.runs.last().unwrap();
        (r.language.clone(), r.code.clone())
    };
    if let Err(e) = ensure(rx, base, t.app, &stop).await {
        let r = msg.runs.last_mut().unwrap();
        r.done = true;
        r.error = e.clone();
        r.status = "Error".into();
        return format!("Run failed.\n\nerror:\n{e}");
    }
    post(&json!({"type": "run", "runId": run_id, "language": language, "code": code}));
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut tools = 0usize;
    let mut result: Option<Result<Value, String>> = None;
    let mut timed_out = false;
    // The limit is for the code itself: a slow tool or a wait for the connection doesn't count.
    let mut tool_ms = 0i64;
    while result.is_none() {
        if stop() {
            post(&json!({"type": "reset"}));
            result = Some(Err("Stopped by the user.".into()));
            break;
        }
        if crate::store::now() - started - tool_ms > RUN_TIMEOUT_MS as i64 {
            post(&json!({"type": "reset"}));
            timed_out = true;
            result = Some(Err(format!("Timed out after {} seconds. The sandbox was restarted, so Python state was lost.", RUN_TIMEOUT_MS / 1000)));
            break;
        }
        let Some(ev) = next(rx, 500).await else { continue };
        if ev["type"] == "fatal" {
            READY.store(false, Ordering::SeqCst);
            result = Some(Err(ev["message"].as_str().unwrap_or("The sandbox crashed.").to_string()));
            break;
        }
        if ev["runId"].as_i64() != Some(run_id) {
            continue;
        }
        let r = msg.runs.last_mut().unwrap();
        match ev["type"].as_str().unwrap_or("") {
            "stdout" => { let s = ev["text"].as_str().unwrap_or(""); stdout.push_str(s); r.output.push_str(s) }
            "stderr" => { let s = ev["text"].as_str().unwrap_or(""); stderr.push_str(s); r.output.push_str(s) }
            "status" => r.status = ev["text"].as_str().unwrap_or("").into(),
            "image" => if let Some(u) = ev["dataUrl"].as_str().filter(|u| (u.starts_with("data:image/png;base64,") || u.starts_with("data:image/jpeg;base64,")) && u.len() < 8 << 20) { r.images.push(u.into()) },
            "media" => if let Some(u) = ev["url"].as_str().filter(|u| u.starts_with("https://") && u.len() < 2048 && !r.media.iter().any(|m| m == u)) {
                r.media.push(u.into())
            },
            "html" => r.pages.push(Page { title: ev["title"].as_str().unwrap_or("").into(), html: ev["html"].as_str().unwrap_or("").into() }),
            "tool" => {
                let name = ev["name"].as_str().unwrap_or("").to_string();
                let call_id = ev["callId"].clone();
                tools += 1;
                let reply = if tools > MAX_TOOL_CALLS {
                    Err(format!("Too many tool calls in one run ({MAX_TOOL_CALLS})."))
                } else {
                    if name != "budget" {
                        r.tools.push(ToolUse { name: name.clone(), detail: detail(&name, &ev["args"]), ..Default::default() });
                        show(msg);
                    }
                    let t0 = crate::store::now();
                    let out = tool(&name, &ev["args"], t).await;
                    tool_ms += crate::store::now() - t0;
                    out
                };
                let r = msg.runs.last_mut().unwrap();
                // A generated video or picture comes back as a URL that expires: show it at once,
                // with a way to open or save it, rather than leaving a link inside JSON.
                if name == "call" && let Ok(v) = &reply {
                    for url in media_urls(&v["output"]) {
                        if !r.media.contains(&url) { r.media.push(url) }
                    }
                }
                if name != "budget" && let Some(u) = r.tools.last_mut() {
                    u.ok = Some(matches!(&reply, Ok(v) if v.get("ok") != Some(&json!(false))));
                    u.cost = reply.as_ref().ok().and_then(|v| v["cost_usd"].as_f64()).unwrap_or(0.0);
                    r.cost += u.cost;
                }
                msg.spent = t.spent;
                post(&match reply {
                    Ok(v) => json!({"type": "tool_result", "callId": call_id, "ok": true, "value": v}),
                    Err(e) => json!({"type": "tool_result", "callId": call_id, "ok": false, "error": e}),
                });
            }
            "done" => result = Some(if ev["ok"] == true { Ok(ev["value"].clone()) } else { Err(ev["error"].as_str().unwrap_or("The run failed.").to_string()) }),
            _ => {}
        }
        show(msg);
    }
    let secs = (crate::store::now() - started) as f64 / 1000.0;
    let r = msg.runs.last_mut().unwrap();
    r.done = true;
    let result = result.unwrap();
    r.ok = result.is_ok();
    r.status = match &result { Ok(_) => format!("Done · {secs:.1}s"), Err(_) if timed_out => "Timed out".into(), Err(_) => format!("Error · {secs:.1}s") };
    let mut parts = vec![match &result { Ok(_) => format!("Run finished in {secs:.1}s."), Err(_) if timed_out => "Run timed out.".into(), Err(_) => "Run failed.".into() }];
    if !stdout.is_empty() { parts.push(format!("stdout:\n{stdout}")) }
    if !stderr.is_empty() { parts.push(format!("stderr:\n{stderr}")) }
    match &result {
        Ok(v) if !v.is_null() && v.as_str() != Some("") => {
            let text = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
            r.output.push_str(&format!("{}→ {text}", if r.output.is_empty() { "" } else { "\n" }));
            parts.push(format!("result:\n{text}"));
        }
        Err(e) => { r.error = e.clone(); parts.push(format!("error:\n{e}")) }
        _ => {}
    }
    if !r.images.is_empty() { parts.push(format!("{} chart(s) were shown to the user.", r.images.len())) }
    if !r.media.is_empty() { parts.push(format!("{} image/video/audio file(s) were shown to the user with a save link: {}.", r.media.len(), r.media.join(", "))) }
    if !r.pages.is_empty() { parts.push(format!("{} HTML page(s) were shown to the user.", r.pages.len())) }
    if !r.tools.is_empty() {
        parts.push(format!("tool calls:\n{}", r.tools.iter().map(|u| format!("- {} {} -> {}{}", u.name, u.detail, if u.ok == Some(true) { "ok" } else { "failed" }, if u.cost > 0.0 { format!(", cost ${}", u.cost) } else { String::new() })).collect::<Vec<_>>().join("\n")));
    }
    if result.as_ref().is_ok_and(|v| v.is_null() || v.as_str() == Some("")) && stdout.is_empty() && stderr.is_empty() && r.images.is_empty() && r.pages.is_empty() {
        parts.push("(no output: print what you need to see)".into());
    }
    clip_middle(&parts.join("\n\n"), t.result_chars)
}

fn clip_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max * 3 / 5;
    let tail = max - head;
    format!("{}\n… [{} characters cut] …\n{}", chars[..head].iter().collect::<String>(), chars.len() - max, chars[chars.len() - tail..].iter().collect::<String>())
}

/// One Code Mode reply, streamed into `live`, then appended to the chat.
/// Picks up the last answer from what it got done, instead of asking again from the start.
pub fn resume(mut app: App) {
    if app.live.peek().is_some() {
        return;
    }
    let model = app.settings.peek().model.clone();
    if !active(&app, &model) {
        return app.say("Turn Tools on and pick a model with tools to continue.");
    }
    let Some(mut prev) = app.conv.write().messages.pop() else { return };
    prev.content.clear();
    prev.reasoning.clear();
    prev.interrupted.clear();
    prev.model = model;
    turn(app, Some(prev));
}

/// Retries a step that failed, unless asking again can't help.
fn worth_retrying(e: &str) -> bool {
    let e = e.to_lowercase();
    !["balance", "credit", "insufficient", "rate limit", "not support", "needs", "not available", "too long", "context"].iter().any(|w| e.contains(w))
}

pub(crate) fn too_long(e: &str) -> bool {
    let e = e.to_lowercase();
    ["too long", "too large", "too many tokens", "context length", "context window", "context_length", "maximum context"].iter().any(|w| e.contains(w))
}

/// Roughly how many tokens these messages are, leaving out images, which aren't counted by size.
fn estimate(msgs: &[Value]) -> u64 {
    let chars: usize = msgs.iter().map(|m| match m["content"].as_array() {
        Some(parts) => parts.iter().map(|p| p["text"].as_str().map_or(0, str::len)).sum(),
        None => m.to_string().len(),
    }).sum();
    chars as u64 / 4
}

/// The latest complete step groups (a step's assistant message and all its results, never split)
/// that fit in `budget` tokens. Always at least the latest one.
fn tail(trace: &[Value], budget: u64) -> Vec<Value> {
    let starts: Vec<usize> = trace.iter().enumerate().filter(|(_, m)| m["role"] != "tool").map(|(i, _)| i).collect();
    let mut from = trace.len();
    for (n, s) in starts.iter().enumerate().rev() {
        if n + 1 != starts.len() && estimate(&trace[*s..]) > budget {
            break;
        }
        from = *s;
    }
    trace[from..].to_vec()
}


/// Summarizes the conversation and starts the request history over from the summary. The request
/// has the same prefix and tools as the step it follows, so it reads from the cache. False if no
/// usable summary came back: nothing has changed then, and the run carries on appending.
#[allow(clippy::too_many_arguments)]
async fn compact(mut app: App, cfg: &Config, model: &str, context: u64, shrink: bool, history: &mut Vec<Value>, trace: &mut Vec<Value>, msg: &mut Msg, t: &mut Turn, stop: &impl Fn() -> bool) -> bool {
    let sent: Vec<Value> = history.iter().chain(trace.iter()).cloned().collect();
    // Too big for one summary request (or already refused as too long): summarize it in chunks
    // that each fit (compact.rs), so a history of any length compacts.
    let big = shrink || crate::compact::tokens(&sent) as f64 > 0.5 * context as f64;
    let summary = if big {
        match crate::compact::summarize(model, &sent, context, stop, &mut |_, _| {}).await {
            Ok(s) => s,
            Err(_) => return false,
        }
    } else {
        let mut messages = sent;
        messages.push(json!({"role": "user", "content": SUMMARY_PROMPT}));
        let body = json!({"model": model, "messages": messages, "stream": true, "stream_options": {"include_usage": true}, "tools": [cfg.run_code_tool]});
        let mut delay = 1000;
        let mut waits = 0;
        let mut retries = 0;
        loop {
            let mut out = Msg::default();
            let r = chat::stream(&body, &mut out, stop, &mut |_: &Msg| {}).await;
            if let Ok(s) = &r {
                t.spent += s.cost;
                msg.spent = t.spent;
            }
            match r {
                Ok(s) if !s.cut => {
                    app.waiting.set(false);
                    break chat::answer_text(&out.content);
                }
                other => {
                    let e = other.err().unwrap_or_else(|| "The connection dropped.".into());
                    if stop() { app.waiting.set(false); return false }
                    if (e == api::OFFLINE || e == api::UNAVAILABLE || e.contains("connection dropped"))
                        && wait_again(app, &mut delay, &mut waits, e == api::OFFLINE).await
                    {
                        continue;
                    }
                    app.waiting.set(false);
                    if retries >= MAX_RETRIES || !worth_retrying(&e) { return false }
                    retries += 1;
                    sleep(1500 * retries).await;
                }
            }
        }
    };
    if summary.chars().count() < MIN_SUMMARY_CHARS {
        return false;
    }
    let keep = (context as f64 * TAIL_SHARE) as u64;
    *trace = tail(trace, keep);
    {
        let mut c = app.conv.write();
        // The request this answers stays word for word, with its attachments, unless it's huge.
        let n = c.messages.len();
        let through = match c.messages.iter().rposition(|m| m.role == "user") {
            Some(u) if estimate(&chat::wire(&c.messages[u..=u], model, true)) <= keep => u,
            _ => n,
        };
        c.compaction = Some(chat::Compaction { summary, through });
    }
    *history = vec![json!({"role": "system", "content": cfg.system_prompt})];
    history.extend(chat::api_history(&app.conv.peek(), model, true));
    msg.compacted = true;
    true
}

/// On disk after every step: a killed app comes back to this, with Continue.
fn checkpoint(app: App, msg: &Msg, trace: &[Value]) {
    let mut c = app.conv.peek().clone();
    let mut m = msg.clone();
    m.trace = trace.to_vec();
    m.interrupted = "The app closed before this finished.".into();
    m.runs.iter_mut().filter(|r| !r.done).for_each(|r| { r.done = true; r.status = "Cut off".into() });
    c.messages.push(m);
    crate::store::save(&format!("chat-{}.json", c.id), &c);
}

/// One answer: the model and its runs, step by step. `from` is an earlier attempt to continue.
pub fn turn(mut app: App, from: Option<Msg>) {
    let model = app.settings.peek().model.clone();
    let cfg = app.code.peek().clone();
    app.stop.set(false);
    let mut first = from.unwrap_or_else(|| Msg { role: "assistant".into(), model: model.clone(), ..Default::default() });
    first.runs.iter_mut().filter(|r| !r.done).for_each(|r| { r.done = true; r.status = "Cut off".into() });
    let mut trace: Vec<Value> = chat::sound_trace(&std::mem::take(&mut first.trace));
    app.live.set(Some(first));
    js("document.body.dataset.working = 1; const m = document.getElementById('msgs'); if (m) { m.dataset.pin = 1; m.scrollTop = m.scrollHeight }; window.android && android.keepAwake && android.keepAwake(true); window.android && android.working && android.working(true)");
    bg(async move {
        let (tx, mut rx) = unbounded();
        *EVENTS.lock().unwrap() = Some(tx);
        let mut msg = app.live.peek().clone().unwrap();
        let mut t = Turn {
            paid: cfg.paid_tools,
            catalog: cfg.catalog,
            web: cfg.web_search,
            per_call: cfg.per_call,
            spent: if msg.spent > 0.0 { msg.spent } else { msg.runs.iter().map(|r| r.cost).sum() },
            result_chars: cfg.result_chars,
            app,
        };
        let mut history = vec![json!({"role": "system", "content": cfg.system_prompt})];
        history.extend(chat::api_history(&app.conv.peek(), &model, true));
        msg.spent = t.spent;
        let context = app.models.peek().iter().find(|m| m.id == model).map(|m| m.context).filter(|c| *c > 0).unwrap_or(DEFAULT_CONTEXT);
        let mut drawn = 0i64;
        let mut show = move |m: &Msg| {
            if crate::store::now() - drawn > 60 {
                drawn = crate::store::now();
                let mut m = m.clone();
                // A tool call in the making isn't for reading.
                if let Some(at) = dsml_start(&m.content) { m.content.truncate(at) }
                app.live.set(Some(m));
            }
        };
        let stop = move || *app.stop.peek();
        let mut failure: Option<String> = None;
        let mut retries = 0u32;
        let mut wait_since: Option<i64> = None;
        let mut lost_waits = 0u32;
        let mut delay = 1000u32;
        let done_steps = msg.steps.max(trace.iter().filter(|m| m["tool_calls"].is_array()).count()).min(MAX_STEPS - 1);
        // The tokens the last step used: what the next one starts from.
        let mut used: Option<u64> = None;
        let mut compact_tries = 0u32;
        let mut overflowed = false;

        let mut fed_back = 0u32;
        let mut repairs = 0u32;

        'steps: for step in done_steps..MAX_STEPS {
            if stop() {
                break;
            }
            let all = |h: &[Value], tr: &[Value]| h.iter().chain(tr.iter()).cloned().collect::<Vec<_>>();
            if compact_tries < MAX_COMPACT_TRIES && used.unwrap_or_else(|| estimate(&all(&history, &trace))) as f64 >= COMPACT_AT * context as f64 && !stop() {
                if compact(app, &cfg, &model, context, false, &mut history, &mut trace, &mut msg, &mut t, &stop).await {
                    used = None;
                    // Still over, with nothing left to summarize: don't do it again for nothing.
                    compact_tries = if estimate(&all(&history, &trace)) as f64 >= COMPACT_AT * context as f64 { MAX_COMPACT_TRIES } else { 0 };
                    app.live.set(Some(msg.clone()));
                    checkpoint(app, &msg, &trace);
                } else {
                    compact_tries += 1;
                }
                if stop() {
                    break;
                }
            }
            let mut body = json!({"model": model, "messages": all(&history, &trace), "stream": true, "stream_options": {"include_usage": true}, "tools": [cfg.run_code_tool]});
            if step == MAX_STEPS - 1 {
                body["tool_choice"] = json!("none");
            }
            let calls = loop {
                msg.reasoning.clear();
                msg.content.clear();
                let out = chat::stream(&body, &mut msg, &stop, &mut show).await.map(|mut s| {
                    if s.tool_calls.is_empty() && let Some(c) = text_tool_call(&msg.content, step) {
                        let words = dsml_start(&msg.content).unwrap_or(0);
                        msg.content.truncate(words);
                        s.tool_calls.push(c);
                    }
                    s
                });
                let problem = match &out {
                    Err(e) => Some(e.clone()),
                    Ok(s) if s.cut => Some("The connection dropped.".to_string()),
                    Ok(_) => None,
                };
                if let Ok(s) = &out {
                    t.spent += s.cost;
                    msg.spent = t.spent;
                }
                if let Some(e) = problem {
                    // Too long for the model: compact, once, and ask again.
                    if too_long(&e) && !overflowed && !stop() {
                        overflowed = true;
                        if compact(app, &cfg, &model, context, true, &mut history, &mut trace, &mut msg, &mut t, &stop).await {
                            used = None;
                            body["messages"] = json!(all(&history, &trace));
                            app.live.set(Some(msg.clone()));
                            checkpoint(app, &msg, &trace);
                            continue;
                        }
                    }
                    // No connection: wait for it for as long as it takes. A server that answers but
                    // keeps failing is waited on MAX_LOST_WAITS times, then reported with Continue.
                    let offline = e == api::OFFLINE;
                    let lost = offline || e == api::UNAVAILABLE || e.contains("connection dropped");
                    let lower = e.to_lowercase();
                    let limited = lower.contains("rate limit") || lower.contains("too many requests");
                    let since = *wait_since.get_or_insert_with(crate::store::now);
                    if limited && crate::store::now() - since < RATE_WAIT_MS && !stop() {
                        app.waiting.set(true);
                        wait_a_bit(app, &mut delay).await;
                        continue;
                    }
                    if lost && !stop() && wait_again(app, &mut delay, &mut lost_waits, offline).await {
                        continue;
                    }
                    let e = if lost && !offline { format!("Obscurify kept failing to answer this chat ({e}). Your chat is kept: press Continue to try again, or switch models.") } else { e };
                    app.waiting.set(false);
                    // The tool results are safe in `trace`: only this one step is asked again.
                    if !lost && retries < MAX_RETRIES && worth_retrying(&e) && !stop() {
                        retries += 1;
                        crate::sleep(1500 * retries).await;
                        continue;
                    }
                    // Still failing: the model hears about it and can do something different.
                    if !lost && !limited && !trace.is_empty() && fed_back < MAX_FED_BACK && worth_retrying(&e) && !stop() {
                        fed_back += 1;
                        retries = 0;
                        trace.push(json!({"role": "user", "content": format!("The last request to the model failed with this error:\n\n{e}\n\nIf something you sent caused it, change that, and carry on.")}));
                        body["messages"] = json!(all(&history, &trace));
                        checkpoint(app, &msg, &trace);
                        continue;
                    }
                    failure = Some(e);
                    break vec![];
                }
                app.waiting.set(false);
                wait_since = None;
                delay = 1000;
                let s = out.unwrap();
                used = s.tokens;
                // A tool call it wrote out but nobody can read: back to the model to make it properly.
                if s.tool_calls.is_empty() && written_call(&msg.content) && repairs < MAX_REPAIRS && step + 1 < MAX_STEPS && !stop() {
                    repairs += 1;
                    trace.push(json!({"role": "assistant", "content": msg.content}));
                    trace.push(json!({"role": "user", "content": "That tool call was written out as text, so it did not run. Call the run_code tool itself, with JSON arguments {\"language\": \"python\" | \"javascript\", \"code\": \"...\"}."}));
                    msg.content.clear();
                    msg.steps = step + 1;
                    checkpoint(app, &msg, &trace);
                    continue 'steps;
                }
                if s.tool_calls.is_empty() {
                    msg.details = s.details;
                    break vec![];
                }
                let mut turn_msg = json!({"role": "assistant", "content": if msg.content.is_empty() { Value::Null } else { json!(msg.content) }, "tool_calls": s.tool_calls});
                if !s.details.is_empty() {
                    turn_msg["reasoning_details"] = json!(s.details);
                }
                trace.push(turn_msg);
                msg.steps = step + 1;
                break s.tool_calls;
            };
            if calls.is_empty() {
                break;
            }
            retries = 0;
            checkpoint(app, &msg, &trace);
            let mut intro = std::mem::take(&mut msg.content);
            for call in calls {
                let id = call["id"].clone();
                let name = call["function"]["name"].as_str().unwrap_or("");
                let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
                let result = if name != "run_code" {
                    format!("There is no tool named \"{name}\". The only tool is run_code.")
                } else if !args["code"].is_string() {
                    "run_code needs JSON arguments {\"language\": \"python\" | \"javascript\", \"code\": \"...\"}.".to_string()
                } else {
                    msg.runs.push(Run {
                        intro: std::mem::take(&mut intro),
                        language: args["language"].as_str().unwrap_or("python").to_lowercase(),
                        code: args["code"].as_str().unwrap_or("").to_string(),
                        status: "Running…".into(),
                        ..Default::default()
                    });
                    app.live.set(Some(msg.clone()));
                    execute(&mut rx, &mut msg, &mut t, &cfg.base, stop, &mut show).await
                };
                trace.push(json!({"role": "tool", "tool_call_id": id, "content": result}));
                app.live.set(Some(msg.clone()));
                checkpoint(app, &msg, &trace);
            }
            if stop() {
                break;
            }
        }
        *EVENTS.lock().unwrap() = None;
        app.waiting.set(false);
        if let Some(e) = failure {
            if msg.content.is_empty() && msg.runs.is_empty() && trace.is_empty() {
                msg.error = true;
                msg.content = e;
            } else {
                msg.interrupted = e;
            }
        } else if msg.content.is_empty() && msg.runs.is_empty() && !stop() {
            msg.error = true;
            msg.content = "No answer came back. Try again or pick another model.".into();
        }
        msg.trace = trace;
        if !msg.content.is_empty() || !msg.runs.is_empty() || !msg.interrupted.is_empty() {
            app.conv.write().messages.push(msg);
        }
        app.live.set(None);
        js("document.body.dataset.working = 0; const f = document.getElementById('follow'); if (f) f.hidden = true; window.android && android.keepAwake && android.keepAwake(false); window.android && android.working && android.working(false)");
        chat::save_chat(app);
        app.refresh_balance();
    });
}

/// The ```html blocks in an answer that aren't pages a run already showed.
pub fn html_blocks(answer: &str, shown: &[&str]) -> Vec<String> {
    let norm = |s: &str| s.split_whitespace().collect::<String>();
    let shown: Vec<String> = shown.iter().map(|s| norm(s)).collect();
    let mut out = vec![];
    let mut rest = answer;
    while let Some(i) = rest.find("```html") {
        let after = &rest[i + 7..];
        let Some(nl) = after.find('\n') else { break };
        let body = &after[nl + 1..];
        let Some(end) = body.find("```") else { break };
        let html = body[..end].to_string();
        let n = norm(&html);
        let dup = shown.iter().any(|s| *s == n || (s.len().abs_diff(n.len()) * 10 < s.len().max(n.len()) && s.chars().take(400).eq(n.chars().take(400))));
        if html.trim().len() >= 20 && !dup && out.len() < 3 {
            out.push(html);
        }
        rest = &body[end + 3..];
    }
    out
}

/// Shows a page full screen, in a sandbox WebView of its own.
pub fn open_page(app: &App, title: &str, html: &str) {
    let base = app.code.peek().base.clone();
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    js(&format!("window.android && android.openPage && android.openPage({}, {}, {})", q(&base), q(title), q(html)));
}
