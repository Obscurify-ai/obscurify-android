//! Everything the app keeps lives in its private files dir as JSON: the account number, settings,
//! chats (one file each) and the voucher wallet.

use serde::{Serialize, de::DeserializeOwned};
use std::path::PathBuf;

/// The app's package: its private files dir is named after it. The Play build is its own app.
/// A test build made with APP_ID (build.sh) is its own app too.
pub const PACKAGE: &str = match option_env!("APP_ID") {
    Some(id) => id,
    None => if cfg!(feature = "play") { "ai.obscurify.play" } else { "ai.obscurify.android" },
};

pub fn dir() -> PathBuf {
    let d = if cfg!(target_os = "android") {
        // Context.getFilesDir(). Each profile (GrapheneOS users, Private Space, work profile) has its
        // own /data/user/<id>, and the id is the app's uid / 100000. Android makes the dir only when Java asks.
        use std::os::unix::fs::MetadataExt;
        let user = std::fs::metadata("/proc/self").map(|m| m.uid() / 100_000).unwrap_or(0);
        PathBuf::from(format!("/data/user/{user}/{PACKAGE}/files"))
    } else if cfg!(target_os = "ios") {
        // The app's own container (iOS sets HOME to it); Application Support is never shown in Files.
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support")
    } else {
        PathBuf::from(std::env::var("OBSCURIFY_DATA").unwrap_or_else(|_| "data".into()))
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read_to_string(dir().join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Kept out of iCloud and computer backups on the iPhone (docs/ios.md, "Backups"): the session and the
/// keys for private chats and chat sync (signing in again brings them back, and a backup that held them
/// could be restored onto another phone already signed in), and the chats themselves (Tony, 2026-11-23).
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub const NOT_BACKED_UP: &[&str] =
    &["session.json", "unlinkable-seed.json", "unlinkable-state.json", "sync-seed.json", "chats.json", "pending.json"];

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn not_backed_up(name: &str) -> bool {
    NOT_BACKED_UP.contains(&name) || (name.starts_with("chat-") && name.ends_with(".json"))
}

/// Writes through a temp file and a rename, so a crash never leaves half a file (the wallet holds money).
pub fn save<T: Serialize>(name: &str, value: &T) {
    let path = dir().join(name);
    let tmp = path.with_extension("tmp");
    if let Ok(s) = serde_json::to_string(value)
        && std::fs::write(&tmp, s).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
        #[cfg(target_os = "ios")]
        if not_backed_up(name) {
            crate::ios::exclude_from_backup(&path)
        }
    }
}

/// The files kept out of backups, as they are now (an older version's, or after an update).
pub fn exclude_secrets_from_backup() {
    #[cfg(target_os = "ios")]
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        if not_backed_up(&e.file_name().to_string_lossy()) { crate::ios::exclude_from_backup(&e.path()) }
    }
}

pub fn remove(name: &str) { let _ = std::fs::remove_file(dir().join(name)); }

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
