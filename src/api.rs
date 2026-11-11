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

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // Phones change networks and VPNs drop: don't reuse a connection that may be dead by now,
        // and notice a dead one in the middle of a read.
        .pool_idle_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(180))
        .user_agent("Obscurify-Android")
        .build()
        .unwrap())
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
pub fn forget() { set_session(None) }

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
    let mut req = client().request(m, format!("{BASE}{path}")).timeout(Duration::from_secs(timeout));
    if signed_in {
        req = with_session(req)
    }
    if let Some(b) = body {
        req = req.json(b)
    }
    let r = req.send().await.map_err(|_| error_text(0, &Value::Null, ""))?;
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

/// Signs in with the 16-digit account number (spaces and dashes are fine). Only the session it
/// starts is kept.
pub async fn login(number: &str) -> Result<(), String> {
    let (_, v, session) = send(false, "POST", "/login", Some(&json!({"account_number": number})), 20).await.map_err(msg)?;
    if v["success"] == false {
        return Err(v["message"].as_str().unwrap_or("Sign-in failed").to_string());
    }
    set_session(Some(session.ok_or("Sign-in didn't start a session")?));
    EXPIRED.store(false, Ordering::SeqCst);
    Ok(())
}

/// Makes a new account (the caller has had the Terms accepted), signed in. Returns its number and
/// any starter credit it got.
pub async fn create_account(referral: &str) -> Result<(String, Option<f64>), String> {
    let mut body = json!({"accept_tos": true});
    if !referral.trim().is_empty() {
        body["referral_code"] = json!(referral.trim());
    }
    let (_, v, session) = send(false, "POST", "/create_account", Some(&body), 20).await.map_err(msg)?;
    let n = v["account_number"].as_str().ok_or("Could not create an account. Try again.")?.to_string();
    set_session(Some(session.ok_or("Sign-up didn't start a session")?));
    EXPIRED.store(false, Ordering::SeqCst);
    let credit = v["starter_credit"].as_f64().or_else(|| v["starter_credit"].as_str().and_then(|s| s.parse().ok()));
    Ok((n, credit))
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
    let r = client().post(format!("{BASE}{path}")).timeout(Duration::from_secs(timeout)).header("Origin", BASE).json(body).send().await.map_err(|_| offline())?;
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
    if let Some((rev, held)) = THREAD.lock().unwrap().as_ref() {
        if held.len() <= sent.len() && held[..] == sent[..held.len()] {
            out["messages"] = Value::Array(messages[held.len()..].to_vec());
            out["thread"] = json!({"rev": rev});
        }
    }
    (out, sent)
}

/// Starts a streamed chat completion (`/web/chat/completions`, every plan). Returns the open
/// response; the caller reads its SSE body. Sends only what the server doesn't already hold.
pub async fn chat(body: &Value) -> Result<ReplyStream, String> {
    let mut body = body.clone();
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
            out.extend_from_slice(&frame);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The next whole events, None at the end, or an error once the answer can't be had.
    /// `stop` ends a wait for the connection.
    pub async fn next(&mut self, stop: &impl Fn() -> bool) -> Option<Result<Vec<u8>, String>> {
        let mut delay = 1000u64;
        let mut lost_since: Option<Instant> = None;
        loop {
            if let Some(events) = self.take_events() {
                return Some(Ok(events));
            }
            let got = tokio::select! {
                got = tokio::time::timeout(STALL, futures_util::StreamExt::next(&mut self.inner)) => got.ok(),
                // Back on screen or online: a connection that has gone quiet is given up now.
                _ = KICK.notified() => { if self.heard.elapsed() < FRESH { continue } None }
            };
            match got {
                Some(Some(Ok(b))) => { self.partial.extend_from_slice(&b); self.heard = Instant::now(); lost_since = None; continue }
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
    let r = with_session(client().get(format!("{BASE}/web/replies/{id}?after={after}")))
        .send()
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
    let mut req = with_session(client().get(format!("{BASE}{path}"))).timeout(Duration::from_secs(timeout));
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let r = req.send().await.map_err(|_| offline())?;
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
        let r = client().post(format!("{BASE}/anon/chat/completions")).json(body).send().await.map_err(|_| error_text(0, &Value::Null, ""))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let text = r.text().await.unwrap_or_default();
        return Err(chat_error(status, &text));
    }
    let mut tries = 0;
    loop {
        let token = csrf().await?;
        let r = with_session(client().post(format!("{BASE}/web/chat/completions")))
            .header("X-CSRF-Token", token)
            .json(body)
            .send()
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

/// A signed-in POST to a web endpoint (`/web/...`), which wants the CSRF token as well as the
/// session (Code Mode's tools).
pub async fn web_post(path: &str, body: &Value, timeout: u64) -> Result<Value, String> {
    for attempt in 0..2 {
        let token = csrf().await.map_err(|e| if e == OFFLINE { offline() } else { e })?;
        let r = with_session(client().post(format!("{BASE}{path}")))
            .timeout(Duration::from_secs(timeout))
            .header("X-CSRF-Token", token)
            .json(body)
            .send()
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
