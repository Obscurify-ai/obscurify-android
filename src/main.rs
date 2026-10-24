//! Obscurify for Android: a thin client of the Obscurify API. Sign in with the account number, chat
//! with any model (streaming, web search, images), keep chats on the phone, manage the plan and
//! API key, and add funds through unlinkable vouchers. No local models. The Android glue (Back,
//! opening links and wallets, copying) is the MainActivity that build.sh writes in.

mod api;
mod chat;
mod store;
mod wallet;

use chat::{Conv, Meta, Msg};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wallet::Wallet;

const CSS: &str = include_str!("../assets/style.css");
const FREE_MODEL: &str = "meta-llama/llama-3.3-70b-instruct";

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
document.addEventListener('visibilitychange', () => dioxus.send({visible: !document.hidden}));
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
const pick = Object.assign(document.createElement('input'), {type: 'file', accept: 'image/*', multiple: true, id: 'pick'});
pick.style.display = 'none';
document.body.appendChild(pick);
document.addEventListener('click', e => { if (e.target.closest('.chip.file')) pick.click(); }, true);
document.addEventListener('change', async e => {
  if (e.target.id !== 'pick') return;
  const files = [...e.target.files].slice(0, 4); e.target.value = '';
  const images = [];
  for (const f of files) { try { images.push(await shrink(f)); } catch (_) {} }
  dioxus.send({images});
});
setInterval(() => {
  const el = document.getElementById('msgs');
  if (!el || el.dataset.watch) return;
  el.dataset.watch = 1; el.dataset.pin = 1;
  el.addEventListener('scroll', () => { el.dataset.pin = el.scrollHeight - el.scrollTop - el.clientHeight < 80 ? 1 : 0; });
  new MutationObserver(() => { if (el.dataset.pin == 1) el.scrollTop = el.scrollHeight; })
    .observe(el, {childList: true, subtree: true, characterData: true});
  el.scrollTop = el.scrollHeight;
}, 100);
await new Promise(() => {});
"#;

fn js(code: &str) { document::eval(code); }
/// Work that must finish even if the screen that started it goes away (Dioxus' `spawn` would cancel it).
fn bg(f: impl std::future::Future<Output = ()> + 'static) { dioxus::core::spawn_forever(f); }
async fn sleep(ms: u32) { let _ = document::eval(&format!("await new Promise(r => setTimeout(r, {ms})); return 0")).join::<u8>().await; }
fn open_url(url: &str) { js(&format!("window.android ? android.open({url:?}) : window.open({url:?})")) }
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
    web_search: bool,
    favorites: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self { Settings { model: FREE_MODEL.into(), web_search: true, favorites: vec![] } }
}

#[derive(Clone, PartialEq)]
struct Model { id: String, name: String, prompt: f64, completion: f64 }

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
    convs: Signal<Vec<Meta>>,
    conv: Signal<Conv>,
    live: Signal<Option<Msg>>,
    stop: Signal<bool>,
    attach: Signal<Vec<String>>,
    wallet: Signal<Wallet>,
    balance: Signal<Option<f64>>,
    toast: Signal<Option<String>>,
    visible: Signal<bool>,
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
        bg(async move {
            if let Ok(v) = api::get("/get_user_balance").await {
                self.balance.set(v["balance"].as_f64())
            }
            if let Ok(v) = api::get("/account_tier").await {
                let tier = v["tier"].as_str().unwrap_or("");
                self.access.set(Some(Access {
                    frontier: v["can_access_frontier"].as_bool().unwrap_or(tier != "pay_as_you_go"),
                    uncensored: v["can_access_uncensored"].as_bool().unwrap_or(tier == "pro"),
                }))
            }
        })
    }
    fn signed_in(mut self) {
        self.screen.set(Screen::Chat);
        self.refresh_balance();
    }
    fn sign_out(mut self) {
        bg(async move { let _ = api::call("POST", "/logout", Some(json!({}))).await; api::forget(); });
        for m in self.convs.peek().iter() {
            store::remove(&format!("chat-{}.json", m.id));
        }
        store::remove("chats.json");
        self.convs.set(vec![]);
        self.conv.set(Conv::new());
        self.balance.set(None);
        self.access.set(None);
        self.screen.set(Screen::Login);
    }

    /// Polls open voucher orders, collects vouchers, and adds the ones that are due.
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

fn main() { dioxus::launch(Root) }

#[component]
fn Root() -> Element {
    store::remove("account.json"); // 0.2.0 kept the number; the app only keeps the session now
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
            convs: Signal::new(convs),
            conv: Signal::new(conv),
            live: Signal::new(None),
            stop: Signal::new(false),
            attach: Signal::new(vec![]),
            wallet: Signal::new(Wallet::load()),
            balance: Signal::new(None),
            toast: Signal::new(None),
            visible: Signal::new(true),
        }
    });

    use_future(move || async move {
        let mut app = app;
        if *app.screen.peek() == Screen::Chat {
            app.refresh_balance()
        }
        if let Ok(v) = api::public("GET", "/models", None).await {
            let mut models: Vec<Model> = v["data"].as_array().into_iter().flatten().map(|m| Model {
                id: m["id"].as_str().unwrap_or("").into(),
                name: m["name"].as_str().or(m["id"].as_str()).unwrap_or("").into(),
                prompt: m["pricing"]["prompt"].as_f64().unwrap_or(0.0),
                completion: m["pricing"]["completion"].as_f64().unwrap_or(0.0),
            }).collect();
            models.sort_by_key(|m| m.name.to_lowercase());
            app.models.set(models);
        }
        let list = |v: &Value, k: &str| v[k].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect::<Vec<_>>();
        if let Ok(v) = api::public("GET", "/frontier_models", None).await { app.frontier.set(list(&v, "frontier_models")) }
        if let Ok(v) = api::public("GET", "/uncensored_models", None).await { app.uncensored.set(list(&v, "uncensored_models")) }
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

    // Vouchers: poll every 10 s while the app is on screen and there's something to do.
    use_future(move || async move {
        loop {
            let w = app.wallet.peek().clone();
            if *app.visible.peek() && (w.active() || w.vouchers.iter().any(|v| !v.invalid && v.due <= store::now())) {
                app.work_wallet(false);
            }
            sleep(10_000).await;
        }
    });

    use_future(move || async move {
        let mut app = app;
        let mut e = document::eval(BRIDGE);
        while let Ok(v) = e.recv::<Value>().await {
            if let Some(url) = v["link"].as_str() {
                open_url(url)
            } else if v["back"] == true {
                if *app.overlay.peek() != Overlay::None {
                    app.overlay.set(Overlay::None)
                } else {
                    let s = *app.screen.peek();
                    app.screen.set(match s { Screen::Funds => Screen::Account, Screen::Signup => Screen::Login, _ => Screen::Chat })
                }
            } else if v["online"] == true {
                app.work_wallet(false)
            } else if let Some(vis) = v["visible"].as_bool() {
                app.visible.set(vis);
                if vis {
                    app.work_wallet(false)
                }
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
            Screen::Funds => rsx! { Funds {} },
        }
        if let Some(t) = app.toast.read().clone() { div { class: "toast", "{t}" } }
    }
}

// ---- sign in and sign up: the web's flow ----

#[component]
fn Login() -> Element {
    let mut app = use_context::<App>();
    let mut number = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let mut sign_in = move |_: ()| {
        let n: String = number.peek().chars().filter(|c| c.is_ascii_digit()).collect();
        if n.len() != 16 {
            error.set(Some("Enter your 16-digit account number.".into()));
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
            p { class: "muted", "Enter your 16-digit account number." }
            input { r#type: "text", inputmode: "numeric", autocomplete: "off", placeholder: "1234 5678 9012 3456", class: "mono",
                oninput: move |e| number.set(e.value()),
                onkeydown: move |e| if e.key() == Key::Enter { sign_in(()) } }
            button { class: "go", disabled: *busy.read(), onclick: move |_| sign_in(()), if *busy.read() { "Signing in…" } else { "Sign In" } }
            if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
            p { class: "muted center", style: "margin-top: 16px",
                "Don't have an account? "
                a { onclick: move |_| app.screen.set(Screen::Signup), "Create one" }
            }
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

    // The number is shown once. We only keep a scrambled copy and can't show it again.
    if let Some(n) = created.read().clone() {
        return rsx! {
            div { class: "login center",
                h1 { "Your account number" }
                p { class: "muted", "Write it down or keep it in a password manager. If you lose it, the account and its balance are gone." }
                div { class: "number mono", "{n}" }
                button { onclick: { let n = n.clone(); move |_| app.copy(&n, "Account number") }, "Copy" }
                label { class: "check",
                    input { r#type: "checkbox", onchange: move |e| saved.set(e.checked()) }
                    "I have saved my account number"
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
            match api::create_account().await {
                Ok(n) => created.set(Some(n)),
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
                p { "We give you a random 16-digit account number. It is the only thing you need to sign in, and we only keep a scrambled copy of it, so we can't look it up or send it to you again." }
            }
            label { class: "check",
                input { r#type: "checkbox", onchange: move |e| tos.set(e.checked()) }
                "I agree to the "
                a { href: "{api::BASE}/terms", "Terms of Service" }
            }
            button { class: "go", disabled: *busy.read(), onclick: create, "Generate Account Number" }
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
    let mut txs = use_signal(|| Value::Null);
    let mut refill = use_signal(|| Value::Null);
    let mut key = use_signal(|| None::<String>);
    let mut new_number = use_signal(|| None::<String>);
    let mut confirm = use_signal(|| None::<&'static str>);
    let mut tx_shown = use_signal(|| 20usize);

    let load = move || {
        app.refresh_balance();
        bg(async move {
            if let Ok(v) = api::get("/account_tier").await { tier.set(v) }
            if let Ok(v) = api::public("GET", "/available_tiers", None).await { tiers.set(v) }
            if let Ok(v) = api::get("/referral_info").await { referral.set(v) }
            if let Ok(v) = api::get("/transactions").await { txs.set(v) }
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
                        button { class: "go", onclick: move |_| { let mut s = app.screen; s.set(Screen::Funds) }, "Add funds" }
                    }
                    if w.held_cents() > 0 {
                        p { class: "muted", "{cents(w.held_cents())} in vouchers is on its way to your balance." }
                    }
                }
                div { class: "section",
                    h2 { "Plan" }
                    if !t.is_null() {
                        p { "{tier_name(&current)}" }
                        if t["included_credit"].as_f64().unwrap_or(0.0) > 0.0 {
                            p { class: "muted", "{usd(t[\"credit_used\"].as_f64().unwrap_or(0.0))} of {usd(t[\"included_credit\"].as_f64().unwrap_or(0.0))} included usage used" }
                        }
                        if let Some(d) = t["next_charge_date"].as_str() { p { class: "muted", "Renews {d.get(..10).unwrap_or(d)} from your balance" } }
                        p { class: "muted", "Tokens this period: {t[\"input_tokens_used\"]} in, {t[\"output_tokens_used\"]} out" }
                    }
                    for x in tiers.read()["tiers"].as_array().cloned().unwrap_or_default() {
                        {
                            let id = x["tier"].as_str().unwrap_or("").to_string();
                            let on = id == current;
                            let fee = x["monthly_fee"].as_f64().unwrap_or(0.0);
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
                                    else { button { onclick: move |_| change_tier(id.clone()), "Switch" } }
                                }
                            }
                        }
                    }
                    p { class: "muted", style: "margin-top: 8px", "Plans are paid from your balance and renew monthly while it covers them." }
                }
                div { class: "section",
                    h2 { "Account number" }
                    if let Some(n) = new_number.read().clone() {
                        div { class: "info center",
                            p { style: "color: #fff; font-weight: 600", "This is your account number" }
                            p { class: "muted", "Your old number no longer works. Save this one now: we can't show it again." }
                            div { class: "number mono", "{n}" }
                            div { class: "row", style: "justify-content: center; border: 0",
                                button { onclick: { let n = n.clone(); move |_| app.copy(&n, "Account number") }, "Copy" }
                                button { class: "go", onclick: move |_| new_number.set(None), "I've Saved It" }
                            }
                        }
                    }
                    div { class: "row",
                        div { class: "grow muted", "Hidden. We only store a scrambled copy, so we can't show it again." }
                        if *confirm.read() != Some("number") {
                            button { onclick: move |_| confirm.set(Some("number")), "Get a New Number" }
                        }
                    }
                    if *confirm.read() == Some("number") {
                        p { class: "muted", style: "margin-top: 8px", "Get a new account number? Your current number stops working right away. Make sure you can save the new one." }
                        div { class: "row", style: "border: 0",
                            button { class: "danger", onclick: move |_| {
                                confirm.set(None);
                                bg(async move {
                                    match api::post("/account_number/rotate", json!({})).await {
                                        Ok(v) => new_number.set(v["account_number"].as_str().map(String::from)),
                                        Err(e) => app.say(e),
                                    }
                                })
                            }, "Get a New Number" }
                            button { class: "bare", onclick: move |_| confirm.set(None), "Cancel" }
                        }
                    }
                }
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
                    p { class: "muted", style: "margin-top: 8px", "Using the API needs the Pro plan." }
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
                if let Some(link) = referral.read()["referral_link"].as_str().map(String::from) {
                    div { class: "section",
                        h2 { "Referrals" }
                        div { class: "row",
                            div { class: "grow mono", style: "font-size: 13px", "{link}" }
                            button { class: "bare", onclick: { let l = link.clone(); move |_| app.copy(&l, "Link") }, "Copy" }
                        }
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
                div { class: "section",
                    h2 { "Transactions" }
                    {
                        let all = txs.read()["transactions"].as_array().cloned().unwrap_or_default();
                        let n = *tx_shown.read();
                        rsx! {
                            if all.is_empty() { p { class: "muted", "None yet" } }
                            for x in all.iter().take(n) {
                                div { class: "row",
                                    div { class: "grow",
                                        div { {capitalize(&x["transaction_type"].as_str().unwrap_or("").replace('_', " "))} }
                                        div { class: "muted", {x["created_at"].as_str().unwrap_or("").get(..16).unwrap_or("").replace('T', " ")} }
                                    }
                                    {
                                        let a: f64 = x["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                                        rsx! { span { class: if a > 0.0 { "ok" } else { "" }, "{usd(a)}" } }
                                    }
                                }
                            }
                            if all.len() > n { button { class: "bare", onclick: move |_| tx_shown.set(n + 20), "More" } }
                        }
                    }
                }
                div { class: "section",
                    if *confirm.read() == Some("out") {
                        p { class: "muted", "Chats on this phone are deleted. Make sure you have your account number." }
                        if w.held_cents() > 0 { p { class: "muted", "{cents(w.held_cents())} in vouchers stays on this phone and goes to the next account that signs in." } }
                        div { class: "row",
                            button { class: "danger", onclick: move |_| app.sign_out(), "Sign out" }
                            button { class: "bare", onclick: move |_| confirm.set(None), "Cancel" }
                        }
                    } else {
                        button { class: "danger", onclick: move |_| confirm.set(Some("out")), "Sign out" }
                    }
                }
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn tier_name(t: &str) -> &'static str {
    match t { "casual" => "Casual", "pro" => "Pro", "pay_as_you_go" => "Pay as you go", _ => "…" }
}

// ---- add funds ----

const METHODS: [(&str, &str); 4] = [("card", "Card"), ("BTC", "Bitcoin"), ("Lightning", "Lightning"), ("XMR", "Monero")];

#[component]
fn Funds() -> Element {
    let app = use_context::<App>();
    let mut method = use_signal(|| "card");
    let mut amount = use_signal(|| "20".to_string());
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut open = use_signal(|| None::<String>); // order being paid
    let mut code = use_signal(String::new);
    let mut card_ready = use_signal(|| false);

    let pay = move |_| {
        let m = *method.peek();
        let dollars: Option<f64> = amount.peek().trim().trim_start_matches('$').parse().ok();
        let c = dollars.map(|d| (d * 100.0).round() as i64);
        if (m == "card" || m == "Lightning") && !matches!(c, Some(50..=1_000_000)) {
            error.set(Some("Enter an amount from $0.50 to $10,000.".into()));
            return;
        }
        busy.set(true);
        error.set(None);
        bg(async move {
            match wallet::create_order(m, c.filter(|c| *c > 0)).await {
                Ok(o) => {
                    let mut w = Wallet::load();
                    w.orders.push(o.clone());
                    w.save();
                    let mut ws = app.wallet;
                    ws.set(w);
                    card_ready.set(false);
                    open.set(Some(o.id.clone()));
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
                Ok(e) if e.is_empty() => { open.set(None); app.say("Paid. Collecting your vouchers…"); app.work_wallet(true) }
                Ok(e) => error.set(Some(e)),
                Err(_) => error.set(Some("Payment failed".into())),
            }
            busy.set(false);
        });
    };

    let w = app.wallet.read().clone();
    let order = open.read().as_ref().and_then(|id| w.orders.iter().find(|o| &o.id == id).cloned());
    let m = *method.read();

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
                                    let uri = match (coin.as_str(), &amt) {
                                        ("Lightning", _) => format!("lightning:{addr}"),
                                        ("XMR", Some(a)) => format!("monero:{addr}?tx_amount={a}"),
                                        ("XMR", None) => format!("monero:{addr}"),
                                        (_, Some(a)) => format!("bitcoin:{addr}?amount={a}"),
                                        _ => format!("bitcoin:{addr}"),
                                    };
                                    let unit = if coin == "Lightning" { "BTC" } else { coin.as_str() };
                                    let heading = match (o.amount_cents, &amt) {
                                        (Some(c), Some(a)) => format!("Send {a} {unit} (about {})", cents(c)),
                                        (Some(c), None) => format!("Send {} in {coin}", cents(c)),
                                        (None, _) => format!("Send any amount of {coin}"),
                                    };
                                    let qr = qrcode::QrCode::new(&uri).map(|q| q.render::<qrcode::render::svg::Color>().quiet_zone(false).build()).unwrap_or_default();
                                    rsx! {
                                        p { class: "lead", "{heading}" }
                                        div { class: "qr", dangerous_inner_html: "{qr}" }
                                        p { class: "mono", style: "font-size: 13px; margin: 12px 0", "{addr}" }
                                        div { class: "row",
                                            button { class: "go", onclick: { let u = uri.clone(); move |_| open_url(&u) }, "Open wallet" }
                                            button { onclick: { let a = addr.clone(); move |_| app.copy(&a, "Address") }, "Copy" }
                                        }
                                    }
                                }
                            }
                            p { class: "muted", style: "margin-top: 12px", "Pay within 1 hour. Whatever you send is added once it confirms, so you can leave and come back." }
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
                        input { class: "field", inputmode: "decimal", initial_value: "{amount.peek()}", placeholder: if m == "BTC" || m == "XMR" { "Any amount (USD, optional)" } else { "Amount (USD)" },
                            oninput: move |e| amount.set(e.value()) }
                        button { class: "go", disabled: *busy.read(), onclick: pay, if *busy.read() { "…" } else { "Continue" } }
                        if let Some(e) = error.read().clone() { p { class: "err", "{e}" } }
                        p { class: "muted", "You pay anonymously and get vouchers the server signs without seeing. They're added to your balance over the next hour, so the payment can't be linked to your account." }
                    }
                }
                {
                    let orders: Vec<_> = w.orders.iter().rev().filter(|o| !o.done).cloned().collect();
                    rsx! {
                        if !orders.is_empty() {
                            div { class: "section",
                                h2 { "Payments" }
                                for o in orders {
                                    div { class: "row",
                                        div { class: "grow",
                                            div { {METHODS.iter().find(|m| m.0 == o.method).map(|m| m.1).unwrap_or("")} " · " {o.amount_cents.map(cents).unwrap_or("any amount".into())} }
                                            div { class: "muted", {if o.funded_cents > 0 { format!("{} received", cents(o.funded_cents)) } else { "Waiting for payment".to_string() }} }
                                        }
                                        if o.crypto() && open.read().as_deref() != Some(&o.id) {
                                            button { class: "bare", onclick: { let id = o.id.clone(); move |_| open.set(Some(id.clone())) }, "Details" }
                                        }
                                    }
                                }
                                button { class: "bare", style: "padding-left: 0", onclick: move |_| app.work_wallet(true), "Check now" }
                            }
                        }
                    }
                }
                if !w.vouchers.is_empty() {
                    div { class: "section",
                        h2 { "Vouchers" }
                        p { "{cents(w.held_cents())} waiting to be added" }
                        p { class: "muted", "Each voucher is added at its own random time within the hour." }
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
