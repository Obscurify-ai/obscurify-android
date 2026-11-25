//! The Obscurify API. Signed-in calls carry the `session` cookie from `POST /login` (the only way a
//! non-Pro account can chat). Like the browser, the app keeps that session, never the account
//! number: when the session ends, you sign in again. Voucher orders send no cookie at all, so the
//! server can't tie a payment to the account.

use crate::store;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const BASE: &str = match option_env!("OBSCURIFY_API") {
    Some(b) => b,
    None => "https://obscurify.ai",
};

/// Where requests go: Obscurify's onion address while Tor is on, else a mirror this run moved to
/// (Pro, after obscurify.ai couldn't be reached), else obscurify.ai.
pub fn base() -> String {
    if crate::tor::wanted() || crate::tor::always() {
        return crate::tor::ONION.to_string();
    }
    ACTIVE.lock().unwrap().clone().unwrap_or_else(|| BASE.to_string())
}

// ---- mirrors ------------------------------------------------------------------------------------

/// A paid Pro plan's mirrors: other addresses of the same server, for networks that block
/// obscurify.ai. They come only from the server (`GET /api/mirrors`) and are kept on the phone.
const MIRRORS_FILE: &str = "mirrors.json";
/// The response header of a 403 that asks a private session for a pass: the pass's label.
const PASS_REQUIRED: &str = "x-pass-required";

/// The mirror this run moved to after obscurify.ai couldn't be reached. The next run tries
/// obscurify.ai first again.
static ACTIVE: Mutex<Option<String>> = Mutex::new(None);

/// Asks the server for this account's mirrors (none unless the plan is Pro) and keeps them.
pub async fn refresh_mirrors() {
    if !has_session() {
        return;
    }
    let req = with_session(client().get(url("/api/mirrors"))).timeout(Duration::from_secs(20));
    let Ok(r) = req.send().await else { return };
    match r.status().as_u16() {
        200 => {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            let list: Vec<String> = v["mirrors"].as_array().into_iter().flatten()
                .filter_map(|m| m.as_str())
                .filter(|m| is_origin(m))
                .take(5)
                .map(|m| m.trim_end_matches('/').to_string())
                .collect();
            store::save(MIRRORS_FILE, &list)
        }
        // Not Pro, or a server without mirrors.
        403 | 404 => store::remove(MIRRORS_FILE),
        _ => {}
    }
}

/// `https://host[:port]`, nothing else (a test build against a local server also takes
/// `http://127.0.0.1:port`).
fn is_origin(m: &str) -> bool {
    let local = BASE.starts_with("http://127.0.0.1:") && m.starts_with("http://127.0.0.1:");
    let Some(host) = m.strip_prefix("https://").or(local.then(|| &m[7..])).map(|h| h.trim_end_matches('/')) else { return false };
    !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// Moves this run to a mirror that answers, after a request to `failed` couldn't connect (DNS,
/// connection or TLS). Never over Tor, and at most one try a minute: a phone that is simply offline
/// shouldn't wait on every mirror for every request.
async fn failover(failed: &str) -> bool {
    static BUSY: tokio::sync::Mutex<Option<Instant>> = tokio::sync::Mutex::const_new(None);
    if crate::tor::wanted() || crate::tor::always() || !has_session() {
        return false;
    }
    let mut last = BUSY.lock().await;
    if base() != failed {
        return true; // another request moved already
    }
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
        return false;
    }
    *last = Some(Instant::now());
    let list: Vec<String> = store::load(MIRRORS_FILE);
    for m in list.into_iter().filter(|m| m != failed) {
        let probe = with_session(direct().get(format!("{m}/api/mirrors"))).timeout(Duration::from_secs(10));
        if probe.send().await.is_ok_and(|r| r.status().is_success()) {
            *ACTIVE.lock().unwrap() = Some(m);
            crate::private::reset();
            return true;
        }
    }
    false
}

/// Sends a request; one that couldn't connect is sent again on a mirror, if the account has one
/// that answers. A server that answers with an error is never a reason to move.
trait Go {
    fn go(self) -> impl std::future::Future<Output = reqwest::Result<reqwest::Response>>;
}

impl Go for reqwest::RequestBuilder {
    async fn go(self) -> reqwest::Result<reqwest::Response> {
        let (c, request) = self.build_split();
        let request = request?;
        let from = request.url().origin().ascii_serialization();
        let again = request.try_clone();
        match c.execute(request).await {
            Err(e) if e.is_connect() => match again {
                Some(mut r) if failover(&from).await => {
                    rebase(&mut r);
                    client().execute(r).await
                }
                _ => Err(e),
            },
            other => other,
        }
    }
}

fn rebase(r: &mut reqwest::Request) {
    let b = base();
    let Ok(to) = reqwest::Url::parse(&b) else { return };
    let u = r.url_mut();
    let _ = u.set_scheme(to.scheme());
    let _ = u.set_host(to.host_str());
    let _ = u.set_port(to.port());
    if r.headers().contains_key("origin") && let Ok(v) = reqwest::header::HeaderValue::from_str(&b) {
        r.headers_mut().insert("origin", v);
    }
}

fn url(path: &str) -> String { format!("{}{path}", base()) }

/// The HTTP client: through Tor while Tor is on (never around it), else direct.
fn client() -> reqwest::Client {
    if let Some(c) = crate::tor::client() {
        return c;
    }
    // "Always connect through Tor" with Tor not running: nothing goes out.
    if crate::tor::always() {
        return dead();
    }
    direct()
}

/// A client whose every request fails: Always-Tor with Tor stopped.
fn dead() -> reqwest::Client {
    static D: OnceLock<reqwest::Client> = OnceLock::new();
    D.get_or_init(|| reqwest::Client::builder().proxy(reqwest::Proxy::all("socks5h://127.0.0.1:9").unwrap()).build().unwrap()).clone()
}

static DIRECT: Mutex<Option<reqwest::Client>> = Mutex::new(None);

/// Closes the direct client's idle connections (Tor just started): it is made again when needed.
#[cfg_attr(not(feature = "tor"), allow(dead_code))] // Only Tor drops it.
pub fn drop_direct() { *DIRECT.lock().unwrap() = None }

fn direct() -> reqwest::Client {
    DIRECT.lock().unwrap().get_or_insert_with(|| reqwest::Client::builder()
        // Never a proxy from the environment: that is Tor's, and this client is for when Tor is off.
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        // Phones change networks and VPNs drop: don't reuse a connection that may be dead by now,
        // and notice a dead one in the middle of a read.
        .pool_idle_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(180))
        .user_agent(if crate::IOS { "Obscurify-iOS" } else { "Obscurify-Android" })
        .build()
        .unwrap()).clone()
}

/// What a request that never got an answer says.
pub const OFFLINE: &str = "Can't reach Obscurify. Check your connection.";

/// What a request that got a proxy's 502/503/504 page says: the server is reachable but failing,
/// which is waited on for a while, not for as long as a lost connection (code::wait_again).
pub const UNAVAILABLE: &str = "Obscurify isn't answering right now. Try again in a minute.";

fn offline() -> String { format!("0\u{1}{OFFLINE}") }

/// The request may work if it's made again: no connection, or the server is briefly unavailable.
pub fn transient(e: &str) -> bool { e.contains('\u{1}') && matches!(status_of(e).0, 0 | 408 | 429 | 502 | 503 | 504) }

static SESSION: Mutex<Option<String>> = Mutex::new(None);
static CSRF: Mutex<Option<(String, Instant)>> = Mutex::new(None);
/// Set when the server says the session is over; the app goes back to sign-in.
pub static EXPIRED: AtomicBool = AtomicBool::new(false);

const SESSION_FILE: &str = "session.json";

/// Picks up the session saved by an earlier run. Returns whether there is one.
pub fn restore() -> bool {
    let s: Option<String> = store::load(SESSION_FILE);
    let some = s.is_some();
    *SESSION.lock().unwrap() = s;
    some
}

fn set_session(token: Option<String>) {
    match &token { Some(t) => store::save(SESSION_FILE, t), None => store::remove(SESSION_FILE) }
    *SESSION.lock().unwrap() = token;
    *CSRF.lock().unwrap() = None;
}

/// Signed in? Without a session the app chats as a guest, like the website.
pub fn has_session() -> bool { SESSION.lock().unwrap().is_some() }

/// Ends the session on this phone.
pub fn forget() {
    set_session(None);
    store::remove(MIRRORS_FILE);
    *ACTIVE.lock().unwrap() = None;
}

fn with_session(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match SESSION.lock().unwrap().as_deref() {
        Some(t) => req.header("Cookie", format!("session={t}")),
        None => req,
    }
}

/// The `session` cookie a sign-in or sign-up response sets.
fn session_cookie(r: &reqwest::Response) -> Option<String> {
    r.headers().get_all("set-cookie").iter().filter_map(|h| h.to_str().ok()).find_map(|c| {
        let v = c.split(';').next()?.trim().strip_prefix("session=")?;
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn expire() {
    forget();
    EXPIRED.store(true, Ordering::SeqCst);
}

fn error_text(status: u16, v: &Value, body: &str) -> String {
    let msg = v["error"]["message"].as_str().or(v["error"].as_str()).or(v["message"].as_str());
    match msg {
        Some(m) => m.to_string(),
        // No answer, or a proxy's error page because the server is restarting.
        None if status == 0 => OFFLINE.into(),
        None if matches!(status, 502..=504) => UNAVAILABLE.into(),
        None => format!("Error {status}: {}", &body[..body.len().min(160)]),
    }
}

/// One request. `signed_in` adds the session cookie; it also returns any session the response sets.
async fn send(signed_in: bool, method: &str, path: &str, body: Option<&Value>, timeout: u64) -> Result<(u16, Value, Option<String>), String> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
    let mut req = client().request(m, url(path)).timeout(Duration::from_secs(timeout));
    if signed_in {
        req = with_session(req)
    }
    if let Some(b) = body {
        req = req.json(b)
    }
    let r = req.go().await.map_err(|_| error_text(0, &Value::Null, ""))?;
    let status = r.status().as_u16();
    let session = session_cookie(&r);
    let text = r.text().await.map_err(|e| e.to_string())?;
    let v = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) { Ok((status, v, session)) } else { Err(format!("{status}\u{1}{}", error_text(status, &v, &text))) }
}

/// Splits our internal "status\x01message" error into its parts.
pub fn status_of(e: &str) -> (u16, &str) {
    match e.split_once('\u{1}') {
        Some((s, m)) => (s.parse().unwrap_or(0), m),
        None => (0, e),
    }
}

pub fn msg(e: String) -> String { status_of(&e).1.to_string() }

/// Signs in with the account code or a 16-digit account number (any spacing). Only the
/// session it starts is kept.
pub async fn login(number: &str) -> Result<(), String> {
    // Only a key derived from the number is sent (private.rs); an account from before that is moved
    // over by sending the number once.
    let by_key = match crate::private::keys(number).await {
        Ok(key) => send(false, "POST", "/login", Some(&json!({"account_key": key})), 20).await.ok().filter(|(_, v, _)| v["success"] != false),
        Err(_) => None,
    };
    // A code never leaves the phone, even when signing in with its key fails: only a 16-digit
    // number from before account keys is sent as it is, once, to move the account over.
    let (_, v, session) = match by_key {
        Some(r) => r,
        None if crate::private::kind_of(number) == Some("code") => {
            return Err("That account code didn't sign in. Check each character.".to_string());
        }
        None => send(false, "POST", "/login", Some(&json!({"account_number": number})), 20).await.map_err(msg)?,
    };
    if v["success"] == false {
        return Err(v["message"].as_str().unwrap_or("Sign-in failed").to_string());
    }
    set_session(Some(session.ok_or("Sign-in didn't start a session")?));
    EXPIRED.store(false, Ordering::SeqCst);
    Ok(())
}

/// Makes a new account (the caller has had the Terms accepted), signed in. Returns its account
/// code and any starter credit it got.
pub async fn create_account(referral: &str) -> Result<(String, Option<f64>), String> {
    // The code is made here and only a key derived from it is sent: the server never learns it.
    let number = crate::private::new_code();
    let key = crate::private::keys(&number).await?;
    let mut body = json!({"accept_tos": true, "account_key": key});
    if !referral.trim().is_empty() {
        body["referral_code"] = json!(referral.trim());
    }
    let (_, v, session) = send(false, "POST", "/create_account", Some(&body), 20).await.map_err(msg)?;
    let n = v["account_number"].as_str().map(str::to_string).unwrap_or(number);
    set_session(Some(session.ok_or("Sign-up didn't start a session")?));
    EXPIRED.store(false, Ordering::SeqCst);
    let credit = v["starter_credit"].as_f64().or_else(|| v["starter_credit"].as_str().and_then(|s| s.parse().ok()));
    Ok((n, credit))
}

/// Switches the account to a new account code made on the phone: only its account key is sent.
/// The old number or code, the old API key and other sessions stop working. Returns the code's seed.
pub async fn switch_to_code(code: &str) -> Result<[u8; 32], String> {
    let k = crate::private::keys_of(code).await?;
    call("POST", "/account_number/rotate", Some(json!({"account_key": k.login_key}))).await?;
    Ok(k.seed.0)
}

/// A signed-in call. A 401 means the session is over: the app goes back to sign-in.
pub async fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    match send(true, method, path, body.as_ref(), 30).await {
        Ok((_, v, _)) => Ok(v),
        Err(e) => {
            if status_of(&e).0 == 401 && has_session() {
                expire()
            }
            Err(e)
        }
    }
}

pub async fn get(path: &str) -> Result<Value, String> { call("GET", path, None).await.map_err(msg) }
pub async fn post(path: &str, body: Value) -> Result<Value, String> { call("POST", path, Some(body)).await.map_err(msg) }

/// Whether the server answers at all (any HTTP status): false means no connection.
pub async fn reachable() -> bool {
    match send(false, "GET", "/health", None, 10).await {
        Ok(_) => true,
        Err(e) => status_of(&e).0 != 0,
    }
}

/// Chat sync (sync.rs): no cookie, only the sync token, by the same route as everything else
/// (Tor, mirrors). `(status, body, Retry-After seconds)`.
pub async fn sync_call(method: &str, path: &str, token: &str, if_match: Option<i64>, body: Option<Vec<u8>>) -> Result<(u16, Vec<u8>, Option<u64>), String> {
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = client().request(m, url(path)).timeout(Duration::from_secs(90)).header("Authorization", format!("Bearer {token}"));
    if let Some(r) = if_match {
        req = req.header("If-Match", r.to_string());
    }
    if let Some(b) = body {
        req = req.header("Content-Type", "application/octet-stream").body(b);
    }
    let r = req.go().await.map_err(|_| offline())?;
    let status = r.status().as_u16();
    let retry = r.headers().get("retry-after").and_then(|v| v.to_str().ok()?.trim().parse().ok());
    let bytes = r.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((status, bytes, retry))
}

/// Public calls with no cookie or key: models and everything about buying vouchers.
pub async fn public(method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    send(false, method, path, body.as_ref(), 30).await.map(|(_, v, _)| v)
}

async fn csrf() -> Result<String, String> {
    if let Some((t, at)) = CSRF.lock().unwrap().clone()
        && at.elapsed() < Duration::from_secs(1500)
    {
        return Ok(t);
    }
    let v = get("/api/csrf-token").await?;
    let t = v["token"].as_str().ok_or("No CSRF token")?.to_string();
    *CSRF.lock().unwrap() = Some((t.clone(), Instant::now()));
    Ok(t)
}

/// A guest's tool call. Guests have no session to tie a CSRF token to, so the server checks that
/// the request says it's from Obscurify; its guest limits apply as they do on the website.
pub async fn guest_post(path: &str, body: &Value, timeout: u64) -> Result<Value, String> {
    let r = client().post(url(path)).timeout(Duration::from_secs(timeout)).header("Origin", base()).json(body).go().await.map_err(|_| offline())?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(|_| offline())?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) { Ok(v) } else { Err(format!("{status}\u{1}{}", error_text(status, &v, &text))) }
}

/// The conversation the server holds for us (in memory, up to an hour): the rev to continue and
/// each message as we sent it. A request whose messages start with those sends only the rest.
static THREAD: Mutex<Option<(String, Vec<String>)>> = Mutex::new(None);
/// A rev the server no longer holds: send the whole conversation again.
const THREAD_EXPIRED: &str = "\u{1}thread_expired";

/// `body` as sent on the held thread, and what it stands for once sent.
fn thread_body(body: &Value) -> (Value, Vec<String>) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let sent: Vec<String> = messages.iter().map(|m| m.to_string()).collect();
    let mut out = body.clone();
    out["thread"] = json!({});
    if let Some((rev, held)) = THREAD.lock().unwrap().as_ref()
        && held.len() <= sent.len() && held[..] == sent[..held.len()]
    {
        out["messages"] = Value::Array(messages[held.len()..].to_vec());
        out["thread"] = json!({"rev": rev});
    }
    (out, sent)
}

/// The iPhone app sends nothing to a model before the AI providers sheet is agreed to (`chat::needs_consent`).
pub fn consent() -> Result<(), String> {
    if crate::chat::needs_consent() {
        *crate::chat::ASK_CONSENT.write() = true;
        return Err(crate::chat::NEEDS_CONSENT.into());
    }
    Ok(())
}

/// Sent first in every completion from the Play build: Play requires AI apps to prevent this content.
pub const PLAY_GUARD: &str = "Never write sexually explicit content, content that sexualizes minors, or instructions that would help someone seriously hurt people.";

/// The Play build: puts `PLAY_GUARD` first in a completion's messages (once). Every completion the
/// app sends goes through here (chat, encrypted, Code Mode, summaries).
pub fn play_guard(body: &mut Value) {
    if !crate::PLAY { return }
    let Some(m) = body["messages"].as_array_mut() else { return };
    if m.first().is_some_and(|f| f["content"] == PLAY_GUARD) { return }
    m.insert(0, json!({"role": "system", "content": PLAY_GUARD}));
}

/// Starts a streamed chat completion (`/web/chat/completions`, every plan). Returns the open
/// response; the caller reads its SSE body. Sends only what the server doesn't already hold.
pub async fn chat(body: &Value) -> Result<ReplyStream, String> {
    consent()?;
    let mut body = body.clone();
    play_guard(&mut body);
    // Signed in, the server finishes the answer whatever happens to the app (closed, killed, offline)
    // and holds it for an hour: `follow` picks it up again.
    if has_session() {
        body["background"] = json!(true);
    }
    let body = &body;
    let (threaded, sent) = thread_body(body);
    let r = match post_chat(&threaded).await {
        Err(e) if e == THREAD_EXPIRED => {
            *THREAD.lock().unwrap() = None;
            let mut whole = body.clone();
            whole["thread"] = json!({});
            post_chat(&whole).await
        }
        other => other,
    }
    .map_err(|e| if e == THREAD_EXPIRED { error_text(409, &Value::Null, "") } else { e })?;
    let rev = r.headers().get("x-obscurify-thread").and_then(|v| v.to_str().ok()).map(str::to_string);
    *THREAD.lock().unwrap() = rev.map(|rev| (rev, sent));
    Ok(ReplyStream::new(r))
}

// ---- Encrypted mode ----------------------------------------------------------------------------

/// The route (`X-Obscurify-TEE-Route`) whose attestation and receipts belong to encrypted replies.
pub const E2EE_ROUTE: &str = "e2ee";

/// What reading and checking an encrypted answer needs, kept with a pending reply so a reopened app
/// can still read it: this request's key (hex), the attestation it was encrypted to and the nonce
/// it answered, and the request in plain text (the enclave must have received exactly that).
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, PartialEq)]
pub struct Sealed {
    pub secret: String,
    pub attestation: String,
    pub nonce: String,
    pub plain: String,
}

fn verifier_options() -> obscurify_tee_verify::Options {
    obscurify_tee_verify::Options { pccs_url: format!("{}/tee/pccs", base()), ..Default::default() }
}

/// Starts an end-to-end encrypted chat completion: the TEE gateway's attestation is fetched with a
/// fresh nonce and checked on the phone, then every message is encrypted to the key it vouches for
/// and sent whole (never as a held thread). Nothing is sent when that fails ("Not encrypted: ...").
/// The stream decrypts each event as it reads it.
pub async fn chat_encrypted(body: &Value) -> Result<ReplyStream, String> {
    consent()?;
    let model = body["model"].as_str().unwrap_or_default().to_string();
    let nonce = obscurify_tee_verify::new_nonce();
    let route = [("X-Obscurify-TEE-Route", E2EE_ROUTE.to_string())];
    let path = format!("/web/tee/attestation?model={}&nonce={nonce}", crate::tee::urlencoding(&model));
    let attestation = match web_get(&path, &route, 100).await {
        Ok((200, text)) => text,
        Ok((status, _)) => return Err(format!("Not encrypted: the enclave's attestation couldn't be fetched (error {status}), so nothing was sent.")),
        Err(_) => return Err(OFFLINE.into()),
    };
    let attested = obscurify_tee_verify::verify_attestation(&attestation, &nonce, &verifier_options()).await;
    let Some(key) = attested.e2ee_public_key.clone().filter(|_| attested.verified) else {
        return Err("Not encrypted: the enclave's attestation didn't verify, so nothing was sent.".into());
    };
    let mut out = body.clone();
    play_guard(&mut out);
    if let Some(o) = out.as_object_mut() {
        for k in ["web_search", "thread", "tee_verify", "tools", "tool_choice"] { o.remove(k); }
    }
    out["stream"] = json!(true);
    out["stream_options"] = json!({"include_usage": true});
    if has_session() {
        out["background"] = json!(true);
    }
    let session = obscurify_tee_verify::e2ee::Session::new();
    let sealed = session.encrypt_request(&key, &out.to_string()).map_err(|e| format!("Not encrypted: {e}, so nothing was sent."))?;
    let headers = [
        (obscurify_tee_verify::e2ee::HEADER_CLIENT_KEY, session.public_key_hex().to_string()),
        (obscurify_tee_verify::e2ee::HEADER_MODEL_KEY, key),
        (obscurify_tee_verify::e2ee::HEADER_SIGNING_ALGO, obscurify_tee_verify::e2ee::SIGNING_ALGO.to_string()),
    ];
    let r = post_raw_chat(sealed.body, &headers).await?;
    let mut stream = ReplyStream::new(r);
    stream.sealed = Some(Sealed { secret: session.secret_hex(), attestation, nonce, plain: sealed.plain });
    stream.session = Some(session);
    Ok(stream)
}

/// An encrypted answer the server is holding, read with the key the app kept for it.
pub async fn follow_encrypted(id: &str, sealed: &Sealed) -> Result<ReplyStream, String> {
    let session = obscurify_tee_verify::e2ee::Session::from_secret_hex(&sealed.secret).map_err(|e| e.to_string())?;
    let mut stream = follow(id).await?;
    stream.session = Some(session);
    stream.sealed = Some(sealed.clone());
    Ok(stream)
}

/// A signed-in POST of `body` exactly as given (an encrypted request) to `/web/chat/completions`.
async fn post_raw_chat(body: String, headers: &[(&str, String)]) -> Result<reqwest::Response, String> {
    let mut tries = 0;
    loop {
        let token = csrf().await?;
        let mut req = with_session(client().post(url("/web/chat/completions")))
            .header("X-CSRF-Token", token)
            .header("Content-Type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let r = req.body(body.clone()).go().await.map_err(|_| error_text(0, &Value::Null, ""))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let text = r.text().await.unwrap_or_default();
        let m = chat_error(status, &text);
        if status == 401 {
            expire();
            return Err(m);
        }
        tries += 1;
        if status == 403 && m.to_lowercase().contains("csrf") && tries < 2 {
            *CSRF.lock().unwrap() = None;
        } else {
            return Err(m);
        }
    }
}

// ---- background replies ------------------------------------------------------------------------

/// What an answer the server no longer holds says (it keeps them an hour).
pub const REPLY_EXPIRED: &str = "This answer is no longer held on the server (answers are held for up to an hour). Try again.";

/// The comment the server sends while a stream is quiet. Not one of the reply's events.
const KEEPALIVE: &[u8] = b": obscurify-keepalive\n\n";

/// Rings when the app comes back on screen or the network comes back: a reply whose connection has
/// been quiet since is asked for again at once, instead of after STALL. A phone that slept (screen
/// off, a VPN or Wi-Fi that dropped) can leave a connection that's dead but never says so.
static KICK: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub fn kick() { KICK.notify_waiters() }

/// The server sends a keepalive every 15 s: a connection with nothing for this long is dead.
const FRESH: Duration = Duration::from_secs(20);

/// The background answer being read now, so Stop can end it on the server too.
static CURRENT: Mutex<Option<String>> = Mutex::new(None);

/// A streamed answer, read event by event. A signed-in answer runs on the server in the background
/// (`X-Obscurify-Reply`), so a dropped or stalled connection is picked up again from the last whole
/// event (`GET /web/replies/{id}?after=N`) instead of failing. `raw` keeps every whole event exactly
/// as it came, which is what a TEE receipt signs.
pub struct ReplyStream {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>>>>,
    pub id: Option<String>,
    /// TEE verification: the receipt that signs these bytes, and the route that answered.
    pub receipt: Option<String>,
    pub route: Option<String>,
    /// Encrypted mode: the key that decrypts each event, and what checking the answer needs.
    pub session: Option<obscurify_tee_verify::e2ee::Session>,
    pub sealed: Option<Sealed>,
    /// An event that couldn't be decrypted: the answer stops there.
    broken: Option<String>,
    events: usize,
    partial: Vec<u8>,
    pub raw: Vec<u8>,
    saw_done: bool,
    /// When anything (a keepalive too) last arrived.
    heard: Instant,
}

fn header(r: &reqwest::Response, name: &str) -> Option<String> {
    r.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

impl ReplyStream {
    pub fn new(r: reqwest::Response) -> Self {
        let id = header(&r, "x-obscurify-reply");
        if id.is_some() {
            *CURRENT.lock().unwrap() = id.clone();
        }
        ReplyStream {
            id,
            receipt: header(&r, "x-obscurify-tee-receipt"),
            route: header(&r, "x-obscurify-tee-route"),
            session: None,
            sealed: None,
            broken: None,
            inner: Box::pin(r.bytes_stream()),
            events: 0,
            partial: vec![],
            raw: vec![],
            saw_done: false,
            heard: Instant::now(),
        }
    }

    /// Whole events taken from what has arrived, if any.
    fn take_events(&mut self) -> Option<Vec<u8>> {
        let mut out = vec![];
        while let Some(end) = self.partial.windows(2).position(|w| w == b"\n\n") {
            let frame: Vec<u8> = self.partial.drain(..end + 2).collect();
            self.raw.extend_from_slice(&frame);
            if frame == KEEPALIVE {
                continue;
            }
            self.events += 1;
            if frame.starts_with(b"data: [DONE]") {
                self.saw_done = true;
            }
            match &self.session {
                Some(session) => match session.decrypt_event(&frame) {
                    Ok(plain) => out.extend_from_slice(&plain),
                    Err(e) => {
                        self.broken = Some(format!("Check failed: the answer couldn't be decrypted ({e}). Stopped reading it."));
                        break;
                    }
                },
                None => out.extend_from_slice(&frame),
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// The next whole events, None at the end, or an error once the answer can't be had.
    /// `stop` ends a wait for the connection.
    pub async fn next(&mut self, stop: &impl Fn() -> bool) -> Option<Result<Vec<u8>, String>> {
        let mut delay = 1000u64;
        let mut lost_since: Option<Instant> = None;
        loop {
            if let Some(e) = self.broken.take() {
                self.finish();
                return Some(Err(e));
            }
            if let Some(events) = self.take_events() {
                return Some(Ok(events));
            }
            let got = tokio::select! {
                got = tokio::time::timeout(STALL, futures_util::StreamExt::next(&mut self.inner)) => got.ok(),
                // Back on screen or online: a connection that has gone quiet is given up now.
                _ = KICK.notified() => { if self.heard.elapsed() < FRESH { continue } None }
            };
            match got {
                Some(Some(Ok(b))) => {
                    #[cfg(target_os = "ios")]
                    crate::ios::activity();
                    self.partial.extend_from_slice(&b);
                    self.heard = Instant::now();
                    lost_since = None;
                    continue
                }
                // A clean end: the answer is over, unless a held answer ended before [DONE] (asked
                // again once: what the server has is the whole of it).
                Some(None) if self.saw_done || self.id.is_none() || lost_since.is_some() => { self.finish(); return None }
                _ => {}
            }
            let Some(id) = self.id.clone() else {
                return Some(Err("The connection dropped.".into()));
            };
            // Pick it up from the last whole event; what came after it arrives again.
            self.partial.clear();
            let since = *lost_since.get_or_insert_with(Instant::now);
            loop {
                if stop() { return None }
                match follow_from(&id, self.events).await {
                    Ok(r) => { self.inner = Box::pin(r.bytes_stream()); self.heard = Instant::now(); break }
                    Err(e) if e == REPLY_EXPIRED => { self.finish(); return Some(Err(e)) }
                    Err(e) if since.elapsed() > LOST_FOR || !transient(&e) && status_of(&e).0 != 0 => { self.finish(); return Some(Err(msg(e))) }
                    Err(_) => {
                        tokio::select! { _ = tokio::time::sleep(Duration::from_millis(delay)) => {}, _ = KICK.notified() => {} }
                        delay = (delay * 2).min(15_000);
                    }
                }
            }
        }
    }

    fn finish(&self) {
        let mut c = CURRENT.lock().unwrap();
        if c.is_some() && *c == self.id {
            *c = None;
        }
    }
}

impl Drop for ReplyStream {
    fn drop(&mut self) { self.finish() }
}

/// A stream that sends nothing (not even a keepalive, every 15 s) for this long is dead.
const STALL: Duration = Duration::from_secs(45);
/// How long a lost connection is waited out before the answer is given up.
const LOST_FOR: Duration = Duration::from_secs(15 * 60);

async fn follow_from(id: &str, after: usize) -> Result<reqwest::Response, String> {
    let req = client().get(url(&format!("/web/replies/{id}?after={after}")));
    let req = match crate::private::secret(Some(id)).await {
        Some(secret) => req.bearer_auth(secret),
        None => with_session(req),
    };
    let r = req
        .go()
        .await
        .map_err(|_| offline())?;
    let status = r.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(r);
    }
    let text = r.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status == 404 && (v["code"] == "reply_expired" || v["error"]["code"] == "reply_expired") {
        return Err(REPLY_EXPIRED.into());
    }
    if status == 401 {
        expire();
    }
    Err(format!("{status}\u{1}{}", error_text(status, &v, &text)))
}

/// An answer the server is holding, from its start: after the app was killed or closed.
pub async fn follow(id: &str) -> Result<ReplyStream, String> {
    loop {
        match follow_from(id, 0).await {
            Ok(r) => return Ok(ReplyStream::new(r)),
            Err(e) if e == REPLY_EXPIRED => return Err(e),
            Err(e) if status_of(&e).0 == 0 => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(e) => return Err(msg(e)),
        }
    }
}

/// Stops the answer being read now on the server: closing the connection no longer does.
pub fn cancel_current() {
    let Some(id) = CURRENT.lock().unwrap().take() else { return };
    dioxus::core::spawn_forever(async move {
        let _ = web_post(&format!("/web/replies/{id}/cancel"), &json!({}), 20).await;
    });
}

/// A signed-in GET to a web endpoint that answers JSON, with its status: TEE receipts and reports.
pub async fn web_get(path: &str, headers: &[(&str, String)], timeout: u64) -> Result<(u16, String), String> {
    let mut req = with_session(client().get(url(path))).timeout(Duration::from_secs(timeout));
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let r = req.go().await.map_err(|_| offline())?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(|_| offline())?;
    Ok((status, text))
}

fn chat_error(status: u16, text: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    if status == 409 && v["code"] == "thread_expired" {
        return THREAD_EXPIRED.into();
    }
    error_text(status, &v, text)
}

async fn post_chat(body: &Value) -> Result<reqwest::Response, String> {
    if !has_session() {
        let r = client().post(url("/anon/chat/completions")).json(body).go().await.map_err(|_| error_text(0, &Value::Null, ""))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let text = r.text().await.unwrap_or_default();
        return Err(chat_error(status, &text));
    }
    if let Some(r) = post_private(body).await {
        return r;
    }
    let mut tries = 0;
    loop {
        let token = csrf().await?;
        let r = with_session(client().post(url("/web/chat/completions")))
            .header("X-CSRF-Token", token)
            .json(body)
            .go()
            .await
            .map_err(|_| error_text(0, &Value::Null, ""))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let text = r.text().await.unwrap_or_default();
        let m = chat_error(status, &text);
        if status == 401 {
            expire();
            return Err(m);
        }
        tries += 1;
        if status == 403 && m.to_lowercase().contains("csrf") && tries < 2 {
            *CSRF.lock().unwrap() = None;
        } else {
            return Err(m);
        }
    }
}

/// A chat request on this chat's private session (private.rs): its secret, no cookie, no CSRF.
/// Tops the session up when it runs out. `None`: private sessions are off; use the cookie.
async fn post_private(body: &Value) -> Option<Result<reqwest::Response, String>> {
    let mut passes = 0;
    let mut none: Vec<String> = Vec::new();
    let mut attempt = 0;
    while attempt < 4 {
        let secret = crate::private::secret(None).await?;
        let r = match client().post(url("/web/chat/completions")).bearer_auth(&secret).json(body).go().await {
            Ok(r) => r,
            Err(_) => return Some(Err(error_text(0, &Value::Null, ""))),
        };
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            if let Some(id) = header(&r, "x-obscurify-reply") {
                crate::private::note_reply(&id, &secret).await;
            }
            return Some(Ok(r));
        }
        // The server wants a pass for this request (a model on a plan, a free model): the
        // session shows one and asks again. A few in a row at most (a long free-model request
        // can take more than one). With none to show, the same once more: the server may
        // serve it another way (the guest model past its free tokens is charged).
        if status == 403 && passes < 8 && let Some(label) = header(&r, PASS_REQUIRED) {
            if crate::private::needs_pass(&secret, &label).await {
                passes += 1;
                continue;
            }
            if !none.contains(&label) {
                none.push(label);
                continue;
            }
        }
        match status {
            402 => crate::private::ran_out(&secret, attempt).await,
            401 => crate::private::expired(&secret).await,
            _ => return Some(Err(chat_error(status, &r.text().await.unwrap_or_default()))),
        }
        attempt += 1;
    }
    Some(Err(error_text(402, &Value::Null, "")))
}

/// A signed-in POST to a web endpoint (`/web/...`), which wants the CSRF token as well as the
/// session (Code Mode's tools).
pub async fn web_post(path: &str, body: &Value, timeout: u64) -> Result<Value, String> {
    // On a private session: the reply's own session for /web/replies/<id>, else this chat's.
    let reply = path.strip_prefix("/web/replies/").and_then(|r| r.split('/').next());
    let private = match reply {
        Some(id) => crate::private::secret(Some(id)).await,
        None => crate::private::secret(None).await,
    };
    if let Some(mut secret) = private {
        // The server holds a reply's (or paid tool's) worst case first: a 402 tops this chat's
        // session up, more each time, and asks again, as post_private does.
        let mut passes = 0;
        let mut none: Vec<String> = Vec::new();
        let mut attempt = 0;
        while attempt < 4 {
            let r = client().post(url(path)).timeout(Duration::from_secs(timeout)).bearer_auth(&secret).json(body).go().await.map_err(|_| offline())?;
            let status = r.status().as_u16();
            let wanted = header(&r, PASS_REQUIRED);
            let text = r.text().await.map_err(|_| offline())?;
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                return Ok(v);
            }
            // A pass the request needs (see `post_private`): shown, then the same again.
            if status == 403 && reply.is_none() && passes < 8 && let Some(label) = wanted {
                if crate::private::needs_pass(&secret, &label).await {
                    passes += 1;
                    continue;
                }
                if !none.contains(&label) {
                    none.push(label);
                    continue;
                }
            }
            match status {
                402 if reply.is_none() && attempt < 3 => crate::private::ran_out(&secret, attempt).await,
                401 if reply.is_none() && attempt < 3 => crate::private::expired(&secret).await,
                _ => return Err(format!("{status}\u{1}{}", error_text(status, &v, &text))),
            }
            match crate::private::secret(None).await {
                Some(s) => secret = s,
                None => break,
            }
            attempt += 1;
        }
        return Err(format!("402\u{1}{}", error_text(402, &Value::Null, "")));
    }
    for attempt in 0..2 {
        let token = csrf().await.map_err(|e| if e == OFFLINE { offline() } else { e })?;
        let r = with_session(client().post(url(path)))
            .timeout(Duration::from_secs(timeout))
            .header("X-CSRF-Token", token)
            .json(body)
            .go()
            .await
            .map_err(|_| offline())?;
        let status = r.status().as_u16();
        // A body that doesn't arrive is a lost connection, not an empty answer.
        let text = r.text().await.map_err(|_| offline())?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(v);
        }
        let m = error_text(status, &v, &text);
        if status == 401 {
            expire();
            return Err(format!("{status}\u{1}{m}"));
        }
        if status == 403 && m.to_lowercase().contains("csrf") && attempt == 0 {
            *CSRF.lock().unwrap() = None;
            continue;
        }
        return Err(format!("{status}\u{1}{m}"));
    }
    Err("The request failed".into())
}
