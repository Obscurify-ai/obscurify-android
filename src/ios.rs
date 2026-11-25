//! The iPhone glue that MainActivity.kt is on Android: links open in the browser, text goes on the
//! clipboard natively (the WebView's clipboard API needs a user gesture, and a copied account code
//! must really be copied), the screen stays on while an answer is worked on, an answer gets iOS's
//! background time to finish and says when it's ready, secrets stay out of backups, and Code Mode's
//! sandbox.
//!
//! Code Mode runs the model's code in a WKWebView of its own, never in the app's, as on Android: no
//! app bridges, a non-persistent data store (no cookies or storage kept), a navigation delegate that
//! lets it load only the server's `/sandbox/` pages, and one message handler (`obsSandbox`) that only
//! hands the runner's messages to `code::on_sandbox`. The runner gets its MessagePort from a
//! MessageChannel made in its own page after it loads (runner.js takes an `init` with no source at
//! the top level, the way the Android app's native port arrives). The model's code runs in the
//! runner's Worker, which can't reach the handler. Pages the model builds open full screen in a
//! WKWebView made the same way, with no handler at all.
//!
//! The iOS app is the store build (`play`, no Tor).

#[cfg(not(feature = "play"))]
compile_error!("The iOS app is the store build: build it with --no-default-features --features play");
#[cfg(feature = "tor")]
compile_error!("The iOS app has no Tor: build it with --no-default-features --features play");

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
use objc2::{class, define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSNumber, NSObjectProtocol, NSString, NSURLIsExcludedFromBackupKey, NSURLRequest, NSURL};
use objc2_ui_kit::{
    UIApplication, UIApplicationState, UIBackgroundTaskInvalid, UIButton, UIButtonType, UIColor, UIControlEvents, UIControlState, UIFont,
    UILabel, UIPasteboard, UIView, UIViewAutoresizing, UIWindow,
};
use objc2_user_notifications::{UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound, UNUserNotificationCenter};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKScriptMessage, WKScriptMessageHandler, WKUserContentController,
    WKWebViewConfiguration, WKWebsiteDataStore,
};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

unsafe extern "C" {
    fn syslog(priority: std::ffi::c_int, format: *const std::ffi::c_char, ...);
}
// BGTaskScheduler, for the continued processing task an answer asks for (iOS 26).
#[link(name = "BackgroundTasks", kind = "framework")]
unsafe extern "C" {}

/// What the native side does goes to stderr (the simulator run in CI keeps it, tools/ios.sh) and to
/// the system log (a phone's, read over USB). The clock is the device's uptime since the first line,
/// so it goes on while the app is suspended.
pub fn log(s: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(std::time::Instant::now).elapsed();
    let line = format!("obscurify-ios: [{:.1}s] {s}", t.as_secs_f32());
    eprintln!("{line}");
    if let Ok(c) = std::ffi::CString::new(line) {
        // SAFETY: a "%s" format with one C string.
        unsafe { syslog(5, c"%s".as_ptr(), c.as_ptr()) } // LOG_NOTICE
    }
}

static BG_NOTED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
/// An answer got more text with the app in the background (said every 5 s at most).
pub fn answer_in_background() {
    let now = crate::store::now();
    if now - BG_NOTED.load(Ordering::SeqCst) > 5000 {
        BG_NOTED.store(now, Ordering::SeqCst);
        log("answer: still coming in with the app in the background")
    }
}

/// Something came in for the answer being worked on (a piece of a reply, a keepalive, a message
/// from the sandbox): what the continued processing task reports as progress.
pub fn activity() { ACTIVITY.fetch_add(1, Ordering::Relaxed); }
static ACTIVITY: AtomicU64 = AtomicU64::new(0);
/// An answer is being worked on (`working`), readable from any thread.
static ANSWERING: AtomicBool = AtomicBool::new(false);
pub fn answering() -> bool { ANSWERING.load(Ordering::SeqCst) }

/// Said every few seconds while an answer is worked on and the app isn't on screen, from where it's
/// said: "process" from a thread of its own (the app runs), "main" from the main queue (iOS runs the
/// main thread), "dioxus" from a Dioxus task (the app's tasks are polled, `crate::working`).
pub fn beat(from: &str) {
    if MainThreadMarker::new().is_some_and(|mtm| UIApplication::sharedApplication(mtm).applicationState() == UIApplicationState::Active) { return }
    log(&format!("beat: {from}"))
}

/// A message from the sandbox (`code::on_sandbox`): progress, and said while the app is in the
/// background (every 2 s at most), so a run shows whether code goes on there.
pub fn sandbox_heard(kind: &str) {
    activity();
    static SAID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let now = crate::store::now();
    if now - SAID.load(Ordering::SeqCst) < 2000 { return }
    if MainThreadMarker::new().is_some_and(|mtm| UIApplication::sharedApplication(mtm).applicationState() != UIApplicationState::Active) {
        SAID.store(now, Ordering::SeqCst);
        log(&format!("sandbox: {kind} with the app in the background"))
    }
}

/// While an answer is worked on: the heartbeats, the app's state changes, and the continued
/// processing task's progress, every 2 s.
fn watch() {
    std::thread::spawn(|| {
        let mut last_state = -1isize;
        let mut n = 0u32;
        while answering() {
            std::thread::sleep(std::time::Duration::from_secs(2));
            n += 1;
            report_progress();
            let (tx, rx) = std::sync::mpsc::channel();
            on_main(move |mtm| { let _ = tx.send(UIApplication::sharedApplication(mtm).applicationState().0); });
            match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(state) => {
                    if state != last_state {
                        log(&format!("app state {state}{}", match state { 0 => " (on screen)", 1 => " (inactive)", _ => " (background)" }));
                        last_state = state;
                    }
                    if state != 0 && n % 2 == 0 { log("beat: process, main") }
                }
                Err(_) => if n % 2 == 0 { log("beat: process (the main thread didn't answer)") },
            }
        }
    });
}

/// Runs `f` on the main thread: now if this is it, else queued there.
fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    match MainThreadMarker::new() {
        Some(mtm) => f(mtm),
        // SAFETY: the main queue runs on the main thread.
        None => dispatch2::DispatchQueue::main().exec_async(move || f(unsafe { MainThreadMarker::new_unchecked() })),
    }
}

pub fn open(url: &str) {
    if webbrowser::open(url).is_err() { log("couldn't open a link") }
}

pub fn copy(text: &str) {
    let text = text.to_string();
    on_main(move |_| unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(&text))) });
}

// ---- backups ----

/// Keeps a file out of iCloud and computer backups (`isExcludedFromBackup`). Set again after every
/// save: a save writes a new file and renames it over the old one.
pub fn exclude_from_backup(path: &std::path::Path) {
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let yes = NSNumber::numberWithBool(true);
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match unsafe { url.setResourceValue_forKey_error(Some(yes.as_ref()), NSURLIsExcludedFromBackupKey) } {
        Ok(()) => {
            // Once per file and launch, read back, so a run shows it took.
            let mut said = SAID.lock().unwrap();
            if !said.contains(&name) {
                said.push(name.clone());
                let mut v: Option<Retained<AnyObject>> = None;
                let _ = unsafe { url.getResourceValue_forKey_error(&mut v, NSURLIsExcludedFromBackupKey) };
                let on = v.and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|n| n.boolValue());
                log(&format!("backup: {name} excluded={on}"));
            }
        }
        Err(e) => log(&format!("backup: couldn't exclude {name}: {}", e.localizedDescription())),
    }
}

// ---- an answer being worked on ----

thread_local! {
    static WORKING: Cell<bool> = const { Cell::new(false) };
    static TASK: Cell<usize> = const { Cell::new(0) };
    static ASKED: Cell<bool> = const { Cell::new(false) };
}

/// An answer started or ended. While it's worked on the screen doesn't lock, and if the app goes to
/// the background iOS gives it its usual half minute or so to finish (`beginBackgroundTask`). An
/// answer that ends with the app in the background says so in a notification. (A signed-in answer
/// the app couldn't finish is finished by the server, and the app picks it up when it's back.)
pub fn working(on: bool) {
    on_main(move |mtm| {
        let app = UIApplication::sharedApplication(mtm);
        app.setIdleTimerDisabled(on);
        if on && !WORKING.get() {
            WORKING.set(true);
            BG_NOTED.store(0, Ordering::SeqCst);
            ANSWERING.store(true, Ordering::SeqCst);
            ask_to_notify();
            end_task(mtm);
            let expired = RcBlock::new(move || {
                log("background time ran out");
                if let Some(mtm) = MainThreadMarker::new() { end_task(mtm) }
            });
            let id = app.beginBackgroundTaskWithName_expirationHandler(Some(&NSString::from_str("Finishing an answer")), Some(&expired));
            TASK.set(id);
            log("working: on");
            continue_start();
            watch();
        } else if !on && WORKING.get() {
            WORKING.set(false);
            ANSWERING.store(false, Ordering::SeqCst);
            let state = app.applicationState();
            if state != UIApplicationState::Active { notify_ready() }
            log(&format!("working: off (app state {})", state.0));
            continue_end(true);
            end_task(mtm);
        }
    })
}

// ---- the continued processing task (iOS 26) ----
//
// Past the half minute `beginBackgroundTask` gives, iOS 26 lets work a person started go on in the
// background as a continued processing task: iOS shows it in a Live Activity ("Obscurify, Writing
// your answer", with its progress), where it can also be stopped. Each answer asks for one when it
// starts, as `<bundle id>.answer.<n>` (Info.plist permits `<bundle id>.answer.*`, tools/ios.sh), and
// ends it when the answer ends. Older iOS has no such task: the half minute, then the server.

/// The running task: set by its launch handler (on a queue of iOS's), ended from the main thread.
struct Continued(Retained<AnyObject>);
// SAFETY: BGTask and its NSProgress may be used from any thread.
unsafe impl Send for Continued {}
static CONTINUED: Mutex<Option<Continued>> = Mutex::new(None);
static CONTINUED_N: AtomicU32 = AtomicU32::new(0);

fn ns(s: &str) -> Retained<NSString> { NSString::from_str(s) }

fn continue_start() {
    let Some(request_class) = objc2::runtime::AnyClass::get(c"BGContinuedProcessingTaskRequest") else {
        return log("continued: not on this iOS (before 26)");
    };
    let bundle: Option<Retained<NSString>> = unsafe {
        let main: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        msg_send![main, bundleIdentifier]
    };
    let Some(bundle) = bundle else { return log("continued: no bundle id") };
    let n = CONTINUED_N.fetch_add(1, Ordering::SeqCst) + 1;
    let ident = ns(&format!("{bundle}.answer.{n}"));
    ACTIVITY.store(0, Ordering::SeqCst);
    unsafe {
        let scheduler: *mut AnyObject = msg_send![class!(BGTaskScheduler), sharedScheduler];
        let launch = RcBlock::new(move |task: std::ptr::NonNull<AnyObject>| {
            let Some(task) = Retained::retain(task.as_ptr()) else { return };
            let expired = RcBlock::new(move || {
                log("continued: expired (stopped from the Live Activity, or iOS needed the room)");
                continue_end(false);
            });
            let _: () = msg_send![&*task, setExpirationHandler: &*expired];
            let progress: *mut AnyObject = msg_send![&*task, progress];
            let _: () = msg_send![progress, setTotalUnitCount: 100i64];
            *CONTINUED.lock().unwrap() = Some(Continued(task));
            log(&format!("continued: running (task {n})"));
            // The answer may have ended while the task waited to start.
            if !answering() { continue_end(true) }
        });
        let registered: Bool = msg_send![scheduler, registerForTaskWithIdentifier: &*ident, usingQueue: std::ptr::null_mut::<AnyObject>(), launchHandler: &*launch];
        if !registered.as_bool() { return log("continued: couldn't register the task (is it in BGTaskSchedulerPermittedIdentifiers?)") }
        let request: Allocated<AnyObject> = msg_send![request_class, alloc];
        let request: Retained<AnyObject> = msg_send![request, initWithIdentifier: &*ident, title: &*ns("Obscurify"), subtitle: &*ns("Writing your answer")];
        let submitted: Result<(), Retained<NSError>> = msg_send![scheduler, submitTaskRequest: &*request, error: _];
        match submitted {
            Ok(()) => log(&format!("continued: submitted (task {n})")),
            Err(e) => log(&format!("continued: not submitted: {} (code {})", e.localizedDescription(), e.code())),
        }
    }
}

/// Progress the Live Activity shows, from what has come in so far: it keeps rising while anything
/// arrives and stands still when nothing does (iOS stops a task that shows none).
fn report_progress() {
    let Some(task) = CONTINUED.lock().unwrap().as_ref().map(|c| c.0.clone()) else { return };
    let a = ACTIVITY.load(Ordering::Relaxed) as i64;
    unsafe {
        let progress: *mut AnyObject = msg_send![&*task, progress];
        let _: () = msg_send![progress, setTotalUnitCount: a + (a / 9).max(20)];
        let _: () = msg_send![progress, setCompletedUnitCount: a];
    }
}

fn continue_end(success: bool) {
    let Some(task) = CONTINUED.lock().unwrap().take() else { return };
    unsafe {
        let progress: *mut AnyObject = msg_send![&*task.0, progress];
        let total: i64 = msg_send![progress, totalUnitCount];
        let _: () = msg_send![progress, setCompletedUnitCount: total];
        let _: () = msg_send![&*task.0, setTaskCompletedWithSuccess: Bool::new(success)];
    }
    log(&format!("continued: ended ({})", if success { "done" } else { "stopped" }));
}

/// Not on screen: iOS suspends the WebView, so nothing should be drawn (`crate::backgrounded`).
pub fn backgrounded() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| UIApplication::sharedApplication(mtm).applicationState() != UIApplicationState::Active)
}

fn end_task(mtm: MainThreadMarker) {
    let id = TASK.replace(0);
    if id != 0 && id != unsafe { UIBackgroundTaskInvalid } {
        UIApplication::sharedApplication(mtm).endBackgroundTask(id)
    }
}

/// Asked when the first answer starts, as on Android: that's when "answer ready" would come.
fn ask_to_notify() {
    if ASKED.replace(true) { return }
    // The e2e build doesn't ask: the alert would cover the simulator's screenshots.
    if cfg!(feature = "e2e") { return log("e2e: notifications not asked for") }
    let done = RcBlock::new(|granted: Bool, _e: *mut NSError| log(&format!("notifications allowed: {}", granted.as_bool())));
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
}

fn notify_ready() {
    // Whether iOS will show it: 0 not asked, 1 denied, 2 allowed, 3 provisional.
    let said = RcBlock::new(|s: std::ptr::NonNull<objc2_user_notifications::UNNotificationSettings>| {
        log(&format!("notifications: authorization status {}", unsafe { s.as_ref() }.authorizationStatus().0))
    });
    UNUserNotificationCenter::currentNotificationCenter().getNotificationSettingsWithCompletionHandler(&said);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("Your answer is ready"));
    content.setBody(&NSString::from_str("Tap to read it."));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str("answer-ready"), &content, None);
    let done = RcBlock::new(|e: *mut NSError| {
        if e.is_null() { log("notified: answer ready") } else { log(&format!("couldn't notify: {}", unsafe { &*e }.localizedDescription())) }
    });
    UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

// ---- Code Mode: the sandbox WebView and full-screen pages ----

/// The runner's end of the MessagePort, made in its page once it has loaded; runner.js answers `ready`.
const INIT_JS: &str = r#"(() => {
  if (window.__obsPort) return 'already';
  const ch = new MessageChannel();
  ch.port1.onmessage = e => window.webkit.messageHandlers.obsSandbox.postMessage(typeof e.data === 'string' ? e.data : JSON.stringify(e.data));
  window.__obsPort = ch.port1;
  window.dispatchEvent(new MessageEvent('message', {data: '{"type":"init"}', ports: [ch.port2]}));
  return 'init';
})()"#;

const HANDLER: &str = "obsSandbox";

pub struct GuardIvars {
    /// `<base>/sandbox/`: the only pages this WebView may load.
    allowed: String,
    /// A full-screen page: the `render` message for artifact.html. None: the runner.
    page: Option<String>,
    renders: Cell<u32>,
}

define_class!(
    /// The sandbox WebView's navigation delegate and message handler, and the Close button's target.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ObscurifySandboxGuard"]
    #[ivars = GuardIvars]
    pub struct Guard;

    unsafe impl NSObjectProtocol for Guard {}

    unsafe impl WKScriptMessageHandler for Guard {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _c: &WKUserContentController, message: &WKScriptMessage) {
            if self.ivars().page.is_some() { return }
            match unsafe { message.body() }.downcast::<NSString>() {
                Ok(s) => crate::code::on_sandbox(&s.to_string()),
                Err(_) => log("sandbox: a message that isn't text"),
            }
        }
    }

    unsafe impl WKNavigationDelegate for Guard {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(&self, _w: &UIView, action: &WKNavigationAction, handler: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            let url = unsafe { action.request() }.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            let main = unsafe { action.targetFrame() }.is_none_or(|f| unsafe { f.isMainFrame() });
            // Only the sandbox pages at the top; frames may load https, data:, blob: and about: as on Android.
            let ok = url.starts_with(&self.ivars().allowed)
                || (!main && ["https://", "data:", "blob:", "about:"].iter().any(|p| url.starts_with(p)));
            if !ok { log(&format!("sandbox: blocked a navigation to {}", url.chars().take(80).collect::<String>())) }
            handler.call((if ok { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, w: &UIView, _n: Option<&WKNavigation>) {
            let url: Option<Retained<NSURL>> = unsafe { msg_send![w, URL] };
            let url = url.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            if !url.starts_with(&self.ivars().allowed) { return }
            match &self.ivars().page {
                None => { log("sandbox: runner loaded"); eval(w, INIT_JS) }
                // A page that reloads itself is sent again, a few times at most.
                Some(m) => if self.ivars().renders.get() < 5 {
                    self.ivars().renders.set(self.ivars().renders.get() + 1);
                    eval(w, &format!("window.dispatchEvent(new MessageEvent('message', {{data: {}}})); 'render'", serde_json::to_string(m).unwrap_or_default()))
                }
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed(&self, _w: &UIView, _n: Option<&WKNavigation>, e: &NSError) {
            log(&format!("sandbox: a page didn't load: {}", e.localizedDescription()))
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _w: &UIView) {
            log("sandbox: its web process ended");
            // The run fails now rather than at its timeout; the next one starts a new sandbox.
            if self.ivars().page.is_none() {
                crate::code::on_sandbox(r#"{"type":"fatal","message":"The code sandbox stopped. Run it again to start a new one."}"#)
            }
        }
    }

    impl Guard {
        #[unsafe(method(close:))]
        fn close(&self, _sender: Option<&AnyObject>) {
            if let Some(p) = PAGE.take() { p.view.removeFromSuperview() }
        }
    }
);

impl Guard {
    fn new(mtm: MainThreadMarker, allowed: String, page: Option<String>) -> Retained<Self> {
        let this = mtm.alloc::<Guard>().set_ivars(GuardIvars { allowed, page, renders: Cell::new(0) });
        unsafe { msg_send![super(this), init] }
    }
}

struct Shown {
    /// The WebView (the runner), or the full-screen view holding one (a page).
    view: Retained<UIView>,
    web: Retained<UIView>,
    _guard: Retained<Guard>,
}

thread_local! {
    static SANDBOX: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static PAGE: RefCell<Option<Shown>> = const { RefCell::new(None) };
}

fn eval(web: &UIView, js: &str) {
    let done = RcBlock::new(|_r: *mut AnyObject, e: *mut NSError| {
        if !e.is_null() { log(&format!("sandbox: script failed: {}", unsafe { &*e }.localizedDescription())) }
    });
    let _: () = unsafe { msg_send![web, evaluateJavaScript: &*NSString::from_str(js), completionHandler: &*done] };
}

#[allow(deprecated)]
fn window(mtm: MainThreadMarker) -> Option<Retained<UIWindow>> {
    let app = UIApplication::sharedApplication(mtm);
    app.keyWindow().or_else(|| app.windows().firstObject())
}

/// A WKWebView with no bridges (the runner gets the one handler), nothing kept, and the guard as its
/// navigation delegate, loading `page` from `allowed`.
fn locked_web_view(mtm: MainThreadMarker, guard: &Guard, frame: CGRect, handler: bool, page: &str) -> Retained<UIView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe { config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm)) };
    if handler {
        unsafe { config.userContentController().addScriptMessageHandler_name(ProtocolObject::from_ref(guard), &NSString::from_str(HANDLER)) };
    }
    let alloc: Allocated<UIView> = unsafe { msg_send![class!(WKWebView), alloc] };
    let web: Retained<UIView> = unsafe { msg_send![alloc, initWithFrame: frame, configuration: &*config] };
    let delegate: &ProtocolObject<dyn WKNavigationDelegate> = ProtocolObject::from_ref(guard);
    let _: () = unsafe { msg_send![&*web, setNavigationDelegate: delegate] };
    if cfg!(feature = "e2e") {
        // e2e builds: Safari's Web Inspector can look inside (iOS 16.4+).
        let _: () = unsafe { msg_send![&*web, setInspectable: true] };
    }
    let url = format!("{}{page}", guard.ivars().allowed);
    match NSURL::URLWithString(&NSString::from_str(&url)) {
        Some(u) => { let _: Option<Retained<AnyObject>> = unsafe { msg_send![&*web, loadRequest: &*NSURLRequest::requestWithURL(&u)] }; }
        None => log("sandbox: a bad URL"),
    }
    web
}

fn drop_sandbox() {
    if let Some(s) = SANDBOX.take() {
        let config: Retained<WKWebViewConfiguration> = unsafe { msg_send![&*s.web, configuration] };
        unsafe { config.userContentController().removeScriptMessageHandlerForName(&NSString::from_str(HANDLER)) };
        let _: () = unsafe { msg_send![&*s.web, stopLoading] };
        s.view.removeFromSuperview();
    }
}

/// (Re)starts the sandbox from `base`'s /sandbox/ pages; it answers `{type: "ready"}`.
pub fn sandbox_start(base: &str) {
    let allowed = format!("{}/sandbox/", base.trim_end_matches('/'));
    on_main(move |mtm| {
        drop_sandbox();
        let Some(win) = window(mtm) else { return log("sandbox: no window") };
        let guard = Guard::new(mtm, allowed, None);
        let web = locked_web_view(mtm, &guard, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0)), true, "runner.html");
        // In the window (1 pt, all but invisible) so iOS doesn't pause its timers.
        web.setAlpha(0.01);
        web.setUserInteractionEnabled(false);
        win.addSubview(&web);
        SANDBOX.set(Some(Shown { view: web.clone(), web, _guard: guard }));
        log("sandbox: starting");
    })
}

/// Closes the full-screen page (its Close button does the same).
#[cfg(feature = "e2e")]
pub fn close_page() {
    on_main(|_| if let Some(p) = PAGE.take() { p.view.removeFromSuperview() })
}

/// A JSON message for the sandbox (`{type: "run" | "tool_result" | "reset", ...}`).
pub fn sandbox_post(json: String) {
    on_main(move |_| SANDBOX.with_borrow(|s| match s {
        Some(s) => eval(&s.web, &format!("window.__obsPort && window.__obsPort.postMessage({})", serde_json::to_string(&json).unwrap_or_default())),
        None => log("sandbox: a message with no sandbox"),
    }))
}

/// Shows a page the model built, full screen, with its title and Close above it.
pub fn open_page(base: &str, title: &str, html: &str) {
    let allowed = format!("{}/sandbox/", base.trim_end_matches('/'));
    let message = serde_json::json!({"type": "render", "html": html}).to_string();
    let title = if title.trim().is_empty() { "Page".to_string() } else { title.to_string() };
    on_main(move |mtm| {
        if let Some(p) = PAGE.take() { p.view.removeFromSuperview() }
        let Some(win) = window(mtm) else { return log("page: no window") };
        let b = win.bounds();
        let top = win.safeAreaInsets().top;
        let bar = 52.0;
        let fill = UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight;
        let view = UIView::initWithFrame(UIView::alloc(mtm), b);
        view.setAutoresizingMask(fill);
        view.setBackgroundColor(Some(&UIColor::blackColor()));
        let guard = Guard::new(mtm, allowed, Some(message));
        let label = UILabel::initWithFrame(UILabel::alloc(mtm), CGRect::new(CGPoint::new(16.0, top), CGSize::new(b.size.width - 112.0, bar)));
        label.setText(Some(&NSString::from_str(&title)));
        unsafe {
            label.setTextColor(Some(&UIColor::whiteColor()));
            label.setFont(Some(&UIFont::systemFontOfSize(17.0)));
        }
        label.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth);
        let close = UIButton::buttonWithType(UIButtonType::System, mtm);
        close.setFrame(CGRect::new(CGPoint::new(b.size.width - 92.0, top), CGSize::new(84.0, bar)));
        close.setTitle_forState(Some(&NSString::from_str("Close")), UIControlState::Normal);
        close.setTitleColor_forState(Some(&UIColor::colorWithRed_green_blue_alpha(0.0, 0.851, 0.494, 1.0)), UIControlState::Normal);
        close.setAutoresizingMask(UIViewAutoresizing::FlexibleLeftMargin);
        unsafe { close.addTarget_action_forControlEvents(Some(guard.as_ref()), sel!(close:), UIControlEvents::TouchUpInside) };
        let y = top + bar;
        let web = locked_web_view(mtm, &guard, CGRect::new(CGPoint::new(0.0, y), CGSize::new(b.size.width, b.size.height - y)), false, "artifact.html");
        web.setAutoresizingMask(fill);
        view.addSubview(&label);
        view.addSubview(&close);
        view.addSubview(&web);
        win.addSubview(&view);
        PAGE.set(Some(Shown { view, web, _guard: guard }));
        log("page: open");
    })
}
