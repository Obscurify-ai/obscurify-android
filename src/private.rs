//! Private sessions (Unlinkable inference), on by default once signed in on this phone. The chat's requests
//! (`/web/chat/completions`, tools, held replies) go out with a session secret paid from a wallet of
//! blind vouchers kept here, and with no cookie, so the server can't tell which account a chat
//! belongs to, or which chats go together. The work is done by the open-source crate
//! `unlinkable-inference` (`manager`): the phone keeps only a seed derived from the account number and
//! the manager's state, and a new or wiped phone gets the wallet back at the next sign-in.
//!
//! Encrypted mode, the account page and buying vouchers still use the signed-in session.
//!
//! The manager keeps the wallet stocked ahead of time: it asks the account at random times, when
//! the app opens, and when a payment or plan change shows up here (`money_added`), so a chat
//! rarely needs it. A chat the wallet can't pay is funded from the account at once (the library's
//! default), and goes on without waiting. What a refusal says comes from what the phone holds and
//! what the account said when it last refused a top-up (a plan's limit used up), never from asking
//! the account again because of the chat.

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

/// While a chat waits for the wallet to refill (only when the manager is set to wait instead of
/// funding at once): about when it goes on (Unix seconds). The chat screen shows it above the
/// composer.
pub static REFILLING: dioxus::prelude::GlobalSignal<Option<u64>> = dioxus::prelude::Signal::global(|| None);

/// Stop was pressed while a chat waited for the wallet: the wait ends and nothing is sent.
static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const STOPPED_TEXT: &str = "Stopped before it was sent.";

/// Stop pressed: a chat waiting for the wallet gives up, and isn't sent later.
pub fn cancel_wait() { CANCEL.store(true, std::sync::atomic::Ordering::SeqCst) }

/// The last wait ended because Stop was pressed (asked once).
pub fn stopped() -> bool { STOPPED.swap(false, std::sync::atomic::Ordering::SeqCst) }

pub const STOPPED_ERROR: &str = STOPPED_TEXT;

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

/// The session secret for held reply `reply`: `Ok(None)` when it isn't a private one.
pub async fn reply_secret(reply: &str) -> Result<Option<String>, String> {
    if !on() || !api::has_session() {
        return Ok(None);
    }
    let mut g = ready().await.ok_or(NOT_STARTED)?;
    Ok(g.as_mut().ok_or(NOT_STARTED)?.reply(reply))
}

pub const NOT_STARTED: &str = "A private session could not be started, so nothing was sent. Check your connection and try again.";
const USED_UP: &str = "Your balance is used up, so nothing was sent. Add funds, or pick a free model.";
const ALL_USED_UP: &str = "Your balance and this month's free usage are used up, so nothing was sent. Add funds to keep chatting.";

/// The session secret to send for the current chat, opening a session. `Ok(None)`: private
/// sessions aren't on here (signed in without the number); use the signed-in session. `Err`:
/// they are on and none could be had, so the request is not sent at all: never through the
/// account instead. The manager funds the session from the account at once when the wallet
/// can't.
pub async fn secret() -> Result<Option<String>, String> {
    if !on() || !api::has_session() {
        return Ok(None);
    }
    loop {
        if let Some(s) = open().await? {
            return Ok(Some(s));
        }
        // No money and no free-model pass, here or in the account.
        if !hold().await {
            if stopped() {
                return Err(STOPPED_TEXT.into());
            }
            return Err(limit_note().await.unwrap_or_else(|| ALL_USED_UP.into()));
        }
    }
}

/// What the account said when it last refused to fill the wallet, when that was a plan's weekly
/// or 5-hour limit used up (and when it starts over).
async fn limit_note() -> Option<String> {
    let g = state().lock().await;
    g.as_ref()?.account_note().filter(|n| n.contains("usage is used up")).map(String::from)
}

/// When the manager says a chat waits for the wallet (set to wait instead of funding at once):
/// show it, keep its clock running until the chat may go on. False: nothing to wait for (the
/// account had nothing at the last contact).
async fn hold() -> bool {
    let at = |m: &Manager| m.refilling();
    let Some(mut when) = ready().await.and_then(|g| (*g).as_ref().and_then(at)) else { return false };
    // Cleared however the wait ends, Stop (the send dropped) included.
    struct Shown;
    impl Drop for Shown {
        fn drop(&mut self) {
            *REFILLING.write() = None;
        }
    }
    let _shown = Shown;
    CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
    loop {
        if CANCEL.swap(false, std::sync::atomic::Ordering::SeqCst) {
            STOPPED.store(true, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        *REFILLING.write() = Some(when);
        crate::sleep(2000).await;
        let Some(mut g) = ready().await else { break };
        let Some(m) = g.as_mut() else { break };
        m.tick().await;
        save(m);
        match m.refilling() {
            Some(w) => when = w,
            None => break,
        }
    }
    true
}

/// Money was added to the account, or its plan changed, here (a moment of the account's own):
/// the wallet asks it now.
pub async fn money_added() {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        m.money_added().await;
        save(m);
    }
}

async fn open() -> Result<Option<String>, String> {
    let mut g = ready().await.ok_or(NOT_STARTED)?;
    let m = g.as_mut().ok_or(NOT_STARTED)?;
    let chat = CHAT.lock().unwrap().clone();
    let s = m.session(&chat).await.ok();
    save(m);
    Ok(s)
}

/// What to say when a request was refused for want of money, with nothing more to add: the
/// account had nothing when it was just asked, so what is left is what this phone holds.
pub async fn refused() -> String {
    let held = spending().await["prepaid_millicents"].as_i64().unwrap_or(0) as f64 / 100_000.0;
    match (held * 100.0).floor() as i64 {
        i64::MIN..1 => limit_note().await.unwrap_or_else(|| USED_UP.into()),
        c @ 1..100 => format!("This needs more than the {c}¢ you have left, so nothing was sent. Pick a cheaper model, or add funds."),
        c => format!("This needs more than the ${}.{:02} you have left, so nothing was sent. Pick a cheaper model, or add funds.", c / 100, c % 100),
    }
}

/// The session ran out (402): add more to it, more each time. False: there is nothing more to
/// add, so the request goes once more as the rest of the session's money (`REST_HEADER`).
pub async fn ran_out(secret: &str, attempt: u32) -> bool {
    loop {
        if add(secret, attempt).await {
            return true;
        }
        if !hold().await {
            return false;
        }
    }
}

async fn add(secret: &str, attempt: u32) -> bool {
    if let Some(mut g) = ready().await
        && let Some(m) = g.as_mut()
    {
        let added = m.ran_out(secret, attempt).await.is_ok();
        save(m);
        return added;
    }
    false
}

/// The server asked this chat's session for a pass (a 403 with `x-pass-required: <label>`):
/// proof the account's plan gives what the request needs (a frontier or uncensored model,
/// free-model tokens), which says nothing of which account. True when one was shown: ask
/// again. False (the plan doesn't give it): show the error.
pub async fn needs_pass(secret: &str, label: &str) -> bool {
    loop {
        let Some(mut g) = ready().await else { return false };
        let Some(m) = g.as_mut() else { return false };
        let answer = m.needs_pass(secret, label).await;
        save(m);
        drop(g);
        match answer {
            Ok(retry) => return retry,
            // The account gets that label and the stock ran out, and the manager waits for it.
            Err(_) if hold().await => continue,
            Err(_) => return false,
        }
    }
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

/// A reply in the current chat said it cost this much (dollars): what its session still holds,
/// as far as this phone knows.
pub async fn note_cost(usd: f64) {
    let chat = CHAT.lock().unwrap().clone();
    if let Some(m) = state().lock().await.as_mut() {
        m.note_cost(&chat, (usd * 100_000.0).round() as i64);
        save(m);
    }
}

/// What this phone's wallet holds and has spent, from what the phone keeps (no request names a
/// session): `{spent: [{at, millicents}], prepaid_millicents}`. `Null` when private sessions are off.
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
