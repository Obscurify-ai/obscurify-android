//! The iPhone glue that MainActivity.kt is on Android: links open in the browser, text goes on the
//! clipboard natively (the WebView's clipboard API needs a user gesture, and a copied account code
//! must really be copied), and the screen stays on while an answer is worked on.
//!
//! The iOS app is the store build (`play`, no Tor). Code Mode is off on iOS for now
//! (`code::code_blocked`): it needs a WKWebView of its own with no bridges, as on Android.

#[cfg(not(feature = "play"))]
compile_error!("The iOS app is the store build: build it with --no-default-features --features play");
#[cfg(feature = "tor")]
compile_error!("The iOS app has no Tor: build it with --no-default-features --features play");

use objc2::MainThreadMarker;
use objc2_foundation::NSString;
use objc2_ui_kit::{UIApplication, UIPasteboard};

pub fn open(url: &str) { let _ = webbrowser::open(url); }

pub fn copy(text: &str) { unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(text))) } }

/// An answer started or ended: the screen doesn't lock in the middle of one.
pub fn working(on: bool) {
    if let Some(mtm) = MainThreadMarker::new() {
        UIApplication::sharedApplication(mtm).setIdleTimerDisabled(on)
    }
}
