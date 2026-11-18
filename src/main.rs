//! Obscurify for Android: a thin client of the Obscurify API. Sign in with the account number, chat
//! with any model (streaming, web search, images), keep chats on the phone, manage the plan and
//! API key, and add funds (top-ups to the account, or blind vouchers). No local models. The Android
//! glue (Back, opening links and wallets, copying) is the MainActivity that build.sh writes in.

mod api;
mod chat;
mod code;
mod compact;
mod private;
mod files;
mod store;
mod sync;
mod tee;
#[cfg(feature = "tor")]
mod tor;
#[cfg(not(feature = "tor"))]
#[path = "tor_off.rs"]
mod tor;
mod wallet;

use chat::{Conv, Meta, Msg};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wallet::Wallet;

const CSS: &str = include_str!("../assets/style.css");
pub const FREE_MODEL: &str = "meta-llama/llama-3.3-70b-instruct";
/// The Google Play build (`--features play`). Play's rules mean no payments in the app (funds and
/// plans are handled on the website), no uncensored or roleplay models (`chat::offered`), and a
/// content guard on every completion (`api::play_guard`). Gated where it can't be got around: the
/// Funds screen never renders and the model list is filtered where it loads.
pub const PLAY: bool = cfg!(feature = "play");

// ---- JS bridge: links, Back, visibility, picked images, the chat pinned to the bottom ----

const BRIDGE: &str = r#"
document.addEventListener('click', e => {
  const a = e.target.closest('a[href]');
  if (!a) return;
  e.preventDefault();
  if (/^https?:/.test(a.href) && !a.href.startsWith(location.origin)) dioxus.send({link: a.href});
}, true);
window.obsBack = () => { const open = !!document.querySelector('[data-back]'); if (open) dioxus.send({back: true}); return open; };
window.obsOnline = () => dioxus.send({online: true});
// Code Mode: messages from the sandbox WebView, relayed by MainActivity.
window.obsSandbox = (s) => dioxus.send({sandbox: s});
// Dioxus cancels this event in release builds, which on Android also takes away the Copy toolbar
// when text is selected.
window.addEventListener('contextmenu', e => e.stopImmediatePropagation(), true);
// The message box grows with what's typed or dictated, up to half the screen, then scrolls.
document.addEventListener('input', e => {
  const t = e.target;
  if (t.id !== 'composer') return;
  t.style.height = 'auto'; t.style.height = t.scrollHeight + 'px';
});
document.addEventListener('visibilitychange', () => dioxus.send({visible: !document.hidden}));
// MainActivity calls this on resume and pause, since the WebView doesn't fire visibilitychange.
window.obsVisible = (v) => dioxus.send({visible: v});
// Photos are shrunk to 1600 px JPEGs before they're attached: phone photos are far over the API's limit.
async function shrink(f) {
  const url = URL.createObjectURL(f), img = new Image();
  img.src = url; await img.decode();
  const k = Math.min(1, 1600 / Math.max(img.width, img.height)), c = document.createElement('canvas');
  c.width = Math.round(img.width * k); c.height = Math.round(img.height * k);
  c.getContext('2d').drawImage(img, 0, 0, c.width, c.height);
  URL.revokeObjectURL(url);
  return c.toDataURL('image/jpeg', 0.85);
}
// The file input lives outside the Dioxus tree: Dioxus intercepts clicks on file inputs it renders
// to open a desktop file dialog, which doesn't exist on Android, so the picker never opened.
const pick = Object.assign(document.createElement('input'), {type: 'file', accept: '*/*', multiple: true, id: 'pick'});
pick.style.display = 'none';
document.body.appendChild(pick);
document.addEventListener('click', e => { if (e.target.closest('.attach')) pick.click(); }, true);
// One button for everything: photos go to vision models; other files go to the app whole
// (base64), one at a time, and it reads them (files.rs).
document.addEventListener('change', async e => {
  if (e.target.id !== 'pick') return;
  const files = [...e.target.files].slice(0, 4); e.target.value = '';
  const images = [];
  for (const f of files) {
    if (f.type.startsWith('image/')) {
      try { images.push(await shrink(f)); } catch (_) { dioxus.send({doc_error: "Couldn't read " + f.name + '.'}); }
      continue;
    }
    if (f.size > 25 * 1024 * 1024) { dioxus.send({doc_error: f.name + ' is over 25 MB.'}); continue; }
    const url = await new Promise((ok, no) => { const r = new FileReader(); r.onload = () => ok(r.result); r.onerror = no; r.readAsDataURL(f); }).catch(() => '');
    dioxus.send({doc: {name: f.name, data: String(url).split(',')[1] || ''}});
  }
  if (images.length) dioxus.send({images});
});
setInterval(() => {
  const el = document.getElementById('msgs');
  if (!el || el.dataset.watch) return;
  el.dataset.watch = 1; el.dataset.pin = 1;
  el.addEventListener('scroll', () => { el.dataset.pin = el.scrollHeight - el.scrollTop - el.clientHeight < 80 ? 1 : 0; });
  // Follow the answer while it streams; scrolled up, it stays put and "New messages below"
  // (bottom right, above the composer) jumps back and follows again, as on the web.
  const jump = document.createElement('button');
  jump.id = 'follow'; jump.className = 'follow'; jump.hidden = true;
  jump.textContent = 'New messages below \u2193';
  jump.addEventListener('click', () => { el.dataset.pin = 1; el.scrollTop = el.scrollHeight; jump.hidden = true; });
  document.body.appendChild(jump);
  const place = () => { const c = document.querySelector('.composer'); jump.style.bottom = ((c ? c.offsetHeight : 64) + 12) + 'px'; };
  el.addEventListener('scroll', () => { if (el.dataset.pin == 1) jump.hidden = true; });
  new MutationObserver(() => {
    if (el.dataset.pin == 1) { el.scrollTop = el.scrollHeight; jump.hidden = true; }
    else if (document.body.dataset.working == 1 && el.scrollHeight - el.scrollTop - el.clientHeight > 80) { place(); jump.hidden = false; }
  }).observe(el, {childList: true, subtree: true, characterData: true});
  el.scrollTop = el.scrollHeight;
}, 100);
await new Promise(() => {});
"#;

fn js(code: &str) { document::eval(code); }
/// Work that must finish even if the screen that started it goes away (Dioxus' `spawn` would cancel it).
fn bg(f: impl std::future::Future<Output = ()> + 'static) { dioxus::core::spawn_forever(f); }
// Native, not a WebView timer: those stall when the app is in the background.
async fn sleep(ms: u32) { tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await }
pub(crate) fn open_url(url: &str) { js(&format!("window.android ? android.open({url:?}) : window.open({url:?})")) }
fn usd(d: f64) -> String { if d.abs() < 0.01 && d != 0.0 { format!("${d:.4}") } else { format!("${d:.2}") } }
fn cents(c: i64) -> String { format!("${}.{:02}", c / 100, c % 100) }

// ---- state ----

#[derive(Clone, Copy, PartialEq)]
enum Screen { Login, Signup, Chat, Account, Funds }

#[derive(Clone, Copy, PartialEq)]
enum Overlay { None, Drawer, Models }

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Settings {
    model: String,
    favorites: Vec<String>,
    /// Code Mode's tools, on unless switched off (settings saved before this have no such field).
    /// With a server that has separate switches, the tools the model calls directly.
    #[serde(default = "yes")]
    tools: bool,
    /// Code Mode's code: the model can run code. On unless switched off.
    #[serde(default = "yes")]
    code: bool,
    /// Encrypted mode: end-to-end encrypted chat with TEE models, each answer checked on the
    /// phone (tee.rs). Settings saved by older versions call it `verify`; it carries over.
    #[serde(default, alias = "verify")]
    encrypted: bool,
}

fn yes() -> bool { true }

impl Default for Settings {
    fn default() -> Self { Settings { model: FREE_MODEL.into(), favorites: vec![], tools: true, code: true, encrypted: false } }
}

#[derive(Clone, PartialEq, Default)]
struct Model { id: String, name: String, prompt: f64, completion: f64, tools: bool, context: u64, categories: Vec<String>, e2ee: bool, created: i64 }

/// What the account's plan lets it chat with, from `/account_tier`.
#[derive(Clone, Copy, PartialEq)]
struct Access { frontier: bool, uncensored: bool }

#[derive(Clone, Copy)]
struct App {
    screen: Signal<Screen>,
    overlay: Signal<Overlay>,
    settings: Signal<Settings>,
    models: Signal<Vec<Model>>,
    frontier: Signal<Vec<String>>,
    uncensored: Signal<Vec<String>>,
    access: Signal<Option<Access>>,
    /// The models the plan opens chats on, best first (`default_models` from `/account_tier`).
    plan_defaults: Signal<Vec<String>>,
    convs: Signal<Vec<Meta>>,
    conv: Signal<Conv>,
    live: Signal<Option<Msg>>,
    stop: Signal<bool>,
    /// A request is waiting for the connection to come back.
    waiting: Signal<bool>,
    attach: Signal<Vec<String>>,
    /// Files read for the next message (see files.rs).
    docs: Signal<Vec<files::Doc>>,
    wallet: Signal<Wallet>,
    /// The account's recent top-ups (`GET /top_ups`), and what each had credited when last seen.
    top_ups: Signal<Vec<wallet::TopUp>>,
    top_up_seen: Signal<Option<std::collections::BTreeMap<String, i64>>>,
    balance: Signal<Option<f64>>,
    toast: Signal<Option<String>>,
    visible: Signal<bool>,
    /// Code Mode settings from the server (`GET /code_mode`).
    code: Signal<code::Config>,
}

impl App {
    fn say(mut self, s: impl Into<String>) {
        let s = s.into();
        self.toast.set(Some(s.clone()));
        bg(async move {
            sleep(3500).await;
            if self.toast.peek().as_deref() == Some(&s) {
                self.toast.set(None)
            }
        });
    }
    fn copy(self, text: &str, what: &str) {
        js(&format!("window.android ? android.copy({text:?}) : navigator.clipboard.writeText({text:?})"));
        self.say(format!("{what} copied"));
    }
    fn save_settings(&self) { store::save("settings.json", &*self.settings.peek()) }
    fn model_name(&self, id: &str) -> String {
        self.models.peek().iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id).to_string())
    }
    /// Balance, and what the plan unlocks (the model picker lists the rest last).
    fn refresh_balance(mut self) {
        if !api::has_session() { return }
        bg(async move {
            code::load(self).await;
            api::refresh_mirrors().await;
            if let Ok(v) = api::get("/get_user_balance").await {
                // Plus what this phone holds for private chats: still the customer's money.
                let held = private::spending().await["prepaid_millicents"].as_i64().unwrap_or(0) as f64 / 100_000.0;
                self.balance.set(v["balance"].as_f64().map(|b| b + held))
            }
            if let Ok(v) = api::get("/account_tier").await {
                let tier = v["tier"].as_str().unwrap_or("");
                self.access.set(Some(Access {
                    frontier: v["can_access_frontier"].as_bool().unwrap_or(tier != "pay_as_you_go"),
                    uncensored: v["can_access_uncensored"].as_bool().unwrap_or(tier == "pro"),
                }));
                self.plan_defaults.set(v["default_models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(String::from)).collect());
                chat::open_on_plan_default(self);
            }
        })
    }
    fn signed_in(mut self) {
        self.screen.set(Screen::Chat);
        self.refresh_balance();
        self.refresh_top_ups();
    }
    fn sign_out(mut self) {
        bg(async move { let _ = api::call("POST", "/logout", Some(json!({}))).await; api::forget(); });
        private::forget();
        sync::forget();
        for m in self.convs.peek().iter() {
            store::remove(&format!("chat-{}.json", m.id));
        }
        store::remove("chats.json");
        self.convs.set(vec![]);
        self.conv.set(Conv::new());
        self.balance.set(None);
        self.access.set(None);
        self.plan_defaults.set(vec![]);
        chat::PLAN_DEFAULT_DONE.store(false, std::sync::atomic::Ordering::SeqCst);
        self.top_ups.set(vec![]);
        self.top_up_seen.set(None);
        self.screen.set(Screen::Login);
    }

    /// Reloads the account's top-ups. The server credits them by itself; this says "$X added"
    /// when one has gone up since the last look, and refreshes the balance.
    fn refresh_top_ups(mut self) {
        static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if PLAY || !api::has_session() || BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        bg(async move {
            if let Ok(list) = wallet::top_ups().await && api::has_session() {
                let now: std::collections::BTreeMap<String, i64> = list.iter().map(|t| (t.order_id.clone(), t.credited_cents)).collect();
                let seen = self.top_up_seen.peek().clone();
                let added: i64 = seen.as_ref().map_or(0, |s| list.iter().map(|t| (t.credited_cents - s.get(&t.order_id).copied().unwrap_or(0)).max(0)).sum());
                let changed = seen.as_ref() != Some(&now);
                self.top_up_seen.set(Some(now));
                self.top_ups.set(list);
                if added > 0 {
                    self.say(format!("{} added to your balance", cents(added)))
                }
                if changed {
                    self.refresh_balance()
                }
            }
            BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }

    /// Polls open blind voucher orders, collects vouchers, and adds the ones that are due.
    fn work_wallet(mut self, force: bool) {
        static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        bg(async move {
            let mut w = Wallet::load();
            let r = wallet::tick(&mut w, force).await;
            self.wallet.set(w.clone());
            if let Err(e) = r && force {
                self.say(e)
            }
            if *self.screen.peek() != Screen::Login && *self.screen.peek() != Screen::Signup && w.vouchers.iter().any(|v| !v.invalid && v.due <= store::now()) {
                match wallet::redeem(&mut w, false).await {
                    Ok(c) if c > 0.0 => { self.say(format!("{} added to your balance", usd(c))); self.refresh_balance(); }
                    Err(e) if force => self.say(e),
                    _ => {}
                }
                self.wallet.set(w);
            }
            BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

fn main() {
    // "Always connect through Tor": Tor starts before anything can make a request.
    if tor::always() {
        tor::start()
    }
    dioxus::launch(Root)
}

/// Reads a picked file off the UI thread (a long PDF takes a while) and adds it to the next message.
fn read_doc(app: App, name: String, b64: String) {
    use base64::Engine;
    app.say(format!("Reading {name}…"));
    let (tx, rx) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let r = match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(bytes) => files::read(&name, &bytes),
            Err(_) => Err(format!("Couldn't read {name}.")),
        };
        let _ = tx.send(r);
    });
    bg(async move {
        let mut app = app;
        match rx.await {
            Ok(Ok(d)) => {
                app.say(format!("{} added ({})", d.name, files::size(&d)));
                let mut docs = app.docs.write();
                if docs.len() < 4 { docs.push(d) }
            }
            Ok(Err(e)) => app.say(e),
            Err(_) => app.say("Couldn't read the file."),
        }
    });
}

#[component]
fn Root() -> Element {
    store::remove("account.json"); // 0.2.0 kept the number; the app only keeps the session now
    // Private sessions: close idle ones and keep the wallet topped up while the app runs.
    use_hook(|| bg(async {
        loop {
            private::tick().await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }));
    let app = use_context_provider(|| {
        let convs: Vec<Meta> = store::load("chats.json");
        let settings: Settings = store::load("settings.json");
        let conv = convs.first().map_or_else(Conv::new, |m| Conv::load(&m.id));
        App {
            screen: Signal::new(if api::restore() { Screen::Chat } else { Screen::Login }),
            overlay: Signal::new(Overlay::None),
            settings: Signal::new(settings),
            models: Signal::new(vec![]),
            frontier: Signal::new(vec![]),
            uncensored: Signal::new(vec![]),
            access: Signal::new(None),
            plan_defaults: Signal::new(vec![]),
            convs: Signal::new(convs),
            conv: Signal::new(conv),
            live: Signal::new(None),
            stop: Signal::new(false),
            waiting: Signal::new(false),
            attach: Signal::new(vec![]),
            docs: Signal::new(vec![]),
            wallet: Signal::new(Wallet::load()),
            top_ups: Signal::new(vec![]),
            top_up_seen: Signal::new(None),
            balance: Signal::new(None),
            toast: Signal::new(None),
            visible: Signal::new(true),
            code: Signal::new(code::Config::default()),
        }
    });

    use_hook(move || if tor::wanted() { chat::watch_tor(app) });
    use_future(move || async move {
        let mut app = app;
        if *app.screen.peek() == Screen::Chat {
            app.refresh_balance();
            app.refresh_top_ups()
        } else {
            code::load(app).await
        }
        if let Ok(v) = api::public("GET", "/models", None).await {
            // Decision models answer typed questions through the API; they can't chat.
            let mut models: Vec<Model> = v["data"].as_array().into_iter().flatten().filter(|m| m["decisions"] != true).map(|m| Model {
                id: m["id"].as_str().unwrap_or("").into(),
                name: m["name"].as_str().or(m["id"].as_str()).unwrap_or("").into(),
                prompt: m["pricing"]["prompt"].as_f64().unwrap_or(0.0),
                completion: m["pricing"]["completion"].as_f64().unwrap_or(0.0),
                tools: m["tools"].as_bool().unwrap_or(true),
                context: m["context_length"].as_u64().unwrap_or(0),
                // Specialist use cases (Writing, Security, ...) the server tags models with.
                categories: m["categories"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect(),
                // Takes end-to-end encrypted requests (Encrypted).
                e2ee: m["e2ee"] == true,
                // When it was first listed; the picker shows the newest first.
                created: m["created"].as_i64().unwrap_or(0),
            }).collect();
            models.sort_by_key(|m| (std::cmp::Reverse(m.created), m.name.to_lowercase()));
            models.retain(|m| chat::offered(m, &[]));
            app.models.set(models);
            chat::open_on_plan_default(app);
        }
        // An answer the app was writing when it closed: the server finished it; show it.
        if *app.screen.peek() == Screen::Chat {
            code::load(app).await;
            chat::finish_pending(app);
        }
        let list = |v: &Value, k: &str| v[k].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect::<Vec<_>>();
        if let Ok(v) = api::public("GET", "/frontier_models", None).await { app.frontier.set(list(&v, "frontier_models")) }
        if let Ok(v) = api::public("GET", "/uncensored_models", None).await {
            let uncensored = list(&v, "uncensored_models");
            if PLAY { app.models.write().retain(|m| chat::offered(m, &uncensored)) }
            app.uncensored.set(uncensored)
        }
    });

    // A session the server has ended sends you back to sign in, as on the web.
    use_future(move || async move {
        let mut app = app;
        loop {
            if api::EXPIRED.swap(false, std::sync::atomic::Ordering::SeqCst) && !matches!(*app.screen.peek(), Screen::Login | Screen::Signup) {
                app.overlay.set(Overlay::None);
                app.balance.set(None);
                app.screen.set(Screen::Login);
                app.say("Your session ended. Sign in again.");
            }
            sleep(1000).await;
        }
    });

    // Chat sync: a round now, then every 30 s while the app is on screen (and 1.5 s after a change).
    use_future(move || async move {
        loop {
            if *app.visible.peek() && !matches!(*app.screen.peek(), Screen::Login | Screen::Signup) {
                sync::tick(app).await;
            }
            sleep(30_000).await;
        }
    });

    // Blind vouchers: poll every 10 s while the app is on screen and there's something to do.
    // Top-ups: every 10 s on Add funds, once a minute elsewhere while one is still open.
    use_future(move || async move {
        let mut last_top_ups = store::now();
        loop {
            let w = app.wallet.peek().clone();
            if *app.visible.peek() && (w.active() || w.vouchers.iter().any(|v| !v.invalid && v.due <= store::now())) {
                app.work_wallet(false);
            }
            let screen = *app.screen.peek();
            let every = if screen == Screen::Funds { 10_000 } else if app.top_ups.peek().iter().any(|t| t.open) { 60_000 } else { i64::MAX };
            if *app.visible.peek() && !matches!(screen, Screen::Login | Screen::Signup) && store::now() - last_top_ups >= every {
                last_top_ups = store::now();
                app.refresh_top_ups();
            }
            sleep(10_000).await;
        }
    });

    use_future(move || async move {
        let mut app = app;
        let mut e = document::eval(BRIDGE);
        while let Ok(v) = e.recv::<Value>().await {
            if let Some(s) = v["sandbox"].as_str() {
                code::on_sandbox(s)
            } else if let Some(url) = v["link"].as_str() {
                open_url(url)
            } else if v["back"] == true {
                if *app.overlay.peek() != Overlay::None {
                    app.overlay.set(Overlay::None)
                } else {
                    let s = *app.screen.peek();
                    app.screen.set(match s { Screen::Funds => Screen::Account, Screen::Signup => Screen::Login, _ => Screen::Chat })
                }
            } else if v["online"] == true {
                api::kick();
                app.work_wallet(false)
            } else if let Some(vis) = v["visible"].as_bool() {
                app.visible.set(vis);
                if vis {
                    api::kick();
                    app.work_wallet(false);
                    bg(sync::tick(app));
                    if !matches!(*app.screen.peek(), Screen::Login | Screen::Signup) { app.refresh_top_ups() }
                }
            } else if let Some(e) = v["doc_error"].as_str() {
                app.say(e)
            } else if let Some(name) = v["doc"]["name"].as_str() {
                read_doc(app, name.to_string(), v["doc"]["data"].as_str().unwrap_or("").to_string())
            } else if let Some(imgs) = v["images"].as_array() {
                let mut a = app.attach.write();
                a.extend(imgs.iter().filter_map(|i| i.as_str().map(String::from)));
                a.truncate(4);
            }
        }
    });

    let screen = *app.screen.read();
    rsx! {
        document::Style { {CSS} }
        match screen {
            Screen::Login => rsx! { Login {} },
            Screen::Signup => rsx! { Signup {} },
            Screen::Chat => rsx! { chat::ChatView {} },
            Screen::Account => rsx! { Account {} },
            // The Play build never shows a way to pay (Play Billing rules): Funds is Account there.
            Screen::Funds if !PLAY => rsx! { Funds {} },
            Screen::Funds => rsx! { Account {} },
        }
        if api::has_session() { sync::JoinAsk {} }
        if let Some(t) = app.toast.read().clone() { div { class: "toast", "{t}" } }
    }
}

// ---- sign in and sign up: the web's flow ----

/// 12 seed words as a numbered grid (a number as it is).
#[component]
fn Words(text: String) -> Element {
    if !text.chars().any(|c| c.is_ascii_alphabetic()) {
        return rsx! { div { class: "number mono", "{text}" } };
    }
    rsx! {
        div { class: "words mono",
            for (i, w) in text.split_whitespace().enumerate() {
                span { key: "{i}", em { "{i + 1}." } " {w}" }
            }
        }
    }
}

#[component]
fn Login() -> Element {
    let mut app = use_context::<App>();
    let mut number = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let mut sign_in = move |_: ()| {
        let n = number.peek().trim().to_string();
        if crate::private::kind_of(&n).is_none() {
            error.set(Some(if n.chars().any(|c| c.is_ascii_alphabetic()) {
                "Those aren't 12 seed words. Check each word and its order: one is misspelled, missing or in the wrong place."
            } else {
                "Enter your 12 seed words, or your 16-digit account number."
            }.into()));
            return;
        }
        busy.set(true);
        error.set(None);
        bg(async move {
            match api::login(&n).await {
                Ok(()) => { js("document.querySelector('.login input').value = ''"); app.signed_in() }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };
    rsx! {
        div { class: "login",
            h1 { "Sign In" }
            label { class: "field-label", r#for: "login-words", "Seed words or account number" }
            input { r#type: "text", id: "login-words", autocomplete: "off", autocapitalize: "none", spellcheck: "false", class: "mono",
                oninput: move |e| number.set(e.value()),
                onkeydown: move |e| if e.key() == Key::Enter { sign_in(()) } }
            button { class: "go", disabled: *busy.read(), onclick: move |_| sign_in(()), if *busy.read() { "Signing in…" } else { "Sign In" } }
            if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
            p { class: "muted center", style: "margin-top: 16px",
                "Don't have an account? "
                a { onclick: move |_| app.screen.set(Screen::Signup), "Create one" }
            }
            button { class: "bare guest", onclick: move |_| { app.screen.set(Screen::Chat); bg(code::load(app)) }, "Chat as a guest" }
            p { class: "muted center", "No account, a free model, up to 30 messages an hour." }
        }
    }
}

#[component]
fn Signup() -> Element {
    let mut app = use_context::<App>();
    let mut tos = use_signal(|| false);
    let mut saved = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut created = use_signal(|| None::<String>);
    let mut credit = use_signal(|| None::<f64>);
    let mut referral = use_signal(String::new);

    // The words are shown once. We only keep a scrambled copy and can't show them again.
    if let Some(n) = created.read().clone() {
        return rsx! {
            div { class: "login center",
                h1 { "Your seed words" }
                p { class: "muted", "Write them down in order. Lose them and the account is gone." }
                if let Some(c) = *credit.read() { p { class: "ok", "{usd(c)} of starter credit is on your balance." } }
                Words { text: n.clone() }
                button { onclick: { let n = n.clone(); move |_| app.copy(&n, "Seed words") }, "Copy" }
                label { class: "check",
                    input { r#type: "checkbox", onchange: move |e| saved.set(e.checked()) }
                    "I have saved my 12 words"
                }
                button { class: "go", disabled: !*saved.read(), onclick: move |_| { created.set(None); app.signed_in() }, "Start Chatting" }
            }
        };
    }

    let create = move |_| {
        if !*tos.peek() {
            error.set(Some("You must accept the Terms of Service to create an account.".into()));
            return;
        }
        busy.set(true);
        error.set(None);
        bg(async move {
            let code = referral.peek().clone();
            match api::create_account(&code).await {
                Ok((n, c)) => { credit.set(c); created.set(Some(n)) }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };
    rsx! {
        div { class: "login", "data-back": "1",
            h1 { "Create Account" }
            p { class: "muted", "No email, no username, no password" }
            div { class: "info",
                h4 { "How it works" }
                p { "You get 12 seed words, made on this phone. They're your only way to sign in, and we never see them." }
            }
            label { class: "check",
                input { r#type: "checkbox", onchange: move |e| tos.set(e.checked()) }
                "I'm 18 or older and agree to the "
                a { href: "{api::BASE}/terms", "Terms of Service" }
            }
            if !PLAY { input { class: "field", autocomplete: "off", placeholder: "Referral code (optional)", oninput: move |e| referral.set(e.value()) } }
            button { class: "go", disabled: *busy.read(), onclick: create, if *busy.read() { "Creating…" } else { "Create Account" } }
            if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
            p { class: "muted center", style: "margin-top: 16px",
                "Already have an account? "
                a { onclick: move |_| app.screen.set(Screen::Login), "Sign in" }
            }
        }
    }
}

// ---- account ----

#[component]
fn Account() -> Element {
    let app = use_context::<App>();
    let mut tier = use_signal(|| Value::Null);
    let mut tiers = use_signal(|| Value::Null);
    let mut referral = use_signal(|| Value::Null);
    let mut tx_rev = use_signal(|| 0u32);
    // What this phone's private chats cost (the server can't list them).
    let mut mine = use_signal(|| Value::Null);
    let mut refill = use_signal(|| Value::Null);
    let mut key = use_signal(|| None::<String>);
    let mut new_number = use_signal(|| None::<String>);
    let mut words_saved = use_signal(|| false);
    let mut switched = use_signal(|| false);
    let mut switching = use_signal(|| false);
    let mut confirm = use_signal(|| None::<&'static str>);
    let mut plan_ask = use_signal(|| None::<String>);
    let mut deleting = use_signal(|| false);

    let load = move || {
        app.refresh_balance();
        bg(async move {
            if let Ok(v) = api::get("/account_tier").await { tier.set(v) }
            if !PLAY {
                if let Ok(v) = api::public("GET", "/available_tiers", None).await { tiers.set(v) }
                if let Ok(v) = api::get("/referral_info").await { referral.set(v) }
            }
            mine.set(private::spending().await);
            tx_rev += 1;
            if let Ok(v) = api::get("/auto_refill_status").await { refill.set(v) }
        });
    };
    use_hook(load);

    let change_tier = move |t: String| {
        bg(async move {
            match api::post("/change_tier", json!({"tier": t, "payment_method": "balance"})).await {
                Ok(v) => { app.say(v["message"].as_str().unwrap_or("Plan changed").to_string()); load() }
                Err(e) => app.say(e),
            }
        })
    };

    let t = tier.read().clone();
    let current = t["tier"].as_str().unwrap_or("").to_string();
    let w = app.wallet.read().clone();

    rsx! {
        div { class: "screen", "data-back": "1",
            div { class: "bar",
                button { class: "icon", onclick: move |_| { let mut s = app.screen; s.set(Screen::Chat) }, "←" }
                div { class: "title", "Account" }
                div { style: "width: 46px" }
            }
            div { class: "scroll",
                div { class: "section",
                    h2 { "Balance" }
                    div { class: "row",
                        div { class: "grow big", {(*app.balance.read()).map(usd).unwrap_or("…".into())} }
                        if !PLAY { button { class: "go", onclick: move |_| { let mut s = app.screen; s.set(Screen::Funds) }, "Add funds" } }
                    }
                    if w.held_cents() > 0 && !PLAY {
                        p { class: "muted", "{cents(w.held_cents())} in blind vouchers is waiting to be added to your balance." }
                    }
                    // Play allows saying where to pay, as plain text: no link, no button.
                    if PLAY { p { class: "muted", "Add funds and change plans on the website, obscurify.ai." } }
                }
                div { class: "section",
                    h2 { "Plan" }
                    if !t.is_null() {
                        // The plan list below marks the current plan; the Play
                        // build has no list, so it names the plan here.
                        if PLAY { p { "{tier_name(&current)}" } }
                        if t["included_credit"].as_f64().unwrap_or(0.0) > 0.0 {
                            p { class: "muted", "{usd(t[\"credit_used\"].as_f64().unwrap_or(0.0))} of {usd(t[\"included_credit\"].as_f64().unwrap_or(0.0))} included usage used" }
                        }
                        if let Some(d) = t["next_charge_date"].as_str() { p { class: "muted", "Renews {d.get(..10).unwrap_or(d)} from your balance" } }
                        p { class: "muted", "Tokens this period: {t[\"input_tokens_used\"]} in, {t[\"output_tokens_used\"]} out" }
                    }
                    for x in if PLAY { vec![] } else { tiers.read()["tiers"].as_array().cloned().unwrap_or_default() } {
                        {
                            let id = x["tier"].as_str().unwrap_or("").to_string();
                            let on = id == current;
                            let fee = x["monthly_fee"].as_f64().unwrap_or(0.0);
                            let now_fee = tiers.read()["tiers"].as_array().into_iter().flatten().find(|y| y["tier"].as_str() == Some(current.as_str())).and_then(|y| y["monthly_fee"].as_f64()).unwrap_or(0.0);
                            let up = fee > now_fee;
                            let bal = app.balance.read().unwrap_or(0.0);
                            let asking = plan_ask.read().as_deref() == Some(id.as_str());
                            let name = tier_name(&id);
                            let pay = id.clone();
                            let credit = x["included_credit"].as_f64().unwrap_or(0.0);
                            let what = match id.as_str() {
                                "casual" => format!("Frontier models, {} of usage included", usd(credit)),
                                "pro" => format!("API access, frontier and uncensored models, {} of usage included", usd(credit)),
                                _ => "Pay only for what you use".to_string(),
                            };
                            rsx! {
                                div { class: "row",
                                    div { class: "grow",
                                        div { class: if on { "ok" } else { "" }, "{tier_name(&id)} · {usd(fee)}/mo" }
                                        div { class: "muted", "{what}" }
                                    }
                                    if on { span { class: "ok", "Current" } }
                                    else { button { onclick: { let i = id.clone(); move |_| plan_ask.set(Some(i.clone())) }, if up { "Upgrade" } else { "Switch" } } }
                                }
                                if asking && up && bal < fee {
                                    p { class: "muted", "{name} costs {usd(fee)} a month and your balance is {usd(bal)}." }
                                    div { class: "row", style: "border: 0",
                                        button { class: "go", onclick: move |_| { plan_ask.set(None); let mut s = app.screen; s.set(Screen::Funds) }, "Add funds" }
                                        button { class: "bare", onclick: move |_| plan_ask.set(None), "Cancel" }
                                    }
                                } else if asking {
                                    p { class: "muted", if up { "Pay {usd(fee)} from your balance ({usd(bal)}) for {name}? It renews monthly while your balance covers it." } else { "Switch to {name}? Your current plan ends now." } }
                                    div { class: "row", style: "border: 0",
                                        button { class: "go", onclick: move |_| { plan_ask.set(None); change_tier(pay.clone()) }, if up { "Pay {usd(fee)}" } else { "Switch" } }
                                        button { class: "bare", onclick: move |_| plan_ask.set(None), "Cancel" }
                                    }
                                }
                            }
                        }
                    }
                    if !PLAY { p { class: "muted", style: "margin-top: 8px", "Plans are paid from your balance and renew monthly while it covers them." } }
                }
                // Seed words (or, for older accounts, a 16-digit number). New words are made on
                // the phone and shown first; only once they're saved does the account switch to
                // them (the server gets only their login key).
                div { class: "section",
                    h2 { if crate::private::words() { "Seed words" } else { "Account number" } }
                    if let Some(n) = new_number.read().clone() {
                        div { class: "info center", id: "new-words",
                            if *switched.read() {
                                p { style: "color: #fff; font-weight: 600", "Your seed words" }
                                p { class: "muted", "Done. Sign in with these seed words from now on; the old number or words and your other sessions no longer work." }
                            } else {
                                p { style: "color: #fff; font-weight: 600", "Your new seed words" }
                                p { class: "muted", if crate::private::words() { "Write these down in order. They replace your current seed words when you press Switch." } else { "Write these down in order. They replace your account number when you press Convert." } }
                            }
                            Words { text: n.clone() }
                            if *switched.read() {
                                div { class: "row", style: "justify-content: center; border: 0",
                                    button { onclick: { let n = n.clone(); move |_| app.copy(&n, "Seed words") }, "Copy" }
                                    button { class: "go", onclick: move |_| { new_number.set(None); switched.set(false) }, "I've Saved It" }
                                }
                            } else {
                                label { class: "check", id: "words-saved",
                                    input { r#type: "checkbox", checked: *words_saved.read(), onchange: move |e| words_saved.set(e.checked()) }
                                    "I've written down all 12 words, in order"
                                }
                                div { class: "row end", style: "border: 0",
                                    button { class: "bare", onclick: move |_| new_number.set(None), "Cancel" }
                                    button { onclick: { let n = n.clone(); move |_| app.copy(&n, "Seed words") }, "Copy" }
                                    button { class: "go", id: "switch-words", disabled: !*words_saved.read() || *switching.read(), onclick: {
                                        let n = n.clone();
                                        move |_| {
                                            let n = n.clone();
                                            switching.set(true);
                                            bg(async move {
                                                match api::switch_to_words(&n).await {
                                                    Ok(seed) => {
                                                        // Synced chats move to the new words; an account that had a
                                                        // number can sync from now on (sync.rs).
                                                        sync::move_to(app, seed).await;
                                                        crate::private::set_kind("words");
                                                        *sync::REFRESH.write() += 1;
                                                        if sync::on() { sync::tick(app).await }
                                                        switched.set(true);
                                                    }
                                                    Err(e) => app.say(e),
                                                }
                                                switching.set(false);
                                            })
                                        }
                                    }, match (*switching.read(), crate::private::words()) { (true, true) => "Switching…", (true, false) => "Converting…", (false, true) => "Switch", (false, false) => "Convert" } }
                                }
                            }
                        }
                    }
                    div { class: "row",
                        div { class: "grow muted", if crate::private::words() {
                            "Hidden. We only store a scrambled copy, so we can't show them again."
                        } else {
                            "To enable encrypted sync, convert your account number to seed words."
                        } }
                        if new_number.read().is_none() {
                            button { id: "rotate-words", onclick: move |_| {
                                words_saved.set(false);
                                switched.set(false);
                                new_number.set(Some(crate::private::new_words()));
                            }, if crate::private::words() { "New Seed Words" } else { "Generate My Seed Words" } }
                        }
                    }
                }
                sync::SyncSection {}
                div { class: "section",
                    h2 { "API key" }
                    if let Some(k) = key.read().clone() {
                        div { class: "row",
                            div { class: "grow mono", style: "font-size: 13px", "{k}" }
                            button { class: "bare", onclick: { let k = k.clone(); move |_| app.copy(&k, "API key") }, "Copy" }
                        }
                    } else {
                        button { onclick: move |_| bg(async move {
                            match api::post("/tokens", json!({})).await { Ok(v) => key.set(v["token"].as_str().map(String::from)), Err(e) => app.say(e) }
                        }), "Show API key" }
                    }
                    p { class: "muted", style: "margin-top: 8px",
                        "Requests with this key are tied to your account. To make API and agent requests unlinkable, like the chat, run the private-sessions proxy and point your tools at it. "
                        a { onclick: move |_| open_url(&format!("{}/tutorials/private_sessions", api::BASE)), "Set it up" }
                    }
                    if *confirm.read() == Some("key") {
                        div { class: "row",
                            button { class: "danger", onclick: move |_| { confirm.set(None); bg(async move {
                                match api::post("/tokens/rotate", json!({})).await { Ok(v) => { key.set(v["token"].as_str().map(String::from)); app.say("New key made; the old one stopped working") } Err(e) => app.say(e) }
                            }) }, "Replace key" }
                            button { class: "bare", onclick: move |_| confirm.set(None), "Cancel" }
                        }
                    } else {
                        button { class: "bare", style: "padding-left: 0", onclick: move |_| confirm.set(Some("key")), "Replace key…" }
                    }
                }
                if let Some(link) = referral.read()["referral_link"].as_str().filter(|_| !PLAY).map(String::from) {
                    div { class: "section",
                        h2 { "Referrals" }
                        div { class: "row",
                            div { class: "grow mono", style: "font-size: 13px", "{link}" }
                            button { class: "bare", onclick: { let l = link.clone(); move |_| app.copy(&l, "Link") }, "Copy" }
                        }
                        if referral.read()["has_pending_bonus"] == true { p { class: "ok", "Your referral bonus is waiting: it's added once you've deposited ${referral.read()[\"deposit_threshold\"].as_str().unwrap_or(\"\")}." } }
                        p { class: "muted", "{referral.read()[\"referral_count\"]} joined · {referral.read()[\"pending_referrals\"]} pending. You both get ${referral.read()[\"bonus_amount\"].as_str().unwrap_or(\"\")} once they add ${referral.read()[\"deposit_threshold\"].as_str().unwrap_or(\"\")}." }
                    }
                }
                if refill.read()["active"] == true {
                    div { class: "section",
                        h2 { "Auto-refill" }
                        div { class: "row",
                            div { class: "grow muted", "Your old card refill is on." }
                            button { onclick: move |_| bg(async move {
                                match api::post("/disable_auto_refill", json!({})).await { Ok(_) => { refill.set(json!({"active": false})); app.say("Auto-refill turned off") } Err(e) => app.say(e) }
                            }), "Turn off" }
                        }
                    }
                }
                History { rev: tx_rev, mine }
                div { class: "section",
                    if *confirm.read() == Some("out") {
                        p { class: "muted", "Chats on this phone are deleted. Make sure you have your seed words (or number)." }
                        if w.held_cents() > 0 { p { class: "muted", "{cents(w.held_cents())} in blind vouchers stays on this phone and goes to the next account that signs in." } }
                        div { class: "row",
                            button { class: "danger", onclick: move |_| app.sign_out(), "Sign out" }
                            button { class: "bare", onclick: move |_| confirm.set(None), "Cancel" }
                        }
                    } else {
                        button { class: "danger", onclick: move |_| confirm.set(Some("out")), "Sign out" }
                    }
                }
                div { class: "section",
                    h2 { "Delete account" }
                    if *confirm.read() == Some("delete") {
                        p { class: "muted", "Your seed words or number, API key and sign-ins are erased for good, and the balance and plan are lost. Payment records stay, with nothing that links them to you. Chats on this phone and their synced copy are deleted." }
                        div { class: "row",
                            button { class: "danger", disabled: *deleting.read(), onclick: move |_| {
                                deleting.set(true);
                                bg(async move {
                                    match api::post("/account/delete", json!({"confirm": true})).await {
                                        Ok(_) => {
                                            // The synced chats aren't linked to the account on the server, so the phone deletes them.
                                            if sync::on() { sync::set_on(app, false, true).await; }
                                            sync::reset_switch();
                                            app.sign_out();
                                            app.say("Your account is deleted")
                                        }
                                        Err(e) => app.say(e),
                                    }
                                    deleting.set(false);
                                })
                            }, if *deleting.read() { "Deleting…" } else { "Delete for good" } }
                            button { class: "bare", onclick: move |_| confirm.set(None), "Cancel" }
                        }
                    } else {
                        button { class: "danger", onclick: move |_| confirm.set(Some("delete")), "Delete account…" }
                    }
                }
            }
        }
    }
}

/// Transaction history, like the web account page: spending per day, week or month (tap one for its
/// charges), or every charge. Paged on the server: heavy accounts have tens of thousands of charges.
#[component]
fn History(rev: ReadSignal<u32>, mine: ReadSignal<Value>) -> Element {
    const PAGE: i64 = 50;
    const PERIODS: i64 = 31;
    let mut view = use_signal(|| "day");
    let mut offset = use_signal(|| 0i64);
    let mut period = use_signal(|| None::<Value>);
    let mut p_offset = use_signal(|| 0i64);
    let mut opened = use_signal(String::new); // the charge whose details show
    let data = use_resource(move || {
        let (v, off, per, poff, _, mine) = (view(), offset(), period(), p_offset(), rev(), mine());
        async move {
            let q = |s: &str| s.replace('+', "%2B");
            // One row before the page too (when there is one): it bounds which of
            // this device's private chats belong on this page.
            let limit = |page: i64, o: i64| (page + (o > 0) as i64, (o - 1).max(0));
            let spent = private_spent(&mine);
            if per.is_some() || v == "all" {
                let o = if per.is_some() { poff } else { off };
                let (l, st) = limit(PAGE, o);
                let mut url = format!("/transactions?limit={l}&offset={st}");
                if let Some(p) = &per {
                    url += &format!("&from={}&to={}", q(p["from"].as_str().unwrap_or("")), q(p["to"].as_str().unwrap_or("")));
                }
                let mut d = api::get(&url).await?;
                merge_private_charges(&mut d, per.as_ref(), o, &spent);
                Ok::<Value, String>(d)
            } else {
                let tz = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
                let (l, st) = limit(PERIODS, off);
                let mut d = api::get(&format!("/transactions/summary?period={v}&tz={}&limit={l}&offset={st}", q(&tz))).await?;
                merge_private_periods(&mut d, v, off, &spent);
                Ok(d)
            }
        }
    });
    let n = |v: &Value| -> f64 { v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0) };
    let plural = |k: i64, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let label = move |ymd: &str| -> String {
        let Ok(d) = chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d") else { return ymd.to_string() };
        let this_year = d.format("%Y").to_string() == chrono::Local::now().format("%Y").to_string();
        match view() {
            "month" => d.format("%B %Y").to_string(),
            "week" => format!("Week of {}", d.format(if this_year { "%b %-d" } else { "%b %-d, %Y" })),
            _ => d.format(if this_year { "%a, %b %-d" } else { "%a, %b %-d, %Y" }).to_string(),
        }
    };
    let when = move |at: &str| -> String {
        let Ok(t) = chrono::DateTime::parse_from_rfc3339(at) else { return at.to_string() };
        let t = t.with_timezone(&chrono::Local);
        if period().is_some() && view() == "day" { return t.format("%-I:%M %p").to_string() }
        let this_year = t.format("%Y").to_string() == chrono::Local::now().format("%Y").to_string();
        t.format(if this_year { "%b %-d, %-I:%M %p" } else { "%b %-d %Y, %-I:%M %p" }).to_string()
    };
    let unit = match view() { "week" => "week", "month" => "month", _ => "day" };
    let result = data.read().clone();
    rsx! {
        div { class: "section history",
            h2 { "Transactions" }
            div { class: "seg",
                for (v, name) in [("day", "Daily"), ("week", "Weekly"), ("month", "Monthly"), ("all", "All")] {
                    button { class: if view() == v { "on" } else { "" },
                        onclick: move |_| { view.set(v); offset.set(0); period.set(None); p_offset.set(0) }, "{name}" }
                }
            }
            if let Some(p) = period() {
                {
                    let mut parts = vec![];
                    if spent_in(&p) != 0.0 { parts.push(format!("{} spent", usd(spent_in(&p)))) }
                    parts.extend(period_detail(&p));
                    if n(&p["added"]) != 0.0 { parts.push(format!("+{} added", usd(n(&p["added"])))) }
                    rsx! { div { class: "history-head",
                        div { class: "grow", b { {label(p["date"].as_str().unwrap_or(""))} } div { class: "muted", {parts.join(" · ")} } }
                    } }
                }
            }
            match result {
                None => rsx! { p { class: "muted", "Loading…" } },
                Some(Err(e)) => rsx! { p { class: "err", "{e}" } },
                Some(Ok(d)) if period().is_none() && view() != "all" => {
                    let periods = d["periods"].as_array().cloned().unwrap_or_default();
                    let has_more = d["has_more"].as_bool().unwrap_or(false);
                    let max = periods.iter().map(spent_in).fold(0.0, f64::max);
                    rsx! {
                        if periods.is_empty() { p { class: "muted", if offset() > 0 { "No older activity" } else { "None yet" } } }
                        for p in periods {
                            {
                                let (spent, added) = (spent_in(&p), n(&p["added"]));
                                let detail = period_detail(&p);
                                let width = if spent > 0.0 && max > 0.0 { (spent / max * 100.0).round().max(1.0) } else { 0.0 };
                                let pick = p.clone();
                                rsx! {
                                    div { class: "row tap", onclick: move |_| { period.set(Some(pick.clone())); p_offset.set(0) },
                                        div { class: "grow",
                                            div { {label(p["date"].as_str().unwrap_or(""))} }
                                            if !detail.is_empty() { div { class: "muted", {detail.join(" · ")} } }
                                            if width > 0.0 { div { class: "meter", div { style: "width:{width}%" } } }
                                        }
                                        div { class: "amounts",
                                            div { if spent != 0.0 { "{usd(spent)}" } else { span { class: "muted", "—" } } }
                                            if added != 0.0 { div { class: "ok", "+{usd(added)}" } }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "pager",
                            span { class: "muted grow", "Tap a {unit} to see its charges" }
                            button { disabled: offset() == 0, onclick: move |_| offset.set((offset() - PERIODS).max(0)), "Newer" }
                            button { disabled: !has_more, onclick: move |_| offset.set(offset() + PERIODS), "Older" }
                        }
                    }
                }
                Some(Ok(d)) => {
                    let rows = d["transactions"].as_array().cloned().unwrap_or_default();
                    let total = d["total"].as_i64().unwrap_or(0);
                    let server_rows = d["server_rows"].as_i64().unwrap_or(rows.len() as i64);
                    let has_private = server_rows < rows.len() as i64;
                    let off = if period().is_some() { p_offset() } else { offset() };
                    let mut page = move |o: i64| if period().is_some() { p_offset.set(o) } else { offset.set(o) };
                    rsx! {
                        if rows.is_empty() { p { class: "muted", "None yet" } }
                        for x in rows.iter() {
                            {
                                let a = n(&x["amount"]);
                                let name = x["label"].as_str().map(str::to_string)
                                    .unwrap_or_else(|| capitalize(&x["transaction_type"].as_str().unwrap_or("").replace('_', " ")));
                                // A deposit's payment method shows when it's tapped.
                                let paid = x["detail"].as_str().map(|d| format!("Paid with {}", match d { "Card" => "card", "Blind voucher" => "a blind voucher", o => o }));
                                let id = x["id"].as_str().unwrap_or("").to_string();
                                let open_this = paid.is_some() && opened() == id;
                                rsx! {
                                    div { class: if paid.is_some() { "row tap" } else { "row" },
                                        onclick: move |_| if paid.is_some() { opened.set(if opened() == id { String::new() } else { id.clone() }) },
                                        div { class: "grow", div { "{name}" } div { class: "muted", {when(x["created_at"].as_str().unwrap_or(""))} }
                                            if open_this { div { class: "muted detail", {x["detail"].as_str().map(|d| format!("Paid with {}", match d { "Card" => "card", "Blind voucher" => "a blind voucher", o => o })).unwrap_or_default()} } }
                                        }
                                        span { class: if a > 0.0 { "ok" } else if a < 0.0 { "debit" } else { "" },
                                            {format!("{}{}", if a < 0.0 { "−" } else if a > 0.0 { "+" } else { "" }, usd(a.abs()))} }
                                    }
                                }
                            }
                        }
                        div { class: "pager",
                            span { class: "muted grow",
                                if has_private { {plural(rows.len() as i64, "charge")} }
                                else if total > 0 { "{off + 1}–{off + rows.len() as i64} of {total}" }
                            }
                            if period().is_some() { button { onclick: move |_| period.set(None), "Back" } }
                            button { disabled: off == 0, onclick: move |_| page((off - PAGE).max(0)), "Newer" }
                            button { disabled: off + server_rows >= total, onclick: move |_| page(off + PAGE), "Older" }
                        }
                    }
                }
            }
        }
    }
}

// ---- private chats in the history ----
// The server can't list them (they're paid from money this phone holds for them,
// which it can't tie to the account); this phone keeps what each one cost.

fn private_spent(mine: &Value) -> Vec<(i64, f64)> {
    mine["spent"].as_array().map(|a| a.iter().map(|e| (e["at"].as_i64().unwrap_or(0), e["millicents"].as_i64().unwrap_or(0) as f64 / 100_000.0)).collect()).unwrap_or_default()
}

fn num(v: &Value) -> f64 { v.as_str().and_then(|s| s.parse().ok()).or(v.as_f64()).unwrap_or(0.0) }

/// What a period cost: the account's charges plus this device's private chats.
fn spent_in(p: &Value) -> f64 { num(&p["spent"]) + p["private_usd"].as_f64().unwrap_or(0.0) }

fn period_detail(p: &Value) -> Vec<String> {
    let plural = |k: i64, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let mut d = vec![];
    let chats = p["private_chats"].as_i64().unwrap_or(0);
    if chats > 0 { d.push(plural(chats, "private chat")) }
    let reqs = p["requests"].as_i64().unwrap_or(0);
    if reqs > 0 { d.push(plural(reqs, "request")) }
    if num(&p["plan"]) != 0.0 { d.push(format!("plan {}", usd(num(&p["plan"])))) }
    d
}

/// The local period (day, week from Monday, month) holding Unix time `at`: (start date, from, to).
fn local_period(view: &str, at: i64) -> Option<(String, String, String)> {
    use chrono::{Datelike, Duration, Months, TimeZone};
    let day = chrono::Local.timestamp_opt(at, 0).single()?.date_naive();
    let start = match view {
        "week" => day - Duration::days(day.weekday().num_days_from_monday() as i64),
        "month" => day.with_day(1)?,
        _ => day,
    };
    let end = match view { "week" => start + Duration::days(7), "month" => start.checked_add_months(Months::new(1))?, _ => start + Duration::days(1) };
    let at_midnight = |d: chrono::NaiveDate| chrono::Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.to_rfc3339());
    Some((start.format("%Y-%m-%d").to_string(), at_midnight(start)?, at_midnight(end)?))
}

/// Adds this device's private chats to a page of `/transactions/summary` (fetched with one extra
/// row before the page when `off > 0`).
fn merge_private_periods(d: &mut Value, view: &str, off: i64, spent: &[(i64, f64)]) {
    let has_more = d["has_more"].as_bool().unwrap_or(false);
    let mut periods = d["periods"].as_array().cloned().unwrap_or_default();
    let upper = if off > 0 { Some(if periods.is_empty() { String::new() } else { periods.remove(0)["date"].as_str().unwrap_or("").to_string() }) } else { None };
    let lower = if has_more { periods.last().and_then(|p| p["date"].as_str()).map(str::to_string) } else { None };
    let mut mine: std::collections::BTreeMap<String, (String, String, f64, i64)> = Default::default();
    for &(at, usd) in spent {
        if let Some((date, from, to)) = local_period(view, at) {
            let e = mine.entry(date).or_insert((from, to, 0.0, 0));
            e.2 += usd;
            e.3 += 1;
        }
    }
    for (date, (from, to, usd, chats)) in mine {
        if upper.as_ref().is_some_and(|u| date >= *u) || lower.as_ref().is_some_and(|l| date < *l) { continue }
        let i = match periods.iter().position(|p| p["date"] == date.as_str()) {
            Some(i) => i,
            None => {
                periods.push(json!({"date": date, "from": from, "to": to, "spent": "0", "added": "0", "requests": 0, "plan": "0", "wallet": "0"}));
                periods.len() - 1
            }
        };
        periods[i]["private_usd"] = json!(usd);
        periods[i]["private_chats"] = json!(chats);
    }
    periods.sort_by(|a, b| b["date"].as_str().cmp(&a["date"].as_str()));
    d["periods"] = Value::Array(periods);
}

/// Adds this device's private chats to a page of `/transactions` (fetched with one extra row
/// before the page when `off > 0`), as "Private chat" charges.
fn merge_private_charges(d: &mut Value, period: Option<&Value>, off: i64, spent: &[(i64, f64)]) {
    let total = d["total"].as_i64().unwrap_or(0);
    let mut rows = d["transactions"].as_array().cloned().unwrap_or_default();
    let ts = |v: &Value| chrono::DateTime::parse_from_rfc3339(v.as_str().unwrap_or("")).map(|t| t.timestamp()).ok();
    let upper = if off > 0 { if rows.is_empty() { i64::MIN } else { ts(&rows.remove(0)["created_at"]).unwrap_or(i64::MAX) } } else { i64::MAX };
    let lower = if off + (rows.len() as i64) < total { rows.last().and_then(|r| ts(&r["created_at"])).unwrap_or(i64::MIN) } else { i64::MIN };
    let from = period.and_then(|p| ts(&p["from"])).unwrap_or(i64::MIN).max(lower);
    let to = period.and_then(|p| ts(&p["to"])).unwrap_or(i64::MAX).min(upper);
    let server_rows = rows.len();
    for &(at, usd) in spent.iter().filter(|(at, _)| *at >= from && *at < to) {
        let when = chrono::DateTime::from_timestamp(at, 0).map(|t| t.to_rfc3339()).unwrap_or_default();
        rows.push(json!({"created_at": when, "label": "Private chat", "amount": format!("{}", -usd)}));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(ts(&r["created_at"]).unwrap_or(0)));
    d["transactions"] = Value::Array(rows);
    d["server_rows"] = json!(server_rows);
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn tier_name(t: &str) -> &'static str {
    match t { "casual" => "Casual", "pro" => "Pro", "pay_as_you_go" => "Pay as you go", _ => "…" }
}

// ---- add funds ----

// "EVM" opens the token and network choice below: ETH, USDC or USDT on one address
// (USDT on Tron gets a Tron address).
const METHODS: [(&str, &str); 5] = [("card", "Card"), ("BTC", "Bitcoin"), ("Lightning", "Lightning"), ("XMR", "Monero"), ("EVM", "USDT/ETH")];
const EVM_TOKENS: [&str; 3] = ["ETH", "USDC", "USDT"];
/// Mainnet networks and their tokens (no USDT on Base, no ETH on BNB Chain,
/// only USDT on Tron); the server has the final say.
const EVM_NETWORKS: [(&str, &str, &[&str]); 6] = [
    ("ethereum", "Ethereum", &["ETH", "USDC", "USDT"]),
    ("base", "Base", &["ETH", "USDC"]),
    ("arbitrum", "Arbitrum", &["ETH", "USDC", "USDT"]),
    ("optimism", "Optimism", &["ETH", "USDC", "USDT"]),
    ("bsc", "BNB Chain", &["USDC", "USDT"]),
    ("tron", "Tron", &["USDT"]),
];

/// "Base, Arbitrum and Optimism"
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn method_name(m: &str) -> String {
    if wallet::is_evm(m) { return m.to_string() }
    METHODS.iter().find(|x| x.0 == m).map(|x| x.1).unwrap_or("").to_string()
}

#[component]
fn Funds() -> Element {
    let app = use_context::<App>();
    let mut method = use_signal(|| "card");
    let mut token = use_signal(|| "ETH");
    let mut network = use_signal(|| "ethereum");
    let mut amount = use_signal(|| "20".to_string());
    // Top-up unless this is on. Not remembered: every visit starts with a top-up.
    let mut blind = use_signal(|| false);
    // Continue was tapped with Blind voucher on; waiting for the second tap.
    let mut asking = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut open = use_signal(|| None::<wallet::Order>); // order being paid
    let mut code = use_signal(String::new);
    let mut card_ready = use_signal(|| false);
    use_hook(move || app.refresh_top_ups());

    let pay = move |_| {
        let m = if *method.peek() == "EVM" { *token.peek() } else { *method.peek() };
        let net = wallet::is_evm(m).then(|| *network.peek());
        let dollars: Option<f64> = amount.peek().trim().trim_start_matches('$').parse().ok();
        let c = dollars.map(|d| (d * 100.0).round() as i64);
        if (m == "card" || m == "Lightning") && !matches!(c, Some(50..=1_000_000)) {
            error.set(Some("Enter an amount from $0.50 to $10,000.".into()));
            return;
        }
        let is_blind = *blind.peek();
        if is_blind && !*asking.peek() {
            asking.set(true);
            return;
        }
        asking.set(false);
        busy.set(true);
        error.set(None);
        bg(async move {
            let c = c.filter(|c| *c > 0);
            let r = if is_blind { wallet::create_order(m, c, net).await } else { wallet::create_top_up(m, c, net).await };
            match r {
                Ok(o) => {
                    if o.to_account {
                        app.refresh_top_ups();
                    } else {
                        let mut w = Wallet::load();
                        w.orders.push(o.clone());
                        w.save();
                        let mut ws = app.wallet;
                        ws.set(w);
                    }
                    card_ready.set(false);
                    open.set(Some(o.clone()));
                    if let (Some(pk), Some(cs)) = (o.publishable_key, o.client_secret) {
                        let r = document::eval(&format!(r#"
                            if (!window.Stripe) await new Promise((ok, no) => {{ const s = document.createElement('script'); s.src = 'https://js.stripe.com/v3/'; s.onload = ok; s.onerror = no; document.head.appendChild(s); }});
                            await new Promise(r => setTimeout(r, 50));
                            window.obsStripe = Stripe({pk:?});
                            window.obsElements = obsStripe.elements({{ clientSecret: {cs:?}, appearance: {{ theme: 'night', variables: {{ colorPrimary: '#00d97e', colorBackground: '#000000', colorText: '#e6e6e6', colorDanger: '#ff5454', borderRadius: '8px' }} }} }});
                            obsElements.create('payment', {{ wallets: {{ googlePay: 'never', applePay: 'never' }} }}).mount('#stripe');
                            return 1;"#)).join::<u8>().await;
                        match r { Ok(_) => card_ready.set(true), Err(_) => error.set(Some("The card form didn't load. Check your connection.".into())) }
                    }
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };

    let confirm_card = move |_| {
        busy.set(true);
        bg(async move {
            let r = document::eval("const r = await obsStripe.confirmPayment({ elements: obsElements, confirmParams: { return_url: location.href }, redirect: 'if_required' }); return r.error ? (r.error.message || 'Payment failed') : '';").join::<String>().await;
            match r {
                Ok(e) if e.is_empty() => {
                    let to_account = open.peek().as_ref().is_some_and(|o| o.to_account);
                    open.set(None);
                    if to_account {
                        app.say("Paid. It goes on your balance in a moment.");
                        app.refresh_top_ups()
                    } else {
                        app.say("Paid. Collecting your vouchers…");
                        app.work_wallet(true)
                    }
                }
                Ok(e) => error.set(Some(e)),
                Err(_) => error.set(Some("Payment failed".into())),
            }
            busy.set(false);
        });
    };

    let w = app.wallet.read().clone();
    let top_ups = app.top_ups.read().clone();
    let order = open.read().clone();
    let m = *method.read();
    let is_blind = *blind.read();

    rsx! {
        div { class: "screen", "data-back": "1",
            div { class: "bar",
                button { class: "icon", onclick: move |_| { let mut s = app.screen; s.set(Screen::Account) }, "←" }
                div { class: "title", "Add funds" }
                div { style: "width: 46px" }
            }
            div { class: "scroll",
                if let Some(o) = order.clone() {
                    div { class: "section",
                        if o.crypto() {
                            for (coin, addr) in o.addresses.clone() {
                                {
                                    // "0.00006075000" -> "0.00006075"
                                    let amt = o.amounts.get(&coin).map(|a| if a.contains('.') { a.trim_end_matches('0').trim_end_matches('.').to_string() } else { a.clone() });
                                    let evm = wallet::is_evm(&coin);
                                    let on = if evm { o.network_label().map(|l| format!(" on {l}")).unwrap_or_default() } else { String::new() };
                                    let also = if evm { o.also_on(&coin) } else { vec![] };
                                    // No common payment link for Tron: copy the address instead.
                                    let tron = o.network.as_deref() == Some("tron");
                                    let uri = match (coin.as_str(), &amt) {
                                        // The recipient only: a wallet picks the token, so no wallet can
                                        // mistake a token contract for where to send.
                                        _ if evm => match o.chain_id() { Some(id) => format!("ethereum:{addr}@{id}"), None => format!("ethereum:{addr}") },
                                        ("Lightning", _) => format!("lightning:{addr}"),
                                        ("XMR", Some(a)) => format!("monero:{addr}?tx_amount={a}"),
                                        ("XMR", None) => format!("monero:{addr}"),
                                        (_, Some(a)) => format!("bitcoin:{addr}?amount={a}"),
                                        _ => format!("bitcoin:{addr}"),
                                    };
                                    let unit = if coin == "Lightning" { "BTC" } else { coin.as_str() };
                                    let heading = match (o.amount_cents, &amt) {
                                        (Some(c), Some(a)) => format!("Send {a} {unit}{on} (about {})", cents(c)),
                                        (Some(c), None) => format!("Send {} in {coin}{on}", cents(c)),
                                        (None, _) => format!("Send any amount of {coin}{on}"),
                                    };
                                    let qr = qrcode::QrCode::new(if evm { addr.as_str() } else { uri.as_str() }).map(|q| q.render::<qrcode::render::svg::Color>().quiet_zone(false).build()).unwrap_or_default();
                                    rsx! {
                                        p { class: "lead", "{heading}" }
                                        div { class: "qr", dangerous_inner_html: "{qr}" }
                                        p { class: "mono", style: "font-size: 13px; margin: 12px 0", "{addr}" }
                                        if !also.is_empty() {
                                            p { class: "muted network-note", style: "margin: -4px 0 12px", {format!("Same address works on {}.", join_names(&also))} }
                                        }
                                        div { class: "row",
                                            if !tron {
                                                button { class: "go", onclick: { let u = uri.clone(); move |_| open_url(&u) }, "Open wallet" }
                                            }
                                            button { onclick: { let a = addr.clone(); move |_| app.copy(&a, "Address") }, "Copy" }
                                        }
                                    }
                                }
                            }
                            if o.to_account {
                                if let Some(t) = top_ups.iter().find(|t| t.order_id == o.id && t.credited_cents > 0) {
                                    p { class: "ok", style: "margin-top: 12px", "Added {cents(t.credited_cents)} to your balance" }
                                }
                                p { class: "muted", style: "margin-top: 12px", "Pay within 1 hour. It goes on your balance when the payment confirms. You can close the app." }
                            } else {
                                p { class: "muted", style: "margin-top: 12px", "Pay within 1 hour. Then open the app again so it can collect your blind vouchers and add them." }
                            }
                        } else {
                            div { id: "stripe", style: "min-height: 120px" }
                            button { class: "go", style: "width: 100%; margin-top: 16px", disabled: *busy.read() || !*card_ready.read(), onclick: confirm_card,
                                {format!("Pay {}", cents(o.amount_cents.unwrap_or(0)))} }
                        }
                        if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
                        button { class: "bare", style: "padding-left: 0; margin-top: 8px", onclick: move |_| open.set(None), "Done" }
                    }
                } else {
                    div { class: "section stack",
                        div { class: "seg",
                            for (id, label) in METHODS {
                                button { class: if m == id { "on" } else { "" }, onclick: move |_| method.set(id), "{label}" }
                            }
                        }
                        if m == "EVM" {
                            div { class: "toggles evm-tokens",
                                for t in EVM_TOKENS {
                                    button { class: if *token.read() == t { "chip on" } else { "chip" },
                                        onclick: move |_| {
                                            token.set(t);
                                            // USDT isn't on Base, ETH isn't on BNB Chain or Tron:
                                            // move to a network that has it.
                                            let has = |k: &str| EVM_NETWORKS.iter().any(|n| n.0 == k && n.2.contains(&t));
                                            if !has(*network.peek()) { network.set("ethereum") }
                                        },
                                        "{t}" }
                                }
                            }
                            div { class: "toggles evm-networks",
                                for (key, label, tokens) in EVM_NETWORKS {
                                    if tokens.contains(&*token.read()) {
                                        button { class: if *network.read() == key { "chip on" } else { "chip" }, onclick: move |_| network.set(key), "{label}" }
                                    }
                                }
                            }
                        }
                        div { class: "toggles",
                            for v in ["5", "10", "20", "50", "100"] {
                                button { class: if *amount.read() == v { "chip on" } else { "chip" }, onclick: move |_| amount.set(v.to_string()), "${v}" }
                            }
                        }
                        input { class: "field", inputmode: "decimal", value: "{amount}", placeholder: if m == "BTC" || m == "XMR" || m == "EVM" { "Any amount (USD, optional)" } else { "Amount (USD)" },
                            oninput: move |e| amount.set(e.value()) }
                        label { class: "check blind-toggle",
                            input { r#type: "checkbox", checked: is_blind, onchange: move |e| { blind.set(e.checked()); asking.set(false) } }
                            "Blind voucher"
                        }
                        if is_blind {
                            div { class: "warn", role: "alert",
                                p { strong { "Open the app again after paying." } }
                                p { "Uninstalling it before the credit is on your balance loses the money. "
                                    a { href: "#", onclick: move |e| { e.prevent_default(); open_url("https://obscurify.ai/articles/blind_payments") }, "How it works" } }
                            }
                        }
                        if *asking.read() {
                            p { class: "confirm", "Pay with a blind voucher? Open the app again after paying." }
                            div { class: "row", style: "border: 0; justify-content: flex-end; padding: 0",
                                button { class: "bare", onclick: move |_| asking.set(false), "Cancel" }
                                button { class: "go", disabled: *busy.read(), onclick: pay, "Yes, continue" }
                            }
                        } else {
                            button { class: "go", disabled: *busy.read(), onclick: pay, if *busy.read() { "…" } else { "Continue" } }
                        }
                        if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
                        if !is_blind {
                            p { class: "muted", "It goes on your balance when the payment confirms, even if you close the app." }
                        }
                    }
                }
                {
                    // Only payments still waiting; finished ones are deposits in the
                    // Account screen's transactions (tap one for how it was paid).
                    let orders: Vec<_> = w.orders.iter().rev().filter(|o| !o.done && !o.to_account).cloned().collect();
                    let waiting: Vec<_> = top_ups.iter().filter(|t| t.open && (t.credited_cents == 0 || t.amount_cents.is_some_and(|a| t.credited_cents < a))).cloned().collect();
                    let showing = order.as_ref().map(|o| o.id.clone());
                    rsx! {
                        if !orders.is_empty() || !waiting.is_empty() {
                            div { class: "section payments",
                                h2 { "In progress" }
                                for t in waiting {
                                    div { class: "row",
                                        div { class: "grow",
                                            div { {method_name(&t.payment_method)} {t.order().network_label().map(|l| format!(" on {l}")).unwrap_or_default()} " · " {t.amount_cents.map(cents).unwrap_or("any amount".into())} }
                                            div { class: if t.credited_cents > 0 { "ok" } else { "muted" },
                                                {if t.credited_cents > 0 { format!("Added {} to your balance", cents(t.credited_cents)) }
                                                 else if t.open { "Waiting for payment. You can close the app.".to_string() }
                                                 else { "No payment received".to_string() }}
                                            }
                                        }
                                        if t.open && t.crypto() && !t.order().addresses.is_empty() && showing.as_deref() != Some(&t.order_id) {
                                            button { class: "bare", onclick: { let o = t.order(); move |_| open.set(Some(o.clone())) }, "Show address" }
                                        }
                                    }
                                }
                                for o in orders {
                                    div { class: "row",
                                        div { class: "grow",
                                            div { "Blind voucher · " {method_name(&o.method)} {o.network_label().map(|l| format!(" on {l}")).unwrap_or_default()} " · " {o.amount_cents.map(cents).unwrap_or("any amount".into())} }
                                            div { class: "muted", {if o.funded_cents > 0 { format!("{} received. Keep the app open.", cents(o.funded_cents)) } else { "Waiting for payment. Open the app again after paying.".to_string() }} }
                                        }
                                        if o.crypto() && showing.as_deref() != Some(&o.id) {
                                            button { class: "bare", onclick: { let o = o.clone(); move |_| open.set(Some(o.clone())) }, "Show address" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if !w.vouchers.is_empty() {
                    div { class: "section",
                        h2 { "Vouchers" }
                        p { "{cents(w.held_cents())} blind voucher" }
                        p { class: "muted", "Added within the hour while the app is open." }
                        if w.vouchers.iter().any(|v| v.invalid) { p { class: "err", "Some vouchers were refused. Copy them and contact support." } }
                        div { class: "row",
                            button { class: "go", onclick: move |_| bg(async move {
                                let mut w = Wallet::load();
                                match wallet::redeem(&mut w, true).await { Ok(c) => { app.say(format!("{} added", usd(c))); app.refresh_balance() } Err(e) => app.say(e) }
                                let mut ws = app.wallet; ws.set(w);
                            }), "Add now" }
                            button { onclick: { let c = w.codes(); move |_| app.copy(&c, "Voucher codes") }, "Copy codes" }
                        }
                    }
                }
                div { class: "section stack",
                    h2 { "Redeem a voucher" }
                    textarea { class: "field", rows: 2, placeholder: "obv1.…", oninput: move |e| code.set(e.value()) }
                    button { onclick: move |_| {
                        let codes: Vec<String> = code.peek().split_whitespace().map(String::from).collect();
                        if codes.is_empty() { return }
                        bg(async move {
                            match api::post("/vouchers/redeem", json!({"vouchers": codes})).await {
                                Ok(v) => { app.say(format!("{} added", usd(v["credited_usd"].as_f64().unwrap_or(0.0)))); app.refresh_balance(); code.set(String::new()); js("document.querySelector('textarea.field').value = ''") }
                                Err(e) => app.say(e),
                            }
                        })
                    }, "Redeem" }
                }
            }
        }
    }
}
