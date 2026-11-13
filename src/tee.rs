//! Encrypted mode: end-to-end encrypted chat with a TEE model, checked on the phone. Before sending,
//! the app fetches the TEE gateway's attestation for a nonce of its own and checks it, then
//! encrypts every message to the key it vouches for (`api::chat_encrypted`); the server passes the
//! request to the enclave unread and the answer back byte for byte, each piece encrypted to the
//! phone's key. After the answer, the app fetches the receipt the gateway signed for it and checks
//! it with `obscurify_tee_verify` against the same attestation: the bytes it got are the ones
//! signed, the enclave received exactly the conversation it encrypted, by a key bound to a genuine
//! Intel TDX machine running an approved gateway build, which checked the model host. Nothing here
//! trusts Obscurify's server.

use crate::{App, api, bg, chat, store};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

/// One model request of an answer and what its check found. A plain answer has one; a Code Mode
/// answer one per step.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Step {
    pub receipt: String,
    /// The verifier's report (`verified`, `checks`, ...), once it has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
    /// Why it couldn't be checked at all (the receipt never came, the app closed first...).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Sent end-to-end encrypted (Encrypted mode).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
}

impl Step {
    fn pending(&self) -> bool { self.report.is_none() && self.error.is_empty() }
    fn verified(&self) -> bool { self.report.as_ref().is_some_and(|r| r["verified"] == true) }
}

/// What an answer's chip says.
#[derive(PartialEq)]
pub enum State { Verifying, Verified, Failed }

pub fn state(steps: &[Step]) -> State {
    if steps.iter().any(Step::pending) {
        State::Verifying
    } else if steps.iter().all(Step::verified) {
        State::Verified
    } else {
        State::Failed
    }
}

/// Finished checks, by receipt, until the answer they belong to has them.
static DONE: Mutex<Option<HashMap<String, Step>>> = Mutex::new(None);
/// Checks running now. A step that's neither running nor done can't be checked any more: the app
/// closed in between, and the reply's bytes went with it.
static CHECKING: Mutex<Vec<String>> = Mutex::new(Vec::new());

const ORPHANED: &str = "The app closed before this reply was checked.";

/// The step as it stands now: finished, still being checked, or never to be.
fn current(s: &Step) -> Step {
    if !s.pending() { return s.clone() }
    if let Some(d) = DONE.lock().unwrap().as_ref().and_then(|m| m.get(&s.receipt)) { return d.clone() }
    if CHECKING.lock().unwrap().contains(&s.receipt) { return s.clone() }
    Step { error: ORPHANED.into(), ..s.clone() }
}

/// Checks one reply (the bytes it came in, its receipt and route) in the background. `model` is
/// the model asked, as the app names it; `sealed`, an encrypted reply's attestation, nonce and
/// plain request. The result lands on the message (`apply`).
pub fn check(app: App, chat: String, model: String, receipt: String, route: Option<String>, raw: Vec<u8>, sealed: Option<api::Sealed>) {
    CHECKING.lock().unwrap().push(receipt.clone());
    bg(async move {
        let step = run(&model, &receipt, route, raw, sealed).await;
        DONE.lock().unwrap().get_or_insert_with(HashMap::new).insert(receipt.clone(), step);
        CHECKING.lock().unwrap().retain(|r| *r != receipt);
        apply(app, &chat);
    });
}

async fn run(model: &str, receipt: &str, route: Option<String>, raw: Vec<u8>, sealed: Option<api::Sealed>) -> Step {
    let encrypted = sealed.is_some();
    let fail = |e: &str| Step { receipt: receipt.to_string(), report: None, error: e.to_string(), encrypted };
    let headers: Vec<(&str, String)> = route.map(|r| vec![("X-Obscurify-TEE-Route", r)]).unwrap_or_default();
    let q = urlencoding(model);
    // A stream's receipt is ready a few seconds after it ends.
    let mut receipt_json = None;
    for _ in 0..12 {
        match api::web_get(&format!("/web/tee/receipts/{receipt}?model={q}"), &headers, 30).await {
            Ok((200, text)) => { receipt_json = Some(text); break }
            Ok((404 | 502 | 503, _)) | Err(_) => crate::sleep(2500).await,
            Ok((status, text)) => return fail(&format!("The receipt couldn't be fetched (error {status}). {}", short(&text))),
        }
    }
    let Some(receipt_json) = receipt_json else { return fail("The receipt didn't come back in time.") };
    // An encrypted reply is checked against the attestation it was encrypted to.
    if let Some(s) = sealed {
        let evidence = obscurify_tee_verify::Evidence { body: raw, receipt_json, attestation_json: s.attestation, nonce: s.nonce, request: Some(s.plain.into_bytes()) };
        let options = obscurify_tee_verify::Options { pccs_url: format!("{}/tee/pccs", api::BASE), ..Default::default() };
        let report = obscurify_tee_verify::verify(&evidence, &options).await;
        return Step { receipt: receipt.to_string(), report: serde_json::to_value(&report).ok(), error: String::new(), encrypted };
    }
    let nonce = obscurify_tee_verify::new_nonce();
    let mut attestation_json = None;
    for _ in 0..3 {
        match api::web_get(&format!("/web/tee/attestation?model={q}&nonce={nonce}"), &headers, 100).await {
            Ok((200, text)) => { attestation_json = Some(text); break }
            Ok((502..=504, _)) | Err(_) => crate::sleep(3000).await,
            Ok((status, text)) => return fail(&format!("The attestation couldn't be fetched (error {status}). {}", short(&text))),
        }
    }
    let Some(attestation_json) = attestation_json else { return fail("The attestation didn't come back in time.") };
    let evidence = obscurify_tee_verify::Evidence { body: raw, receipt_json, attestation_json, nonce, request: None };
    let options = obscurify_tee_verify::Options { pccs_url: format!("{}/tee/pccs", api::BASE), ..Default::default() };
    let report = obscurify_tee_verify::verify(&evidence, &options).await;
    Step { receipt: receipt.to_string(), report: serde_json::to_value(&report).ok(), error: String::new(), encrypted }
}

fn short(text: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("").chars().take(160).collect()
}

pub fn urlencoding(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Fills in the checks that have finished. Whether any were.
pub fn fill(steps: &mut [Step]) -> bool {
    let done = DONE.lock().unwrap();
    let Some(map) = done.as_ref() else { return false };
    let mut changed = false;
    for s in steps.iter_mut().filter(|s| s.pending()) {
        if let Some(r) = map.get(&s.receipt) { *s = r.clone(); changed = true }
    }
    changed
}

/// Puts finished checks on their messages: the open chat (and the answer still being written), or
/// the chat's file if another one is open now.
pub fn apply(mut app: App, chat_id: &str) {
    if let Some(live) = app.live.write().as_mut() {
        fill(&mut live.tee);
    }
    if app.conv.peek().id == chat_id {
        let mut changed = false;
        for m in app.conv.write().messages.iter_mut() {
            changed |= fill(&mut m.tee);
        }
        if changed { chat::save_chat(app) }
        return;
    }
    let name = format!("chat-{chat_id}.json");
    let mut c: chat::Conv = store::load(&name);
    if c.id.is_empty() { return }
    let mut changed = false;
    for m in c.messages.iter_mut() {
        changed |= fill(&mut m.tee);
    }
    if changed { store::save(&name, &c) }
}

/// The model to use when Encrypted is switched on and the current one takes no encrypted requests.
pub fn pick_model(app: &App) -> Option<String> {
    let models = app.models.peek();
    let favs = app.settings.peek().favorites.clone();
    let ok: Vec<&crate::Model> = models.iter().filter(|m| m.e2ee).collect();
    ok.iter().find(|m| favs.contains(&m.id))
        .or_else(|| ok.iter().find(|m| m.id == "tee/glm-5.3"))
        .or_else(|| ok.iter().find(|m| m.id == "tee/glm-5.3-flash"))
        .or(ok.first())
        .map(|m| m.id.clone())
}

#[component]
pub fn Chip(steps: Vec<Step>) -> Element {
    let app = use_context::<App>();
    let steps: Vec<Step> = steps.iter().map(current).collect();
    let mut open = use_signal(|| false);
    let st = state(&steps);
    let (class, text) = match st {
        State::Verifying => ("tee-chip wait", "Verifying…".to_string()),
        State::Verified => ("tee-chip ok", "Verified".to_string()),
        State::Failed => ("tee-chip bad", "Not verified".to_string()),
    };
    let encrypted = steps.iter().any(|s| s.encrypted);
    let text = if encrypted { format!("Encrypted · {text}") } else { text };
    let reports: Vec<Value> = steps.iter().map(|s| s.report.clone().unwrap_or_else(|| json!({"receipt_id": s.receipt, "error": s.error}))).collect();
    let many = steps.len() > 1;
    // The first thing that failed, said on the chip itself.
    let why = steps.iter().find_map(|s| {
        if !s.error.is_empty() { return Some(s.error.clone()) }
        s.report.as_ref()?["checks"].as_array()?.iter().find(|c| c["status"] == "fail").and_then(|c| c["label"].as_str()).map(|l| format!("Failed: {l}"))
    });
    rsx! {
        div { class: "tee",
            button { class, onclick: move |_| { let o = !*open.peek(); open.set(o) }, aria_expanded: "{open}",
                svg { view_box: "0 0 24 24", width: "14", height: "14", fill: "none", stroke: "currentColor", stroke_width: "2.2", stroke_linecap: "round", stroke_linejoin: "round",
                    if encrypted {
                        rect { x: "5", y: "11", width: "14", height: "10", rx: "2" } path { d: "M8 11V7a4 4 0 0 1 8 0v4" }
                    } else {
                        path { d: "M12 3l7 3v5c0 4.5-3 8.5-7 10-4-1.5-7-5.5-7-10V6z" }
                        if st == State::Verified { path { d: "M8.5 12l2.5 2.5 4.5-5" } }
                    }
                }
                "{text}"
                if many { span { class: "muted", " · {steps.len()} steps" } }
            }
            if st == State::Failed && !*open.read() { if let Some(w) = why { span { class: "tee-why", "{w}" } } }
            if *open.read() {
                div { class: "tee-checks",
                    for (si, s) in steps.iter().enumerate() {
                        div { key: "{s.receipt}",
                            if many { div { class: "tee-step", "Step {si + 1}" } }
                            if !s.error.is_empty() { div { class: "tee-row fail", span { class: "mark", "✗" } span { "{s.error}" } } }
                            for c in s.report.as_ref().and_then(|r| r["checks"].as_array().cloned()).unwrap_or_default() {
                                div { class: format!("tee-row {}", c["status"].as_str().unwrap_or("")),
                                    span { class: "mark", {match c["status"].as_str() { Some("pass") => "✓", Some("fail") => "✗", _ => "–" }} }
                                    div {
                                        div { {c["label"].as_str().unwrap_or("").to_string()} }
                                        div { class: "muted small", {c["detail"].as_str().unwrap_or("").to_string()} }
                                    }
                                }
                            }
                            div { class: "muted small", "Receipt {s.receipt}" }
                        }
                    }
                    button { class: "bare", onclick: move |_| app.copy(&serde_json::to_string_pretty(&reports).unwrap_or_default(), "Report"), "Copy report" }
                }
            }
        }
    }
}
