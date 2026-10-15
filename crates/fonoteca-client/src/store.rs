//! Keeps the [`Account`] in the OS secret store: the Keychain on macOS, the
//! Secret Service (GNOME Keyring, KWallet, KeePassXC) on Linux. It is never
//! written anywhere else, so without a secret store nothing is saved.

use crate::Account;

#[cfg(not(target_family = "wasm"))]
pub use native::*;

#[cfg(target_family = "wasm")]
pub use browser::*;

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::sync::OnceLock;

    use keyring_core::Entry;

    use super::Account;

    fn entry() -> Result<Entry, String> {
        static STORE: OnceLock<Result<(), String>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                #[cfg(test)]
                let store = keyring_core::mock::Store::new();
                #[cfg(all(not(test), target_os = "linux"))]
                let store = zbus_secret_service_keyring_store::Store::new();
                #[cfg(all(not(test), target_os = "macos"))]
                let store = apple_native_keyring_store::keychain::Store::new();
                store
                    .map(|store| keyring_core::set_default_store(store))
                    .map_err(|e| e.to_string())
            })
            .clone()?;
        Entry::new("fonoteca-client", "account").map_err(|e| e.to_string())
    }

    /// The saved account, if there is one and the store hands it out.
    pub async fn load() -> Option<Account> {
        off_the_ui_thread(|| {
            let json = entry()?.get_password().map_err(|e| e.to_string())?;
            serde_json::from_str(&json).map_err(|e| e.to_string())
        })
        .await
        .ok()
    }

    pub async fn save(account: Account) -> Result<(), String> {
        off_the_ui_thread(move || {
            let json = serde_json::to_string(&account).map_err(|e| e.to_string())?;
            entry()?.set_password(&json).map_err(|e| e.to_string())
        })
        .await
    }

    pub async fn forget() -> Result<(), String> {
        off_the_ui_thread(|| match entry()?.delete_credential() {
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
    use super::Account;

    pub async fn load() -> Option<Account> {
        None
    }

    pub async fn save(_: Account) -> Result<(), String> {
        Err("the browser preview doesn't save.".into())
    }

    pub async fn forget() -> Result<(), String> {
        Ok(())
    }
}
