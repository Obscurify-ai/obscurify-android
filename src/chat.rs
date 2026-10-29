//! Chat: the conversation, streaming answers, web search, images, the chats drawer and the model
//! picker. Chats live only on the phone (`chats.json` lists them, `chat-<id>.json` holds each), the
//! same as the web, which keeps them in the browser.

use crate::{Access, App, Model, Overlay, Screen, api, bg, js, store, usd};
use dioxus::prelude::*;
use futures_util::StreamExt;
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
    /// Code Mode: the code the model ran for this answer, and the tool turns to replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<crate::code::Run>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<Value>,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub updated: i64,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Conv {
    pub id: String,
    pub title: String,
    pub messages: Vec<Msg>,
}

impl Conv {
    pub fn new() -> Self { Conv { id: format!("c{}", store::now()), ..Default::default() } }
    pub fn load(id: &str) -> Self { let c: Conv = store::load(&format!("chat-{id}.json")); if c.id.is_empty() { Conv::new() } else { c } }
}

/// Saves the open chat and moves it to the top of the list.
pub(crate) fn save_chat(app: App) { save(app) }

fn save(mut app: App) {
    let c = app.conv.peek().clone();
    if c.messages.is_empty() {
        return;
    }
    store::save(&format!("chat-{}.json", c.id), &c);
    let mut list = app.convs.write();
    list.retain(|m| m.id != c.id);
    list.insert(0, Meta { id: c.id.clone(), title: c.title.clone(), updated: store::now() });
    store::save("chats.json", &*list);
}

/// A fresh chat; it starts on the first favorite model, like the web.
fn new_chat(mut app: App) {
    if app.live.peek().is_some() {
        return;
    }
    app.conv.set(Conv::new());
    let first = app.settings.peek().favorites.first().cloned();
    if let Some(f) = first {
        app.settings.write().model = f;
        app.save_settings();
    }
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
            let mut out: Vec<Value> = m.trace.iter().map(|t| {
                let mut t = t.clone();
                if m.model != model && let Some(o) = t.as_object_mut() { o.remove("reasoning_details"); }
                t
            }).collect();
            out.push(json!({"role": "assistant", "content": answer}));
            return out;
        }
        let mut text = String::new();
        for r in &m.runs {
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

/// What one streamed completion asked for besides its text.
pub(crate) struct Step {
    pub tool_calls: Vec<Value>,
    pub details: Vec<Value>,
}

/// Streams one completion into `msg` (content and reasoning), calling `show` as it grows. Returns
/// the tool calls it made (OpenAI shape) and its reasoning_details.
pub(crate) async fn stream(body: &Value, msg: &mut Msg, stop: &impl Fn() -> bool, show: &mut impl FnMut(&Msg)) -> Result<Step, String> {
    let resp = api::chat(body).await?;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = vec![];
    let mut calls: Vec<Value> = vec![];
    let mut details: Vec<Value> = vec![];
    let mut got_any = false;
    'read: while let Some(chunk) = stream.next().await {
        if stop() { break }
        let Ok(chunk) = chunk else { if !got_any { return Err("The connection dropped.".into()) } break };
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" { break 'read }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if !v["error"].is_null() {
                return Err(v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("The model returned an error").to_string());
            }
            let d = &v["choices"][0]["delta"];
            if let Some(s) = d["content"].as_str() { merge(&mut msg.content, s); got_any = true }
            if let Some(s) = d["reasoning"].as_str() { merge(&mut msg.reasoning, s); got_any = true }
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
    Ok(Step { tool_calls: calls, details })
}

/// Asks for the next answer and streams it into `live`, then appends it to the chat.
fn run(mut app: App) {
    if app.live.peek().is_some() {
        return;
    }
    let model = app.settings.peek().model.clone();
    if crate::code::active(&app, &model) {
        return crate::code::turn(app);
    }
    let web_search = app.settings.peek().web_search;
    app.stop.set(false);
    app.live.set(Some(Msg { role: "assistant".into(), model: model.clone(), ..Default::default() }));
    js("const m = document.getElementById('msgs'); if (m) { m.dataset.pin = 1; m.scrollTop = m.scrollHeight }");
    bg(async move {
        let mut body = json!({"model": model, "messages": wire(&app.conv.peek().messages, &model, false), "stream": true});
        if web_search {
            body["web_search"] = json!(true);
        }
        let mut msg = app.live.peek().clone().unwrap();
        match api::chat(&body).await {
            Err(e) => { msg.error = true; msg.content = e; }
            Ok(resp) => {
                let mut stream = resp.bytes_stream();
                let mut buf: Vec<u8> = vec![];
                let mut drawn = store::now();
                'read: while let Some(chunk) = stream.next().await {
                    if *app.stop.peek() { break }
                    let Ok(chunk) = chunk else { if msg.content.is_empty() { msg.error = true; msg.content = "The connection dropped.".into() } break };
                    buf.extend_from_slice(&chunk);
                    while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=nl).collect();
                        let line = String::from_utf8_lossy(&line);
                        let Some(data) = line.trim().strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data == "[DONE]" { break 'read }
                        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                        if !v["error"].is_null() {
                            msg.error = true;
                            let e = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("The model returned an error");
                            msg.content = if msg.content.is_empty() { e.to_string() } else { format!("{}\n\n{e}", msg.content) };
                            continue;
                        }
                        let d = &v["choices"][0]["delta"];
                        if let Some(s) = d["content"].as_str() { merge(&mut msg.content, s) }
                        if let Some(s) = d["reasoning"].as_str() { merge(&mut msg.reasoning, s) }
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
                if msg.content.is_empty() && msg.reasoning.is_empty() && !*app.stop.peek() {
                    msg.error = true;
                    msg.content = "No answer came back. Try again or pick another model.".into();
                }
            }
        }
        if !msg.content.is_empty() || !msg.reasoning.is_empty() {
            app.conv.write().messages.push(msg);
        }
        app.live.set(None);
        save(app);
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
    if m.prompt == 0.0 && m.completion == 0.0 { "Free".into() } else { format!("{} / {} per 1M tokens", usd(m.prompt * 1e6), usd(m.completion * 1e6)) }
}

// ---- views ----

#[component]
pub fn ChatView() -> Element {
    let mut app = use_context::<App>();
    let mut text = use_signal(String::new);
    let c = app.conv.read().clone();
    let live = app.live.read().clone();
    let model = app.settings.read().model.clone();
    let streaming = live.is_some();
    let code_on = app.code.read().enabled && app.models.read().iter().find(|m| m.id == model).is_none_or(|m| m.tools);
    let n = c.messages.len();
    let last_user = c.messages.iter().rposition(|m| m.role == "user");

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
                div { class: "title model", onclick: move |_| app.overlay.set(Overlay::Models), {app.model_name(&model)} " ▾" }
                button { class: "icon", onclick: move |_| { new_chat(app); }, "＋" }
            }
            div { id: "msgs",
                if n == 0 && !streaming {
                    div { class: "empty", "Ask anything." }
                }
                for (i, m) in c.messages.iter().enumerate() {
                    MsgView { key: "{c.id}-{i}", m: m.clone(), last: i + 1 == n && !streaming, edit_at: if Some(i) == last_user.map(|u| u + 1).filter(|_| i + 1 == n) { last_user } else { None } }
                }
                if let Some(m) = live { MsgView { key: "{c.id}-live", m, last: false, edit_at: None, live: true } }
            }
            div { class: "composer",
                div { class: "toggles",
                    // With Code Mode, web search is one of the model's tools: nothing to switch.
                    if !code_on {
                        button { class: if app.settings.read().web_search { "chip on" } else { "chip" },
                            onclick: move |_| { let v = !app.settings.peek().web_search; app.settings.write().web_search = v; app.save_settings() }, "Web search" }
                    }
                    // Opens the picker through the bridge's own file input (see BRIDGE in main.rs).
                    span { class: "chip file", "Image" }
                    span { class: "chip doc", "File" }
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
                div { class: "line",
                    textarea { id: "composer", rows: 1, placeholder: "Message", oninput: move |e| text.set(e.value()) }
                    if streaming {
                        button { class: "send stop", aria_label: "Stop", onclick: move |_| app.stop.set(true),
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
fn MsgView(m: Msg, last: bool, edit_at: Option<usize>, #[props(default)] live: bool) -> Element {
    let mut app = use_context::<App>();
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
            if m.error { p { "{m.content}" } }
            else { div { class: if live { "cursor" } else { "" }, dangerous_inner_html: "{html}" } }
            if !live {
                for (pi, page) in crate::code::html_blocks(answer, &m.runs.iter().flat_map(|r| r.pages.iter().map(|p| p.html.as_str())).collect::<Vec<_>>()).into_iter().enumerate() {
                    button { key: "page-{pi}", class: "page-btn", onclick: move |_| crate::code::open_page(&app, "Page", &page), "▶ Open this page" }
                }
            }
            if last {
                div { class: "meta",
                    if !m.error {
                        button { class: "bare", onclick: { let t = answer.to_string(); move |_| app.copy(&t, "Answer") }, "Copy" }
                    }
                    button { class: "bare", onclick: move |_| { app.conv.write().messages.pop(); run(app) }, "Retry" }
                    if let Some(u) = edit_at {
                        button { class: "bare", onclick: move |_| {
                            let t = { let mut c = app.conv.write(); let t = c.messages[u].content.clone(); c.messages.truncate(u); t };
                            js(&format!("const c = document.getElementById('composer'); c.value = {t:?}; c.dispatchEvent(new Event('input', {{bubbles: true}})); c.focus()"));
                            save(app);
                        }, "Edit" }
                    }
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
            if !r.output.is_empty() || !r.error.is_empty() {
                pre { class: "run-out",
                    "{r.output}"
                    if !r.error.is_empty() { span { class: "run-err", if r.output.is_empty() { "{r.error}" } else { "\n{r.error}" } } }
                }
            }
            for (i, img) in r.images.iter().enumerate() { img { key: "{i}", class: "run-img", src: "{img}" } }
            for (i, p) in r.pages.iter().enumerate() {
                button { key: "{i}", class: "page-btn",
                    onclick: { let p = p.clone(); move |_| crate::code::open_page(&app, &p.title, &p.html) },
                    "▶ Open " {if p.title.is_empty() { "page".to_string() } else { p.title.clone() }}
                }
            }
        }
    }
}

#[component]
fn Drawer() -> Element {
    let mut app = use_context::<App>();
    let mut confirm = use_signal(|| None::<String>);
    let current = app.conv.read().id.clone();
    rsx! {
        div { class: "veil", "data-back": "1", onclick: move |_| app.overlay.set(Overlay::None) }
        div { class: "drawer",
            div { class: "bar",
                div { class: "title", style: "text-align: left; padding-left: 8px", "Chats" }
                button { class: "bare", onclick: move |_| { new_chat(app); app.overlay.set(Overlay::None) }, "New chat" }
            }
            div { class: "scroll", style: "padding: 0",
                if app.convs.read().is_empty() { p { class: "muted", style: "padding: 16px", "No chats yet" } }
                for m in app.convs.read().clone() {
                    div { key: "{m.id}", class: if m.id == current { "item on" } else { "item" },
                        div { class: "grow", onclick: { let id = m.id.clone(); move |_| {
                            if app.live.peek().is_none() { app.conv.set(Conv::load(&id)) }
                            app.overlay.set(Overlay::None)
                        } }, "{m.title}" }
                        if confirm.read().as_deref() == Some(&m.id) {
                            button { class: "danger", style: "padding: 4px 10px", onclick: { let id = m.id.clone(); move |_| { delete(app, &id); confirm.set(None) } }, "Delete" }
                        } else {
                            button { class: "bare", onclick: { let id = m.id.clone(); move |_| confirm.set(Some(id.clone())) }, "✕" }
                        }
                    }
                }
            }
            div { class: "foot",
                button { onclick: move |_| { app.overlay.set(Overlay::None); app.screen.set(Screen::Account) },
                    "Account" {(*app.balance.read()).map(|b| format!(" · {}", usd(b))).unwrap_or_default()} }
            }
        }
    }
}

const ROLEPLAY: [&str; 15] = ["*mythomax*", "*remm-slerp*", "*rocinante*", "*magnum*", "*hanami*", "*euryale*", "*lunaris*", "*weaver*", "*lumimaid*", "*cydonia*", "*magidonia*", "*storybreaker*", "*tenyxchat*", "*aion-rp*", "*airoboros*"];
const REASONING: [&str; 5] = ["*thinking*", "*reasoning*", "*deepseek-r1*", "*-r1-*", "*magistral*"];
const CATEGORIES: [&str; 8] = ["Favorites", "All", "Free", "Cheap", "Reasoning", "Uncensored", "Roleplay", "Frontier"];

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
    let mut cat = use_signal(move || if favs.is_empty() { "All" } else { "Favorites" });
    let mut q = use_signal(String::new);
    let favs = app.settings.read().favorites.clone();
    let frontier = app.frontier.read().clone();
    let uncensored = app.uncensored.read().clone();
    let access = *app.access.read();
    let models = app.models.read().clone();
    let query = q.read().to_lowercase();
    let c = *cat.read();
    let any = |id: &str, pats: &[&str]| pats.iter().any(|p| glob(id, p));
    let needs = |id: &str| needs_plan(id, access, &frontier, &uncensored);
    let in_cat = |m: &Model, k: &str| {
        let free = m.prompt == 0.0 && m.completion == 0.0;
        match k {
            "Favorites" => favs.contains(&m.id),
            "Free" => free,
            "Cheap" => free || (m.prompt <= 0.000_000_25 && m.completion <= 0.000_000_5),
            "Reasoning" => any(&m.id, &REASONING),
            "Roleplay" => any(&m.id, &ROLEPLAY),
            "Uncensored" => uncensored.iter().any(|p| glob(&m.id, p)),
            "Frontier" => frontier.iter().any(|p| glob(&m.id, p)),
            _ => true,
        }
    };
    // Categories with models the plan can use come first and ones it can't use any of last;
    // empty ones are hidden (Favorites stays, to say how to fill it).
    let mut chips: Vec<(&'static str, bool)> = CATEGORIES.iter().filter_map(|&k| {
        let mut ms = models.iter().filter(|m| in_cat(m, k)).peekable();
        if ms.peek().is_none() { return (k == "Favorites" || k == "All").then_some((k, false)) }
        Some((k, ms.all(|m| needs(&m.id).is_some())))
    }).collect();
    chips.sort_by_key(|&(_, locked)| locked);
    // A search looks through every model, whatever category is picked.
    let (usable, locked): (Vec<Model>, Vec<Model>) = models.iter()
        .filter(|m| !query.is_empty() || in_cat(m, c))
        .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query) || m.id.to_lowercase().contains(&query))
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
