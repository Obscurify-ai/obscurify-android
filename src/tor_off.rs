//! The build without in-app Tor (the Play app): Tor is never on.

pub const ONION: &str = "";

#[derive(Clone, PartialEq, Debug)]
#[allow(dead_code)]
pub enum Status { Off, Connecting, On, Failed(String) }

pub fn always() -> bool { false }
pub fn set_always(_: bool) {}
pub fn wanted() -> bool { false }
pub fn status() -> Status { Status::Off }
pub fn client() -> Option<reqwest::Client> { None }
pub fn start() {}
pub fn stop() {}
