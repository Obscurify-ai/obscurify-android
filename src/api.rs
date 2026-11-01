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
        None if status == 0 => OFFLINE.into(),
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

/// Starts a streamed chat completion (`/web/chat/completions`, every plan). Returns the open
/// response; the caller reads its SSE body.
pub async fn chat(body: &Value) -> Result<reqwest::Response, String> {
    if !has_session() {
        let r = client().post(format!("{BASE}/anon/chat/completions")).json(body).send().await.map_err(|_| error_text(0, &Value::Null, ""))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let text = r.text().await.unwrap_or_default();
        return Err(error_text(status, &serde_json::from_str(&text).unwrap_or(Value::Null), &text));
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
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let m = error_text(status, &v, &text);
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
