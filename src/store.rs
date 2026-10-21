//! Everything the app keeps lives in its private files dir as JSON: the account number, settings,
//! chats (one file each) and the voucher wallet.

use serde::{Serialize, de::DeserializeOwned};
use std::path::PathBuf;

pub const PACKAGE: &str = "ai.obscurify.android";

pub fn dir() -> PathBuf {
    let d = if cfg!(target_os = "android") {
        // Context.getFilesDir(). Each profile (GrapheneOS users, Private Space, work profile) has its
        // own /data/user/<id>, and the id is the app's uid / 100000. Android makes the dir only when Java asks.
        use std::os::unix::fs::MetadataExt;
        let user = std::fs::metadata("/proc/self").map(|m| m.uid() / 100_000).unwrap_or(0);
        PathBuf::from(format!("/data/user/{user}/{PACKAGE}/files"))
    } else {
        PathBuf::from(std::env::var("OBSCURIFY_DATA").unwrap_or_else(|_| "data".into()))
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read_to_string(dir().join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Writes through a temp file and a rename, so a crash never leaves half a file (the wallet holds money).
pub fn save<T: Serialize>(name: &str, value: &T) {
    let path = dir().join(name);
    let tmp = path.with_extension("tmp");
    if let Ok(s) = serde_json::to_string(value)
        && std::fs::write(&tmp, s).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub fn remove(name: &str) { let _ = std::fs::remove_file(dir().join(name)); }

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
