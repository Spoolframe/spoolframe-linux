//! The play queue and context saved per profile (P1-23), so they come back
//! on the next start, paused where they were. A JSON file beside the SDK's
//! data, `queues/<hash>.json` in the app's data directory, named by a
//! hash of the server and the username: usernames need not fit a file
//! name.

use std::time::Duration;

use gtk::glib;
use serde::{Deserialize, Serialize};
use spoolframe_linux_core::backend::Profile;
use spoolframe_linux_core::storage::DataStore;

use crate::queue::Queue;

#[derive(Serialize, Deserialize)]
struct Saved {
    queue: Queue,
    /// Where the current item was, in milliseconds.
    position_ms: u64,
}

pub struct SavedQueue {
    store: DataStore,
    key: String,
}

impl SavedQueue {
    /// The profile's saved queue, in the default data directory; `None`
    /// when there is none (no `HOME`).
    pub fn for_profile(profile: &Profile) -> Option<Self> {
        Some(Self::in_store(
            DataStore::new(DataStore::default_dir()?),
            profile,
        ))
    }

    fn in_store(store: DataStore, profile: &Profile) -> Self {
        let who = format!("{}/{}", profile.server.0, profile.username);
        let hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &who)
            .map(|sum| sum.to_string())
            .unwrap_or_default();
        Self {
            store,
            key: format!("queues/{hash}.json"),
        }
    }

    /// The saved queue and position; `None` when nothing was saved, or the
    /// file cannot be read (logged: a queue is not worth failing for).
    pub fn load(&self) -> Option<(Queue, Duration)> {
        let bytes = match self.store.get(&self.key) {
            Ok(bytes) => bytes?,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the saved queue");
                return None;
            }
        };
        match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) => Some((saved.queue, Duration::from_millis(saved.position_ms))),
            Err(e) => {
                tracing::warn!(error = %e, "the saved queue is unreadable; starting without it");
                None
            }
        }
    }

    pub fn save(&self, queue: &Queue, position: Duration) {
        let saved = Saved {
            queue: queue.clone(),
            position_ms: position.as_millis() as u64,
        };
        let result = serde_json::to_vec(&saved)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                self.store
                    .set(&self.key, Some(&bytes))
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot save the queue");
        }
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::ServerKey;
    use spoolframe_linux_core::test_dir::TestDir;
    use spoolframe_types::{Item, MediaId, MediaKind, Namespace, PlaybackState, Track};

    use super::*;

    fn song(name: &str) -> Item {
        let id = |kind, key: &str| MediaId::new(Namespace::Plex, kind, key).unwrap();
        Item::Track(Track {
            id: id(MediaKind::Track, name),
            album_id: id(MediaKind::Album, "1"),
            artist_id: id(MediaKind::Artist, "1"),
            title: name.into(),
            track: None,
            disc: None,
            duration: None,
            playback: PlaybackState::default(),
        })
    }

    fn profile(username: &str) -> Profile {
        Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: username.into(),
            display_name: username.into(),
            locked: false,
        }
    }

    fn dir(name: &str) -> TestDir {
        TestDir::new_blocking(&format!("spoolframe-saved-queue-{name}"))
    }

    fn titles(queue: &Queue) -> Vec<String> {
        let mut out: Vec<String> = queue.current().iter().map(|i| title(i)).collect();
        out.extend(queue.queued().map(|q| title(&q.item)));
        out
    }

    fn title(item: &Item) -> String {
        match item {
            Item::Track(t) => t.title.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn a_saved_queue_and_position_come_back() {
        let store = dir("back");
        let saved = SavedQueue::in_store(DataStore::new(store.path()), &profile("Andrey"));
        let mut queue = Queue::new(vec![song("a"), song("b")], 1, None, Vec::new());
        queue.add(vec![song("x")], None);
        saved.save(&queue, Duration::from_millis(61_500));
        let (restored, position) = saved.load().unwrap();
        assert_eq!(
            (titles(&restored), position),
            (titles(&queue), Duration::from_millis(61_500))
        );
    }

    #[test]
    fn each_profile_has_its_own_queue() {
        let store = dir("profiles");
        let andrey = SavedQueue::in_store(DataStore::new(store.path()), &profile("andrey"));
        let alice = SavedQueue::in_store(DataStore::new(store.path()), &profile("alice"));
        andrey.save(
            &Queue::new(vec![song("a")], 0, None, Vec::new()),
            Duration::ZERO,
        );
        assert!(alice.load().is_none());
    }

    #[test]
    fn an_unreadable_file_is_no_queue() {
        let store = dir("unreadable");
        let saved = SavedQueue::in_store(DataStore::new(store.path()), &profile("andrey"));
        DataStore::new(store.path())
            .set(&saved.key, Some(b"not json"))
            .unwrap();
        assert!(saved.load().is_none());
    }
}
