//! Keeps the [`Account`] and, in an entry of its own, the [`ListenBrainz`]
//! token in the OS secret store: the Keychain on macOS, the
//! Secret Service (GNOME Keyring, KWallet, KeePassXC) on Linux, and on Android
//! the app's preferences, encrypted with a key in the Keystore. It is never
//! written anywhere else, so without a secret store nothing is saved. The
//! token is apart from the account so that signing out or in as someone else
//! leaves it as it is.
//!
//! The browser preview has no secret store and keeps both, unencrypted, in
//! the page's localStorage, the account until sign out and the token until
//! it is removed. A cookie would hand them to the dev server with every
//! request.

use crate::{Account, ListenBrainz};

#[cfg(not(target_family = "wasm"))]
pub use native::*;

#[cfg(target_family = "wasm")]
pub use browser::*;

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::sync::OnceLock;

    use keyring_core::Entry;
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::{Account, ListenBrainz};

    pub const PLACE: &str = "your system's secret store";

    const ACCOUNT: &str = "account";
    const LISTENBRAINZ: &str = "listenbrainz";

    fn entry(name: &str) -> Result<Entry, String> {
        static STORE: OnceLock<Result<(), String>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                #[cfg(test)]
                let store = keyring_core::mock::Store::new();
                #[cfg(all(not(test), target_os = "linux"))]
                let store = zbus_secret_service_keyring_store::Store::new();
                #[cfg(all(not(test), target_os = "macos"))]
                let store = apple_native_keyring_store::keychain::Store::new();
                #[cfg(all(not(test), target_os = "android"))]
                let store = android_native_keyring_store::Store::new();
                store
                    .map(|store| keyring_core::set_default_store(store))
                    .map_err(|e| e.to_string())
            })
            .clone()?;
        Entry::new("fonoteca-client", name).map_err(|e| e.to_string())
    }

    /// The saved account, if there is one and the store hands it out.
    pub async fn load() -> Option<Account> {
        read(ACCOUNT).await
    }

    pub async fn save(account: Account) -> Result<(), String> {
        write(ACCOUNT, account).await
    }

    pub async fn forget() -> Result<(), String> {
        remove(ACCOUNT).await
    }

    /// The saved token, if there is one and the store hands it out.
    pub async fn load_listenbrainz() -> Option<ListenBrainz> {
        read(LISTENBRAINZ).await
    }

    pub async fn save_listenbrainz(listenbrainz: ListenBrainz) -> Result<(), String> {
        write(LISTENBRAINZ, listenbrainz).await
    }

    pub async fn forget_listenbrainz() -> Result<(), String> {
        remove(LISTENBRAINZ).await
    }

    async fn read<T: DeserializeOwned + Send + 'static>(name: &'static str) -> Option<T> {
        off_the_ui_thread(move || {
            let json = entry(name)?.get_password().map_err(|e| e.to_string())?;
            serde_json::from_str(&json).map_err(|e| e.to_string())
        })
        .await
        .ok()
    }

    async fn write<T: Serialize + Send + 'static>(
        name: &'static str,
        value: T,
    ) -> Result<(), String> {
        off_the_ui_thread(move || {
            let json = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            entry(name)?.set_password(&json).map_err(|e| e.to_string())
        })
        .await
    }

    async fn remove(name: &'static str) -> Result<(), String> {
        off_the_ui_thread(move || match entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        })
        .await
    }

    /// The store can wait on an unlock prompt, which must not freeze the window.
    async fn off_the_ui_thread<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|e| e.to_string())?
    }
}

#[cfg(target_family = "wasm")]
mod browser {
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use web_sys::Storage;

    use super::{Account, ListenBrainz};

    pub const PLACE: &str = "this browser";

    const ACCOUNT: &str = "fonoteca-client.account";
    const LISTENBRAINZ: &str = "fonoteca-client.listenbrainz";

    fn storage() -> Result<Storage, String> {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .ok_or_else(|| "this browser blocks site storage.".into())
    }

    pub async fn load() -> Option<Account> {
        read(ACCOUNT)
    }

    pub async fn save(account: Account) -> Result<(), String> {
        write(ACCOUNT, &account)
    }

    pub async fn forget() -> Result<(), String> {
        remove(ACCOUNT)
    }

    pub async fn load_listenbrainz() -> Option<ListenBrainz> {
        read(LISTENBRAINZ)
    }

    pub async fn save_listenbrainz(listenbrainz: ListenBrainz) -> Result<(), String> {
        write(LISTENBRAINZ, &listenbrainz)
    }

    pub async fn forget_listenbrainz() -> Result<(), String> {
        remove(LISTENBRAINZ)
    }

    fn read<T: DeserializeOwned>(key: &str) -> Option<T> {
        let json = storage().ok()?.get_item(key).ok()??;
        serde_json::from_str(&json).ok()
    }

    fn write<T: Serialize>(key: &str, value: &T) -> Result<(), String> {
        let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
        storage()?
            .set_item(key, &json)
            .map_err(|_| "the browser refused to store it.".into())
    }

    fn remove(key: &str) -> Result<(), String> {
        // Without storage nothing was saved.
        let Ok(storage) = storage() else {
            return Ok(());
        };
        storage
            .remove_item(key)
            .map_err(|_| "the browser refused to remove it.".into())
    }
}
