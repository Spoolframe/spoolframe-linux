//! Each library's sort and filter saved per profile, so a library opens
//! sorted and filtered as it was left, after a restart too: the profile's
//! `library-choices` value in its store, an object of choices by library
//! id. Only choices other than the default are in it; with none, there
//! is no value.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use spoolframe_linux_core::backend::{Client, Profile, ProfileStore};
use spoolframe_linux_core::storage::DataStore;

use crate::library_browse::Choice;
use crate::saved_queue::{profile_key, write};

/// The choices' name in the profile's store.
const CHOICES: &str = "library-choices";

/// One profile's choices, read when its libraries' cache is made and
/// saved on every change.
#[derive(Default)]
pub struct SavedChoices {
    /// Where they are saved; `None` until they are read, and they are
    /// kept for the session only.
    store: Option<Arc<dyn ProfileStore>>,
    choices: HashMap<String, Choice>,
}

impl std::fmt::Debug for SavedChoices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedChoices")
            .field("choices", &self.choices)
            .finish_non_exhaustive()
    }
}

impl SavedChoices {
    /// The profile's saved choices, from its store.
    pub fn for_profile(
        client: &dyn Client,
        profile: &Profile,
    ) -> impl Future<Output = Self> + Send + 'static {
        Self::in_store(
            client.profile_store(&profile.server, &profile.username),
            DataStore::default_dir().map(DataStore::new),
            profile,
        )
    }

    /// The choices in `store`. With none there, the file this app kept
    /// them in before (`library-choices/<hash>.json` in `legacy`) is moved
    /// over, and goes.
    pub fn in_store(
        store: Arc<dyn ProfileStore>,
        legacy: Option<DataStore>,
        profile: &Profile,
    ) -> impl Future<Output = Self> + Send + 'static {
        // TODO(after 2026-11-30): remove the legacy library-choices/ and queues/ files' migration.
        let legacy = legacy.map(|data| (data, profile_key("library-choices", profile)));
        async move {
            let choices = load(store.as_ref(), legacy).await;
            Self {
                store: Some(store),
                choices,
            }
        }
    }

    /// `library`'s choice; `None` for the default.
    pub fn get(&self, library: &str) -> Option<&Choice> {
        self.choices.get(library)
    }

    /// Keeps `choice` for `library` and saves them all. The default takes
    /// the library out, as if it had never been chosen.
    pub fn set(&mut self, library: &str, choice: Choice) {
        if choice == Choice::default() {
            if self.choices.remove(library).is_none() {
                return;
            }
        } else {
            self.choices.insert(library.to_owned(), choice);
        }
        self.save();
    }

    /// The saved choices, read: those chosen while they were being read
    /// stay, and are saved with them.
    pub fn read(&mut self, saved: SavedChoices) {
        let chosen = std::mem::replace(self, saved);
        for (library, choice) in chosen.choices {
            self.set(&library, choice);
        }
    }

    fn save(&self) {
        let Some(store) = &self.store else {
            return;
        };
        if self.choices.is_empty() {
            return write(store.clone(), CHOICES, None);
        }
        match serde_json::to_vec(&self.choices) {
            Ok(bytes) => write(store.clone(), CHOICES, Some(bytes)),
            Err(e) => tracing::warn!(error = %e, "cannot save the libraries' sort and filter"),
        }
    }
}

/// The saved choices; none when nothing was saved, or they cannot be read
/// (logged: the libraries open plain).
async fn load(
    store: &dyn ProfileStore,
    legacy: Option<(DataStore, String)>,
) -> HashMap<String, Choice> {
    let bytes = match store.get(CHOICES).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => match legacy {
            Some((data, key)) => match moved(store, &data, &key).await {
                Some(bytes) => bytes,
                None => return HashMap::new(),
            },
            None => return HashMap::new(),
        },
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the libraries' sort and filter");
            return HashMap::new();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the libraries' sort and filter are unreadable; opening them plain");
        HashMap::new()
    })
}

// TODO(after 2026-11-30): remove the legacy library-choices/ and queues/ files' migration.
/// The choices in file `key` of `data`, from before the store, moved into
/// `store`; the file goes once they are there. `None` with no file.
async fn moved(store: &dyn ProfileStore, data: &DataStore, key: &str) -> Option<Vec<u8>> {
    let bytes = match data.get(key) {
        Ok(bytes) => bytes?,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the libraries' sort and filter saved before");
            return None;
        }
    };
    match store.set(CHOICES, Some(bytes.clone())).await {
        Ok(()) => {
            if let Err(e) = data.set(key, None) {
                tracing::warn!(error = %e, "cannot remove the libraries' sort and filter saved before");
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot move the libraries' sort and filter"),
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::{ItemFilterValues, ItemSort, ServerKey, SortKey};
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::{TestDir, runtime};

    use super::*;
    use crate::saved_queue::written;

    fn profile(username: &str) -> Profile {
        Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: username.into(),
            display_name: username.into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        }
    }

    fn dir(name: &str) -> TestDir {
        TestDir::new_blocking(&format!("spoolframe-saved-choices-{name}"))
    }

    fn store(client: &FakeClient, username: &str) -> Arc<dyn ProfileStore> {
        let profile = profile(username);
        client.profile_store(&profile.server, &profile.username)
    }

    /// `username`'s choices as read now, with `legacy` the data directory
    /// from before the store.
    fn read(client: &FakeClient, legacy: Option<DataStore>, username: &str) -> SavedChoices {
        let store = store(client, username);
        runtime().block_on(SavedChoices::in_store(store, legacy, &profile(username)))
    }

    fn newest() -> Choice {
        Choice {
            sort: ItemSort {
                key: SortKey::Added,
                descending: true,
            },
            ..Choice::default()
        }
    }

    #[test]
    fn a_saved_choice_comes_back() {
        let client = FakeClient::new(None);
        read(&client, None, "andrey").set("movies", newest());
        written();
        assert_eq!(read(&client, None, "andrey").get("movies"), Some(&newest()));
    }

    #[test]
    fn each_profile_has_its_own_choices() {
        let client = FakeClient::new(None);
        read(&client, None, "andrey").set("movies", newest());
        written();
        assert!(read(&client, None, "alice").get("movies").is_none());
    }

    #[test]
    fn choosing_the_default_forgets_the_library() {
        let client = FakeClient::new(None);
        let mut saved = read(&client, None, "andrey");
        saved.set("movies", newest());
        saved.set("shows", newest());
        saved.set("movies", Choice::default());
        written();
        let reopened = read(&client, None, "andrey");
        assert_eq!(
            (reopened.get("movies"), reopened.get("shows")),
            (None, Some(&newest()))
        );
    }

    #[test]
    fn the_last_default_removes_the_value() {
        let client = FakeClient::new(None);
        let mut saved = read(&client, None, "andrey");
        saved.set("movies", newest());
        saved.set("movies", Choice::default());
        written();
        let value = runtime().block_on(store(&client, "andrey").get(CHOICES));
        assert_eq!(value, Ok(None));
    }

    #[test]
    fn an_unreadable_value_is_no_choices() {
        let client = FakeClient::new(None);
        write(
            store(&client, "andrey"),
            CHOICES,
            Some(b"not json".to_vec()),
        );
        written();
        assert!(read(&client, None, "andrey").choices.is_empty());
    }

    #[test]
    fn choices_from_before_the_store_move_into_it_and_their_file_goes() {
        let (client, data) = (FakeClient::new(None), dir("legacy"));
        let key = profile_key("library-choices", &profile("andrey"));
        let file = serde_json::json!({ "movies": newest() });
        DataStore::new(data.path())
            .set(&key, Some(&serde_json::to_vec(&file).unwrap()))
            .unwrap();
        let moved = read(&client, Some(DataStore::new(data.path())), "andrey");
        assert_eq!(
            (
                moved.get("movies"),
                read(&client, None, "andrey").get("movies"),
                DataStore::new(data.path()).get(&key).unwrap(),
            ),
            (Some(&newest()), Some(&newest()), None)
        );
    }

    #[test]
    fn a_choice_made_while_they_are_read_stays() {
        let client = FakeClient::new(None);
        read(&client, None, "andrey").set("shows", newest());
        written();
        let mut shown = SavedChoices::default();
        shown.set("movies", newest());
        shown.read(read(&client, None, "andrey"));
        written();
        let reopened = read(&client, None, "andrey");
        assert_eq!(
            (reopened.get("movies"), reopened.get("shows")),
            (Some(&newest()), Some(&newest()))
        );
    }

    /// A sort a later build saved reads as `Unknown`, which the library
    /// refuses: the page falls back, and forgets it.
    #[test]
    fn a_saved_sort_this_build_does_not_know_is_refused() {
        let client = FakeClient::new(None);
        let file = serde_json::json!({ "movies": { "sort": { "key": "loudness" }, "filter": {} } });
        write(
            store(&client, "andrey"),
            CHOICES,
            Some(serde_json::to_vec(&file).unwrap()),
        );
        written();
        let reopened = read(&client, None, "andrey");
        let values = ItemFilterValues {
            sorts: vec![SortKey::Title, SortKey::Added],
            ..ItemFilterValues::default()
        };
        assert!(matches!(
            crate::library_browse::plan(reopened.get("movies").unwrap(), Some(&values)),
            crate::library_browse::Plan::Refused(_)
        ));
    }
}
