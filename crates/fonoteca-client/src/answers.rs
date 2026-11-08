//! The answers the pages read, kept on this device for the account signed
//! in, so that the first pages after a start show at once too: a file for
//! each account in an answers folder in the app's data folder, beside the
//! downloads, removed on sign out. Its name doesn't say whose it is.
//!
//! The browser keeps them in memory only. Its localStorage holds about 5 MB,
//! and a library of 640 albums comes to 27 MB once every page has been seen.
//!
//! What ListenBrainz answered is kept the same way, in a file of its own per
//! account: a scan forgets every page kept, and a heart synced from
//! ListenBrainz stars songs while its loves are read.

// The browser keeps nothing between starts, so there only the clock is used.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opensubsonic::Answers;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// How far the flow test has moved its clock on, in seconds.
#[cfg(test)]
pub static LATER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Held while the answers are written or removed, so that a removal can't be
/// overtaken by a write that started before it.
static WRITING: Mutex<()> = Mutex::new(());

/// The answers kept for `account`, read off the UI thread: once every page
/// has been seen they are megabytes. The pages' calls wait until they are
/// in. None when there are none or they can't be read.
#[cfg(not(target_family = "wasm"))]
pub fn load(account: &str) -> Arc<Answers> {
    let answers = Arc::new(Answers::loading(now));
    let (file, loading) = (file(account), answers.clone());
    tokio::task::spawn_blocking(move || {
        let saved = file.and_then(|file| std::fs::read_to_string(file).ok());
        loading.load(saved.as_deref());
    });
    answers
}

#[cfg(target_family = "wasm")]
pub fn load(_: &str) -> Arc<Answers> {
    Arc::new(Answers::new(now))
}

/// `answers` once those saved on this device are in, waited for off the UI
/// thread.
#[cfg(not(target_family = "wasm"))]
pub async fn loaded(answers: Arc<Answers>) -> Arc<Answers> {
    if !answers.is_loaded() {
        let loading = answers.clone();
        tokio::task::spawn_blocking(move || loading.wait_until_loaded())
            .await
            .expect("waiting doesn't panic");
    }
    answers
}

#[cfg(target_family = "wasm")]
pub async fn loaded(answers: Arc<Answers>) -> Arc<Answers> {
    answers
}

/// Writes the answers for `account` if they changed since they last were,
/// off the UI thread. One that fails is written at the next save.
#[cfg(not(target_family = "wasm"))]
pub fn save(account: &str, answers: &Arc<Answers>) {
    let Some(file) = file(account) else { return };
    let answers = answers.clone();
    tokio::task::spawn_blocking(move || {
        let _writing = WRITING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(unsaved) = answers.unsaved() else {
            return;
        };
        match write(&file, &unsaved.json) {
            Ok(()) => answers.saved(&unsaved),
            // Meanwhile a page whose answer isn't kept asks the server.
            Err(e) => eprintln!("The pages' answers couldn't be kept: {e}"),
        }
    });
}

/// Forgets the answers kept for `account`, on this device and in `answers`,
/// which saves none from now on.
pub fn forget(account: &str, answers: &Answers) -> io::Result<()> {
    answers.forget();
    remove(account)
}

/// Forgets the answers kept for `account`, on this device and in `answers`,
/// which goes on keeping those asked for from now on.
pub fn clear(account: &str, answers: &Answers) -> io::Result<()> {
    answers.clear();
    remove(account)
}

/// Removes the answers saved for `account` on this device.
fn remove(account: &str) -> io::Result<()> {
    let Some(file) = file(account) else {
        return Ok(());
    };
    let _writing = WRITING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What `answers` keep under `key` as a `T`, and whether it is aged; none
/// when nothing is, or when it was kept in another shape.
pub fn find<T: DeserializeOwned>(answers: &Answers, key: &str) -> Option<(T, bool)> {
    let found = answers.find(key)?;
    let value = serde_json::from_value(Value::Object(found.answer)).ok()?;
    Some((value, found.aged))
}

/// Keeps `value` under `key`, fresh, unless the answers were cleared since
/// there were `writes`.
pub fn keep<T: Serialize>(answers: &Answers, key: String, value: &T, writes: u64) {
    answers.keep(key, &object(value), writes);
}

/// Puts `value` in place of what is kept under `key`, as old as that, when
/// anything is.
pub fn amend<T: Serialize>(answers: &Answers, key: &str, value: &T) {
    answers.amend(key, &object(value));
}

/// `value` as the JSON object the answers keep.
fn object<T: Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value).expect("kept values are JSON") {
        Value::Object(object) => object,
        _ => panic!("kept values are JSON objects"),
    }
}

/// Seconds since 1970, the clock the answers' age is told by.
pub fn now() -> u64 {
    let now = crate::prefs::now();
    #[cfg(test)]
    let now = now + LATER.load(std::sync::atomic::Ordering::Relaxed);
    now
}

/// Written beside it and moved over it, so a crash halfway leaves the old
/// file whole.
fn write(file: &std::path::Path, json: &str) -> io::Result<()> {
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let partial = file.with_extension("json.partial");
    std::fs::write(&partial, json)?;
    std::fs::rename(&partial, file)
}

/// The answers folder beside the downloads in the app's data folder.
#[cfg(not(any(target_family = "wasm", test)))]
fn file(account: &str) -> Option<PathBuf> {
    let folder = crate::offline::folder()?.with_file_name("answers");
    Some(folder.join(format!("{account}.json")))
}

#[cfg(target_family = "wasm")]
fn file(_: &str) -> Option<PathBuf> {
    None
}

/// Each test process keeps its own.
#[cfg(test)]
pub fn file(account: &str) -> Option<PathBuf> {
    let folder = std::env::temp_dir().join(format!("fonoteca-answers-{}", std::process::id()));
    Some(folder.join(format!("{account}.json")))
}
