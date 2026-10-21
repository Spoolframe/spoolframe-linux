//! Where the client keeps what must outlive it: secrets in the desktop's
//! keyring, everything else in files. The SDK decides what to store and
//! under which keys (`spoolframe_client_sdk::storage::keys`); this decides
//! where (P1-13, P1-15). The four calls match the SDK's `Storage` trait, so
//! its adapter is a pass-through.
//!
//! Secrets (the device's endpoint key, session tokens) go to the freedesktop
//! Secret Service through oo7, which gnome-keyring, KWallet and KeePassXC
//! provide. Without one, [`Storage::open`] fails: secrets are never written
//! to a plain file instead. Everything else (saved servers, their pins and
//! addresses) is one file per key in the app's data directory ([`crate::paths`]).

use std::path::{Path, PathBuf};

/// Why storage cannot be used. Never "not found": a missing value is
/// `Ok(None)`.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(
        "no Secret Service is running (gnome-keyring, KWallet or KeePassXC); \
         Spoolframe keeps its keys and sessions there: {0}"
    )]
    NoSecretService(String),
    #[error("the keyring refused: {0}")]
    Keyring(String),
    #[error("{path}: {source}")]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Keys are the SDK's own names; anything else is a bug, not user input.
    #[error("{0:?} is not a storage key")]
    BadKey(String),
}

/// Values stored as files, one per key, in a directory.
pub struct DataStore {
    dir: PathBuf,
}

impl DataStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The default: the app's data directory ([`crate::paths::data_dir`]).
    pub fn default_dir() -> Option<PathBuf> {
        crate::paths::data_dir()
    }

    fn path(&self, key: &str) -> Result<PathBuf, StorageError> {
        check_key(key)?;
        Ok(self.dir.join(key))
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let path = self.path(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::File { path, source }),
        }
    }

    /// Writes `value`, or removes the key for `None`. The write goes to a
    /// temporary file renamed over the old one, so a crash never leaves a
    /// half-written value.
    pub fn set(&self, key: &str, value: Option<&[u8]>) -> Result<(), StorageError> {
        let path = self.path(key)?;
        let file_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| StorageError::File { path, source }
        };
        let Some(value) = value else {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(file_error(&path)(e)),
                _ => Ok(()),
            };
        };
        // A key with a `/` lives in a subdirectory; the temporary file goes
        // next to the value, so the rename never crosses directories.
        let dir = path.parent().unwrap_or(&self.dir).to_path_buf();
        std::fs::create_dir_all(&dir).map_err(file_error(&dir))?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let partial = dir.join(format!(".{name}.partial"));
        std::fs::write(&partial, value).map_err(file_error(&partial))?;
        std::fs::rename(&partial, &path).map_err(file_error(&path))
    }
}

/// A key is a relative path of `/`-separated names (the SDK keeps one
/// server's sessions under `sessions/<server key>`). Each name is
/// lowercase letters, digits, `.`, `-` and `_`, not empty and not starting
/// with a dot, so no key is hidden or climbs out with `..`.
fn check_key(key: &str) -> Result<(), StorageError> {
    let fine = key.split('/').all(|name| {
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
    });
    if fine {
        Ok(())
    } else {
        Err(StorageError::BadKey(key.to_owned()))
    }
}

/// The attribute every Spoolframe secret carries, so the client finds its
/// own items and nothing else in the keyring.
const APPLICATION: &str = "casa.coldsteel.Spoolframe";

/// Secrets in the desktop's keyring, and data in files.
pub struct Storage {
    keyring: oo7::Keyring,
    data: DataStore,
}

impl Storage {
    /// Opens the keyring and the data directory. Fails when there is no
    /// Secret Service: the caller says so to the user and stops.
    pub async fn open(data: DataStore) -> Result<Self, StorageError> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| StorageError::NoSecretService(e.to_string()))?;
        keyring
            .unlock()
            .await
            .map_err(|e| StorageError::Keyring(e.to_string()))?;
        Ok(Self { keyring, data })
    }

    pub async fn secret(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        check_key(key)?;
        let items = self
            .keyring
            .search_items(&[("application", APPLICATION), ("key", key)])
            .await
            .map_err(|e| StorageError::Keyring(e.to_string()))?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item
            .secret()
            .await
            .map_err(|e| StorageError::Keyring(e.to_string()))?;
        Ok(Some(secret.as_bytes().to_vec()))
    }

    /// Stores a secret, replacing the old one, or deletes it for `None`.
    pub async fn set_secret(&self, key: &str, value: Option<Vec<u8>>) -> Result<(), StorageError> {
        check_key(key)?;
        let attributes = [("application", APPLICATION), ("key", key)];
        let done = match value {
            Some(value) => {
                self.keyring
                    .create_item(&format!("Spoolframe: {key}"), &attributes, value, true)
                    .await
            }
            None => self.keyring.delete(&attributes).await,
        };
        done.map_err(|e| StorageError::Keyring(e.to_string()))
    }

    pub async fn data(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.data.get(key)
    }

    pub async fn set_data(&self, key: &str, value: Option<Vec<u8>>) -> Result<(), StorageError> {
        self.data.set(key, value.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testing::TestDir;

    /// A store in a directory of the test's own, which is not made until a
    /// value is written.
    fn temp_store(name: &str) -> (TestDir, DataStore) {
        let dir = TestDir::new_blocking(&format!("spoolframe-storage-{name}"));
        let store = DataStore::new(dir.join("data"));
        (dir, store)
    }

    #[test]
    fn a_missing_key_is_none() {
        let (_dir, store) = temp_store("missing");
        assert_eq!(store.get("servers").unwrap(), None);
    }

    #[test]
    fn a_stored_value_reads_back() {
        let (_dir, store) = temp_store("roundtrip");
        store.set("servers", Some(b"[]")).unwrap();
        assert_eq!(store.get("servers").unwrap(), Some(b"[]".to_vec()));
    }

    #[test]
    fn a_new_value_replaces_the_old_one() {
        let (_dir, store) = temp_store("replace");
        store.set("servers", Some(b"one")).unwrap();
        store.set("servers", Some(b"two")).unwrap();
        assert_eq!(store.get("servers").unwrap(), Some(b"two".to_vec()));
    }

    #[test]
    fn setting_none_removes_the_value() {
        let (_dir, store) = temp_store("remove");
        store.set("servers", Some(b"x")).unwrap();
        store.set("servers", None).unwrap();
        assert_eq!(store.get("servers").unwrap(), None);
    }

    #[test]
    fn removing_a_missing_value_is_fine() {
        let (_dir, store) = temp_store("remove-missing");
        store.set("servers", None).unwrap();
    }

    #[test]
    fn a_key_cannot_leave_the_directory() {
        let (_dir, store) = temp_store("escape");
        assert!(matches!(
            store.set("../etc", Some(b"x")),
            Err(StorageError::BadKey(_))
        ));
    }

    #[test]
    fn a_key_with_a_directory_round_trips() {
        let (_dir, store) = temp_store("nested");
        store.set("sessions/9e80325910d054d2", Some(b"x")).unwrap();
        assert_eq!(
            store.get("sessions/9e80325910d054d2").unwrap(),
            Some(b"x".to_vec())
        );
    }

    #[test]
    fn a_key_cannot_climb_out_through_a_directory() {
        assert!(matches!(
            check_key("sessions/../servers"),
            Err(StorageError::BadKey(_))
        ));
    }

    #[test]
    fn a_key_cannot_have_an_empty_name() {
        assert!(matches!(
            check_key("sessions//x"),
            Err(StorageError::BadKey(_))
        ));
    }

    #[test]
    fn a_key_cannot_be_hidden() {
        assert!(matches!(
            check_key(".servers"),
            Err(StorageError::BadKey(_))
        ));
    }

    /// The SDK keeps each profile's offline lock (PX-60) as a secret
    /// under a key of its own, not one of the sessions': it is a key
    /// here, and a value under it reads back.
    #[test]
    fn the_offline_locks_key_round_trips() {
        let key = spoolframe_client_sdk::storage::keys::OFFLINE_LOCKS;
        let (_dir, store) = temp_store("offline-locks");
        store.set(key, Some(b"{}")).unwrap();
        assert_eq!(
            (check_key(key).is_ok(), store.get(key).unwrap()),
            (true, Some(b"{}".to_vec()))
        );
    }

    /// Against the desktop's real keyring; run with `--ignored` in a
    /// session that has one. Leaves nothing behind.
    #[tokio::test]
    #[ignore = "needs a running Secret Service"]
    async fn a_secret_round_trips_through_the_keyring() {
        let dir = spoolframe_test_dir::TestDir::new("spoolframe-storage-keyring").await;
        let storage = Storage::open(DataStore::new(dir.as_ref().join("data")))
            .await
            .unwrap();
        // Shaped like the SDK's session keys, which have a `/`.
        let key = "sessions/test-roundtrip";
        storage
            .set_secret(key, Some(b"hunter2".to_vec()))
            .await
            .unwrap();
        assert_eq!(
            storage.secret(key).await.unwrap(),
            Some(b"hunter2".to_vec())
        );
        storage.set_secret(key, None).await.unwrap();
        assert_eq!(storage.secret(key).await.unwrap(), None);
    }
}
