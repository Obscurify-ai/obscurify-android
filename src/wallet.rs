//! Adding funds, the way the web's account page does it.
//!
//! By default a payment is a top-up (`POST /top_ups`, card, BTC, Lightning or XMR): it's tied to the
//! signed-in account, and the server puts every confirmed payment on the balance by itself, so the
//! app can close and any device lists it (`GET /top_ups`). Nothing about it is kept on the phone.
//!
//! Blind vouchers are the opt-in, for a payment that can't be linked to the account. An order is
//! paid anonymously. Once it's funded, the app blinds random voucher messages (RFC 9474,
//! RSABSSA-SHA384-PSS-Deterministic), the server signs them without seeing them, and the app
//! unblinds them into voucher codes. Each code is added to the balance at its own random time 5-60
//! minutes later, so the redemption can't be matched to the payment by timing. The catch: until a
//! voucher is added it exists only in this app's storage, and the app does the work only while it's
//! open, so uninstalling or clearing its data first loses the money (Copy codes keeps them). Every
//! step is saved before the next network call: a batch is written before it's sent (signing the same
//! batch again is idempotent), so a crash or lost reply never loses money.

use crate::{api, store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use blind_rsa_signatures::{BlindMessage, BlindSignature, BlindingResult, DefaultRng, PublicKeySha384PSSDeterministic as Pk, Secret};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FILE: &str = "wallet.json";
const MIN: i64 = 60_000;

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Wallet {
    pub orders: Vec<Order>,
    pub batches: Vec<Batch>,
    pub vouchers: Vec<Voucher>,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Order {
    pub id: String,
    pub method: String,
    pub amount_cents: Option<i64>,
    pub created: i64,
    #[serde(default)]
    pub addresses: BTreeMap<String, String>,
    /// Crypto with an amount: how much of the coin to send, priced on the invoice
    #[serde(default)]
    pub amounts: BTreeMap<String, String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub publishable_key: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub funded_cents: i64,
    #[serde(default)]
    pub available_cents: i64,
    #[serde(default)]
    pub last_poll: i64,
    #[serde(default)]
    pub done: bool,
    /// A top-up: the server credits the account itself. Never stored in wallet.json, and
    /// `tick`/`collect` skip it if it ever is.
    #[serde(default)]
    pub to_account: bool,
}

/// One of the account's top-ups from `GET /top_ups` (last 7 days, newest first).
#[derive(Deserialize, Clone, Default, PartialEq)]
pub struct TopUp {
    pub order_id: String,
    pub payment_method: String,
    #[serde(default)]
    pub amount_cents: Option<i64>,
    #[serde(default)]
    pub credited_cents: i64,
    #[serde(default)]
    pub addresses: Value,
    #[serde(default)]
    pub amounts: Value,
    #[serde(default)]
    pub open: bool,
}

impl TopUp {
    pub fn crypto(&self) -> bool { self.payment_method != "card" }
    /// As an order, to show its address again.
    pub fn order(&self) -> Order {
        Order {
            id: self.order_id.clone(),
            method: self.payment_method.clone(),
            amount_cents: self.amount_cents,
            addresses: strings(&self.addresses),
            amounts: strings(&self.amounts),
            to_account: true,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Batch {
    pub order_id: String,
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Item {
    pub cents: i64,
    pub key_id: String,
    pub der: String,
    pub msg: String,
    pub blinded: String,
    pub secret: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Voucher {
    pub code: String,
    pub cents: i64,
    pub due: i64,
    #[serde(default)]
    pub invalid: bool,
}

impl Order {
    pub fn crypto(&self) -> bool { self.method != "card" }
    /// Card payments finish in minutes; crypto keeps counting late and repeat payments for a day.
    fn stale(&self, now: i64) -> bool {
        now - self.created > if self.crypto() { 24 * 60 * MIN } else { 30 * MIN }
    }
}

impl Wallet {
    pub fn load() -> Self { store::load(FILE) }
    pub fn save(&self) { store::save(FILE, self) }
    pub fn held_cents(&self) -> i64 { self.vouchers.iter().filter(|v| !v.invalid).map(|v| v.cents).sum() }
    pub fn active(&self) -> bool { self.orders.iter().any(|o| !o.done && !o.to_account) || !self.batches.is_empty() }
    pub fn codes(&self) -> String { self.vouchers.iter().map(|v| v.code.as_str()).collect::<Vec<_>>().join("\n") }
}

/// A blind voucher order: sent with no cookie or key.
pub async fn create_order(method: &str, amount_cents: Option<i64>) -> Result<Order, String> {
    let v = api::public("POST", "/vouchers/orders", Some(order_body(method, amount_cents))).await.map_err(api::msg)?;
    parse_order(&v, method, amount_cents, false)
}

/// A top-up of the signed-in account.
pub async fn create_top_up(method: &str, amount_cents: Option<i64>) -> Result<Order, String> {
    let v = api::post("/top_ups", order_body(method, amount_cents)).await?;
    parse_order(&v, method, amount_cents, true)
}

pub async fn top_ups() -> Result<Vec<TopUp>, String> {
    let v = api::get("/top_ups").await?;
    serde_json::from_value(v["top_ups"].clone()).map_err(|e| e.to_string())
}

fn order_body(method: &str, amount_cents: Option<i64>) -> Value {
    let mut body = json!({"payment_method": method});
    if let Some(c) = amount_cents {
        body["amount_cents"] = json!(c);
    }
    body
}

/// `{"BTC": "bcrt1…"}` as a map, skipping anything that isn't a string.
fn strings(v: &Value) -> BTreeMap<String, String> {
    v.as_object().map(|m| m.iter().filter_map(|(k, a)| Some((k.clone(), a.as_str()?.to_string()))).collect()).unwrap_or_default()
}

fn parse_order(v: &Value, method: &str, amount_cents: Option<i64>, to_account: bool) -> Result<Order, String> {
    let map = |k: &str| strings(&v[k]);
    Ok(Order {
        id: v["order_id"].as_str().ok_or("No order id came back")?.to_string(),
        method: method.to_string(),
        amount_cents,
        created: store::now(),
        addresses: map("addresses"),
        amounts: map("amounts"),
        client_secret: v["client_secret"].as_str().map(String::from),
        publishable_key: v["publishable_key"].as_str().map(String::from),
        status: "waiting".into(),
        to_account,
        ..Default::default()
    })
}

/// Polls open orders and turns whatever they've received into vouchers. Polls each order every
/// 10 s for its first 10 minutes, then once a minute (`force` polls now). Returns true if anything changed.
pub async fn tick(w: &mut Wallet, force: bool) -> Result<bool, String> {
    let now = store::now();
    let mut changed = false;
    for i in 0..w.orders.len() {
        let o = &w.orders[i];
        let pending = w.batches.iter().any(|b| b.order_id == o.id);
        if o.to_account || (o.done && !pending) {
            continue;
        }
        let every = if now - o.created < 10 * MIN { 10_000 } else { MIN };
        if !force && !pending && now - o.last_poll < every {
            continue;
        }
        match api::public("GET", &format!("/vouchers/orders/{}", o.id), None).await {
            Ok(v) => {
                let o = &mut w.orders[i];
                o.status = v["status"].as_str().unwrap_or("").to_string();
                o.funded_cents = v["funded_cents"].as_i64().unwrap_or(0);
                o.available_cents = v["available_cents"].as_i64().unwrap_or(0);
                o.last_poll = now;
            }
            Err(e) if api::status_of(&e).0 == 404 => w.orders[i].done = true,
            Err(e) => return Err(api::msg(e)),
        }
        changed = true;
        w.save();
        collect(w, i).await?;
        let o = &mut w.orders[i];
        let funded_card = !o.crypto() && o.funded_cents > 0;
        if o.available_cents == 0 && (funded_card || o.status == "expired" || o.stale(now)) {
            o.done = true;
            w.save();
        }
    }
    Ok(changed)
}

/// Signs everything the order has available: first any batch saved but never answered, then new ones.
async fn collect(w: &mut Wallet, i: usize) -> Result<(), String> {
    if w.orders[i].to_account {
        return Ok(());
    }
    let id = w.orders[i].id.clone();
    let mut keys: Option<Vec<Value>> = None;
    loop {
        if let Some(b) = w.batches.iter().position(|b| b.order_id == id) {
            let batch = w.batches[b].clone();
            let body = json!({"vouchers": batch.items.iter().map(|it| json!({"denomination_cents": it.cents, "key_id": it.key_id, "blinded_message": it.blinded})).collect::<Vec<_>>()});
            match api::public("POST", &format!("/vouchers/orders/{id}/sign"), Some(body)).await {
                Ok(v) => {
                    let sigs: Vec<&str> = v["blind_signatures"].as_array().map(|a| a.iter().filter_map(|s| s.as_str()).collect()).unwrap_or_default();
                    let now = store::now();
                    for (it, sig) in batch.items.iter().zip(sigs) {
                        match unblind(it, sig) {
                            Ok(code) => w.vouchers.push(Voucher { code, cents: it.cents, due: now + rand::random_range(5 * MIN..60 * MIN), invalid: false }),
                            Err(e) => eprintln!("voucher {} didn't verify: {e}", it.key_id),
                        }
                    }
                    w.batches.remove(b);
                    w.orders[i].available_cents = v["available_cents"].as_i64().unwrap_or(0);
                    w.save();
                }
                // The keys changed before this batch was signed: blind again with the new ones.
                Err(e) if api::status_of(&e).0 == 409 => {
                    w.batches.remove(b);
                    w.save();
                    keys = None;
                }
                Err(e) => return Err(api::msg(e)),
            }
            continue;
        }
        let available = w.orders[i].available_cents;
        if available <= 0 {
            return Ok(());
        }
        if keys.is_none() {
            let v = api::public("GET", "/vouchers/keys", None).await.map_err(api::msg)?;
            keys = Some(v["keys"].as_array().cloned().unwrap_or_default());
        }
        let items = blind_batch(keys.as_ref().unwrap(), available)?;
        w.batches.push(Batch { order_id: id.clone(), items });
        w.save();
    }
}

/// Splits `cents` into voucher values, largest first (there's a 1¢ value, so it's exact), and blinds
/// one random message per voucher. At most 64 per batch, the server's limit.
fn blind_batch(keys: &[Value], mut cents: i64) -> Result<Vec<Item>, String> {
    let mut keys: Vec<&Value> = keys.iter().collect();
    keys.sort_by_key(|k| -k["denomination_cents"].as_i64().unwrap_or(0));
    let mut items = vec![];
    for k in keys {
        let d = k["denomination_cents"].as_i64().unwrap_or(0);
        while d > 0 && cents >= d && items.len() < 64 {
            let der = k["public_key_der"].as_str().unwrap_or("");
            let pk = Pk::from_der(&B64.decode(der).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let mut msg = [0u8; 32];
            rand::fill(&mut msg);
            let r = pk.blind(&mut DefaultRng, msg).map_err(|e| e.to_string())?;
            items.push(Item {
                cents: d,
                key_id: k["key_id"].as_str().unwrap_or("").to_string(),
                der: der.to_string(),
                msg: B64.encode(msg),
                blinded: B64.encode(&r.blind_message.0),
                secret: B64.encode(&r.secret.0),
            });
            cents -= d;
        }
    }
    if items.is_empty() {
        return Err("No voucher keys".into());
    }
    Ok(items)
}

fn unblind(it: &Item, blind_sig: &str) -> Result<String, String> {
    let d = |s: &str| B64.decode(s).map_err(|e| e.to_string());
    let pk = Pk::from_der(&d(&it.der)?).map_err(|e| e.to_string())?;
    let msg = d(&it.msg)?;
    let r = BlindingResult { blind_message: BlindMessage(d(&it.blinded)?), secret: Secret(d(&it.secret)?), msg_randomizer: None };
    let sig = pk.finalize(&BlindSignature(d(blind_sig)?), &r, &msg).map_err(|e| e.to_string())?;
    Ok(format!("obv1.{}.{}.{}.{}", it.cents, it.key_id, B64.encode(&msg), B64.encode(&sig.0)))
}

/// Adds vouchers to the signed-in account: the ones whose time has come, or all of them with `now`.
/// Returns dollars credited.
pub async fn redeem(w: &mut Wallet, all: bool) -> Result<f64, String> {
    let now = store::now();
    let due: Vec<String> = w.vouchers.iter().filter(|v| !v.invalid && (all || v.due <= now)).map(|v| v.code.clone()).collect();
    let mut credited = 0.0;
    for chunk in due.chunks(20) {
        let v = api::post("/vouchers/redeem", json!({"vouchers": chunk})).await?;
        let results = v["results"].as_array().cloned().unwrap_or_default();
        for (code, r) in chunk.iter().zip(results) {
            match r["status"].as_str() {
                Some("credited") | Some("already_redeemed") => w.vouchers.retain(|v| &v.code != code),
                Some("invalid") => w.vouchers.iter_mut().filter(|v| &v.code == code).for_each(|v| v.invalid = true),
                _ => {}
            }
        }
        credited += v["credited_usd"].as_f64().unwrap_or(0.0);
        w.save();
    }
    Ok(credited)
}
