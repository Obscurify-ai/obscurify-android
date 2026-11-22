//! Private sessions (Unlinkable inference), on by default once signed in on this phone. The chat's requests
//! (`/web/chat/completions`, tools, held replies) go out with a session secret paid from a wallet of
//! blind vouchers kept here, and with no cookie, so the server can't tell which account a chat
//! belongs to, or which chats go together. The work is done by the open-source crate
//! `unlinkable-inference` (`manager`): the phone keeps only a seed derived from the account number and
//! the manager's state, and a new or wiped phone gets the wallet back at the next sign-in.
//!
//! Encrypted mode, the account page and buying vouchers still use the signed-in session.

use crate::{api, store};
use serde_json::{json, Value};
use std::sync::{Mutex as StdMutex, OnceLock};
use unlinkable_inference::manager::Manager;
use unlinkable_inference::seed::{account_keys, normalize_credential, AccountKeys, Credential, Seed};

const SEED_FILE: &str = "unlinkable-seed.json";
const STATE_FILE: &str = "unlinkable-state.json";
/// How this phone signed in: "code" (an account code) or "number" (16 digits). None: before
/// codes existed, so a number. Chat sync is only for codes (sync.rs).
const KIND_FILE: &str = "account-kind.json";

fn state() -> &'static tokio::sync::Mutex<Option<Manager>> {
    static S: OnceLock<tokio::sync::Mutex<Option<Manager>>> = OnceLock::new();
    S.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// The chat whose requests are being made: each chat gets its own session.
static CHAT: StdMutex<String> = StdMutex::new(String::new());

pub fn set_chat(id: &str) { *CHAT.lock().unwrap() = id.to_string() }

/// On: there is a seed from signing in on this phone.
pub fn on() -> bool { store::load::<Option<String>>(SEED_FILE).is_some() }

/// The account key and seed for an account code or a 16-digit number, kept nowhere. Argon2id is
/// slow on purpose, so it runs off the UI thread.
pub async fn keys_of(code_or_number: &str) -> Result<AccountKeys, String> {
    let n = code_or_number.to_string();
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || { let _ = tx.send(account_keys(&n)); });
    rx.await.map_err(|_| "Could not derive the key".to_string())?.map_err(|e| e.to_string())
}

/// The account key for the code or number (sent to sign in, instead of it), keeping its seed for
/// the wallet and which kind it was.
pub async fn keys(code_or_number: &str) -> Result<String, String> {
    let k = keys_of(code_or_number).await?;
    store::save(SEED_FILE, &Some(k.seed.0.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    set_kind(kind_of(code_or_number).unwrap_or("number"));
    // A sign-in replaces whatever key chat sync moved to after a switch here (sync.rs), which may
    // belong to another account if that session ended without signing out.
    crate::sync::forget_moved_key();
    Ok(k.login_key)
}

/// "code" (an account code whose typo check holds), "number" (16 digits), or None for what was typed.
pub fn kind_of(input: &str) -> Option<&'static str> {
    match normalize_credential(input)? {
        Credential::Number(_) => Some("number"),
        Credential::Code(_) => Some("code"),
    }
}

/// This phone signed in with an account code (or converted to one).
pub fn has_code() -> bool { store::load::<Option<String>>(KIND_FILE).as_deref() == Some("code") }
pub fn set_kind(kind: &str) { store::save(KIND_FILE, &Some(kind.to_string())) }

/// A new account code (96 random bits), made on the phone (the server never learns it).
pub fn new_code() -> String { unlinkable_inference::seed::new_account_code() }

/// Where requests go changed (Tor on or off, a mirror): the manager connects again on its next
/// use, to the new address and with a client made for it (its state is saved after every call).
pub fn reset() {
    if let Ok(mut s) = state().try_lock() {
        *s = None;
    } else {
        dioxus::core::spawn_forever(async { *state().lock().await = None; });
    }
}

/// Signing out: the seed and wallet go (they come back from the number at the next sign-in).
pub fn forget() {
    store::remove(SEED_FILE);
    store::remove(STATE_FILE);
    store::remove(KIND_FILE);
    if let Ok(mut s) = state().try_lock() {
        *s = None;
    }
}

/// Saved after every call (atomically: it holds money). A crash between saves loses nothing either:
/// `Manager::start` brings back whatever it missed.
fn save(m: &Manager) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&m.state()) {
        store::save(STATE_FILE, &v);
    }
}

pub(crate) fn seed() -> Option<[u8; 32]> {
    let hex: String = store::load::<Option<String>>(SEED_FILE)?;
    let seed: Vec<u8> = (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect();
    seed.try_into().ok()
}

async fn start() -> Result<Manager, String> {
    let seed = seed().ok_or("no seed")?;
    let saved = store::load::<Option<serde_json::Value>>(STATE_FILE).map(|v| v.to_string());
    let mut m = Manager::connect(&api::base(), Seed(seed), saved.as_deref()).await.map_err(|e| format!("{e:#}"))?;
    m.api_key = api::post("/tokens", json!({})).await.ok().and_then(|v| v["token"].as_str().map(str::to_string));
    if let Err(e) = m.start().await {
        eprintln!("private: restore: {e:#}");
    }
    save(&m);
    Ok(m)
}

async fn ready() -> Option<tokio::sync::MutexGuard<'static, Option<Manager>>> {
    if !on() || !api::has_session() {
        return None;
    }
    let mut g = state().lock().await;
    if g.is_none() {
        match start().await {
            Ok(m) => *g = Some(m),
            Err(e) => {
                eprintln!("private: off for now: {e}");
                return None;
            }
        }
    }
    Some(g)
}

/// The session secret to send for the current chat (opening a session), or for held reply
/// `reply`. `None`: private sessions are off or unavailable; use the signed-in session.
pub async fn secret(reply: Option<&str>) -> Option<String> {
    let mut g = ready().await?;
    let m = g.as_mut()?;
    if let Some(id) = reply {
        return m.reply(id);
    }
    let chat = CHAT.lock().unwrap().clone();
    let s = m.session(&chat).await.ok();
    save(m);
    s
}

/// The account's plan has weekly or 5-hour limits (`/account_tier`, as last read).
static LIMITED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn note_plan(tier: &Value) {
    LIMITED.store(tier["usage_week"].is_object() || tier["usage_5h"].is_object(), std::sync::atomic::Ordering::Relaxed);
}

/// No session could be opened, or it ran out and the wallet couldn't be filled. A plan with weekly
/// or 5-hour limits runs out and fills up again every day, so on one the account is asked again now
/// (the wallet waits ten minutes after a refusal), and failing that the chat is refused here with
/// which limit it is: it never goes out tied to the account. `Ok(Some)`: a session to use.
/// `Ok(None)`: not such a plan.
pub async fn ask_again() -> Result<Option<String>, String> {
    if !on() { return Ok(None) }
    let tier = match api::get("/account_tier").await {
        Ok(t) => { note_plan(&t); t }
        Err(e) if LIMITED.load(std::sync::atomic::Ordering::Relaxed) => return Err(api::status_of(&e).1.to_string()),
        Err(_) => return Ok(None),
    };
    if !LIMITED.load(std::sync::atomic::Ordering::Relaxed) { return Ok(None) }
    // Less than a session's 10¢ to be had: nothing to ask for.
    let balance = api::get("/get_user_balance").await.ok().and_then(|v| v["balance"].as_f64()).unwrap_or(0.0);
    if tier["credit_available"].as_f64().unwrap_or(0.0) + balance.max(0.0) < 0.1 { return Err(limit_text(&tier)) }
    {
        let mut g = state().lock().await;
        if let Some(mut v) = store::load::<Option<Value>>(STATE_FILE)
            && v["refused_at"].as_u64().unwrap_or(0) > 0
        {
            v["refused_at"] = json!(0);
            store::save(STATE_FILE, &v);
            *g = None;
        }
    }
    if let Some(s) = secret(None).await { return Ok(Some(s)) }
    Err(limit_text(&tier))
}

fn limit_text(tier: &Value) -> String {
    for (name, key) in [("weekly", "usage_week"), ("5-hour", "usage_5h")] {
        let w = &tier[key];
        if w.is_object() && w["limit"].as_f64().unwrap_or(0.0) - w["used"].as_f64().unwrap_or(0.0) < 0.1 {
            let at = w["frees_at"].as_str().map(|t| format!(" It starts over {}.", crate::local_time(t))).unwrap_or_default();
            return format!("Your plan's {name} usage is used up.{at} Add balance to keep going now.");
        }
    }
    "Your plan's usage for this month is used up. Add balance to keep going now.".into()
}

/// The session ran out (402): add more to it, more each time.
pub async fn ran_out(secret: &str, attempt: u32) {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        let _ = m.ran_out(secret, attempt).await;
        save(m);
    }
}

/// The server asked this chat's session for a pass (a 403 with `x-pass-required: <label>`):
/// proof the account's plan gives what the request needs (a frontier or uncensored model,
/// free-model tokens), which says nothing of which account. True when one was shown: ask
/// again. False (the plan doesn't give it): show the error.
pub async fn needs_pass(secret: &str, label: &str) -> bool {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        let retry = m.needs_pass(secret, label).await.unwrap_or(false);
        save(m);
        return retry;
    }
    false
}

/// The server no longer takes this session (401): the chat gets a new one next time.
pub async fn expired(secret: &str) {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        m.expired(secret);
        save(m);
    }
}

pub async fn note_reply(id: &str, secret: &str) {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        m.note_reply(id, secret);
        save(m);
    }
}

/// Every 30 s while the app runs: close quiet chats' sessions for their change, top up.
pub async fn tick() {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        m.tick().await;
        save(m);
    }
}

/// What this device's private chats cost, for the account's history, as JSON:
/// `{spent: [{at, millicents}], prepaid_millicents}`. `Null` when private sessions are off.
/// Read-only: uses the running manager if there is one, else reads the saved state without
/// starting anything (no restore, no top-up from the account).
pub async fn spending() -> Value {
    if !on() || !api::has_session() {
        return Value::Null;
    }
    let g = state().lock().await;
    let s = match g.as_ref() {
        Some(m) => m.spending().await,
        None => {
            let Some(saved) = store::load::<Option<Value>>(STATE_FILE) else { return Value::Null };
            let Some(seed) = seed() else { return Value::Null };
            match Manager::connect(&api::base(), Seed(seed), Some(&saved.to_string())).await {
                Ok(m) => m.spending().await,
                Err(_) => return Value::Null,
            }
        }
    };
    serde_json::to_value(s).unwrap_or(Value::Null)
}
