//! Code Mode: the model gets one tool, `run_code`, and its Python or JavaScript runs on the phone
//! in a sandbox WebView of its own (see MainActivity.kt), never in the app's WebView. Code reaches
//! the outside only through `tools.*`, which comes back here: web search, and paid tools billed to
//! the balance within the reply's budget. The server says what's on, the budgets and the system
//! prompt (`GET /code_mode`), so the app and the web tell the model the same thing.
//!
//! A reply is an agent loop: stream a completion; if it asks to run code, run it, send the result
//! back, and repeat until the model answers in words. The tool turns are kept on the message
//! (`trace`) so the next request can replay them.

use crate::chat::{self, Msg};
use crate::{App, api, bg, js, sleep};
use dioxus::prelude::*;
use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use futures_util::{StreamExt, future::{self, Either}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_STEPS: usize = 10;
const RUN_TIMEOUT_MS: u32 = 180_000;
const MAX_TOOL_CALLS: usize = 40;

/// The server's Code Mode settings for this account.
#[derive(Clone, PartialEq, Default)]
pub struct Config {
    pub enabled: bool,
    /// Where the sandbox pages are (this server, or a separate origin).
    pub base: String,
    pub paid_tools: bool,
    pub web_search: bool,
    pub reply_budget: f64,
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
        reply_budget: v["reply_budget_usd"].as_f64().unwrap_or(0.0),
        per_call: v["per_call_usd"].as_f64().unwrap_or(0.0),
        result_chars: v["result_chars"].as_u64().unwrap_or(12_000) as usize,
        system_prompt: v["system_prompt"].as_str().unwrap_or("").to_string(),
        run_code_tool: v["run_code_tool"].clone(),
    });
}

/// Code runs for this model: Code Mode is on and the model can call tools.
pub fn active(app: &App, model: &str) -> bool {
    app.code.peek().enabled && app.models.peek().iter().find(|m| m.id == model).is_none_or(|m| m.tools)
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
async fn ensure(rx: &mut UnboundedReceiver<Value>, base: &str) -> Result<(), String> {
    if READY.load(Ordering::SeqCst) {
        return Ok(());
    }
    js(&format!("window.android && android.sandboxStart && android.sandboxStart({})", serde_json::to_string(base).unwrap_or_default()));
    let until = crate::store::now() + 30_000;
    while crate::store::now() < until {
        if let Some(v) = next(rx, 1000).await
            && v["type"] == "ready"
        {
            READY.store(true, Ordering::SeqCst);
            return Ok(());
        }
    }
    Err("The code sandbox didn't start. Check your connection and try again.".into())
}

/// The reply's spending and tools.
struct Turn {
    paid: bool,
    web: bool,
    budget: f64,
    per_call: f64,
    spent: f64,
    result_chars: usize,
}

async fn tool(name: &str, args: &Value, t: &mut Turn) -> Result<Value, String> {
    let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
    match name {
        "budget" => Ok(json!({
            "spent_usd": t.spent,
            "remaining_usd": if t.paid { (t.budget - t.spent).max(0.0) } else { 0.0 },
            "per_call_max_usd": if t.paid { t.per_call } else { 0.0 },
        })),
        "web_search" if t.web => api::web_post("/web/tools/web_search", &json!({"query": s("query")}), 60).await,
        "web_search" => Err("Web search is not available.".into()),
        "search" | "inspect" | "call" if !t.paid => Err("Paid tools need a signed-in Obscurify account.".into()),
        "search" => api::web_post("/web/tools/search", &json!({"query": s("query"), "limit": args["limit"].as_u64().unwrap_or(8)}), 60).await.map(|v| v["tools"].clone()),
        "inspect" => api::web_post("/web/tools/inspect", &json!({"id": s("id")}), 60).await,
        "call" => {
            let remaining = t.budget - t.spent;
            if remaining < 0.000_001 {
                return Err(format!("The paid-tool budget for this reply (${:.2}) is used up.", t.budget));
            }
            let asked = args["max_cost_usd"].as_f64().filter(|a| *a > 0.0).unwrap_or(t.per_call);
            let max = t.per_call.min(remaining).min(asked);
            let v = api::web_post("/web/tools/call", &json!({"id": s("id"), "input": args["input"].clone(), "max_cost_usd": (max * 1e6).round() / 1e6}), 170).await?;
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
    if let Err(e) = ensure(rx, base).await {
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
    while result.is_none() {
        if stop() {
            post(&json!({"type": "reset"}));
            result = Some(Err("Stopped by the user.".into()));
            break;
        }
        if crate::store::now() - started > RUN_TIMEOUT_MS as i64 {
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
                    tool(&name, &ev["args"], t).await
                };
                let r = msg.runs.last_mut().unwrap();
                if name != "budget" && let Some(u) = r.tools.last_mut() {
                    u.ok = Some(matches!(&reply, Ok(v) if v.get("ok") != Some(&json!(false))));
                    u.cost = reply.as_ref().ok().and_then(|v| v["cost_usd"].as_f64()).unwrap_or(0.0);
                    r.cost += u.cost;
                }
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
pub fn turn(mut app: App) {
    let model = app.settings.peek().model.clone();
    let cfg = app.code.peek().clone();
    app.stop.set(false);
    app.live.set(Some(Msg { role: "assistant".into(), model: model.clone(), ..Default::default() }));
    js("const m = document.getElementById('msgs'); if (m) { m.dataset.pin = 1; m.scrollTop = m.scrollHeight }");
    bg(async move {
        let (tx, mut rx) = unbounded();
        *EVENTS.lock().unwrap() = Some(tx);
        let mut t = Turn {
            paid: cfg.paid_tools,
            web: cfg.web_search,
            budget: cfg.reply_budget,
            per_call: cfg.per_call.min(cfg.reply_budget),
            spent: 0.0,
            result_chars: cfg.result_chars,
        };
        let mut history = vec![json!({"role": "system", "content": cfg.system_prompt})];
        history.extend(chat::wire(&app.conv.peek().messages, &model, true));
        let mut trace: Vec<Value> = vec![];
        let mut msg = app.live.peek().clone().unwrap();
        let mut drawn = 0i64;
        let mut show = move |m: &Msg| {
            if crate::store::now() - drawn > 60 {
                drawn = crate::store::now();
                app.live.set(Some(m.clone()));
            }
        };
        let stop = move || *app.stop.peek();

        for step in 0..MAX_STEPS {
            let mut body = json!({"model": model, "messages": history.iter().chain(trace.iter()).collect::<Vec<_>>(), "stream": true, "tools": [cfg.run_code_tool]});
            if step == MAX_STEPS - 1 {
                body["tool_choice"] = json!("none");
            }
            msg.reasoning.clear();
            let step_out = chat::stream(&body, &mut msg, &stop, &mut show).await;
            let calls = match step_out {
                Err(e) => { msg.error = true; msg.content = e; break }
                Ok(s) if s.tool_calls.is_empty() => { msg.details = s.details; break }
                Ok(s) => {
                    let mut turn_msg = json!({"role": "assistant", "content": if msg.content.is_empty() { Value::Null } else { json!(msg.content) }, "tool_calls": s.tool_calls});
                    if !s.details.is_empty() {
                        turn_msg["reasoning_details"] = json!(s.details);
                    }
                    trace.push(turn_msg);
                    s.tool_calls
                }
            };
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
            }
            if stop() {
                break;
            }
        }
        *EVENTS.lock().unwrap() = None;
        if msg.content.is_empty() && msg.runs.is_empty() && !msg.error && !stop() {
            msg.error = true;
            msg.content = "No answer came back. Try again or pick another model.".into();
        }
        msg.trace = trace;
        if !msg.content.is_empty() || !msg.runs.is_empty() {
            app.conv.write().messages.push(msg);
        }
        app.live.set(None);
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
