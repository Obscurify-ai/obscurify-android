//! Encrypted chat sync with the website and other phones signed in with the same account code.
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
//!
//! Joining: a phone with chats from before it had the number (signed out, or an app from before
//! sync) that finds other devices' chats already synced asks first ("Combine chats?", [`JoinAsk`]).
//! Until then it takes nothing in and sends nothing. Combine puts all of them on every device;
//! "Not on this phone" turns sync off here only (the synced copy and the other devices carry on).

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
/// The account (owner) this phone joined: agreed to combine with, or had nothing to combine.
const JOINED: &str = "sync-joined.json";
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
    /// Over the record size limit: stays on this phone.
    #[serde(default)]
    too_big: bool,
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
    /// The store's random epoch (`None` until first contact): another one means it was deleted.
    #[serde(default)]
    epoch: Option<String>,
    /// Chats made under another account on this phone (the session ended without signing out):
    /// never uploaded to this one.
    #[serde(default)]
    foreign: BTreeSet<String>,
    after: i64,
    convs: BTreeMap<String, Rec>,
    /// Images known to the server, by hash.
    atts: BTreeMap<String, i64>,
    /// Images deleted on the server (tombstones, by hash: their revision). A chat here that still
    /// uses one puts it back over the tombstone.
    #[serde(default)]
    gone_atts: BTreeMap<String, i64>,
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
/// Wipe in progress: nothing from the server may be written back meanwhile.
static WIPING: AtomicBool = AtomicBool::new(false);
static FULL: AtomicBool = AtomicBool::new(false);
static BACKOFF_UNTIL: AtomicI64 = AtomicI64::new(0);

fn seed() -> Option<[u8; 32]> {
    let hex: Option<String> = store::load(SEED);
    match hex {
        Some(h) => unhex(&h),
        None => crate::private::seed(),
    }
}

/// Only accounts with an account code sync: 16 digits can be guessed by someone holding the
/// server, words can't (private.rs keeps which this phone signed in with).
pub fn available() -> bool { crate::private::has_code() && seed().is_some() }
/// Bumped when the account switches to words (Account), so the switch shows it.
pub static REFRESH: GlobalSignal<u32> = Signal::global(|| 0);
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
    /// 409: the record's revision and data now, and the store's epoch (`None`: no store).
    Conflict(i64, Option<String>, Option<String>),
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
        409 => Got::Conflict(v["rev"].as_i64().unwrap_or(0), v["data"].as_str().map(String::from), v["epoch"].as_str().map(String::from)),
        404 => { STOPPED.store(true, Ordering::Relaxed); Got::Failed }
        413 => { FULL.store(true, Ordering::Relaxed); Got::Failed }
        _ => {
            BACKOFF_UNTIL.store(store::now() + 1000 * retry.unwrap_or(60).min(3600) as i64, Ordering::Relaxed);
            Got::Failed
        }
    }
}

/// False: the store isn't the one this phone synced with (deleted, or deleted and made again):
/// sync was turned off on another device, so it stops here too.
fn same_store(epoch: Option<&str>) -> bool {
    let mine = with_state(|s| s.epoch.clone());
    if mine.as_deref() == epoch {
        return true;
    }
    if mine.is_some() {
        turned_off_elsewhere();
        return false;
    }
    with_state(|s| s.epoch = epoch.map(String::from));
    true
}

/// A write answered from another store: this phone made it again after another device deleted it.
async fn made_here(k: &Keys, v: &Value) -> bool {
    let epoch = v["epoch"].as_str();
    let mine = with_state(|s| s.epoch.clone());
    match mine {
        None => { with_state(|s| s.epoch = epoch.map(String::from)); true }
        Some(e) if Some(e.as_str()) == epoch => true,
        Some(_) => {
            let _ = call(k, "DELETE", "", None, None).await;
            turned_off_elsewhere();
            false
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
    // Over a tombstone (deleted elsewhere while a chat here still used it): at its revision.
    let mut if_match = with_state(|s| s.gone_atts.get(h).copied()).unwrap_or(0);
    for _ in 0..2 {
        match call(k, "PUT", &format!("/records/{id}"), Some(if_match), Some(blob.clone())).await {
            Got::Ok(v) => {
                if !made_here(k, &v).await { return false }
                with_state(|s| { s.atts.insert(h.into(), v["rev"].as_i64().unwrap_or(0)); s.gone_atts.remove(h); });
                return true;
            }
            Got::Conflict(rev, data, epoch) => {
                if !same_store(epoch.as_deref()) { return false }
                let there = data
                    .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
                    .and_then(|b| unseal(k, &id, &b));
                if there.as_ref().is_some_and(|e| e["deleted"] == true) {
                    if_match = rev; // a tombstone: put it back
                    continue;
                }
                // Already there (the same image in another chat, or from another device).
                with_state(|s| { s.atts.insert(h.into(), rev); s.gone_atts.remove(h); });
                return true;
            }
            Got::Failed => return false,
        }
    }
    false
}
/// Images no synced chat here uses any more become tombstones on the server (FORMAT, chat-sync.js).
async fn drop_images(k: &Keys, hashes: Vec<String>) {
    for h in hashes {
        let used = with_state(|s| s.convs.values().any(|r| !r.deleted && r.atts.contains(&h)));
        let Some(rev) = with_state(|s| s.atts.get(&h).copied()) else { continue };
        if used {
            continue;
        }
        let id = record_id(k, &format!("att:{h}"));
        let tomb = seal(k, &id, &json!({ "v": 1, "k": "att", "h": h, "deleted": true }));
        if let Got::Ok(v) = call(k, "PUT", &format!("/records/{id}"), Some(rev), Some(tomb)).await {
            let new = v["rev"].as_i64().unwrap_or(0);
            with_state(|s| { s.atts.remove(&h); s.gone_atts.insert(h.clone(), new); });
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
    if WIPING.load(Ordering::SeqCst) {
        return;
    }
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
        if s.foreign.contains(id) {
            return;
        }
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
        // Off: nothing to send, but its record (its title as last synced) goes.
        with_state(|s| { s.convs.remove(id); });
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

/// False: deferred. The chat is open with an answer being written; applying it now would be
/// overwritten when the answer is saved, so it comes again next round.
async fn apply(mut app: App, k: &Keys, id: &str, rev: i64, data: &str) -> bool {
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(data) else { return true };
    let Some(env) = unseal(k, id, &blob) else { eprintln!("sync: a record could not be read"); return true };
    match env["k"].as_str() {
        Some("att") => {
            let Some(h) = env["h"].as_str().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())) else { return true };
            if env["deleted"] == true {
                // Deleted elsewhere: chats here that still use it put it back (next push).
                with_state(|s| {
                    s.atts.remove(h);
                    s.gone_atts.insert(h.into(), rev);
                    for r in s.convs.values_mut().filter(|r| !r.deleted && r.rev > 0 && r.atts.iter().any(|a| a == h)) {
                        r.dirty = true;
                        r.ver += 1;
                    }
                });
            } else if let Some(d) = env["data"].as_str() {
                let _ = std::fs::write(store::dir().join(att_file(h)), d);
                with_state(|s| { s.atts.insert(h.into(), rev); s.gone_atts.remove(h); });
                // Chats here that came without it (it was gone when they did) get it now.
                let placeholder = format!("{ATT_PREFIX}{h}");
                let waiting: Vec<String> = with_state(|s| s.convs.iter().filter(|(_, r)| r.atts.iter().any(|a| a == h)).map(|(id, _)| id.clone()).collect());
                for cid in waiting {
                    if let Some(v) = local(&app, &cid) && v.to_string().contains(&placeholder) {
                        write(app, &join_images(&v));
                    }
                }
            }
        }
        Some("prefs") => {
            let mine = sha(&favorites(&app).to_string());
            let changed_here = with_state(|s| s.prefs.as_ref().is_some_and(|p| p.hash != mine));
            if changed_here {
                with_state(|s| if let Some(p) = s.prefs.as_mut() { p.rev = rev });
                return true;
            }
            if let Some(fav) = env["prefs"]["favorites"].as_array() {
                app.settings.write().favorites = fav.iter().filter_map(|f| f.as_str().map(String::from)).collect();
                app.save_settings();
            }
            let hash = sha(&favorites(&app).to_string());
            with_state(|s| s.prefs = Some(Prefs { rev, hash, last: env["prefs"].clone() }));
        }
        Some("conv") => {
            let Some(cid) = env["id"].as_str().filter(|c| valid_id(c)).map(String::from) else { return true };
            // This phone's own upload coming back (or one it already has): nothing new. Merging it with
            // what the chat has become since made a copy of a chat that synced while it was being
            // answered, and brought back chats deleted right after they went up.
            if with_state(|s| s.convs.get(&cid).is_some_and(|r| r.rev == rev)) {
                return true;
            }
            if app.conv.peek().id == cid && app.live.peek().is_some() {
                return false;
            }
            let rec = with_state(|s| s.convs.get(&cid).cloned()).unwrap_or_default();
            let here = local(&app, &cid);
            let changed_here = rec.dirty && here.is_some() && !rec.deleted;
            if env["deleted"] == true {
                if changed_here {
                    with_state(|s| if let Some(r) = s.convs.get_mut(&cid) { r.rev = rev });
                    return true;
                }
                if !WIPING.load(Ordering::SeqCst) {
                    remove(app, &cid);
                }
                with_state(|s| s.convs.insert(cid, Rec { rev, deleted: true, ..Default::default() }));
                return true;
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
            with_state(|s| s.convs.insert(cid.clone(), Rec { rev, dirty, deleted: false, ver: rec.ver + 1, base: Some(fields_of(&remote)), atts: refs(&env["conv"]), too_big: false }));
            write(app, &keep);
        }
        _ => {}
    }
    true
}

async fn pull(app: App, k: &Keys) -> bool {
    for _ in 0..100 {
        // An answer being written is saved when it ends; pull after that.
        if app.live.peek().is_some() {
            return true;
        }
        let after = with_state(|s| s.after);
        let Got::Ok(v) = call(k, "GET", &format!("/records?after={after}"), None, None).await else { return false };
        if !same_store(v["epoch"].as_str()) {
            return false;
        }
        if v["rev"].as_i64().unwrap_or(0) < after {
            // Same store, older revisions (restored from a backup): send everything again. Their
            // revisions are forgotten: the same number can be another device's write now.
            with_state(|s| {
                s.after = 0;
                for r in s.convs.values_mut() {
                    r.rev = 0;
                    if !r.deleted { r.dirty = true; r.ver += 1; }
                }
            });
            continue;
        }
        for r in v["records"].as_array().into_iter().flatten() {
            let (Some(id), Some(rev), Some(data)) = (r["id"].as_str(), r["rev"].as_i64(), r["data"].as_str()) else { continue };
            if !apply(app, k, id, rev, data).await {
                return true;
            }
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
        with_state(|s| if let Some(r) = s.convs.get_mut(id) { r.dirty = false; r.too_big = true });
        return Pushed::Done;
    }
    match call(k, "PUT", &format!("/records/{rid}"), Some(rec.rev), Some(blob)).await {
        Got::Ok(v) => {
            if !made_here(k, &v).await {
                return Pushed::Failed;
            }
            let rev = v["rev"].as_i64().unwrap_or(0);
            let old = rec.atts.clone();
            if env["deleted"] != true {
                FULL.store(false, Ordering::Relaxed); // room again
            }
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
        Got::Conflict(_, _, epoch) if !same_store(epoch.as_deref()) => Pushed::Failed,
        Got::Conflict(rev, Some(data), _) => if apply(app, k, &rid, rev, &data).await { Pushed::Again } else { Pushed::Failed },
        // The record is gone from the store: make it again.
        Got::Conflict(_, None, _) => { with_state(|s| if let Some(r) = s.convs.get_mut(id) { r.rev = 0 }); Pushed::Again }
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
        Got::Ok(v) => {
            if made_here(k, &v).await {
                let rev = v["rev"].as_i64().unwrap_or(0);
                with_state(|s| s.prefs = Some(Prefs { rev, hash, last: out }));
            }
        }
        Got::Conflict(_, _, epoch) if !same_store(epoch.as_deref()) => {}
        // Ours wins: try again next round at the revision there now.
        Got::Conflict(rev, _, _) => with_state(|s| match s.prefs.as_mut() {
            Some(p) => { p.rev = rev; p.hash = String::new() }
            None => s.prefs = Some(Prefs { rev, hash: String::new(), last: json!({}) }),
        }),
        Got::Failed => {}
    }
}

// ---- running ------------------------------------------------------------------------------------

/// Forgets the synced copy, but not which account this phone's chats were made under.
fn clear() {
    let s: State = store::load(STATE);
    for h in s.atts.keys() {
        store::remove(&att_file(h));
    }
    // Each chat's title, folder etc. as last synced stay: on joining again, a rename made
    // elsewhere meanwhile isn't undone by this phone's older title (merge() sees no change here).
    // (Only for chats still here: a wiped or deleted chat leaves nothing of itself behind.)
    let here: BTreeSet<String> = store::load::<Vec<Meta>>("chats.json").into_iter().map(|m| m.id).collect();
    let convs = s.convs.into_iter()
        .filter(|(id, r)| !r.deleted && here.contains(id))
        .map(|(id, r)| (id, Rec { base: r.base, ..Default::default() }))
        .collect();
    store::save(STATE, &State { owner: s.owner, foreign: s.foreign, convs, ..Default::default() });
}

/// The synced copy is gone (turned off on another device): stop here too, keep the chats.
fn turned_off_elsewhere() {
    store::save(OFF, &true);
    store::remove(JOINED);
    clear();
}

// ---- joining: asking before this phone's chats and the synced ones are combined --------------

/// The question on screen: (this phone's chats, the account's from other devices).
pub static JOIN_ASK: GlobalSignal<Option<(usize, usize)>> = Signal::global(|| None);

/// Joined: agreed to combine, or had nothing to combine. Phones that synced before the question
/// existed count as joined.
fn is_joined(k: &Keys) -> bool {
    store::load::<Option<String>>(JOINED).as_deref() == Some(k.owner.as_str()) || {
        let s: State = store::load(STATE);
        s.owner == k.owner && s.epoch.is_some()
    }
}
fn agree(k: &Keys) { store::save(JOINED, &Some(k.owner.clone())) }
/// Whether this phone joined the synced copy (switching off then deletes it).
pub fn joined() -> bool { on() && keys().is_some_and(|k| is_joined(&k)) }

/// The chats in the synced copy, read without joining it: nothing is kept, and the epoch isn't
/// recorded (`same_store`), so the next round still knows this phone hasn't joined.
async fn remote_chats(k: &Keys) -> Option<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    let mut after = 0;
    for _ in 0..100 {
        let Got::Ok(v) = call(k, "GET", &format!("/records?after={after}"), None, None).await else { return None };
        if v["epoch"].is_null() {
            return Some(ids); // no synced copy
        }
        for r in v["records"].as_array().into_iter().flatten() {
            let (Some(id), Some(rev), Some(data)) = (r["id"].as_str(), r["rev"].as_i64(), r["data"].as_str()) else { continue };
            after = rev;
            let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(data) else { continue };
            let Some(env) = unseal(k, id, &blob) else { continue };
            if env["k"] == "conv" && let Some(cid) = env["id"].as_str() {
                if env["deleted"] == true { ids.remove(cid); } else { ids.insert(cid.to_string()); }
            }
        }
        if v["more"] != true {
            break;
        }
    }
    Some(ids)
}

/// true: go ahead and sync. false: wait (the question is on screen, or the server can't be
/// reached). The question is about chats this phone had before it had this account's key: they
/// carry no record yet. Chats made since, signed in to this account, are its own ([`changed`]).
async fn may_join(app: App, k: &Keys) -> bool {
    if is_joined(k) {
        return true;
    }
    if JOIN_ASK.peek().is_some() {
        return false;
    }
    let local: Vec<String> = app.convs.peek().iter().map(|m| m.id.clone()).collect();
    let here: Vec<String> = with_state(|s| local.iter().filter(|id| !s.convs.contains_key(*id) && !s.foreign.contains(*id)).cloned().collect());
    if !here.is_empty() {
        let Some(there) = remote_chats(k).await else { return false };
        let new_here = here.iter().filter(|id| !there.contains(*id)).count();
        let new_there = there.iter().filter(|id| !local.contains(id)).count();
        if new_here > 0 && new_there > 0 {
            if on() {
                *JOIN_ASK.write() = Some((new_here, new_there));
            }
            return false;
        }
    }
    if !on() {
        return false;
    }
    agree(k);
    true
}

/// The answer to "Combine chats?".
pub fn answer_join(app: App, combine: bool) {
    *JOIN_ASK.write() = None;
    if combine {
        if let Some(k) = keys() {
            agree(&k);
        }
        bg_run(async move { tick(app).await });
    } else {
        store::save(OFF, &true); // here only: the synced copy stays
    }
}

/// "Combine chats?", over whatever screen is showing.
#[component]
pub fn JoinAsk() -> Element {
    let app = use_context::<App>();
    let Some((here, there)) = *JOIN_ASK.read() else { return rsx! {} };
    let chats = |n: usize| if n == 1 { "1 chat".to_string() } else { format!("{n} chats") };
    let that = if here == 1 { "that isn't" } else { "that aren't" };
    rsx! {
        div { class: "veil" }
        div { class: "sheet", id: "sync-join",
            h2 { "Combine chats?" }
            p { class: "muted", id: "sync-join-text",
                "This phone has {chats(here)} {that} synced, and your account has {chats(there)} from your other devices. Combine puts all {here + there} on every device signed in with your account code. Nothing is deleted."
            }
            div { class: "row end",
                button { class: "bare", id: "sync-join-no", onclick: move |_| answer_join(app, false), "Not on this phone" }
                button { class: "go", id: "sync-join-combine", onclick: move |_| answer_join(app, true), "Combine" }
            }
        }
    }
}

async fn run(app: App) {
    if !on() || WIPING.load(Ordering::SeqCst) || STOPPED.load(Ordering::Relaxed) || store::now() < BACKOFF_UNTIL.load(Ordering::Relaxed) || !api::has_session() {
        return;
    }
    let Some(k) = keys() else { return };
    let ids: Vec<String> = app.convs.peek().iter().map(|m| m.id.clone()).collect();
    with_state(|s| if s.owner != k.owner {
        // Signed in to another account while chats from the last one are still here (its
        // session ended without signing out): they stay on the phone and never go up.
        let foreign = if s.owner.is_empty() { s.foreign.clone() } else { ids.iter().cloned().collect() };
        *s = State { owner: k.owner.clone(), foreign, ..Default::default() };
    });
    if !may_join(app, &k).await {
        return;
    }
    // Chats with nothing on record yet go up, as do ones known from before sync was last off.
    with_state(|s| for id in ids {
        if s.foreign.contains(&id) {
            continue;
        }
        let r = s.convs.entry(id).or_insert_with(|| Rec { dirty: true, ver: 1, ..Default::default() });
        if r.rev == 0 && !r.dirty && !r.deleted && !r.too_big {
            r.dirty = true;
            r.ver += 1;
        }
    });
    if !pull(app, &k).await || !on() {
        return;
    }
    let dirty: Vec<String> = with_state(|s| s.convs.iter().filter(|(_, r)| r.dirty).map(|(id, _)| id.clone()).collect());
    let mut probed = false;
    for id in dirty {
        // Full: deletes (which make room), and one new chat a round to see if there's room again
        // (other devices may have deleted chats).
        if FULL.load(Ordering::Relaxed) && !with_state(|s| s.convs.get(&id).is_some_and(|r| r.deleted)) {
            if probed {
                continue;
            }
            probed = true;
        }
        for _ in 0..3 {
            if !matches!(push_conv(app, &k, &id).await, Pushed::Again) { break }
        }
        if STOPPED.load(Ordering::Relaxed) || store::now() < BACKOFF_UNTIL.load(Ordering::Relaxed) || !on() {
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
/// The switch. On is agreeing to combine. Off deletes the synced copy (the other devices then
/// stop too) if this phone joined it, or always with `everywhere` (account deletion); a phone
/// that never joined only stops here.
pub async fn set_on(app: App, v: bool, everywhere: bool) -> bool {
    *JOIN_ASK.write() = None;
    if v {
        store::remove(OFF);
        if let Some(k) = keys() {
            agree(&k);
        }
        STOPPED.store(false, Ordering::Relaxed);
        BACKOFF_UNTIL.store(0, Ordering::Relaxed);
        tick(app).await;
        return true;
    }
    let was_joined = joined();
    store::save(OFF, &true);
    while BUSY.load(Ordering::SeqCst) {
        crate::sleep(50).await;
    }
    let ok = if was_joined || everywhere { delete_remote().await } else { true };
    store::remove(JOINED);
    clear();
    ok
}

/// Wipe starts (chat.rs, before it deletes the chats): nothing from the server is written here
/// until `wipe` has deleted the synced copy.
pub fn begin_wipe() { WIPING.store(true, Ordering::SeqCst) }

/// Wipe: the synced copy goes too, or the chats would come straight back.
pub async fn wipe() {
    while BUSY.load(Ordering::SeqCst) {
        crate::sleep(50).await;
    }
    *JOIN_ASK.write() = None;
    if joined() {
        delete_remote().await;
    }
    clear();
    WIPING.store(false, Ordering::SeqCst);
}

/// The account was deleted (its synced copy with it): the next account here starts with sync on.
pub fn reset_switch() { store::remove(OFF) }

/// Signing out: this phone's chats go (chat.rs), the synced copy stays for the next sign-in.
/// Signing in: the key a switch here moved sync to goes (the sign-in's own seed is used).
pub fn forget_moved_key() { store::remove(SEED) }
pub fn forget() {
    clear();
    store::remove(SEED);
    store::remove(JOINED);
    *JOIN_ASK.write() = None;
}

/// A new account number (Account): the chats move to the new number's store.
/// The old number's copy is deleted even if this phone never joined it: devices still on the
/// old number would otherwise keep syncing to it.
pub async fn move_to(app: App, new_seed: [u8; 32]) {
    let was_on = on();
    let was_joined = joined();
    *JOIN_ASK.write() = None;
    while BUSY.load(Ordering::SeqCst) {
        crate::sleep(50).await;
    }
    if was_on && let Some(old) = keys() {
        let _ = call(&old, "DELETE", "", None, None).await;
    }
    clear();
    // The chats are this account's under its new number, not another account's.
    let fresh = keys_of(new_seed);
    with_state(|s| s.owner = fresh.owner.clone());
    if was_joined { agree(&fresh) } else { store::remove(JOINED) }
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
    // "Not on this phone" ([`JoinAsk`]) turns it off.
    // and switching the account to words turns it on.
    use_effect(move || { let _ = REFRESH.read(); if JOIN_ASK.read().is_none() && !*busy.peek() { enabled.set(on()) } });
    let _ = REFRESH.read();
    rsx! {
        div { class: "section",
            h2 { "Sync" }
            if !available() {
                p { class: "muted", id: "sync-needs-words", if crate::private::on() && !crate::private::has_code() {
                    "Off until you convert to an account code above."
                } else {
                    "Sign in with your account code on this phone to sync chats."
                } }
            } else {
                label { class: "tor-always", id: "sync-switch",
                    input { r#type: "checkbox", id: "sync-on", checked: enabled(), disabled: busy(),
                        onchange: move |e| {
                            if e.checked() {
                                enabled.set(true);
                                busy.set(true);
                                bg_run(async move { set_on(app, true, false).await; busy.set(false) });
                            } else if joined() {
                                enabled.set(true);
                                asking.set(true);
                            } else {
                                // Never joined: nothing to delete, it just stops here.
                                enabled.set(false);
                                busy.set(true);
                                bg_run(async move { set_on(app, false, false).await; busy.set(false) });
                            }
                        } }
                    "Sync encrypted chats between devices"
                }
                p { class: "muted", "We never store any unencrypted data." }
                if full() { p { class: "muted", "Sync storage for this account is full: new chats stay on this phone." } }
                if asking() {
                    p { class: "muted", "Stop syncing? The synced copy is deleted and your other devices stop syncing too. Chats stay on each device." }
                    div { class: "row", style: "border: 0",
                        button { class: "danger", id: "sync-off-confirm", onclick: move |_| {
                            asking.set(false);
                            enabled.set(false);
                            busy.set(true);
                            bg_run(async move { set_on(app, false, false).await; busy.set(false) });
                        }, "Stop syncing" }
                        button { class: "bare", onclick: move |_| asking.set(false), "Cancel" }
                    }
                }
            }
        }
    }
}

fn bg_run(f: impl std::future::Future<Output = ()> + 'static) { dioxus::core::spawn_forever(f); }
