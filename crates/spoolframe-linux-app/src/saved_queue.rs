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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistResume {
    pub playing: spoolframe_linux_core::playlists::Playing,
    pub settings: Option<spoolframe_linux_core::backend::PlaySettings>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    queue: Queue,
    /// Where the current item was, in milliseconds.
    position_ms: u64,
    #[serde(default)]
    playlist: Option<PlaylistResume>,
}

/// The profile's file in `dir`: `<dir>/<hash>.json`, named by a hash of
/// the server and the username, as usernames need not fit a file name.
pub fn profile_key(dir: &str, profile: &Profile) -> String {
    let who = format!("{}/{}", profile.server.0, profile.username);
    let hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &who)
        .map(|sum| sum.to_string())
        .unwrap_or_default();
    format!("{dir}/{hash}.json")
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
        Self {
            store,
            key: profile_key("queues", profile),
        }
    }

    /// The saved queue and position; `None` when nothing was saved, or the
    /// file cannot be read (logged: a queue is not worth failing for).
    pub fn load(&self) -> Option<(Queue, Duration, Option<PlaylistResume>)> {
        let bytes = match self.store.get(&self.key) {
            Ok(bytes) => bytes?,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the saved queue");
                return None;
            }
        };
        match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) => Some((
                saved.queue,
                Duration::from_millis(saved.position_ms),
                saved.playlist,
            )),
            Err(e) => {
                tracing::warn!(error = %e, "the saved queue is unreadable; starting without it");
                None
            }
        }
    }

    pub fn save(&self, queue: &Queue, position: Duration, playlist: Option<PlaylistResume>) {
        let saved = Saved {
            playlist,
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
    use spoolframe_linux_core::testing::TestDir;
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
            codec: None,
            bit_depth: None,
            sample_rate: None,
            album_track_count: None,
            artist_name: None,
            album_title: None,
            artwork: Vec::new(),
        })
    }

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
        saved.save(&queue, Duration::from_millis(61_500), None);
        let (restored, position, playlist) = saved.load().unwrap();
        assert!(playlist.is_none());
        assert_eq!(
            (titles(&restored), position),
            (titles(&queue), Duration::from_millis(61_500))
        );
    }

    #[test]
    fn a_playlist_context_and_modes_survive_restarting_and_old_saves_still_load() {
        let store = dir("playlist");
        let saved = SavedQueue::in_store(DataStore::new(store.path()), &profile("andrey"));
        let queue = Queue::new(vec![song("a")], 0, None, Vec::new());
        let playlist = PlaylistResume {
            playing: spoolframe_linux_core::playlists::Playing {
                id: "playlist-id".into(),
                name: "Favorites".into(),
                entry: 42,
            },
            settings: Some(spoolframe_linux_core::backend::PlaySettings {
                shuffle: true,
                repeat: spoolframe_linux_core::backend::Repeat::All,
                skip_watched: false,
            }),
        };
        saved.save(&queue, Duration::from_secs(12), Some(playlist.clone()));
        let reopened = SavedQueue::in_store(DataStore::new(store.path()), &profile("andrey"));
        let (_, at, restored) = reopened.load().unwrap();
        assert_eq!(at, Duration::from_secs(12));
        assert_eq!(restored, Some(playlist));
        let old = serde_json::json!({ "queue": queue, "position_ms": 12000 });
        DataStore::new(store.path())
            .set(&saved.key, Some(&serde_json::to_vec(&old).unwrap()))
            .unwrap();
        assert!(reopened.load().unwrap().2.is_none());
    }

    #[test]
    fn each_profile_has_its_own_queue() {
        let store = dir("profiles");
        let andrey = SavedQueue::in_store(DataStore::new(store.path()), &profile("andrey"));
        let alice = SavedQueue::in_store(DataStore::new(store.path()), &profile("alice"));
        andrey.save(
            &Queue::new(vec![song("a")], 0, None, Vec::new()),
            Duration::ZERO,
            None,
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
