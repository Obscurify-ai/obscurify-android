//! In-app Tor, with Arti (the Tor Project's Rust client). Off until the Tor button is tapped or
//! "Always connect through Tor" is on, so with Tor off the app makes no Tor connections and pays
//! no startup cost.
//!
//! On, every request goes to Obscurify's onion address over Tor. A SOCKS5 proxy on 127.0.0.1 (a
//! random port, a username and password made for this run) hands each connection to Arti, and
//! names are resolved inside Tor (`socks5h`). The proxy is also set as `ALL_PROXY` while Tor is on,
//! so HTTP clients made by the crates the app uses (private sessions, Encrypted mode's checks) go
//! through it too. If Tor can't connect, requests fail: nothing falls back to the open internet.
//!
//! Arti keeps its state and directory cache in the app's private files dir (`tor/`). Android's app
//! data dir is the app's own sandbox, so Arti's file permission checks are turned off.

use crate::store;
use std::sync::Mutex;

/// Obscurify's onion service. Test builds can point it elsewhere with OBSCURIFY_ONION.
pub const ONION: &str = match option_env!("OBSCURIFY_ONION") {
    Some(o) => o,
    None => "http://obscurizgk4wr5bg5fatq5tedqszcjrbgkot6qw7tnip74bakp6oztid.onion",
};

const ALWAYS_FILE: &str = "tor.json";

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    Off,
    Connecting,
    On,
    /// Tor couldn't connect. Requests still go to Tor (and fail) until it's turned off.
    Failed(String),
}

struct Running {
    client: reqwest::Client,
    stop: Option<std::sync::mpsc::Sender<()>>,
}

static RUN: Mutex<Option<Running>> = Mutex::new(None);
static STATUS: Mutex<Status> = Mutex::new(Status::Off);
/// Bumped on every start, so a stopped Tor's late news can't change the status.
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// "Always connect through Tor": Tor starts with the app and the app never goes around it.
/// Kept in its own file, so Wipe (which clears chats and settings) doesn't turn it off.
pub fn always() -> bool { store::load::<bool>(ALWAYS_FILE) }

pub fn set_always(on: bool) {
    if on { store::save(ALWAYS_FILE, &true) } else { store::remove(ALWAYS_FILE) }
}

/// Requests go through Tor now (connecting or connected).
pub fn wanted() -> bool { RUN.lock().unwrap().is_some() }

pub fn status() -> Status { STATUS.lock().unwrap().clone() }

fn set_status(run_id: u64, s: Status) {
    if GEN.load(std::sync::atomic::Ordering::SeqCst) == run_id {
        *STATUS.lock().unwrap() = s;
    }
}

/// The HTTP client that goes through Tor, while Tor is on.
pub fn client() -> Option<reqwest::Client> { RUN.lock().unwrap().as_ref().map(|r| r.client.clone()) }

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::fill(&mut b[..]);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Starts Tor. Returns at once: requests made from now on wait at the proxy until Tor is ready.
pub fn start() {
    let mut run = RUN.lock().unwrap();
    if run.is_some() {
        return;
    }
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            *STATUS.lock().unwrap() = Status::Failed(e.to_string());
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let (user, pass) = (random_hex(16), random_hex(16));
    let proxy = format!("socks5h://{user}:{pass}@127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).expect("proxy url"))
        // An onion connection takes several seconds, longer while Tor is still starting.
        .connect_timeout(std::time::Duration::from_secs(120))
        .pool_idle_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(180))
        .user_agent("Obscurify-Android")
        .build()
        .expect("tor client");
    // SAFETY: set before the clients that read it are made. Rust's own environment access is
    // serialized; nothing in the app reads these variables from C.
    unsafe {
        std::env::set_var("ALL_PROXY", &proxy);
        std::env::set_var("all_proxy", &proxy);
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
    let run_id = GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    *STATUS.lock().unwrap() = Status::Connecting;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("tor".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return set_status(run_id, Status::Failed(e.to_string())),
            };
            rt.block_on(async move {
                let stop = tokio::task::spawn_blocking(move || { let _ = rx.recv(); });
                tokio::select! {
                    _ = stop => {}
                    r = serve(listener, user, pass, run_id) => if let Err(e) = r { set_status(run_id, Status::Failed(e)) },
                }
            });
            // Dropping the runtime ends every Tor connection.
            rt.shutdown_background();
        })
        .expect("tor thread");
    *run = Some(Running { client, stop: Some(tx) });
    drop(run);
    crate::api::drop_direct();
    crate::private::reset();
}

/// Stops Tor. Requests go to obscurify.ai directly again.
pub fn stop() {
    let Some(mut r) = RUN.lock().unwrap().take() else { return };
    if let Some(tx) = r.stop.take() {
        let _ = tx.send(());
    }
    GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *STATUS.lock().unwrap() = Status::Off;
    // SAFETY: as in `start`.
    unsafe {
        std::env::remove_var("ALL_PROXY");
        std::env::remove_var("all_proxy");
    }
    crate::private::reset();
}

/// Tor's built-in bridges, as Tor Browser ships them (tor-browser-build, pt_config.json), for
/// networks that block Tor: obfs4 and Snowflake, through lyrebird.
const BRIDGES: &[&str] = &[
    "obfs4 37.218.245.14:38224 D9A82D2F9C2F65A18407B1D2B764F130847F8B5D cert=bjRaMrr1BRiAW8IE9U5z27fQaYgOhX1UCmOpg2pFpoMvo6ZgQMzLsaTzzQNTlm7hNcb+Sg iat-mode=0",
    "obfs4 209.148.46.65:443 74FAD13168806246602538555B5521A0383A1875 cert=ssH+9rP8dG2NLDN2XuFw63hIO/9MNNinLmxQDpVa+7kTOa9/m+tGWT1SmSYpQ9uTBGa6Hw iat-mode=0",
    "obfs4 146.57.248.225:22 10A6CD36A537FCE513A322361547444B393989F0 cert=K1gDtDAIcUfeLqbstggjIw2rtgIKqdIhUlHp82XRqNSq/mtAjp1BIC9vHKJ2FAEpGssTPw iat-mode=0",
    "obfs4 45.145.95.6:27015 C5B7CD6946FF10C5B3E89691A7D3F2C122D2117C cert=TD7PbUO0/0k6xYHMPW3vJxICfkMZNdkRrb63Zhl5j9dW3iRGiCx0A7mPhe5T2EDzQ35+Zw iat-mode=0",
    "obfs4 51.222.13.177:80 5EDAC3B810E12B01F6FD8050D2FD3E277B289A08 cert=2uplIpLQ0q9+0qMFrK5pkaYRDOe460LL9WHBvatgkuRr/SL31wBOEupaMMJ6koRE6Ld0ew iat-mode=0",
    "obfs4 212.83.43.95:443 BFE712113A72899AD685764B211FACD30FF52C31 cert=ayq0XzCwhpdysn5o0EyDUbmSOx3X/oTEbzDMvczHOdBJKlvIdHHLJGkZARtT4dcBFArPPg iat-mode=1",
    "obfs4 212.83.43.74:443 39562501228A4D5E27FCA4C0C81A01EE23AE3EE4 cert=PBwr+S8JTVZo6MPdHnkTwXJPILWADLqfMGoVvhZClMq/Urndyd42BwX9YFJHZnBB3H0XCw iat-mode=1",
    "snowflake 192.0.2.3:80 2B280B23E1107BB62ABFC40DDCC8824814F80A72 fingerprint=2B280B23E1107BB62ABFC40DDCC8824814F80A72 url=https://1098762253.rsc.cdn77.org/ fronts=app.datapacket.com,www.datapacket.com ice=stun:stun.epygi.com:3478,stun:stun.uls.co.za:3478,stun:stun.voipgate.com:3478,stun:stun.mixvoip.com:3478,stun:stun.telnyx.com:3478,stun:stun.hot-chilli.net:3478,stun:stun.fitauto.ru:3478,stun:stun.m-online.net:3478 utls-imitate=hellorandomizedalpn",
    "snowflake 192.0.2.4:80 8838024498816A039FCBBAB14E6F40A0843051FA fingerprint=8838024498816A039FCBBAB14E6F40A0843051FA url=https://1098762253.rsc.cdn77.org/ fronts=app.datapacket.com,www.datapacket.com ice=stun:stun.epygi.com:3478,stun:stun.uls.co.za:3478,stun:stun.voipgate.com:3478,stun:stun.mixvoip.com:3478,stun:stun.telnyx.com:3478,stun:stun.hot-chilli.net:3478,stun:stun.fitauto.ru:3478,stun:stun.m-online.net:3478 utls-imitate=hellorandomizedalpn",
];

/// Set when Tor last connected only through bridges: the next start goes straight to them.
const BRIDGES_FILE: &str = "tor-bridges.json";

/// How long a direct connection gets before bridges are tried.
const DIRECT_FOR: std::time::Duration = std::time::Duration::from_secs(40);

/// The packaged lyrebird (obfs4, Snowflake), next to the app's own native library. Android only
/// runs programs from the app's library dir, so it ships as `liblyrebird.so`.
fn lyrebird() -> Option<std::path::PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let lib = maps.lines().filter_map(|l| l.split_whitespace().nth(5)).find(|p| p.ends_with("/libmain.so"))?;
    let path = std::path::Path::new(lib).parent()?.join("liblyrebird.so");
    path.exists().then_some(path)
}

type Tor = std::sync::Arc<arti_client::TorClient<tor_rtcompat::tokio::TokioRustlsRuntime>>;

fn make(bridges: bool) -> Result<Tor, String> {
    use arti_client::config::{BridgeConfigBuilder, CfgPath, TorClientConfigBuilder, pt::TransportConfigBuilder};
    use arti_client::{BootstrapBehavior, TorClient};
    // Separate dirs for the two ways in: each client locks its own.
    let dir = store::dir().join("tor").join(if bridges { "bridges" } else { "direct" });
    let mut b = TorClientConfigBuilder::from_directories(dir.join("state"), dir.join("cache"));
    b.storage().permissions().dangerously_trust_everyone();
    b.address_filter().allow_onion_addrs(true);
    if bridges {
        let pt = lyrebird().ok_or("no bridge transport in this build")?;
        for line in BRIDGES {
            b.bridges().bridges().push(format!("Bridge {line}").parse::<BridgeConfigBuilder>().map_err(|e| e.to_string())?);
        }
        let mut t = TransportConfigBuilder::default();
        t.protocols(vec!["obfs4".parse().map_err(|e| format!("{e}"))?, "snowflake".parse().map_err(|e| format!("{e}"))?])
            .path(CfgPath::new_literal(pt))
            .run_on_startup(false);
        b.bridges().transports().push(t);
    }
    let config = b.build().map_err(|e| e.to_string())?;
    let rt = tor_rtcompat::tokio::TokioRustlsRuntime::current().map_err(|e| e.to_string())?;
    TorClient::with_runtime(rt)
        .config(config)
        .bootstrap_behavior(BootstrapBehavior::OnDemand)
        .create_unbootstrapped()
        .map_err(|e| e.to_string())
}

/// Connects directly first; if that hasn't worked within DIRECT_FOR (or failed), through the
/// built-in bridges. A phone that last needed bridges starts with them.
async fn connect(current: tokio::sync::watch::Sender<Option<Tor>>, run_id: u64) {
    let can_bridge = lyrebird().is_some();
    let order: &[bool] = match (can_bridge, store::load::<bool>(BRIDGES_FILE)) {
        (true, true) => &[true, false],
        (true, false) => &[false, true],
        (false, _) => &[false],
    };
    let mut last = String::new();
    for (i, &bridges) in order.iter().enumerate() {
        let tor = match make(bridges) {
            Ok(t) => t,
            Err(e) => { last = e; continue }
        };
        current.send_replace(Some(tor.clone()));
        let more = i + 1 < order.len();
        let r: Result<(), String> = if more {
            match tokio::time::timeout(DIRECT_FOR, tor.bootstrap()).await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(_) => Err("timed out".into()),
            }
        } else {
            tor.bootstrap().await.map_err(|e| e.to_string())
        };
        match r {
            Ok(()) => {
                if bridges { store::save(BRIDGES_FILE, &true) } else { store::remove(BRIDGES_FILE) }
                return set_status(run_id, Status::On);
            }
            Err(e) => {
                last = e;
                if more {
                    current.send_replace(None);
                }
            }
        }
    }
    set_status(run_id, Status::Failed(last));
}

async fn serve(listener: std::net::TcpListener, user: String, pass: String, run_id: u64) -> Result<(), String> {
    // Arti's TLS needs a process-wide rustls provider; the app's HTTP client uses ring too.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (tx, rx) = tokio::sync::watch::channel::<Option<Tor>>(None);
    tokio::spawn(connect(tx, run_id));
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    loop {
        let (sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let (mut rx, user, pass) = (rx.clone(), user.clone(), pass.clone());
        tokio::spawn(async move {
            // While Tor switches to bridges there's no client for a moment: wait for one.
            let Ok(tor) = rx.wait_for(Option::is_some).await.map(|t| t.clone().unwrap()) else { return };
            if let Err(e) = socks(sock, &tor, &user, &pass).await {
                eprintln!("tor: {e}");
            }
        });
    }
}

/// One SOCKS5 connection (RFC 1928, username/password RFC 1929): CONNECT to a host name only.
async fn socks(mut s: tokio::net::TcpStream, tor: &Tor, user: &str, pass: &str) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let e = |e: std::io::Error| e.to_string();
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await.map_err(e)?;
    if h[0] != 5 {
        return Err("not socks5".into());
    }
    let mut methods = vec![0u8; h[1] as usize];
    s.read_exact(&mut methods).await.map_err(e)?;
    if !methods.contains(&2) {
        s.write_all(&[5, 0xff]).await.map_err(e)?;
        return Err("no password offered".into());
    }
    s.write_all(&[5, 2]).await.map_err(e)?;
    // Username and password.
    let mut v = [0u8; 2];
    s.read_exact(&mut v).await.map_err(e)?;
    let mut u = vec![0u8; v[1] as usize];
    s.read_exact(&mut u).await.map_err(e)?;
    let mut pl = [0u8; 1];
    s.read_exact(&mut pl).await.map_err(e)?;
    let mut p = vec![0u8; pl[0] as usize];
    s.read_exact(&mut p).await.map_err(e)?;
    if u != user.as_bytes() || p != pass.as_bytes() {
        s.write_all(&[1, 1]).await.map_err(e)?;
        return Err("wrong proxy password".into());
    }
    s.write_all(&[1, 0]).await.map_err(e)?;
    // The request: CONNECT, by host name (resolved inside Tor).
    let mut r = [0u8; 4];
    s.read_exact(&mut r).await.map_err(e)?;
    let fail = |code: u8| [5, code, 0, 1, 0, 0, 0, 0, 0, 0];
    if r[1] != 1 || r[3] != 3 {
        s.write_all(&fail(if r[1] != 1 { 7 } else { 8 })).await.map_err(e)?;
        return Err("only CONNECT to a host name".into());
    }
    let mut l = [0u8; 1];
    s.read_exact(&mut l).await.map_err(e)?;
    let mut host = vec![0u8; l[0] as usize];
    s.read_exact(&mut host).await.map_err(e)?;
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await.map_err(e)?;
    let host = String::from_utf8(host).map_err(|_| "bad host".to_string())?;
    let port = u16::from_be_bytes(port);
    let mut stream = match tor.connect((host.as_str(), port)).await {
        Ok(st) => st,
        Err(err) => {
            let _ = s.write_all(&fail(4)).await;
            return Err(format!("{host}: {err}"));
        }
    };
    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.map_err(e)?;
    let _ = tokio::io::copy_bidirectional(&mut s, &mut stream).await;
    Ok(())
}
