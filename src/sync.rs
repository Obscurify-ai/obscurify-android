//! Encrypted chat sync with the website and other phones signed in with the same account number.
//!
//! Chats and favorites are encrypted here with a key from the account number's seed (private.rs)
//! before they leave the phone. The server (`/sync/v1`, see the website's src/api/sync.rs) gets
//! a token made from the same seed and blobs it can't open, under ids it can't tell apart, with
//! no cookie, over the same route as every other request (Tor, mirrors).
//!
//! The record format, keys and merge rules are the website's (static/js/chat/chat-sync.js,
//! "FORMAT"): a synced chat is the web's conversation object. This app keeps chats in its own
//! shape (chat.rs), so they are converted on the way in and out; web fields it doesn't use
//! (folders, pins, chat settings, Code Mode work) ride along in `extra`, and a web field it can't
//! read is kept under `_web_<name>` and put back on the way out.
//!
//! On by default once signed in with the number. The switch (Account) is per phone; off deletes
//! the synced copy, and the other devices stop too (they keep their chats).

use crate::chat::{Conv, Meta, Msg};
use crate::{App, api, store};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use dioxus::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

const STATE: &str = "sync-state.json";
const OFF: &str = "sync-off.json";
/// The seed after the account number changed here (else private.rs's, from signing in).
const SEED: &str = "sync-seed.json";
const ATT_MIN: usize = 2048;
const ATT_PREFIX: &str = "obscurify-att:";
const PAD: usize = 4096;
const MAX_RECORD: usize = 6 * 1024 * 1024;
const META_FIELDS: [&str; 6] = ["title", "titleSet", "folder", "pinned", "model", "settings"];
/// A web message field this app can't read, kept as it was.
const STASH: &str = "_web_";

// ---- state --------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone)]
struct Rec {
    #[serde(default)]
    rev: i64,
    #[serde(default)]
    dirty: bool,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    ver: u64,
    #[serde(default)]
    base: Option<Value>,
    #[serde(default)]
    atts: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Prefs {
    rev: i64,
    hash: String,
    /// The prefs as last synced: fields only the web has (presets) go back unchanged.
    last: Value,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    owner: String,
    after: i64,
    convs: BTreeMap<String, Rec>,
    /// Images known to the server, by hash.
    atts: BTreeMap<String, i64>,
    prefs: Option<Prefs>,
}

/// Load, change and save in one go (no await in between, so no other task sees it half done).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut s: State = store::load(STATE);
    let r = f(&mut s);
    store::save(STATE, &s);
    r
}

static STOPPED: AtomicBool = AtomicBool::new(false);
static FULL: AtomicBool = AtomicBool::new(false);
static BACKOFF_UNTIL: AtomicI64 = AtomicI64::new(0);

fn seed() -> Option<[u8; 32]> {
    let hex: Option<String> = store::load(SEED);
    match hex {
        Some(h) => unhex(&h),
        None => crate::private::seed(),
    }
}

pub fn available() -> bool { seed().is_some() }
pub fn on() -> bool { available() && !store::load::<bool>(OFF) }
pub fn full() -> bool { FULL.load(Ordering::Relaxed) }

// ---- keys and records (FORMAT) ------------------------------------------------------------------

struct Keys {
    token: String,
    aes: Aes256Gcm,
    id_key: [u8; 32],
    owner: String,
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn unhex(h: &str) -> Option<[u8; 32]> {
    let v: Vec<u8> = (0..h.len()).step_by(2).filter_map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect();
    v.try_into().ok()
}

fn keys_of(seed: [u8; 32]) -> Keys {
    let s = unlinkable_inference::seed::Seed(seed);
    let token = format!("sync1_{}", hex(&s.derive(&[b"chat-sync", b"token"])));
    let owner = hex(&Sha256::digest(token.as_bytes()))[..16].to_string();
    Keys {
        aes: Aes256Gcm::new_from_slice(&s.derive(&[b"chat-sync", b"key"])).expect("32-byte key"),
        id_key: s.derive(&[b"chat-sync", b"id"]),
        token,
        owner,
    }
}

fn keys() -> Option<Keys> { seed().map(keys_of) }

fn record_id(k: &Keys, local_key: &str) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&k.id_key).expect("any key length");
    m.update(local_key.as_bytes());
    hex(&m.finalize().into_bytes())[..32].to_string()
}

fn sha(s: &str) -> String { hex(&Sha256::digest(s.as_bytes())) }

fn seal(k: &Keys, id: &str, envelope: &Value) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = gz.write_all(envelope.to_string().as_bytes());
    let z = gz.finish().unwrap_or_default();
    let mut plain = vec![0u8; (z.len() + 4).div_ceil(PAD) * PAD];
    plain[..4].copy_from_slice(&(z.len() as u32).to_be_bytes());
    plain[4..4 + z.len()].copy_from_slice(&z);
    let nonce: [u8; 12] = rand::random();
    let ct = k.aes.encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: id.as_bytes() }).unwrap_or_default();
    let mut out = Vec::with_capacity(13 + ct.len());
    out.push(1);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn unseal(k: &Keys, id: &str, blob: &[u8]) -> Option<Value> {
    if blob.len() < 13 || blob[0] != 1 {
        return None;
    }
    let plain = k.aes.decrypt(Nonce::from_slice(&blob[1..13]), Payload { msg: &blob[13..], aad: id.as_bytes() }).ok()?;
    let n = u32::from_be_bytes(plain.get(..4)?.try_into().ok()?) as usize;
    let mut text = String::new();
    flate2::read::GzDecoder::new(plain.get(4..4 + n)?).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

// ---- the server ---------------------------------------------------------------------------------

enum Got {
    Ok(Value),
    Conflict(i64, Option<String>),
    Failed,
}

async fn call(k: &Keys, method: &str, path: &str, if_match: Option<i64>, body: Option<Vec<u8>>) -> Got {
    let (status, bytes, retry) = match api::sync_call(method, &format!("/sync/v1{path}"), &k.token, if_match, body).await {
        Ok(r) => r,
        Err(_) => {
            BACKOFF_UNTIL.store(store::now() + 60_000, Ordering::Relaxed);
            return Got::Failed;
        }
    };
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match status {
        200..=299 => Got::Ok(v),
        409 => Got::Conflict(v["rev"].as_i64().unwrap_or(0), v["data"].as_str().map(String::from)),
        404 => { STOPPED.store(true, Ordering::Relaxed); Got::Failed }
        413 => { FULL.store(true, Ordering::Relaxed); Got::Failed }
        _ => {
            BACKOFF_UNTIL.store(store::now() + 1000 * retry.unwrap_or(60).min(3600) as i64, Ordering::Relaxed);
            Got::Failed
        }
    }
}

// ---- the web's chats in this app's shape --------------------------------------------------------

/// The fields of `Msg` this app reads; anything else from the web goes in `extra`.
const MSG_FIELDS: [&str; 15] = ["role", "images", "files", "reasoning", "details", "searches", "model", "error", "interrupted", "runs", "trace", "spent", "steps", "compacted", "tee"];

fn text_of(m: &Value) -> String {
    match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn msg_from_web(v: &Value) -> Msg {
    let Some(src) = v.as_object() else { return Msg::default() };
    let mut out = Map::new();
    out.insert("content".into(), Value::String(text_of(v)));
    if let Value::Array(parts) = &src["content"] {
        let images: Vec<Value> = parts.iter().filter_map(|p| p["image_url"]["url"].as_str().map(|u| Value::String(u.into()))).collect();
        out.insert("images".into(), Value::Array(images));
        out.insert(format!("{STASH}content"), src["content"].clone());
    }
    for (key, val) in src {
        if key == "content" || key == "images" {
            continue;
        }
        let (key, val) = if key == "reasoning_details" { ("details".to_string(), val) } else { (key.clone(), val) };
        if MSG_FIELDS.contains(&key.as_str()) {
            let probe = json!({ "role": "user", "content": "", key.as_str(): val });
            if serde_json::from_value::<Msg>(probe).is_err() {
                out.insert(format!("{STASH}{}", if key == "details" { "reasoning_details" } else { &key }), val.clone());
                continue;
            }
        }
        out.insert(key, val.clone());
    }
    serde_json::from_value(Value::Object(out)).unwrap_or_default()
}

fn msg_to_web(m: &Msg) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
    let Some(o) = v.as_object_mut() else { return v };
    if o.get("error") == Some(&Value::Bool(false)) {
        o.remove("error");
    }
    let images: Vec<String> = o.remove("images").and_then(|i| serde_json::from_value(i).ok()).unwrap_or_default();
    let orig = o.remove(&format!("{STASH}content"));
    match orig {
        // The web's own content, unless the text changed here.
        Some(c) if text_of(&json!({ "content": c })).trim() == m.content.trim() => { o.insert("content".into(), c); }
        _ if m.role == "user" => {
            let mut parts = vec![json!({ "type": "text", "text": m.content })];
            parts.extend(images.iter().map(|u| json!({ "type": "image_url", "image_url": { "url": u } })));
            o.insert("content".into(), Value::Array(parts));
        }
        _ => {
            if !images.is_empty() {
                o.insert("images".into(), json!(images));
            }
        }
    }
    if let Some(d) = o.remove("details") {
        o.insert("reasoning_details".into(), d);
    }
    let stashed: Vec<String> = o.keys().filter(|k| k.starts_with(STASH)).cloned().collect();
    for k in stashed {
        if let Some(val) = o.remove(&k) {
            o.insert(k[STASH.len()..].to_string(), val);
        }
    }
    v
}

fn valid_id(id: &str) -> bool { (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') }

fn conv_from_web(v: &Value) -> Option<(Conv, i64)> {
    let o = v.as_object()?;
    let id = o.get("id")?.as_str()?.to_string();
    if !valid_id(&id) {
        return None;
    }
    let mut extra = Map::new();
    let mut compaction = None;
    for (k, val) in o {
        match k.as_str() {
            "id" | "title" | "messages" => {}
            "compaction" => match serde_json::from_value(val.clone()) {
                Ok(c) => compaction = c,
                Err(_) => { extra.insert(format!("{STASH}compaction"), val.clone()); }
            },
            _ => { extra.insert(k.clone(), val.clone()); }
        }
    }
    let updated = o.get("updatedAt").and_then(Value::as_i64).unwrap_or_else(store::now);
    let conv = Conv {
        temp: false,
        id,
        title: o.get("title").and_then(Value::as_str).unwrap_or("New conversation").to_string(),
        messages: o.get("messages").and_then(Value::as_array).map(|m| m.iter().map(msg_from_web).collect()).unwrap_or_default(),
        compaction,
        extra,
    };
    Some((conv, updated))
}

fn conv_to_web(c: &Conv, updated: i64) -> Value {
    let mut o = c.extra.clone();
    let stashed: Vec<String> = o.keys().filter(|k| k.starts_with(STASH)).cloned().collect();
    for k in stashed {
        if let Some(val) = o.remove(&k) {
            o.insert(k[STASH.len()..].to_string(), val);
        }
    }
    o.insert("id".into(), json!(c.id));
    o.insert("title".into(), json!(c.title));
    o.insert("messages".into(), Value::Array(c.messages.iter().map(msg_to_web).collect()));
    if let Some(cp) = &c.compaction {
        o.insert("compaction".into(), serde_json::to_value(cp).unwrap_or(Value::Null));
    }
    o.insert("updatedAt".into(), json!(updated));
    if !o.contains_key("createdAt") {
        let made = c.id.strip_prefix('c').and_then(|t| t.parse::<i64>().ok()).unwrap_or(updated);
        o.insert("createdAt".into(), json!(made));
    }
    if !o.contains_key("model") && let Some(m) = c.messages.iter().rev().find(|m| !m.model.is_empty()) {
        o.insert("model".into(), json!(m.model));
    }
    Value::Object(o)
}

// ---- images: records of their own ---------------------------------------------------------------

fn att_file(h: &str) -> String { format!("sync-att-{h}.txt") }

fn map_strings(v: &Value, f: &mut impl FnMut(&str) -> Option<String>) -> Value {
    match v {
        Value::String(s) => f(s).map(Value::String).unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(|x| map_strings(x, f)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), map_strings(x, f))).collect()),
        _ => v.clone(),
    }
}

fn split_images(conv: &Value) -> (Value, BTreeMap<String, String>) {
    let mut atts = BTreeMap::new();
    let out = map_strings(conv, &mut |s| {
        (s.len() > ATT_MIN && s.starts_with("data:")).then(|| {
            let h = sha(s);
            atts.entry(h.clone()).or_insert_with(|| s.to_string());
            format!("{ATT_PREFIX}{h}")
        })
    });
    (out, atts)
}

fn refs(conv: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    map_strings(conv, &mut |s| { if let Some(h) = s.strip_prefix(ATT_PREFIX) { out.insert(h.to_string()); } None });
    out.into_iter().collect()
}

fn join_images(conv: &Value) -> Value {
    map_strings(conv, &mut |s| {
        let h = s.strip_prefix(ATT_PREFIX)?;
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) { return None }
        std::fs::read_to_string(store::dir().join(att_file(h))).ok()
    })
}

async fn upload_image(k: &Keys, h: &str, data: &str) -> bool {
    if with_state(|s| s.atts.contains_key(h)) {
        return true;
    }
    let _ = std::fs::write(store::dir().join(att_file(h)), data);
    let id = record_id(k, &format!("att:{h}"));
    let blob = seal(k, &id, &json!({ "v": 1, "k": "att", "h": h, "data": data }));
    if blob.len() > MAX_RECORD {
        return true; // too big to sync: other devices see it missing
    }
    match call(k, "PUT", &format!("/records/{id}"), Some(0), Some(blob)).await {
        Got::Ok(v) => { with_state(|s| s.atts.insert(h.into(), v["rev"].as_i64().unwrap_or(0))); true }
        Got::Conflict(rev, _) => { with_state(|s| s.atts.insert(h.into(), rev)); true }
        Got::Failed => false,
    }
}

async fn drop_images(k: &Keys, hashes: Vec<String>) {
    for h in hashes {
        let used = with_state(|s| s.convs.values().any(|r| !r.deleted && r.atts.contains(&h)) || !s.atts.contains_key(&h));
        if used {
            continue;
        }
        if let Got::Ok(_) = call(k, "DELETE", &format!("/records/{}", record_id(k, &format!("att:{h}"))), None, None).await {
            with_state(|s| s.atts.remove(&h));
            store::remove(&att_file(&h));
        }
    }
}

// ---- merging (FORMAT) ---------------------------------------------------------------------------

fn same_message(a: &Value, b: &Value) -> bool { a["role"] == b["role"] && text_of(a).trim() == text_of(b).trim() }

fn is_prefix(a: &[Value], b: &[Value]) -> bool { a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| same_message(x, y)) }

fn fields_of(c: &Value) -> Value {
    let mut o = Map::new();
    for f in META_FIELDS {
        if let Some(v) = c.get(f).filter(|v| !v.is_null()) {
            o.insert(f.into(), v.clone());
        }
    }
    Value::Object(o)
}

/// What to keep, and a copy of `local` when the two can't be put together.
fn merge(local: &Value, remote: &Value, base: Option<&Value>) -> (Value, Option<Value>) {
    let empty = vec![];
    let l = local["messages"].as_array().unwrap_or(&empty);
    let r = remote["messages"].as_array().unwrap_or(&empty);
    let mut out = remote.clone();
    if is_prefix(r, l) {
        out["messages"] = local["messages"].clone();
        match local.get("compaction") { Some(c) => out["compaction"] = c.clone(), None => { out.as_object_mut().map(|o| o.remove("compaction")); } }
    } else if !is_prefix(l, r) {
        let mut copy = local.clone();
        copy["id"] = json!(format!("c{}", store::now()));
        return (remote.clone(), Some(copy));
    }
    for f in META_FIELDS {
        let mine = local.get(f).filter(|v| !v.is_null());
        let was = base.and_then(|b| b.get(f)).filter(|v| !v.is_null());
        if mine != was {
            match mine {
                Some(v) => out[f] = v.clone(),
                None => { out.as_object_mut().map(|o| o.remove(f)); }
            }
        }
    }
    let at = local["updatedAt"].as_i64().unwrap_or(0).max(remote["updatedAt"].as_i64().unwrap_or(0));
    out["updatedAt"] = json!(at);
    (out, None)
}

// ---- this phone's chats -------------------------------------------------------------------------

fn updated_of(app: &App, id: &str) -> i64 { app.convs.peek().iter().find(|m| m.id == id).map(|m| m.updated).unwrap_or_else(store::now) }

fn local(app: &App, id: &str) -> Option<Value> {
    if !app.convs.peek().iter().any(|m| m.id == id) {
        return None;
    }
    let c: Conv = store::load(&format!("chat-{id}.json"));
    (!c.id.is_empty()).then(|| conv_to_web(&c, updated_of(app, id)))
}

/// Writes a chat from another device here (not a change to send back).
fn write(mut app: App, v: &Value) {
    let Some((conv, updated)) = conv_from_web(v) else { return };
    store::save(&format!("chat-{}.json", conv.id), &conv);
    {
        let mut list = app.convs.write();
        list.retain(|m| m.id != conv.id);
        list.push(Meta { id: conv.id.clone(), title: conv.title.clone(), updated });
        list.sort_by_key(|m| std::cmp::Reverse(m.updated));
        store::save("chats.json", &*list);
    }
    if app.conv.peek().id == conv.id && app.live.peek().is_none() && *app.conv.peek() != conv {
        app.conv.set(conv);
    }
}

fn remove(mut app: App, id: &str) {
    store::remove(&format!("chat-{id}.json"));
    let mut list = app.convs.write();
    list.retain(|m| m.id != id);
    store::save("chats.json", &*list);
    if app.conv.peek().id == id && app.live.peek().is_none() {
        app.conv.set(Conv::new());
    }
}

/// A chat was saved here (chat.rs).
pub fn changed(app: App, id: &str) {
    if !on() {
        return;
    }
    with_state(|s| {
        let r = s.convs.entry(id.to_string()).or_default();
        r.dirty = true;
        r.deleted = false;
        r.ver += 1;
    });
    soon(app);
}

/// A chat was deleted here (chat.rs).
pub fn deleted(app: App, id: &str) {
    if !on() {
        return;
    }
    with_state(|s| match s.convs.get_mut(id) {
        Some(r) if r.rev > 0 => { r.dirty = true; r.deleted = true; r.ver += 1; }
        _ => { s.convs.remove(id); }
    });
    soon(app);
}

fn favorites(app: &App) -> Value { json!(app.settings.peek().favorites) }

// ---- pulling ------------------------------------------------------------------------------------

async fn apply(mut app: App, k: &Keys, id: &str, rev: i64, data: &str) {
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(data) else { return };
    let Some(env) = unseal(k, id, &blob) else { eprintln!("sync: a record could not be read"); return };
    match env["k"].as_str() {
        Some("att") => {
            if let (Some(h), Some(d)) = (env["h"].as_str(), env["data"].as_str())
                && h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
            {
                let _ = std::fs::write(store::dir().join(att_file(h)), d);
                with_state(|s| s.atts.insert(h.into(), rev));
            }
        }
        Some("prefs") => {
            let mine = sha(&favorites(&app).to_string());
            let changed_here = with_state(|s| s.prefs.as_ref().is_some_and(|p| p.hash != mine));
            if changed_here {
                with_state(|s| if let Some(p) = s.prefs.as_mut() { p.rev = rev });
                return;
            }
            if let Some(fav) = env["prefs"]["favorites"].as_array() {
                app.settings.write().favorites = fav.iter().filter_map(|f| f.as_str().map(String::from)).collect();
                app.save_settings();
            }
            let hash = sha(&favorites(&app).to_string());
            with_state(|s| s.prefs = Some(Prefs { rev, hash, last: env["prefs"].clone() }));
        }
        Some("conv") => {
            let Some(cid) = env["id"].as_str().filter(|c| valid_id(c)).map(String::from) else { return };
            let rec = with_state(|s| s.convs.get(&cid).cloned()).unwrap_or_default();
            let here = local(&app, &cid);
            let changed_here = rec.dirty && here.is_some() && !rec.deleted;
            if env["deleted"] == true {
                if changed_here {
                    with_state(|s| if let Some(r) = s.convs.get_mut(&cid) { r.rev = rev });
                    return;
                }
                remove(app, &cid);
                with_state(|s| s.convs.insert(cid, Rec { rev, deleted: true, ..Default::default() }));
                return;
            }
            let mut remote = join_images(&env["conv"]);
            remote["id"] = json!(cid);
            let mut keep = remote.clone();
            let mut dirty = false;
            if changed_here && let Some(here) = here {
                let (merged, copy) = merge(&here, &remote, rec.base.as_ref());
                dirty = merged != remote;
                keep = merged;
                if let Some(copy) = copy {
                    write(app, &copy);
                    if let Some(id) = copy["id"].as_str() { changed(app, id) }
                }
            }
            with_state(|s| s.convs.insert(cid.clone(), Rec { rev, dirty, deleted: false, ver: rec.ver + 1, base: Some(fields_of(&remote)), atts: refs(&env["conv"]) }));
            write(app, &keep);
        }
        _ => {}
    }
}

async fn pull(app: App, k: &Keys) -> bool {
    for _ in 0..100 {
        // An answer being written is saved when it ends; pull after that.
        if app.live.peek().is_some() {
            return true;
        }
        let after = with_state(|s| s.after);
        let Got::Ok(v) = call(k, "GET", &format!("/records?after={after}"), None, None).await else { return false };
        if v["rev"].as_i64().unwrap_or(0) < after {
            turned_off_elsewhere();
            return false;
        }
        for r in v["records"].as_array().into_iter().flatten() {
            let (Some(id), Some(rev), Some(data)) = (r["id"].as_str(), r["rev"].as_i64(), r["data"].as_str()) else { continue };
            apply(app, k, id, rev, data).await;
            with_state(|s| s.after = rev);
        }
        if v["more"] != true {
            break;
        }
    }
    true
}

// ---- pushing ------------------------------------------------------------------------------------

enum Pushed { Done, Again, Failed }

async fn push_conv(app: App, k: &Keys, id: &str) -> Pushed {
    let Some(rec) = with_state(|s| s.convs.get(id).cloned()) else { return Pushed::Done };
    if !rec.dirty {
        return Pushed::Done;
    }
    let here = local(&app, id);
    let mut hashes = vec![];
    let env = match &here {
        Some(conv) if !rec.deleted => {
            let (conv, atts) = split_images(conv);
            for (h, data) in &atts {
                if !upload_image(k, h, data).await { return Pushed::Failed }
            }
            hashes = atts.into_keys().collect();
            json!({ "v": 1, "k": "conv", "id": id, "conv": conv })
        }
        _ => {
            if rec.rev == 0 {
                with_state(|s| s.convs.remove(id));
                return Pushed::Done;
            }
            json!({ "v": 1, "k": "conv", "id": id, "deleted": true })
        }
    };
    let rid = record_id(k, &format!("conv:{id}"));
    let blob = seal(k, &rid, &env);
    if blob.len() > MAX_RECORD {
        with_state(|s| if let Some(r) = s.convs.get_mut(id) { r.dirty = false });
        return Pushed::Done;
    }
    match call(k, "PUT", &format!("/records/{rid}"), Some(rec.rev), Some(blob)).await {
        Got::Ok(v) => {
            let rev = v["rev"].as_i64().unwrap_or(0);
            let old = rec.atts.clone();
            if env["deleted"] == true {
                with_state(|s| s.convs.insert(id.into(), Rec { rev, deleted: true, ..Default::default() }));
                drop_images(k, old).await;
            } else {
                let base = here.as_ref().map(fields_of);
                let keep = hashes.clone();
                with_state(|s| {
                    let r = s.convs.entry(id.into()).or_default();
                    r.dirty = r.ver != rec.ver;
                    r.rev = rev;
                    r.deleted = false;
                    r.base = base;
                    r.atts = keep;
                });
                drop_images(k, old.into_iter().filter(|h| !hashes.contains(h)).collect()).await;
            }
            Pushed::Done
        }
        Got::Conflict(rev, Some(data)) => { apply(app, k, &rid, rev, &data).await; Pushed::Again }
        Got::Conflict(_, None) => { turned_off_elsewhere(); Pushed::Failed }
        Got::Failed => Pushed::Failed,
    }
}

async fn push_prefs(app: App, k: &Keys) {
    let fav = favorites(&app);
    let hash = sha(&fav.to_string());
    let prefs = with_state(|s| s.prefs.clone());
    if prefs.as_ref().is_some_and(|p| p.hash == hash) || (prefs.is_none() && fav.as_array().is_none_or(|f| f.is_empty())) {
        return;
    }
    let mut out = prefs.as_ref().map(|p| p.last.clone()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    out["favorites"] = fav;
    let rid = record_id(k, "prefs");
    let blob = seal(k, &rid, &json!({ "v": 1, "k": "prefs", "prefs": out }));
    match call(k, "PUT", &format!("/records/{rid}"), Some(prefs.as_ref().map_or(0, |p| p.rev)), Some(blob)).await {
        Got::Ok(v) => { let rev = v["rev"].as_i64().unwrap_or(0); with_state(|s| s.prefs = Some(Prefs { rev, hash, last: out })); }
        // Ours wins: try again next round at the revision there now.
        Got::Conflict(rev, Some(_)) => with_state(|s| match s.prefs.as_mut() {
            Some(p) => p.rev = rev,
            None => s.prefs = Some(Prefs { rev, hash: String::new(), last: json!({}) }),
        }),
        Got::Conflict(_, None) => turned_off_elsewhere(),
        Got::Failed => {}
    }
}

// ---- running ------------------------------------------------------------------------------------

fn clear() {
    let s: State = store::load(STATE);
    for h in s.atts.keys() {
        store::remove(&att_file(h));
    }
    store::remove(STATE);
}

/// The synced copy is gone (turned off on another device): stop here too, keep the chats.
fn turned_off_elsewhere() {
    store::save(OFF, &true);
    clear();
}

async fn run(app: App) {
    if !on() || STOPPED.load(Ordering::Relaxed) || store::now() < BACKOFF_UNTIL.load(Ordering::Relaxed) || !api::has_session() {
        return;
    }
    let Some(k) = keys() else { return };
    with_state(|s| if s.owner != k.owner { *s = State { owner: k.owner.clone(), ..Default::default() } });
    // Chats with nothing on record yet go up.
    let ids: Vec<String> = app.convs.peek().iter().map(|m| m.id.clone()).collect();
    with_state(|s| for id in ids {
        s.convs.entry(id).or_insert_with(|| Rec { dirty: true, ver: 1, ..Default::default() });
    });
    if !pull(app, &k).await || !on() {
        return;
    }
    let dirty: Vec<String> = with_state(|s| s.convs.iter().filter(|(_, r)| r.dirty).map(|(id, _)| id.clone()).collect());
    for id in dirty {
        for _ in 0..3 {
            if !matches!(push_conv(app, &k, &id).await, Pushed::Again) { break }
        }
        if STOPPED.load(Ordering::Relaxed) || FULL.load(Ordering::Relaxed) || store::now() < BACKOFF_UNTIL.load(Ordering::Relaxed) || !on() {
            return;
        }
    }
    push_prefs(app, &k).await;
}

static BUSY: AtomicBool = AtomicBool::new(false);
static AGAIN: AtomicBool = AtomicBool::new(false);

/// One round: what changed elsewhere comes down, what changed here goes up.
pub async fn tick(app: App) {
    if BUSY.swap(true, Ordering::SeqCst) {
        AGAIN.store(true, Ordering::SeqCst);
        return;
    }
    run(app).await;
    BUSY.store(false, Ordering::SeqCst);
    if AGAIN.swap(false, Ordering::SeqCst) {
        soon(app);
    }
}

static SOON: AtomicI64 = AtomicI64::new(0);

/// A round 1.5 s after the last change.
fn soon(app: App) {
    let at = store::now() + 1500;
    SOON.store(at, Ordering::SeqCst);
    dioxus::core::spawn_forever(async move {
        crate::sleep(1500).await;
        if SOON.load(Ordering::SeqCst) == at {
            tick(app).await;
        }
    });
}

async fn delete_remote() -> bool {
    let Some(k) = keys() else { return true };
    matches!(call(&k, "DELETE", "", None, None).await, Got::Ok(_)) || STOPPED.load(Ordering::Relaxed)
}

/// The switch on the Account screen. Off deletes the synced copy.
pub async fn set_on(app: App, v: bool) -> bool {
    if v {
        store::remove(OFF);
        STOPPED.store(false, Ordering::Relaxed);
        BACKOFF_UNTIL.store(0, Ordering::Relaxed);
        tick(app).await;
        return true;
    }
    store::save(OFF, &true);
    while BUSY.load(Ordering::SeqCst) {
        crate::sleep(50).await;
    }
    let ok = delete_remote().await;
    clear();
    ok
}

/// Wipe: the synced copy goes too, or the chats would come straight back.
pub async fn wipe() {
    if on() {
        delete_remote().await;
    }
    clear();
}

/// Signing out: this phone's chats go (chat.rs), the synced copy stays for the next sign-in.
pub fn forget() {
    clear();
    store::remove(SEED);
}

/// A new account number (Account): the chats move to the new number's store.
pub async fn move_to(app: App, new_seed: [u8; 32]) {
    let was_on = on();
    while BUSY.load(Ordering::SeqCst) {
        crate::sleep(50).await;
    }
    if was_on && let Some(old) = keys() {
        let _ = call(&old, "DELETE", "", None, None).await;
    }
    clear();
    store::save(SEED, &Some(hex(&new_seed)));
    if was_on {
        tick(app).await;
    }
}

/// The Account screen's switch.
#[component]
pub fn SyncSection() -> Element {
    let app = use_context::<App>();
    let mut enabled = use_signal(on);
    let mut asking = use_signal(|| false);
    let mut busy = use_signal(|| false);
    rsx! {
        div { class: "section",
            h2 { "Sync" }
            if !available() {
                p { class: "muted", "Sign in with your account number on this phone to sync chats." }
            } else {
                label { class: "tor-always", id: "sync-switch",
                    input { r#type: "checkbox", id: "sync-on", checked: enabled(), disabled: busy(),
                        onchange: move |e| {
                            if e.checked() {
                                enabled.set(true);
                                busy.set(true);
                                bg_run(async move { set_on(app, true).await; busy.set(false) });
                            } else {
                                enabled.set(true);
                                asking.set(true);
                            }
                        } }
                    "Sync chats between devices"
                }
                p { class: "muted", "Encrypted on this phone with a key from your account number. We hold only ciphertext, never the key." }
                if full() { p { class: "muted", "Sync storage for this account is full: new chats stay on this phone." } }
                if asking() {
                    p { class: "muted", "Stop syncing? The synced copy is deleted and your other devices stop syncing too. Chats stay on each device." }
                    div { class: "row", style: "border: 0",
                        button { class: "danger", id: "sync-off-confirm", onclick: move |_| {
                            asking.set(false);
                            enabled.set(false);
                            busy.set(true);
                            bg_run(async move { set_on(app, false).await; busy.set(false) });
                        }, "Stop syncing" }
                        button { class: "bare", onclick: move |_| asking.set(false), "Cancel" }
                    }
                }
            }
        }
    }
}

fn bg_run(f: impl std::future::Future<Output = ()> + 'static) { dioxus::core::spawn_forever(f); }
