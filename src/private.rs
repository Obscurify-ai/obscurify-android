//! Private sessions (unlinkable), on by default once signed in on this phone. The chat's requests
//! (`/web/chat/completions`, tools, held replies) go out with a session secret paid from a wallet of
//! blind vouchers kept here, and with no cookie, so the server can't tell which account a chat
//! belongs to, or which chats go together. Everything comes from the account number (the
//! open-source unlinkable crate): the phone keeps only a seed derived from it, and a new or wiped
//! phone gets the wallet back at the next sign-in. Nothing is lost.
//!
//! Encrypted mode, the account page and buying vouchers still use the signed-in session.

use crate::{api, store};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use unlinkable_core::net::{Client, Session, Transport};
use unlinkable_core::{Family, Wallet};

const SEED_FILE: &str = "unlinkable-seed.json";
const WALLET_FILE: &str = "unlinkable-wallet.json";
/// Which session each chat and each held reply uses, so a restarted app carries on with them.
const CHATS_FILE: &str = "unlinkable-chats.json";

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Kept {
    sessions: HashMap<String, String>,
    replies: HashMap<String, String>,
}
/// What a session starts with; the wallet is topped up by REFILL when below KEEP.
const FUNDING_CENTS: i64 = 50;
const KEEP_CENTS: i64 = 50;
const REFILL_CENTS: i64 = 100;
const KEEP_PASSES: usize = 10;
const IDLE: Duration = Duration::from_secs(5 * 60);

struct State {
    client: Client,
    wallet: Wallet,
    /// chat id → (session secret, last used)
    sessions: HashMap<String, (String, Instant)>,
    /// held reply id → the secret of the session that asked
    replies: HashMap<String, String>,
    /// The account's API key, for topping the wallet up (from the signed-in session)
    key: Option<String>,
}

fn state() -> &'static tokio::sync::Mutex<Option<State>> {
    static S: OnceLock<tokio::sync::Mutex<Option<State>>> = OnceLock::new();
    S.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// The chat whose requests are being made: each chat gets its own session.
static CHAT: StdMutex<String> = StdMutex::new(String::new());

pub fn set_chat(id: &str) { *CHAT.lock().unwrap() = id.to_string() }

/// On: there is a seed from signing in on this phone.
pub fn on() -> bool { store::load::<Option<String>>(SEED_FILE).is_some() }

/// The login key for `number` (sent to sign in, instead of the number), keeping its seed for the
/// wallet. Argon2id is slow on purpose, so it runs off the UI thread.
pub async fn keys(number: &str) -> Result<String, String> {
    let n = number.to_string();
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || { let _ = tx.send(unlinkable_core::seed::account_keys(&n)); });
    let k = rx.await.map_err(|_| "Could not derive the key".to_string())?.map_err(|e| e.to_string())?;
    store::save(SEED_FILE, &Some(k.seed.0.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    Ok(k.login_key)
}

/// A new 16-digit account number, made on the phone (the server never learns it).
pub fn new_number() -> String {
    let mut digits = String::new();
    while digits.len() < 16 {
        let b: u8 = rand::random();
        if b < 250 {
            digits.push(char::from(b'0' + b % 10));
        }
    }
    digits
}

/// Signing out: the seed and wallet go (they come back from the number at the next sign-in).
pub fn forget() {
    store::remove(SEED_FILE);
    store::remove(WALLET_FILE);
    store::remove(CHATS_FILE);
    if let Ok(mut s) = state().try_lock() {
        *s = None;
    }
}

async fn start() -> Result<State, String> {
    let hex: String = store::load::<Option<String>>(SEED_FILE).ok_or("no seed")?;
    let seed: Vec<u8> = (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect();
    let seed: [u8; 32] = seed.try_into().map_err(|_| "bad seed")?;
    let mut wallet = Wallet::open(store::dir().join(WALLET_FILE)).map_err(|e| format!("{e:#}"))?;
    let transport = Transport::new(api::BASE);
    let keyset = transport.fetch_keyset().await.map_err(|e| format!("{e:#}"))?;
    keyset.check().map_err(|e| format!("{e:#}"))?;
    let fp = keyset.fingerprint();
    match wallet.pinned() {
        Some(p) if p != fp => return Err(format!("the server's keys changed ({fp}, pinned {p})")),
        Some(_) => {}
        None => wallet.pin(Some(fp)),
    }
    let client = Client::new(transport, keyset).with_seed(unlinkable_core::seed::Seed(seed));
    let key = api::post("/tokens", json!({})).await.ok().and_then(|v| v["token"].as_str().map(str::to_string));
    // Sessions a chat or a held reply still uses carry on; any other this phone opened is closed
    // for its change.
    let kept: Kept = store::load(CHATS_FILE);
    let in_use: Vec<&String> = kept.sessions.values().chain(kept.replies.values()).collect();
    for open in wallet.sessions().to_vec() {
        if !in_use.contains(&&open.secret) {
            let _ = client.close(&mut wallet, &Session { secret: open.secret.clone(), family: open.family }).await;
        }
    }
    // Whatever a closed app or another phone left behind (an open session, change not yet
    // collected, the whole wallet after a wipe) comes back.
    if let Err(e) = client.restore(&mut wallet, key.as_deref()).await {
        eprintln!("private: restore: {e:#}");
    }
    let now = Instant::now();
    let sessions = kept.sessions.into_iter().map(|(chat, secret)| (chat, (secret, now))).collect();
    let mut st = State { client, wallet, sessions, replies: kept.replies, key };
    refill(&mut st).await;
    Ok(st)
}

/// Keep the wallet and passes topped up, and let the server forget what is fully spent: at quiet
/// moments, never right as a session opens.
async fn refill(st: &mut State) {
    if let Some(key) = st.key.clone() {
        if st.wallet.balance_cents(Family::Paid) < KEEP_CENTS
            && let Err(e) = st.client.withdraw(&mut st.wallet, &key, REFILL_CENTS).await
        {
            eprintln!("private: top-up: {e:#}");
        }
        if st.wallet.passes(&unlinkable_core::pass::current_period()).len() < KEEP_PASSES {
            let _ = st.client.get_passes(&mut st.wallet, &key, 30).await;
        }
        let _ = st.client.flush_done(&mut st.wallet, Some(&key)).await;
    }
    let _ = st.wallet.save();
}

fn keep(st: &State) {
    store::save(CHATS_FILE, &Kept {
        sessions: st.sessions.iter().map(|(k, (s, _))| (k.clone(), s.clone())).collect(),
        replies: st.replies.clone(),
    });
}

async fn ready() -> Option<tokio::sync::MutexGuard<'static, Option<State>>> {
    if !on() || !api::has_session() {
        return None;
    }
    let mut g = state().lock().await;
    if g.is_none() {
        match start().await {
            Ok(s) => *g = Some(s),
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
    let st = g.as_mut()?;
    if let Some(id) = reply {
        return st.replies.get(id).cloned();
    }
    let chat = CHAT.lock().unwrap().clone();
    if let Some((s, used)) = st.sessions.get_mut(&chat) {
        *used = Instant::now();
        return Some(s.clone());
    }
    let opened = match st.client.open(&mut st.wallet, Family::Paid, FUNDING_CENTS).await {
        Ok(s) => Some(s),
        Err(_) => {
            refill(st).await;
            st.client.open(&mut st.wallet, Family::Paid, FUNDING_CENTS).await.ok()
        }
    }?;
    let _ = st.wallet.save();
    st.sessions.insert(chat, (opened.secret.clone(), Instant::now()));
    keep(st);
    Some(opened.secret)
}

/// The session ran out (402): add more to it, more each time.
pub async fn ran_out(secret: &str, attempt: u32) {
    let Some(mut g) = ready().await else { return };
    let Some(st) = g.as_mut() else { return };
    let session = Session { secret: secret.to_string(), family: Family::Paid };
    if st.client.topup(&mut st.wallet, &session, FUNDING_CENTS << attempt).await.is_err() {
        refill(st).await;
        let _ = st.client.topup(&mut st.wallet, &session, FUNDING_CENTS << attempt).await;
    }
    let _ = st.wallet.save();
}

/// The server no longer takes this session (401): the chat gets a new one next time.
pub async fn expired(secret: &str) {
    if let Some(mut g) = ready().await
        && let Some(st) = g.as_mut()
    {
        st.sessions.retain(|_, (s, _)| s != secret);
        keep(st);
    }
}

pub async fn note_reply(id: &str, secret: &str) {
    if let Some(mut g) = ready().await
        && let Some(st) = g.as_mut()
    {
        st.replies.insert(id.to_string(), secret.to_string());
        keep(st);
    }
}

/// Every 30 s while the app runs: close sessions idle for 5 minutes (their change comes back to the
/// wallet) and top up.
pub async fn tick() {
    let Some(mut g) = ready().await else { return };
    let Some(st) = g.as_mut() else { return };
    let idle: Vec<String> = st.sessions.iter().filter(|(_, (_, used))| used.elapsed() > IDLE).map(|(k, _)| k.clone()).collect();
    for chat in idle {
        if let Some((secret, _)) = st.sessions.remove(&chat) {
            st.replies.retain(|_, s| *s != secret);
            let _ = st.client.close(&mut st.wallet, &Session { secret, family: Family::Paid }).await;
        }
    }
    keep(st);
    refill(st).await;
}
