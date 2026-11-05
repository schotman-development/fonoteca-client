//! The answers the pages read, kept on this device for the account signed
//! in, so that the first pages after a start show at once too: a file for
//! each account in an answers folder in the app's data folder, beside the
//! downloads, removed on sign out. Its name doesn't say whose it is.
//!
//! The browser keeps them in memory only. Its localStorage holds about 5 MB,
//! and a library of 640 albums comes to 27 MB once every page has been seen.

// The browser keeps nothing between starts, so there only the clock is used.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opensubsonic::Answers;

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
