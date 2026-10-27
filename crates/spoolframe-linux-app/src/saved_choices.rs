//! Each library's sort and filter saved per profile, so a library opens
//! sorted and filtered as it was left, after a restart too. A JSON file
//! beside the saved queue, `library-choices/<hash>.json` in the app's data
//! directory, named as the queue's is: an object of choices by library id.
//! Only choices other than the default are in it; with none, there is no
//! file.

use std::collections::HashMap;

use spoolframe_linux_core::backend::Profile;
use spoolframe_linux_core::storage::DataStore;

use crate::library_browse::Choice;
use crate::saved_queue::profile_key;

/// One profile's choices, as loaded when its libraries' cache is made and
/// saved on every change.
#[derive(Default)]
pub struct SavedChoices {
    /// Where they are saved; `None` with no data directory (no `HOME`),
    /// and they are kept for the session only.
    file: Option<(DataStore, String)>,
    choices: HashMap<String, Choice>,
}

impl SavedChoices {
    /// The profile's saved choices, in the default data directory.
    pub fn for_profile(profile: &Profile) -> Self {
        match DataStore::default_dir() {
            Some(dir) => Self::in_store(DataStore::new(dir), profile),
            None => Self::default(),
        }
    }

    pub fn in_store(store: DataStore, profile: &Profile) -> Self {
        let key = profile_key("library-choices", profile);
        let choices = load(&store, &key);
        Self {
            file: Some((store, key)),
            choices,
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

    fn save(&self) {
        let Some((store, key)) = &self.file else {
            return;
        };
        let result = if self.choices.is_empty() {
            store.set(key, None).map_err(|e| e.to_string())
        } else {
            serde_json::to_vec(&self.choices)
                .map_err(|e| e.to_string())
                .and_then(|bytes| store.set(key, Some(&bytes)).map_err(|e| e.to_string()))
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot save the libraries' sort and filter");
        }
    }
}

/// The saved choices; none when nothing was saved, or the file cannot be
/// read (logged: the libraries open plain).
fn load(store: &DataStore, key: &str) -> HashMap<String, Choice> {
    let bytes = match store.get(key) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return HashMap::new(),
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

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::{ItemFilterValues, ItemSort, ServerKey, SortKey};
    use spoolframe_linux_core::testing::TestDir;

    use super::*;

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
        let store = dir("back");
        SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"))
            .set("movies", newest());
        let reopened = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        assert_eq!(reopened.get("movies"), Some(&newest()));
    }

    #[test]
    fn each_profile_has_its_own_choices() {
        let store = dir("profiles");
        SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"))
            .set("movies", newest());
        let alice = SavedChoices::in_store(DataStore::new(store.path()), &profile("alice"));
        assert!(alice.get("movies").is_none());
    }

    #[test]
    fn choosing_the_default_forgets_the_library() {
        let store = dir("default");
        let mut saved = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        saved.set("movies", newest());
        saved.set("shows", newest());
        saved.set("movies", Choice::default());
        let reopened = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        assert_eq!(
            (reopened.get("movies"), reopened.get("shows")),
            (None, Some(&newest()))
        );
    }

    #[test]
    fn the_last_default_removes_the_file() {
        let store = dir("last");
        let mut saved = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        saved.set("movies", newest());
        saved.set("movies", Choice::default());
        let (_, key) = saved.file.as_ref().unwrap();
        assert!(DataStore::new(store.path()).get(key).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_file_is_no_choices() {
        let store = dir("unreadable");
        let saved = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        let (_, key) = saved.file.as_ref().unwrap();
        DataStore::new(store.path())
            .set(key, Some(b"not json"))
            .unwrap();
        let reopened = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        assert!(reopened.choices.is_empty());
    }

    /// A sort a later build saved reads as `Unknown`, which the library
    /// refuses: the page falls back, and forgets it.
    #[test]
    fn a_saved_sort_this_build_does_not_know_is_refused() {
        let store = dir("unknown");
        let saved = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
        let (_, key) = saved.file.as_ref().unwrap();
        let file = serde_json::json!({ "movies": { "sort": { "key": "loudness" }, "filter": {} } });
        DataStore::new(store.path())
            .set(key, Some(&serde_json::to_vec(&file).unwrap()))
            .unwrap();
        let reopened = SavedChoices::in_store(DataStore::new(store.path()), &profile("andrey"));
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
