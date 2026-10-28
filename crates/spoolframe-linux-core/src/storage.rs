//! Where the client keeps what must outlive it: secrets in the desktop's
//! keyring, everything else in files. The SDK decides what to store and
//! under which keys (`spoolframe_client_sdk::storage::keys`); this decides
//! where (P1-13, P1-15). The four calls match the SDK's `Storage` trait, so
//! its adapter is a pass-through.
//!
//! Secrets (the device's endpoint key, session tokens) go to the freedesktop
//! Secret Service through libsecret, which gnome-keyring, KWallet and
//! KeePassXC provide. In a Flatpak, libsecret keeps them in a file of the
//! app's own (`keyrings/default.keyring` in its data directory), encrypted
//! with a key the Secret portal holds. Without either, [`Storage::open`]
//! fails: secrets are never written to a plain file instead. Everything
//! else (saved servers, their pins and addresses) is one file per key in
//! the app's data directory ([`crate::paths`]).
//!
//! Secrets stored by oo7, which the client used before, are found as they
//! are: the same two attributes, the same default collection, and in a
//! Flatpak the same file, whose format oo7 wrote to be libsecret's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use libsecret::prelude::*;

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
/// own items and nothing else in the keyring. Also the schema's name. It
/// is the app's id.
const APPLICATION: &str = "app.spoolframe.relm4";

/// The app's id until 0.6.1, which the secrets from before carry. A key
/// not found under [`APPLICATION`] is read under this one and moved: stored
/// again under the new id, then deleted here (see [`keyring::lookup`]).
const LEGACY_APPLICATION: &str = "casa.coldsteel.Spoolframe";

/// What oo7 stored a `Vec<u8>` as, kept so an item reads the same to any
/// other Secret Service client whichever library wrote it.
const CONTENT_TYPE: &std::ffi::CStr = c"application/octet-stream";

/// The two attributes every secret is stored and found by, byte for byte
/// those the client gave oo7: `application` and `key`.
fn attributes(key: &str) -> HashMap<&str, &str> {
    attributes_of(APPLICATION, key)
}

/// The same attributes as the secrets from before the app's id changed.
fn legacy_attributes(key: &str) -> HashMap<&str, &str> {
    attributes_of(LEGACY_APPLICATION, key)
}

fn attributes_of<'a>(application: &'a str, key: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("application", application), ("key", key)])
}

/// The item's label, as the keyring's own app shows it.
fn label(key: &str) -> String {
    format!("Spoolframe: {key}")
}

/// libsecret checks attributes against a schema. Storing through one also
/// adds `xdg:schema` to the Secret Service's item, which oo7 never did:
/// `DONT_MATCH_NAME` leaves it out of searches, so the client's items from
/// before match too.
fn schema() -> libsecret::Schema {
    use libsecret::SchemaAttributeType::String;
    libsecret::Schema::new(
        APPLICATION,
        libsecret::SchemaFlags::DONT_MATCH_NAME,
        HashMap::from([("application", String), ("key", String)]),
    )
}

/// `bytes` as libsecret's value, any bytes: the binding's own constructor
/// takes only UTF-8, and a device key is not text.
fn secret_value(bytes: &[u8]) -> libsecret::Value {
    use glib::translate::FromGlibPtrFull as _;
    let length = isize::try_from(bytes.len()).expect("a slice is never over isize::MAX bytes");
    // An empty slice's pointer is dangling, and C may not even copy zero
    // bytes from one.
    let start = if bytes.is_empty() {
        c"".as_ptr()
    } else {
        bytes.as_ptr().cast()
    };
    // SAFETY: secret_value_new copies `length` bytes from `start` (live
    // for the call) and the NUL-terminated content type, and returns a new
    // reference, which from_glib_full takes over.
    unsafe {
        libsecret::Value::from_glib_full(libsecret::ffi::secret_value_new(
            start,
            length,
            CONTENT_TYPE.as_ptr(),
        ))
    }
}

/// Of the items a search found, the one written last: until a store
/// tidies up, the item oo7 wrote and libsecret's can both be there (see
/// [`keyring::store`]).
fn newest<T>(items: impl IntoIterator<Item = T>, modified: impl Fn(&T) -> u64) -> Option<T> {
    items.into_iter().max_by_key(|item| modified(item))
}

/// A keyring error, saying what was being done to which key.
fn keyring_error(doing: &str, key: &str) -> impl FnOnce(glib::Error) -> StorageError {
    let context = format!("{doing} {key:?}");
    move |e| StorageError::Keyring(format!("{context}: {e}"))
}

/// libsecret's blocking calls. Each runs on tokio's blocking pool, not on
/// a runtime or the GTK thread: it may wait on D-Bus and on an unlock
/// prompt the user answers. They run one at a time, so two calls never
/// raise two prompts.
mod keyring {
    use super::*;

    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    const NO_CANCEL: Option<&gio::Cancellable> = None;

    /// Runs `call` off the async threads, after the one before it.
    pub(super) async fn blocking<T: Send + 'static>(
        call: impl FnOnce() -> Result<T, StorageError> + Send + 'static,
    ) -> Result<T, StorageError> {
        tokio::task::spawn_blocking(move || {
            let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
            call()
        })
        .await
        .map_err(|e| StorageError::Keyring(format!("the keyring call stopped: {e}")))?
    }

    /// Reaches the keyring and unlocks the client's items, as opening the
    /// keyring did with oo7: a search, which starts the Secret Service (or
    /// in a Flatpak asks the portal for the file's key).
    pub(super) fn reach() -> Result<(), glib::Error> {
        let flags = libsecret::SearchFlags::ALL | libsecret::SearchFlags::UNLOCK;
        // The items from before the app's id changed are unlocked too, so
        // moving them later raises no prompt of its own.
        for application in [APPLICATION, LEGACY_APPLICATION] {
            let mine = HashMap::from([("application", application)]);
            libsecret::password_search_sync(Some(&schema()), mine, flags, NO_CANCEL)?;
        }
        Ok(())
    }

    /// `key`'s value: under the app's id, or else under its id from before,
    /// which then moves to the new one. A move that fails is logged and the
    /// value still read: it moves on the next lookup.
    pub(super) fn lookup(key: &str) -> Result<Option<Vec<u8>>, glib::Error> {
        if let Some(value) = find(attributes(key))? {
            return Ok(Some(value));
        }
        let Some(value) = find(legacy_attributes(key))? else {
            return Ok(None);
        };
        match store(key, &value) {
            Ok(()) => tracing::info!(key, "secret moved to the app's new id"),
            Err(e) => {
                tracing::warn!(key, error = %e, "secret read under the app's old id, not moved")
            }
        }
        Ok(Some(value))
    }

    /// The newest item's secret among those with `attributes`.
    fn find(attributes: HashMap<&str, &str>) -> Result<Option<Vec<u8>>, glib::Error> {
        let flags = libsecret::SearchFlags::ALL | libsecret::SearchFlags::UNLOCK;
        let items = libsecret::password_search_sync(Some(&schema()), attributes, flags, NO_CANCEL)?;
        let Some(item) = newest(items, |item| item.modified()) else {
            return Ok(None);
        };
        Ok(item
            .retrieve_secret_sync(NO_CANCEL)?
            .map(|value| value.get()))
    }

    /// Stores `value` in the default collection over the old one.
    ///
    /// libsecret replaces an item only when its attributes are the same
    /// set, and the item oo7 wrote has another (no `xdg:schema` on the
    /// Secret Service; an `xdg:content-type` in a Flatpak's file). So the
    /// first store after the move from oo7 leaves two: then both go and
    /// the new value is stored again, alone. The new one is written first,
    /// so no moment is without it but that one.
    pub(super) fn store(key: &str, value: &[u8]) -> Result<(), glib::Error> {
        let (schema, value, label) = (schema(), secret_value(value), label(key));
        let put = || {
            libsecret::password_store_binary_sync(
                Some(&schema),
                attributes(key),
                None,
                &label,
                &value,
                NO_CANCEL,
            )
        };
        put()?;
        let all = libsecret::password_search_sync(
            Some(&schema),
            attributes(key),
            libsecret::SearchFlags::ALL,
            NO_CANCEL,
        )?;
        if all.len() > 1 {
            libsecret::password_clear_sync(Some(&schema), attributes(key), NO_CANCEL)?;
            put()?;
        }
        // An item under the old id would be read again once this one goes.
        libsecret::password_clear_sync(Some(&schema), legacy_attributes(key), NO_CANCEL)?;
        Ok(())
    }

    /// Deletes every item of `key`, under the app's id from before too.
    pub(super) fn clear(key: &str) -> Result<(), glib::Error> {
        libsecret::password_clear_sync(Some(&schema()), attributes(key), NO_CANCEL)?;
        libsecret::password_clear_sync(Some(&schema()), legacy_attributes(key), NO_CANCEL)
    }
}

/// Secrets in the desktop's keyring, and data in files.
pub struct Storage {
    data: DataStore,
}

impl Storage {
    /// Opens the keyring and the data directory. Fails when there is no
    /// Secret Service: the caller says so to the user and stops.
    pub async fn open(data: DataStore) -> Result<Self, StorageError> {
        keyring::blocking(|| {
            keyring::reach().map_err(|e| StorageError::NoSecretService(e.to_string()))
        })
        .await?;
        Ok(Self { data })
    }

    pub async fn secret(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        check_key(key)?;
        let key = key.to_owned();
        keyring::blocking(move || keyring::lookup(&key).map_err(keyring_error("reading", &key)))
            .await
    }

    /// Stores a secret, replacing the old one, or deletes it for `None`.
    pub async fn set_secret(&self, key: &str, value: Option<Vec<u8>>) -> Result<(), StorageError> {
        check_key(key)?;
        let key = key.to_owned();
        keyring::blocking(move || match value {
            Some(value) => keyring::store(&key, &value).map_err(keyring_error("storing", &key)),
            None => keyring::clear(&key).map_err(keyring_error("deleting", &key)),
        })
        .await
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

    /// The attributes oo7 stored the client's secrets under, exactly, with
    /// the app's id: one more or one renamed and none of them is found.
    #[test]
    fn a_secret_is_found_by_application_and_key_alone() {
        assert_eq!(
            attributes("sessions/9e80325910d054d2"),
            HashMap::from([
                ("application", "app.spoolframe.relm4"),
                ("key", "sessions/9e80325910d054d2"),
            ])
        );
    }

    /// A secret from before the app's id changed is looked for under the
    /// old id, with the same key.
    #[test]
    fn a_secret_from_before_is_found_under_the_old_id() {
        assert_eq!(
            legacy_attributes("device-key"),
            HashMap::from([
                ("application", "casa.coldsteel.Spoolframe"),
                ("key", "device-key"),
            ])
        );
    }

    #[test]
    fn an_items_label_names_its_key() {
        assert_eq!(label("device-key"), "Spoolframe: device-key");
    }

    /// The schema's name is left out of searches: oo7's items have no
    /// `xdg:schema`, and a search with one would miss every one of them.
    #[test]
    fn the_schema_does_not_match_on_its_name() {
        use glib::translate::ToGlibPtr as _;
        let schema = schema();
        let raw: *const libsecret::ffi::SecretSchema = schema.to_glib_none().0;
        // SAFETY: `schema` owns the struct and outlives the read.
        let flags = unsafe { (*raw).flags };
        assert_eq!(
            flags,
            libsecret::ffi::SECRET_SCHEMA_DONT_MATCH_NAME,
            "flags of {}",
            APPLICATION
        );
    }

    /// A device key is any 32 bytes, NULs and all, not text.
    #[test]
    fn a_binary_secret_survives_libsecrets_value() {
        let key: Vec<u8> = (0..=255).rev().step_by(8).chain([0, 0]).collect();
        let value = secret_value(&key);
        assert_eq!(
            (value.get(), value.content_type().as_str()),
            (key, "application/octet-stream")
        );
    }

    #[test]
    fn an_empty_secret_survives_libsecrets_value() {
        assert_eq!(secret_value(&[]).get(), Vec::<u8>::new());
    }

    /// The item oo7 wrote and the one libsecret wrote after it: the
    /// later one is read.
    #[test]
    fn of_two_items_the_one_written_last_is_read() {
        let items = [("oo7's", 1_700_000_000), ("libsecret's", 1_800_000_000)];
        assert_eq!(
            newest(items, |(_, modified)| *modified).map(|(who, _)| who),
            Some("libsecret's")
        );
    }

    #[test]
    fn no_items_is_no_secret() {
        assert_eq!(newest(Vec::<u64>::new(), |m| *m), None);
    }

    #[test]
    fn a_keyring_error_says_what_was_done_to_which_key() {
        let e = glib::Error::new(gio::IOErrorEnum::Failed, "the prompt was dismissed");
        assert_eq!(
            keyring_error("storing", "sessions/x")(e).to_string(),
            "the keyring refused: storing \"sessions/x\": the prompt was dismissed"
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
