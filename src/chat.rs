//! Chat: the conversation, streaming answers, web search, images, the chats drawer and the model
//! picker. Chats live only on the phone (`chats.json` lists them, `chat-<id>.json` holds each), the
//! same as the web, which keeps them in the browser.

use crate::{Access, App, Model, Overlay, Screen, api, bg, code, js, store, usd};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Search {
    pub query: String,
    pub status: String,
    #[serde(default)]
    pub results: String,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Msg {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Files attached to a user message, as text (see files.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::Doc>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub searches: Vec<Search>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default)]
    pub error: bool,
    /// Why this answer stopped short. What it got done (text, runs, tool results) is kept, stays in
    /// the conversation, and Continue picks up from it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub interrupted: String,
    /// Code Mode: the code the model ran for this answer, and the tool turns to replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<crate::code::Run>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<Value>,
    /// What this answer has cost so far: the model's steps and the paid tools.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spent: f64,
    /// Model steps taken. Not the trace's length, which compaction shortens.
    #[serde(default, skip_serializing_if = "is_zero_steps")]
    pub steps: usize,
    /// Earlier work was summarized while this answer ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compacted: bool,
    /// Verify mode: each model request's receipt and what its check found (tee.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tee: Vec<crate::tee::Step>,
}

/// The answer being written when the app last ran, so a reopened app finishes it: the server
/// kept writing it (a background reply) and holds it for an hour.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Pending {
    pub reply: String,
    pub chat: String,
    pub model: String,
    /// A Code Mode step (the run carries on after it) rather than a plain answer.
    #[serde(default)]
    pub code: bool,
}

const PENDING: &str = "pending.json";

/// Remembers the answer now being written (not for temporary chats, which aren't kept).
pub(crate) fn remember(app: App, reply: &Option<String>, model: &str, code: bool) {
    let c = app.conv.peek();
    match reply {
        Some(r) if !c.temp => store::save(PENDING, &Pending { reply: r.clone(), chat: c.id.clone(), model: model.into(), code }),
        _ => store::remove(PENDING),
    }
}

pub(crate) fn forget_pending() { store::remove(PENDING) }

/// Stops the answer: here, and on the server, where it would otherwise carry on.
pub(crate) fn stop_answer(mut app: App) {
    app.stop.set(true);
    api::cancel_current();
    forget_pending();
}

/// On start: an answer the app was writing when it was closed or killed is picked up where the
/// server has it (from the start: the phone kept none of it), into its own chat.
pub fn finish_pending(mut app: App) {
    let p: Pending = store::load(PENDING);
    if p.reply.is_empty() || !api::has_session() || app.live.peek().is_some() {
        return;
    }
    if app.conv.peek().id != p.chat {
        let c = Conv::load(&p.chat);
        if c.id != p.chat { return forget_pending() }
        app.conv.set(c);
    }
    if !p.model.is_empty() && app.settings.peek().model != p.model {
        app.settings.write().model = p.model.clone();
    }
    if p.code {
        // The last step saved (a checkpoint) is where the run goes on from, with this reply.
        let last_is_answer = app.conv.peek().messages.last().is_some_and(|m| m.role == "assistant");
        let from = if last_is_answer { app.conv.write().messages.pop() } else { None };
        let from = from.map(|mut m| { m.interrupted.clear(); m.content.clear(); m.reasoning.clear(); m });
        crate::code::turn_following(app, from, Some(p.reply));
    } else if app.conv.peek().messages.last().is_some_and(|m| m.role == "user") {
        run_with(app, Some(p.reply));
    } else {
        forget_pending();
    }
}

fn is_zero(v: &f64) -> bool { *v == 0.0 }
fn is_zero_steps(v: &usize) -> bool { *v == 0 }

/// What's sent in place of the older messages once a conversation has outgrown the context. All of
/// it stays on screen and on disk: this changes only what the model is sent.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Compaction {
    pub summary: String,
    /// Messages before this index are covered by the summary.
    pub through: usize,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub updated: i64,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Conv {
    /// A temporary chat: never written to the phone, gone when the app closes or another chat opens.
    #[serde(skip)]
    pub temp: bool,
    pub id: String,
    pub title: String,
    pub messages: Vec<Msg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Compaction>,
}

impl Conv {
    pub fn new() -> Self { Conv { id: format!("c{}", store::now()), ..Default::default() } }
    pub fn load(id: &str) -> Self { let c: Conv = store::load(&format!("chat-{id}.json")); if c.id.is_empty() { Conv::new() } else { c } }
}

/// Saves the open chat and moves it to the top of the list.
pub(crate) fn save_chat(app: App) { save(app) }

fn save(mut app: App) {
    let c = app.conv.peek().clone();
    if c.messages.is_empty() || c.temp {
        return;
    }
    store::save(&format!("chat-{}.json", c.id), &c);
    let mut list = app.convs.write();
    list.retain(|m| m.id != c.id);
    list.insert(0, Meta { id: c.id.clone(), title: c.title.clone(), updated: store::now() });
    store::save("chats.json", &*list);
}

/// Runs `then` once no answer is streaming. One that is gets stopped first and is saved, as far
/// as it got, to its own chat (as on the web): switching used to do nothing until it finished.
fn after_answer(app: App, then: impl FnOnce(App) + 'static) {
    if app.live.peek().is_none() {
        return then(app);
    }
    stop_answer(app);
    bg(async move {
        for _ in 0..100 {
            if app.live.peek().is_none() {
                break;
            }
            crate::sleep(50).await;
        }
        then(app);
    });
}

/// A fresh chat; it starts on the first favorite model, like the web.
fn new_chat(app: App) {
    after_answer(app, start_new_chat);
}

fn start_new_chat(mut app: App) {
    if app.live.peek().is_some() {
        return;
    }
    // A new chat stays temporary while Temp is on, as on the web.
    let temp = app.conv.peek().temp;
    app.conv.set(Conv { temp, ..Conv::new() });
    let first = app.settings.peek().favorites.first().cloned();
    if let Some(f) = first {
        app.settings.write().model = f;
        app.save_settings();
    }
}

/// Temp on: a fresh chat that is never saved. Temp off: a fresh, ordinary chat.
/// Verify on: answers come from TEE models whose replies the phone checks; the picker lists only
/// those, and the model switches to one if it has to.
fn toggle_verify(mut app: App) {
    if !api::has_session() {
        return app.say("Sign in to verify answers");
    }
    let on = !app.settings.peek().verify;
    if on {
        let model = app.settings.peek().model.clone();
        let ok = app.models.peek().iter().any(|m| m.id == model && m.verifiable);
        if !ok {
            let Some(pick) = crate::tee::pick_model(&app) else { return app.say("No verifiable model is available right now") };
            app.settings.write().model = pick;
        }
    }
    app.settings.write().verify = on;
    app.save_settings();
    app.say(if on { "Verify on: each answer is checked on this phone" } else { "Verify off" });
}

fn toggle_temp(app: App) {
    after_answer(app, |mut app| {
        let temp = !app.conv.peek().temp;
        start_new_chat(app);
        app.conv.write().temp = temp;
    });
}

/// Deletes every chat and the chat settings from the phone. The session and the wallet
/// (vouchers not yet added to the balance, which are money) stay.
fn wipe(app: App) {
    after_answer(app, |mut app| {
        if let Ok(dir) = std::fs::read_dir(store::dir()) {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("chat-") && name.ends_with(".json") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        store::remove("chats.json");
        store::remove("settings.json");
        app.convs.set(vec![]);
        app.conv.set(Conv::new());
        app.settings.set(crate::Settings::default());
        app.overlay.set(Overlay::None);
        app.say("Chats and settings deleted from this phone");
    });
}

fn delete(mut app: App, id: &str) {
    store::remove(&format!("chat-{id}.json"));
    let mut list = app.convs.write();
    list.retain(|m| m.id != id);
    store::save("chats.json", &*list);
    if app.conv.peek().id == id {
        app.conv.set(Conv::new())
    }
}

/// `<think>…</think>` at the start of an answer is reasoning some models put inline.
fn split_think(s: &str) -> (&str, &str) {
    let t = s.trim_start();
    if let Some(rest) = t.strip_prefix("<think>") {
        return match rest.split_once("</think>") { Some((th, ans)) => (th, ans.trim_start()), None => (rest, "") };
    }
    ("", s)
}

/// An answer without any inline reasoning.
pub(crate) fn answer_text(s: &str) -> String { split_think(s).1.trim().to_string() }

/// Some providers stream whole snapshots instead of deltas, and some repeat a chunk.
pub(crate) fn merge(cur: &mut String, inc: &str) {
    if inc.is_empty() || cur.ends_with(inc) && !cur.is_empty() {
        return;
    }
    if !cur.is_empty() && inc.starts_with(cur.as_str()) {
        *cur = inc.to_string()
    } else {
        cur.push_str(inc)
    }
}

/// Fragments of one reasoning block share an `index`; their text is joined, late fields filled in.
pub(crate) fn merge_details(acc: &mut Vec<Value>, inc: &Value) {
    for item in inc.as_array().into_iter().flatten() {
        let Some(obj) = item.as_object() else { continue };
        let target = match item["index"].as_i64() {
            Some(i) => acc.iter_mut().position(|d| d["index"].as_i64() == Some(i)),
            None => acc.last().filter(|l| l["index"].is_null() && l["type"] == item["type"]).map(|_| acc.len() - 1),
        };
        let Some(t) = target else { acc.push(item.clone()); continue };
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            match (k.as_str(), v.as_str()) {
                ("text" | "summary" | "data", Some(s)) => { let old = acc[t][k].as_str().unwrap_or("").to_string(); acc[t][k] = json!(old + s) }
                _ => acc[t][k] = v.clone(),
            }
        }
    }
}

/// The history as the API wants it: text plus image parts for user turns, the clean answer for
/// assistant turns, and reasoning_details only when it's the same model (signed reasoning isn't portable).
/// A Code Mode answer replays its tool turns when the request offers tools (`tools`), and is
/// otherwise flattened into text: its code, what the code printed, and the answer.
pub(crate) fn wire(msgs: &[Msg], model: &str, tools: bool) -> Vec<Value> {
    msgs.iter().filter(|m| !m.error).flat_map(|m| {
        if m.role == "user" {
            let text = crate::files::for_model(&m.content, &m.files);
            if m.images.is_empty() {
                return vec![json!({"role": "user", "content": text})];
            }
            let mut parts = vec![json!({"type": "text", "text": text})];
            parts.extend(m.images.iter().map(|u| json!({"type": "image_url", "image_url": {"url": u}})));
            return vec![json!({"role": "user", "content": parts})];
        }
        let answer = split_think(&m.content).1.trim().to_string();
        if !m.trace.is_empty() && tools {
            let mut out: Vec<Value> = sound_trace(&m.trace).into_iter().map(|mut t| {
                if m.model != model && let Some(o) = t.as_object_mut() { o.remove("reasoning_details"); }
                t
            }).collect();
            if !answer.is_empty() {
                out.push(json!({"role": "assistant", "content": answer}));
            }
            return out;
        }
        let mut text = String::new();
        for r in &m.runs {
            if r.direct {
                text.push_str(&r.tools.iter().map(|u| format!("- {} {}\n", u.name, u.detail)).collect::<String>());
                continue;
            }
            text.push_str(&format!("```{}\n{}\n```\n", r.language, r.code));
            let o = r.output.trim();
            if !o.is_empty() { text.push_str(&format!("Output:\n```\n{}\n```\n", o.chars().take(2000).collect::<String>())) }
        }
        text.push_str(&answer);
        let mut v = json!({"role": "assistant", "content": text.trim()});
        if m.model == model && !m.details.is_empty() && m.runs.is_empty() {
            v["reasoning_details"] = json!(m.details);
        }
        vec![v]
    }).collect()
}

/// A conversation as the model is sent it: the summary, if there is one, then what came after it.
/// A user message always follows the acknowledgment, since strict providers refuse two assistant
/// messages in a row.
pub(crate) fn api_history(c: &Conv, model: &str, tools: bool) -> Vec<Value> {
    let Some(k) = c.compaction.as_ref().filter(|k| k.through <= c.messages.len()) else {
        return wire(&c.messages, model, tools);
    };
    let rest = wire(&c.messages[k.through..], model, tools);
    let mut out = vec![
        json!({"role": "user", "content": format!("Summary of the conversation so far:\n\n{}", k.summary)}),
        json!({"role": "assistant", "content": "Understood. Continuing."}),
    ];
    if rest.first().is_none_or(|m| m["role"] != "user") {
        out.push(json!({"role": "user", "content": "Continue."}));
    }
    out.extend(rest);
    out
}

/// Every tool call in a trace with its result: a trace saved in the middle of a run has a call
/// with none, and the API refuses that.
pub(crate) fn sound_trace(trace: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let mut open: Vec<Value> = vec![];
    for t in trace {
        if t["role"] == "tool" {
            let id = t["tool_call_id"].clone();
            open.retain(|o| *o != id);
        } else {
            out.extend(open.drain(..).map(|id| json!({"role": "tool", "tool_call_id": id, "content": "This run was cut off before it finished. Its result is unknown."})));
            open = t["tool_calls"].as_array().into_iter().flatten().map(|c| c["id"].clone()).collect();
        }
        out.push(t.clone());
    }
    out.extend(open.drain(..).map(|id| json!({"role": "tool", "tool_call_id": id, "content": "This run was cut off before it finished. Its result is unknown."})));
    out
}

/// What one streamed completion asked for besides its text.
pub(crate) struct Step {
    pub tool_calls: Vec<Value>,
    pub details: Vec<Value>,
    /// The stream ended without finishing: the text is partial and any tool calls are dropped.
    pub cut: bool,
    /// Tokens this step used (prompt + completion), if the server said.
    pub tokens: Option<u64>,
    pub cost: f64,
    /// Verify mode: the receipt, the route that answered, and the reply's bytes to check.
    pub tee: Option<(String, Option<String>, Vec<u8>)>,
}

/// Streams one completion into `msg` (content and reasoning), calling `show` as it grows. Returns
/// the tool calls it made (OpenAI shape) and its reasoning_details. `follow` reads a background
/// reply the server is holding instead of asking.
pub(crate) async fn stream(body: &Value, msg: &mut Msg, stop: &impl Fn() -> bool, show: &mut impl FnMut(&Msg), follow: Option<String>) -> Result<Step, String> {
    stream_noting(body, msg, stop, show, follow, &mut |_: &Option<String>| {}).await
}

/// `stream`, telling `note` the background reply's id as soon as it's known.
pub(crate) async fn stream_noting(body: &Value, msg: &mut Msg, stop: &impl Fn() -> bool, show: &mut impl FnMut(&Msg), follow: Option<String>, note: &mut impl FnMut(&Option<String>)) -> Result<Step, String> {
    let mut resp = match follow {
        Some(id) => api::follow(&id).await?,
        None => api::chat(body).await?,
    };
    note(&resp.id);
    let mut buf: Vec<u8> = vec![];
    let mut calls: Vec<Value> = vec![];
    let mut details: Vec<Value> = vec![];
    let mut got_any = false;
    let mut finished = false;
    let mut tokens = None;
    let mut cost = 0.0;
    'read: while let Some(chunk) = resp.next(stop).await {
        if stop() { finished = true; break }
        let chunk = match chunk { Ok(c) => c, Err(e) => { if !got_any { return Err(e) } break } };
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" { finished = true; break 'read }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if v["choices"][0]["finish_reason"].is_string() { finished = true }
            if v["usage"].is_object() {
                tokens = Some(v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) + v["usage"]["completion_tokens"].as_u64().unwrap_or(0));
                cost = v["usage"]["cost"].as_f64().unwrap_or(0.0);
            }
            if !v["error"].is_null() {
                return Err(v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("The model returned an error").to_string());
            }
            let d = &v["choices"][0]["delta"];
            if let Some(s) = d["content"].as_str() { merge(&mut msg.content, s); got_any = true }
            if let Some(s) = reasoning(d) { merge(&mut msg.reasoning, s); got_any = true }
            if d["reasoning_details"].is_array() { merge_details(&mut details, &d["reasoning_details"]) }
            for tc in d["tool_calls"].as_array().into_iter().flatten() {
                got_any = true;
                let i = tc["index"].as_u64().map(|i| i as usize).unwrap_or(calls.len());
                while calls.len() <= i { calls.push(json!({"id": "", "type": "function", "function": {"name": "", "arguments": ""}})) }
                let c = &mut calls[i];
                if let Some(id) = tc["id"].as_str() { c["id"] = json!(id) }
                if let Some(n) = tc["function"]["name"].as_str() { c["function"]["name"] = json!(n) }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    let old = c["function"]["arguments"].as_str().unwrap_or("").to_string();
                    c["function"]["arguments"] = json!(old + a);
                }
            }
            show(msg);
        }
    }
    for (i, c) in calls.iter_mut().enumerate() {
        if c["id"].as_str().unwrap_or("").is_empty() { c["id"] = json!(format!("call_{}_{i}", store::now())) }
    }
    if !finished {
        calls.clear();
    }
    let tee = if finished && !stop() { resp.receipt.take().map(|r| (r, resp.route.take(), std::mem::take(&mut resp.raw))) } else { None };
    Ok(Step { tool_calls: calls, details, cut: !finished, tokens, cost, tee })
}

/// Reasoning text in a delta: `reasoning`, or `reasoning_content` as TEE gateways send it.
pub(crate) fn reasoning(d: &Value) -> Option<&str> { d["reasoning"].as_str().or(d["reasoning_content"].as_str()) }

/// Asks for the next answer and streams it into `live`, then appends it to the chat.
fn run(app: App) { run_with(app, None) }

/// `run`, or with `follow`, the background reply the server holds for the last question.
fn run_with(mut app: App, follow: Option<String>) {
    if app.live.peek().is_some() {
        return;
    }
    // A guest chats with the one model guests get.
    if !api::has_session() {
        app.settings.write().model = crate::FREE_MODEL.into();
    }
    let model = app.settings.peek().model.clone();
    if crate::code::active(&app, &model) {
        return crate::code::turn(app, None);
    }
    // With the Tools switch off there's no web search either, as on the web.
    let verify = api::has_session() && app.settings.peek().verify;
    let web_search = api::has_session() && app.settings.peek().web_search && !crate::code::capable(&app, &model) && !verify;
    app.stop.set(false);
    app.live.set(Some(Msg { role: "assistant".into(), model: model.clone(), ..Default::default() }));
    js("document.body.dataset.working = 1; const m = document.getElementById('msgs'); if (m) { m.dataset.pin = 1; m.scrollTop = m.scrollHeight }; window.android && android.working && android.working(true)");
    bg(async move {
        let stop = || *app.stop.peek();
        let context = app.models.peek().iter().find(|m| m.id == model).map(|m| m.context).filter(|c| *c > 0).unwrap_or(crate::code::DEFAULT_CONTEXT);
        // A long chat: summarize the earlier messages first so the request fits the model's
        // context (compact.rs). The latest message always goes as it is. `resent`: once refused
        // as too long, it compacts (all of it, now) and sends the same message again, once.
        let mut resent = false;
        let mut compacted = false;
        let mut msg;
        let mut follow = follow;
        let chat_id = app.conv.peek().id.clone();
        let mut checked: Option<(String, Option<String>, Vec<u8>)> = None;
        loop {
            let force = resent;
            let mut shown = app.live.peek().clone().unwrap_or_default();
            let summarized = if follow.is_some() { Ok(false) } else { crate::compact::if_needed(app, &model, context, force, &stop, &mut |note: String| {
                shown.content = note;
                app.live.set(Some(shown.clone()));
            }).await };
            match summarized {
                Ok(done) => compacted |= done,
                Err(e) => {
                    msg = app.live.peek().clone().unwrap_or_default();
                    msg.error = true;
                    msg.content = format!("Couldn’t summarize the earlier messages to fit the model’s context: {e}");
                    break;
                }
            }
            if compacted {
                save(app);
            }
            let mut fresh = app.live.peek().clone().unwrap_or_default();
            fresh.content.clear();
            fresh.compacted = compacted;
            app.live.set(Some(fresh));
            let mut overflow = false;
            let mut body = json!({"model": model, "messages": api_history(&app.conv.peek(), &model, false), "stream": true});
            if web_search {
                body["web_search"] = json!(true);
            }
            if verify {
                body["tee_verify"] = json!(true);
            }
            msg = app.live.peek().clone().unwrap();
            // No connection: wait for it. A server answering 502/503/504 is waited on for a while.
            let mut delay = 1000;
            let mut waits = 0;
            let following = follow.take();
            let first = loop {
                let asked = match &following { Some(id) => api::follow(id).await, None => api::chat(&body).await };
                match asked {
                    Err(e) if (e == api::OFFLINE || e == api::UNAVAILABLE) && !*app.stop.peek()
                        && crate::code::wait_again(app, &mut delay, &mut waits, e == api::OFFLINE).await => {}
                    other => { app.waiting.set(false); break other }
                }
            };
            match first {
                Err(e) => { msg.error = true; msg.content = e; }
                Ok(mut resp) => {
                    remember(app, &resp.id, &model, false);
                    let mut buf: Vec<u8> = vec![];
                    let mut drawn = store::now();
                    let mut ended = false;
                    'read: while let Some(chunk) = resp.next(&stop).await {
                        if *app.stop.peek() { break }
                        let chunk = match chunk { Ok(c) => c, Err(e) => { if msg.content.is_empty() { msg.error = true; msg.content = e } break } };
                        buf.extend_from_slice(&chunk);
                        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=nl).collect();
                            let line = String::from_utf8_lossy(&line);
                            let Some(data) = line.trim().strip_prefix("data:") else { continue };
                            let data = data.trim();
                            if data == "[DONE]" { ended = true; break 'read }
                            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                            if !v["error"].is_null() {
                                let e = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("The model returned an error");
                                // Refused as too long: compact and send again, once.
                                if !resent && msg.content.is_empty() && crate::code::too_long(&e.to_lowercase()) {
                                    overflow = true;
                                    break 'read;
                                }
                                msg.error = true;
                                msg.content = if msg.content.is_empty() { e.to_string() } else { format!("{}\n\n{e}", msg.content) };
                                continue;
                            }
                            let d = &v["choices"][0]["delta"];
                            if let Some(s) = d["content"].as_str() { merge(&mut msg.content, s) }
                            if let Some(s) = reasoning(d) { merge(&mut msg.reasoning, s) }
                            if d["reasoning_details"].is_array() { merge_details(&mut msg.details, &d["reasoning_details"]) }
                            let ws = &d["web_search"];
                            if let Some(q) = ws["query"].as_str() {
                                let s = Search { query: q.into(), status: ws["status"].as_str().unwrap_or("").into(), results: ws["results"].as_str().or(ws["error"].as_str()).unwrap_or("").into() };
                                match msg.searches.iter_mut().find(|x| x.query == q) { Some(x) => *x = s, None => msg.searches.push(s) }
                            }
                        }
                        if store::now() - drawn > 60 {
                            drawn = store::now();
                            app.live.set(Some(msg.clone()));
                        }
                    }
                    if !overflow && msg.content.is_empty() && msg.reasoning.is_empty() && !*app.stop.peek() {
                        msg.error = true;
                        msg.content = "No answer came back. Try again or pick another model.".into();
                    }
                    if ended && !msg.error && !*app.stop.peek() && let Some(r) = resp.receipt.take() {
                        msg.tee.push(crate::tee::Step { receipt: r.clone(), ..Default::default() });
                        checked = Some((r, resp.route.take(), std::mem::take(&mut resp.raw)));
                    }
                }
            }
            if overflow {
                resent = true;
                continue;
            }
            break;
        }
        if !msg.content.is_empty() || !msg.reasoning.is_empty() {
            app.conv.write().messages.push(msg);
        }
        app.live.set(None);
        js("document.body.dataset.working = 0; const f = document.getElementById('follow'); if (f) f.hidden = true; window.android && android.working && android.working(false)");
        save(app);
        forget_pending();
        if let Some((receipt, route, raw)) = checked {
            crate::tee::check(app, chat_id, model, receipt, route, raw);
        }
        app.refresh_balance();
    });
}

fn send(mut app: App, text: String) {
    if app.live.peek().is_some() || (text.trim().is_empty() && app.attach.peek().is_empty() && app.docs.peek().is_empty()) {
        return;
    }
    let images = std::mem::take(&mut *app.attach.write());
    let files = std::mem::take(&mut *app.docs.write());
    {
        let mut c = app.conv.write();
        if c.title.is_empty() {
            c.title = text.trim().chars().take(60).collect();
            if c.title.is_empty() { c.title = files.first().map_or("Image".into(), |f| f.name.clone()) }
        }
        c.messages.push(Msg { role: "user".into(), content: text.trim().to_string(), images, files, ..Default::default() });
    }
    save(app);
    run(app);
}

/// Markdown to HTML with raw HTML shown as text, only http(s)/mailto links, and images turned into
/// links (a model's image would load from a third party and reveal the phone's IP).
pub fn md(s: &str) -> String {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
    let safe = |u: &CowStr| { let l = u.to_lowercase(); l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:") };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(s, opts).map(|ev| match ev {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            let dest_url = if safe(&dest_url) { dest_url } else { CowStr::from("") };
            Event::Start(Tag::Link { link_type, dest_url, title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            let dest_url = if safe(&dest_url) { dest_url } else { CowStr::from("") };
            Event::Start(Tag::Link { link_type, dest_url, title, id })
        }
        Event::End(TagEnd::Image) => Event::End(TagEnd::Link),
        e => e,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn glob(id: &str, pat: &str) -> bool {
    let pat = pat.trim();
    if !pat.contains('*') {
        return id == pat;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let mut rest = id;
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() { continue }
        if i == 0 {
            let Some(r) = rest.strip_prefix(p) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            let Some(at) = rest.find(p) else { return false };
            rest = &rest[at + p.len()..];
        }
    }
    true
}

fn per_million(m: &Model) -> String {
    let price = if m.prompt == 0.0 && m.completion == 0.0 { "Free".into() } else { format!("{} / {} per 1M tokens", usd(m.prompt * 1e6), usd(m.completion * 1e6)) };
    let price = if m.context >= 1000 { format!("{price} · {}k context", m.context / 1000) } else { price };
    let mut line = if m.tools { price } else { format!("{price} · no tools") };
    // How privately it runs, as the server tags it.
    for (tag, label) in [("Zero retention", "ZDR"), ("TEE", "TEE")] {
        if m.categories.iter().any(|c| c == tag) { line = format!("{line} · {label}") }
    }
    line
}

/// Who makes a model: "Anthropic" from "Anthropic: Claude Opus 5".
fn provider(m: &Model) -> String {
    m.name.split_once(": ").map_or_else(|| m.id.split('/').next().unwrap_or("").to_string(), |(p, _)| p.to_string())
}

// ---- views ----

#[component]
pub fn ChatView() -> Element {
    let mut app = use_context::<App>();
    let mut text = use_signal(String::new);
    // The + menu in the message box (Attach files, Tools, Code), as on the web.
    let mut menu = use_signal(|| false);
    let c = app.conv.read().clone();
    let live = app.live.read().clone();
    let model = app.settings.read().model.clone();
    let streaming = live.is_some();
    let capable = app.code.read().enabled && app.models.read().iter().find(|m| m.id == model).is_none_or(|m| m.tools);
    // `settings` and `code` are read above and below, so the view follows the switches.
    let separate = app.code.read().separate;
    let tools_on = crate::code::tools_on(&app);
    let code_on = crate::code::code_on(&app);
    let n = c.messages.len();

    let mut do_send = move || {
        let t = text.peek().clone();
        js("document.getElementById('composer').value = ''");
        text.set(String::new());
        send(app, t);
    };

    rsx! {
        div { class: "screen",
            div { class: "bar",
                button { class: "icon", onclick: move |_| app.overlay.set(Overlay::Drawer), "☰" }
                div { class: "title model", onclick: move |_| {
                    if api::has_session() { app.overlay.set(Overlay::Models) }
                    else { app.say("Create an account to choose from every model"); app.screen.set(Screen::Signup) }
                },
                    if app.settings.read().verify && api::has_session() {
                        svg { class: "verify-mark", view_box: "0 0 24 24", width: "15", height: "15", fill: "none", stroke: "currentColor", stroke_width: "2.2", stroke_linecap: "round", stroke_linejoin: "round",
                            path { d: "M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6z" } path { d: "M8.5 12l2.5 2.5 4.5-5" } }
                    }
                    {app.model_name(&model)} " ▾" }
                button { class: "icon", onclick: move |_| { new_chat(app); }, "＋" }
            }
            div { id: "msgs",
                if n == 0 && !streaming {
                    div { class: "empty", "Ask anything."
                        if capable && (tools_on || code_on) {
                            p { class: "muted", {match (code_on, tools_on, app.code.read().paid_tools) {
                                (true, true, true) => "It can run code, make charts and pages, search the web and use paid tools billed to your balance.",
                                (true, true, false) => "It can run code, make charts and pages and search the web.",
                                (true, false, _) => "It can run code and make charts and pages.",
                                (false, _, true) => "It can search the web and use paid tools billed to your balance.",
                                (false, _, false) => "It can search the web.",
                            }} }
                        }
                    }
                }
                for (i, m) in c.messages.iter().enumerate() {
                    MsgView { key: "{c.id}-{i}", i, m: m.clone(), last: i + 1 == n && !streaming }
                }
                if let Some(m) = live { MsgView { key: "{c.id}-live", i: n, m, last: false, live: true } }
            }
            if *app.waiting.read() {
                div { class: "waiting", "Waiting for Obscurify. This picks up by itself." }
            }
            div { class: "composer",
                if !capable && api::has_session() {
                    div { class: "toggles",
                        button { class: if app.settings.read().web_search { "chip on" } else { "chip" },
                            onclick: move |_| { let v = !app.settings.peek().web_search; app.settings.write().web_search = v; app.save_settings() }, "Web search" }
                    }
                }
                if !app.docs.read().is_empty() {
                    div { class: "docs",
                        for (i, d) in app.docs.read().iter().enumerate() {
                            span { key: "{i}", class: "chip on", onclick: move |_| { app.docs.write().remove(i); }, "📄 {d.name} ✕" }
                        }
                    }
                }
                if !app.attach.read().is_empty() {
                    div { class: "thumbs",
                        for (i, src) in app.attach.read().iter().enumerate() {
                            img { key: "{i}", src: "{src}", onclick: move |_| { app.attach.write().remove(i); } }
                        }
                    }
                }
                if *menu.read() {
                    div { class: "menu-veil", onclick: move |_| menu.set(false) }
                }
                div { class: "line",
                    textarea { id: "composer", rows: 1, placeholder: if app.conv.read().temp { "Temporary chat" } else { "Message" }, oninput: move |e| text.set(e.value()) }
                    div { class: "plus-wrap",
                        button { class: if *menu.read() { "plus open" } else { "plus" }, aria_label: "Attach, tools and code", aria_expanded: "{menu}",
                            onclick: move |_| { let v = !*menu.peek(); menu.set(v) },
                            svg { view_box: "0 0 24 24", width: "22", height: "22",
                                path { d: "M12 5v14M5 12h14", fill: "none", stroke: "currentColor", stroke_width: "2.2", stroke_linecap: "round" }
                            }
                        }
                        div { class: if *menu.read() { "plus-menu open" } else { "plus-menu" }, role: "menu",
                            // Any file or photo; the app tells which (see BRIDGE in main.rs).
                            button { class: "item attach", role: "menuitem", onclick: move |_| menu.set(false),
                                svg { view_box: "0 0 24 24", width: "20", height: "20",
                                    path { d: "M21 11.5l-8.6 8.6a5.5 5.5 0 0 1-7.8-7.8l8.6-8.6a3.7 3.7 0 0 1 5.2 5.2l-8.6 8.6a1.8 1.8 0 0 1-2.6-2.6l7.9-7.9", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round" }
                                }
                                span { class: "label", "Attach files" }
                            }
                            // Tools and Code on or off; the menu stays open to show the change.
                            if app.code.read().enabled {
                                button { class: if !capable { "item tools-switch na" } else if tools_on { "item tools-switch on" } else { "item tools-switch" },
                                    role: "switch", aria_checked: "{capable && tools_on}", aria_disabled: "{!capable}",
                                    onclick: move |_| {
                                        if !capable { app.say("This model can't use tools"); return }
                                        let v = !app.settings.peek().tools; app.settings.write().tools = v; app.save_settings()
                                    },
                                    svg { view_box: "0 0 24 24", width: "20", height: "20",
                                        path { d: "M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.4-.6-.6-2.4z", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round" }
                                    }
                                    span { class: "label", "Tools" }
                                    span { class: "switch" }
                                }
                                if separate {
                                    button { class: if !capable { "item code-switch na" } else if code_on { "item code-switch on" } else { "item code-switch" },
                                        role: "switch", aria_checked: "{capable && code_on}", aria_disabled: "{!capable}",
                                        onclick: move |_| {
                                            if !capable { app.say("This model can't run code"); return }
                                            let v = !app.settings.peek().code; app.settings.write().code = v; app.save_settings()
                                        },
                                        svg { view_box: "0 0 24 24", width: "20", height: "20",
                                            path { d: "m8 7-5 5 5 5M16 7l5 5-5 5", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round" }
                                        }
                                        span { class: "label", "Code" }
                                        span { class: "switch" }
                                    }
                                }
                            }
                        }
                    }
                    if streaming {
                        button { class: "send stop", aria_label: "Stop", onclick: move |_| stop_answer(app),
                            svg { view_box: "0 0 24 24", width: "18", height: "18", rect { x: "5", y: "5", width: "14", height: "14", rx: "2", fill: "currentColor" } }
                        }
                    } else {
                        button { class: if text.read().trim().is_empty() && app.attach.read().is_empty() && app.docs.read().is_empty() { "send idle" } else { "send" }, aria_label: "Send", onclick: move |_| do_send(),
                            svg { view_box: "0 0 24 24", width: "22", height: "22",
                                path { d: "M12 19V5M5.5 11.5L12 5l6.5 6.5", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round", stroke_linejoin: "round" }
                            }
                        }
                    }
                }
            }
        }
        match *app.overlay.read() {
            Overlay::Drawer => rsx! { Drawer {} },
            Overlay::Models => rsx! { Models {} },
            Overlay::None => rsx! {},
        }
    }
}

#[component]
fn MsgView(i: usize, m: Msg, last: bool, #[props(default)] live: bool) -> Element {
    let mut app = use_context::<App>();
    let mut raw = use_signal(|| false);
    if m.role == "user" {
        return rsx! {
            div { class: "msg user",
                for src in m.images.iter() { img { src: "{src}" } }
                for d in m.files.iter() {
                    div { class: "doc", "📄 {d.name}" span { class: "muted", " · {d.kind}, {crate::files::size(d)}" if d.truncated { " (cut)" } } }
                }
                "{m.content}"
            }
        };
    }
    let (think, answer) = split_think(&m.content);
    let reasoning = if m.reasoning.is_empty() { think.to_string() } else { m.reasoning.clone() };
    let html = md(answer);
    let thinking = live && answer.is_empty();
    rsx! {
        div { class: if m.error { "msg err" } else { "msg assistant" },
            for s in m.searches.iter() {
                details { class: "sources",
                    summary { {match s.status.as_str() { "searching" => format!("Searching “{}”…", s.query), "error" => format!("Search failed: {}", s.query), _ => format!("Searched “{}”", s.query) }} }
                    div { class: "think", "{s.results}" }
                }
            }
            if !reasoning.is_empty() {
                details { class: "sources", open: thinking,
                    summary { if thinking { "Thinking…" } else { "Thought" } }
                    div { class: "think", "{reasoning}" }
                }
            }
            for (ri, r) in m.runs.iter().enumerate() {
                div { key: "run-{ri}",
                    if !r.intro.trim().is_empty() { div { dangerous_inner_html: md(&r.intro) } }
                    RunView { r: r.clone() }
                }
            }
            if m.compacted { p { class: "compacted", "Compacted earlier work to save context" } }
            if m.spent > 0.0 {
                p { class: "spend", "Spent {usd(m.spent)}" }
            }
            if !live && !m.tee.is_empty() { crate::tee::Chip { steps: m.tee.clone() } }
            if !m.interrupted.is_empty() {
                p { class: "interrupted", "Stopped early: {m.interrupted} Everything so far is kept." }
            }
            if m.error { p { "{m.content}" } }
            else if *raw.read() { pre { class: "raw", "{m.content}" } }
            else { div { class: if live { "cursor" } else { "" }, dangerous_inner_html: "{html}" } }
            if !live {
                for (pi, page) in crate::code::html_blocks(answer, &m.runs.iter().flat_map(|r| r.pages.iter().map(|p| p.html.as_str())).collect::<Vec<_>>()).into_iter().enumerate() {
                    button { key: "page-{pi}", class: "page-btn", onclick: move |_| crate::code::open_page(&app, "Page", &page), "▶ Open this page" }
                }
            }
            if last || !live && !m.error {
                div { class: "meta",
                    if !m.error {
                        button { class: "bare", onclick: { let t = answer.to_string(); move |_| app.copy(&t, "Answer") }, "Copy" }
                        button { class: "bare", onclick: move |_| { let r = !*raw.peek(); raw.set(r) }, if *raw.read() { "Rendered" } else { "Raw" } }
                    }
                    // An answer with tool work continues from it: paid calls aren't made again.
                    if last && (!m.trace.is_empty() || !m.interrupted.is_empty()) {
                        button { class: if m.interrupted.is_empty() { "bare" } else { "go continue" }, onclick: move |_| crate::code::resume(app),
                            if m.interrupted.is_empty() { "Retry" } else { "Continue" } }
                    } else if last {
                        button { class: "bare", onclick: move |_| { app.conv.write().messages.pop(); run(app) }, "Retry" }
                    }
                    // Back to the question this answered: later messages go, as on the web.
                    button { class: "bare", onclick: move |_| {
                        let u = app.conv.peek().messages.iter().take(i).rposition(|m| m.role == "user");
                        let Some(u) = u else { return };
                        let q = { let mut c = app.conv.write(); let q = c.messages[u].clone(); c.messages.truncate(u); if c.compaction.as_ref().is_some_and(|k| k.through >= u) { c.compaction = None } q };
                        app.attach.set(q.images);
                        app.docs.set(q.files);
                        js(&format!("const c = document.getElementById('composer'); c.value = {:?}; c.dispatchEvent(new Event('input', {{bubbles: true}})); c.focus()", q.content));
                        save(app);
                    }, "Edit" }
                    if !m.model.is_empty() { span { " · {app.model_name(&m.model)}" } }
                }
            }
        }
    }
}

/// One piece of code the model ran: its code, the tools it called, what it printed, its charts,
/// and the pages it built (they open full screen).
#[component]
fn RunView(r: crate::code::Run) -> Element {
    let app = use_context::<App>();
    let lang = if r.language.starts_with('j') { "JavaScript" } else { "Python" };
    let lines = r.code.lines().count();
    let class = if !r.done { "run going" } else if r.ok { "run ok" } else { "run err" };
    let media = rsx! {
        for (i, url) in r.media.iter().enumerate() {
            div { key: "media-{i}", class: "run-media",
                if code::is_video(url) {
                    video { class: "run-video", controls: true, playsinline: true, preload: "metadata", src: "{url}" }
                } else if code::is_audio(url) {
                    audio { class: "run-audio", controls: true, preload: "metadata", src: "{url}" }
                } else {
                    img { class: "run-img", src: "{url}", loading: "lazy" }
                }
                // The links expire (a day at most), and this is the only way to keep the file.
                button { class: "media-save",
                    onclick: { let url = url.clone(); move |_| crate::open_url(&url) },
                    "⤓ Save " {code::media_filename(url)}
                }
            }
        }
    };
    let tools = rsx! {
        if !r.tools.is_empty() {
            div { class: "run-tools",
                for (i, u) in r.tools.iter().enumerate() {
                    div { key: "{i}", class: match u.ok { None => "tool going", Some(true) => "tool", Some(false) => "tool failed" },
                        span { {match u.name.as_str() { "web_search" => "🔍", "search" => "🧰", "inspect" => "📋", "call" => "💳", _ => "•" }} }
                        span { class: "tool-name", " {u.name} " }
                        span { class: "tool-detail", "{u.detail}" }
                        if u.cost > 0.0 { span { class: "run-cost", " {usd(u.cost)}" } }
                    }
                }
            }
        }
    };
    // Tools the model called directly: only their chips and what they made.
    if r.direct {
        return rsx! { div { class: "run direct", {tools} {media} } };
    }
    rsx! {
        div { class,
            div { class: "run-head",
                span { class: "run-lang", "{lang}" }
                span { "{r.status}" }
                if r.cost > 0.0 { span { class: "run-cost", {usd(r.cost)} } }
            }
            details { class: "run-code", open: lines <= 12,
                summary { "Code · {lines} " if lines == 1 { "line" } else { "lines" } }
                pre { "{r.code}" }
            }
            {tools}
            if !r.output.is_empty() || !r.error.is_empty() {
                pre { class: "run-out",
                    "{r.output}"
                    if !r.error.is_empty() { span { class: "run-err", if r.output.is_empty() { "{r.error}" } else { "\n{r.error}" } } }
                }
            }
            for (i, img) in r.images.iter().enumerate() { img { key: "{i}", class: "run-img", src: "{img}" } }
            {media}
            for (i, p) in r.pages.iter().enumerate() {
                button { key: "{i}", class: "page-btn",
                    onclick: { let p = p.clone(); move |_| crate::code::open_page(&app, &p.title, &p.html) },
                    "▶ Open " {if p.title.is_empty() { "page".to_string() } else { p.title.clone() }}
                }
            }
        }
    }
}

/// Obscurify's onion site, opened in a Tor browser.
const ONION: &str = "http://obscurizgk4wr5bg5fatq5tedqszcjrbgkot6qw7tnip74bakp6oztid.onion/";

/// "3h ago", beside a chat in the list.
fn ago(ms: i64) -> String {
    let m = (store::now() - ms).max(0) / 60_000;
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m}m ago"),
        60..=1439 => format!("{}h ago", m / 60),
        _ => format!("{}d ago", m / 1440),
    }
}

#[component]
fn Drawer() -> Element {
    let mut app = use_context::<App>();
    let mut confirm = use_signal(|| None::<String>);
    let mut wiping = use_signal(|| false);
    let current = app.conv.read().id.clone();
    let temp = app.conv.read().temp;
    let verify = app.settings.read().verify && api::has_session();
    rsx! {
        div { class: "veil", "data-back": "1", onclick: move |_| app.overlay.set(Overlay::None) }
        div { class: "drawer",
            div { class: "bar",
                div { class: "title", style: "text-align: left; padding-left: 8px", "Chats" }
                button { class: "bare", onclick: move |_| { new_chat(app); app.overlay.set(Overlay::None) }, "New chat" }
            }
            // Quick controls, as on the web (which also has Local; the app has no local models).
            div { class: "quick",
                button { class: if temp { "q on" } else { "q" }, id: "temp", aria_pressed: "{temp}",
                    onclick: move |_| { toggle_temp(app); app.overlay.set(Overlay::None) },
                    svg { view_box: "0 0 24 24", width: "20", height: "20", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round",
                        circle { cx: "12", cy: "12", r: "9", stroke_dasharray: "3 3" } path { d: "M12 7v5l3 2" } }
                    span { "Temp" }
                }
                button { class: if verify { "q on" } else { "q" }, id: "verify", aria_pressed: "{verify}",
                    onclick: move |_| { toggle_verify(app); app.overlay.set(Overlay::None) },
                    svg { view_box: "0 0 24 24", width: "20", height: "20", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                        path { d: "M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6z" } path { d: "M8.5 12l2.5 2.5 4.5-5" } }
                    span { "Verify" }
                }
                a { class: "q", id: "tor", href: "{ONION}",
                    svg { view_box: "0 0 24 24", width: "20", height: "20", fill: "none", stroke: "currentColor", stroke_width: "2",
                        circle { cx: "12", cy: "13", r: "8" } circle { cx: "12", cy: "13", r: "5" } circle { cx: "12", cy: "13", r: "2" } }
                    span { "Tor" }
                }
                button { class: "q danger", id: "wipe", onclick: move |_| wiping.set(true),
                    svg { view_box: "0 0 24 24", width: "20", height: "20", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round",
                        path { d: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" } }
                    span { "Wipe" }
                }
            }
            if wiping() {
                div { class: "wipe-ask",
                    p { "Delete every chat and setting on this phone? It can't be undone. You stay signed in, and vouchers you haven't added yet are kept." }
                    div { class: "row",
                        button { onclick: move |_| wiping.set(false), "Cancel" }
                        button { class: "danger", id: "wipe-yes", onclick: move |_| { wiping.set(false); wipe(app) }, "Wipe" }
                    }
                }
            }
            div { class: "scroll", style: "padding: 0",
                if app.convs.read().is_empty() { p { class: "muted", style: "padding: 16px", "No chats yet" } }
                for m in app.convs.read().clone() {
                    div { key: "{m.id}", class: if m.id == current { "item on" } else { "item" },
                        div { class: "grow", onclick: { let id = m.id.clone(); move |_| {
                            let id = id.clone();
                            after_answer(app, move |mut app| { if app.live.peek().is_none() { app.conv.set(Conv::load(&id)) } });
                            app.overlay.set(Overlay::None)
                        } }, "{m.title}" span { class: "when", {ago(m.updated)} } }
                        if confirm.read().as_deref() == Some(&m.id) {
                            button { class: "danger", style: "padding: 4px 10px", onclick: { let id = m.id.clone(); move |_| { delete(app, &id); confirm.set(None) } }, "Delete" }
                        } else {
                            button { class: "bare", onclick: { let id = m.id.clone(); move |_| confirm.set(Some(id.clone())) }, "✕" }
                        }
                    }
                }
            }
            div { class: "foot",
                if api::has_session() {
                    button { class: "account", onclick: move |_| { app.overlay.set(Overlay::None); app.screen.set(Screen::Account) },
                        span { "Account" }
                        if let Some(b) = *app.balance.read() { span { class: "bal", {usd(b)} } }
                    }
                } else {
                    button { class: "account", onclick: move |_| { app.overlay.set(Overlay::None); app.screen.set(Screen::Login) }, span { "Sign in" } }
                }
                // As on the web: the rest folds under More info, the legal line stays.
                details { class: "more",
                    summary { "More info" }
                    div { class: "links",
                        a { href: "{api::BASE}/tools", "All tools" }
                        a { href: "{api::BASE}/model_cards", "Model cards" }
                        a { href: "{api::BASE}/about", "About" }
                        a { href: "{api::BASE}/faq", "FAQ" }
                        a { href: "{api::BASE}/guides", "Guides" }
                    }
                }
                div { class: "legal",
                    a { href: "{api::BASE}/privacy", "Privacy" }
                    a { href: "{api::BASE}/terms", "Terms" }
                    a { href: "{api::BASE}/canary", "Canary" }
                    a { href: "{api::BASE}/contact", "Support" }
                }
            }
        }
    }
}

const REASONING: [&str; 5] = ["*thinking*", "*reasoning*", "*deepseek-r1*", "*-r1-*", "*magistral*"];
/// Roleplay and everything after Frontier (privacy, then use cases) come from the `categories` the
/// server tags models with.
const CATEGORIES: [&str; 18] = ["Favorites", "All", "Tools", "Free", "Cheap", "Reasoning", "Uncensored", "Roleplay", "Frontier",
    "Zero retention", "TEE", "Writing", "Security", "Legal", "Translation", "Languages", "Code tools", "Documents"];

/// The plan a model needs that the account doesn't have (uncensored models need Pro, frontier
/// models Casual or Pro, as the server enforces). Nothing is locked until the plan is known.
fn needs_plan(id: &str, access: Option<Access>, frontier: &[String], uncensored: &[String]) -> Option<&'static str> {
    let a = access?;
    if !a.uncensored && uncensored.iter().any(|p| glob(id, p)) {
        Some("Pro")
    } else if !a.frontier && frontier.iter().any(|p| glob(id, p)) {
        Some("Casual or Pro")
    } else {
        None
    }
}

#[component]
fn Models() -> Element {
    let mut app = use_context::<App>();
    let favs = app.settings.read().favorites.clone();
    let verifying = app.settings.read().verify && api::has_session();
    let fav_ok = { let ms = app.models.read(); favs.iter().any(|f| !verifying || ms.iter().any(|m| &m.id == f && m.verifiable)) };
    let mut cat = use_signal(move || if fav_ok { "Favorites" } else { "All" });
    let mut q = use_signal(String::new);
    let favs = app.settings.read().favorites.clone();
    let frontier = app.frontier.read().clone();
    let uncensored = app.uncensored.read().clone();
    let access = *app.access.read();
    let mut prov = use_signal(|| None::<String>);
    let models = app.models.read().clone();
    let query = q.read().to_lowercase();
    let c = *cat.read();
    // Verify on: only models whose answers the phone can check.
    let verify = app.settings.read().verify && api::has_session();
    let any = |id: &str, pats: &[&str]| pats.iter().any(|p| glob(id, p));
    let needs = |id: &str| needs_plan(id, access, &frontier, &uncensored);
    let in_cat = |m: &Model, k: &str| {
        let free = m.prompt == 0.0 && m.completion == 0.0;
        match k {
            "Favorites" => favs.contains(&m.id),
            "Tools" => m.tools,
            "Free" => free,
            "Cheap" => free || (m.prompt <= 0.000_000_25 && m.completion <= 0.000_000_5),
            "Reasoning" => any(&m.id, &REASONING),
            "Uncensored" => uncensored.iter().any(|p| glob(&m.id, p)),
            "Frontier" => frontier.iter().any(|p| glob(&m.id, p)),
            "All" => true,
            _ => m.categories.iter().any(|c| c == k),
        }
    };
    // Categories with models the plan can use come first and ones it can't use any of last;
    // empty ones are hidden (Favorites stays, to say how to fill it).
    let mut chips: Vec<(&'static str, bool)> = CATEGORIES.iter().filter_map(|&k| {
        let mut ms = models.iter().filter(|m| (!verify || m.verifiable) && in_cat(m, k)).peekable();
        if ms.peek().is_none() { return (k == "Favorites" || k == "All").then_some((k, false)) }
        Some((k, ms.all(|m| needs(&m.id).is_some())))
    }).collect();
    chips.sort_by_key(|&(_, locked)| locked);
    // A search looks through every model, whatever category is picked.
    let shown: Vec<&Model> = models.iter()
        .filter(|m| !verify || m.verifiable)
        .filter(|m| !query.is_empty() || in_cat(m, c))
        .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query) || m.id.to_lowercase().contains(&query))
        .collect();
    // Only providers with models in the category (or the search) are offered; a picked provider
    // the category doesn't have stops filtering until one that has it is picked again.
    let mut providers: Vec<String> = shown.iter().map(|m| provider(m)).collect();
    providers.sort_by_key(|p| p.to_lowercase());
    providers.dedup();
    let chosen = prov.read().clone().filter(|p| providers.contains(p));
    let (usable, locked): (Vec<Model>, Vec<Model>) = shown.into_iter()
        .filter(|m| chosen.as_ref().is_none_or(|p| &provider(m) == p))
        .cloned()
        .partition(|m| needs(&m.id).is_none());
    let locked: Vec<(Model, &'static str)> = locked.into_iter().map(|m| { let n = needs(&m.id).unwrap_or(""); (m, n) }).collect();

    rsx! {
        div { class: "drawer", "data-back": "1", style: "width: 100%; max-width: none; border: 0",
            div { class: "bar",
                button { class: "icon", onclick: move |_| app.overlay.set(Overlay::None), "←" }
                div { class: "title", "Model" }
                div { style: "width: 46px" }
            }
            input { class: "search", placeholder: "Search models", oninput: move |e| q.set(e.value()) }
            div { class: "toggles", style: "padding: 0 12px 8px",
                for (k, off) in chips {
                    button { class: format!("chip{}{}", if c == k && query.is_empty() { " on" } else { "" }, if off { " locked" } else { "" }), onclick: move |_| cat.set(k), "{k}" }
                }
            }
            div { class: "toggles providers", style: "padding: 0 12px 8px",
                for p in providers {
                    button { class: if chosen.as_ref() == Some(&p) { "chip on" } else { "chip" },
                        onclick: { let p = p.clone(); move |_| { let same = prov.peek().as_ref() == Some(&p); prov.set(if same { None } else { Some(p.clone()) }) } }, "{p}" }
                }
            }
            div { class: "scroll", style: "padding: 0",
                if usable.is_empty() && locked.is_empty() { p { class: "muted", style: "padding: 16px", if c == "Favorites" { "Star models to keep them here. The first one is used for new chats." } else { "No models" } } }
                for m in usable { ModelRow { key: "{m.id}", m: m.clone(), needs: None } }
                if !locked.is_empty() { h2 { class: "group", "Upgrade to use" } }
                for (m, n) in locked { ModelRow { key: "{m.id}", m: m.clone(), needs: Some(n) } }
            }
        }
    }
}

/// A model in the picker. One the plan doesn't include (`needs` names the plan it needs) opens
/// Account, where the plan can be changed, instead of being chosen.
#[component]
fn ModelRow(m: Model, needs: Option<&'static str>) -> Element {
    let mut app = use_context::<App>();
    let current = app.settings.read().model.clone();
    let fav = app.settings.read().favorites.contains(&m.id);
    let class = format!("pick row{}{}", if m.id == current { " on" } else { "" }, if needs.is_some() { " locked" } else { "" });
    rsx! {
        div { class, style: "padding: 10px 16px",
            div { class: "grow", onclick: { let m = m.clone(); move |_| {
                    if let Some(plan) = needs {
                        app.say(format!("{} needs the {plan} plan", m.name));
                        app.overlay.set(Overlay::None);
                        app.screen.set(Screen::Account);
                        return;
                    }
                    app.settings.write().model = m.id.clone();
                    app.save_settings();
                    app.overlay.set(Overlay::None);
                } },
                div { "{m.name}" }
                div { class: "price", if let Some(plan) = needs { "Needs {plan} · " } {per_million(&m)} }
            }
            button { class: "bare", style: if fav { "color: var(--g)" } else { "" },
                onclick: { let id = m.id.clone(); move |_| {
                    let mut s = app.settings.write();
                    if let Some(i) = s.favorites.iter().position(|f| f == &id) { s.favorites.remove(i); } else if s.favorites.len() < 20 { s.favorites.push(id.clone()) }
                    drop(s);
                    app.save_settings();
                } }, if fav { "★" } else { "☆" } }
        }
    }
}
