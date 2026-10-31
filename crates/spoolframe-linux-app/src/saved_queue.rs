//! The play queue and context saved per profile (P1-23), so they come back
//! on the next start, paused where they were: the profile's `queue` value
//! in its store ([`Client::profile_store`]). Saves go to the runtime, one
//! after another, and quitting waits for the last ([`shutdown`]).

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use gtk::glib;
use serde::{Deserialize, Serialize};
use spoolframe_linux_core::backend::{Client, Profile, ProfileStore};
use spoolframe_linux_core::storage::DataStore;

use crate::queue::Queue;

/// The queue's name in the profile's store.
const QUEUE: &str = "queue";

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

/// What [`SavedQueue::load`] gives back: the queue, where its current item
/// was, and the playlist it played from.
pub type Restored = (Queue, Duration, Option<PlaylistResume>);

// TODO(after 2026-11-30): remove the legacy library-choices/ and queues/ files' migration.
/// The profile's file in `dir` of the app's data directory, as this app
/// kept them before the profile's store: `<dir>/<hash>.json`, named by a
/// hash of the server and the username (the SDK's [`profile_hash`]).
///
/// [`profile_hash`]: spoolframe_client_sdk::profile_hash
pub fn profile_key(dir: &str, profile: &Profile) -> String {
    let who = format!("{}/{}", profile.server.0, profile.username);
    let hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &who)
        .map(|sum| sum.to_string())
        .unwrap_or_default();
    format!("{dir}/{hash}.json")
}

pub struct SavedQueue {
    store: Arc<dyn ProfileStore>,
    // TODO(after 2026-11-30): remove the legacy library-choices/ and queues/ files' migration.
    /// The queue's file from before the store, deleted unread.
    legacy: Option<(DataStore, String)>,
}

impl SavedQueue {
    /// The profile's saved queue, in its store.
    pub fn for_profile(client: &dyn Client, profile: &Profile) -> Self {
        Self::in_store(
            client.profile_store(&profile.server, &profile.username),
            DataStore::default_dir().map(DataStore::new),
            profile,
        )
    }

    fn in_store(
        store: Arc<dyn ProfileStore>,
        legacy: Option<DataStore>,
        profile: &Profile,
    ) -> Self {
        Self {
            store,
            legacy: legacy.map(|data| (data, profile_key("queues", profile))),
        }
    }

    /// The saved queue and position; `None` when nothing was saved, or it
    /// cannot be read (logged: a queue is not worth failing for). A queue
    /// file from before the store is not moved over: it goes.
    pub fn load(&self) -> impl Future<Output = Option<Restored>> + Send + 'static {
        let store = self.store.clone();
        let legacy = self.legacy.clone();
        async move {
            if let Some((data, key)) = legacy
                && let Err(e) = data.set(&key, None)
            {
                tracing::warn!(error = %e, "cannot remove the queue saved before");
            }
            let bytes = match store.get(QUEUE).await {
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
    }

    pub fn save(&self, queue: &Queue, position: Duration, playlist: Option<PlaylistResume>) {
        let saved = Saved {
            playlist,
            queue: queue.clone(),
            position_ms: position.as_millis() as u64,
        };
        match serde_json::to_vec(&saved) {
            Ok(bytes) => write(self.store.clone(), QUEUE, Some(bytes)),
            Err(e) => tracing::warn!(error = %e, "cannot save the queue"),
        }
    }
}

thread_local! {
    /// The last write [`write`] began, which waits for the one before it.
    static WRITING: RefCell<Option<tokio::task::JoinHandle<()>>> = const { RefCell::new(None) };
}

/// Sets value `name` of `store` on the runtime, once the write before it
/// is done: a later save never lands under an earlier one.
pub fn write(store: Arc<dyn ProfileStore>, name: &'static str, value: Option<Vec<u8>>) {
    WRITING.with(|writing| {
        let before = writing.borrow_mut().take();
        let write = relm4::spawn(async move {
            if let Some(before) = before {
                let _ = before.await;
            }
            if let Err(e) = store.set(name, value).await {
                tracing::warn!(error = %e, name, "cannot save the profile's value");
            }
        });
        *writing.borrow_mut() = Some(write);
    });
}

/// The last write begun, and with it every one before; `None` when all
/// are done.
fn last_write() -> Option<tokio::task::JoinHandle<()>> {
    WRITING.with(|writing| writing.borrow_mut().take())
}

/// Waits, up to [`SHUTDOWN_WAIT`], for the saves still being written:
/// left, they would be cancelled when the process exits, and the queue
/// saved last lost. Call once the app's main loop has returned.
pub fn shutdown() {
    let Some(last) = last_write() else {
        return;
    };
    let waited =
        glib::MainContext::default().block_on(glib::future_with_timeout(SHUTDOWN_WAIT, last));
    if waited.is_err() {
        tracing::warn!("the last queue save was not written before quitting");
    }
}

/// How long quitting waits for the last save.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

/// The saves begun, written.
#[cfg(test)]
pub(crate) fn written() {
    if let Some(last) = last_write() {
        spoolframe_linux_core::testing::runtime()
            .block_on(last)
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::ServerKey;
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::{TestDir, runtime};
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

    /// `username`'s saved queue in `client`'s store, with no files from
    /// before it.
    fn saved(client: &FakeClient, username: &str) -> SavedQueue {
        SavedQueue::for_test(client, None, username)
    }

    impl SavedQueue {
        fn for_test(client: &FakeClient, legacy: Option<DataStore>, username: &str) -> Self {
            let profile = profile(username);
            Self::in_store(
                client.profile_store(&profile.server, &profile.username),
                legacy,
                &profile,
            )
        }
    }

    fn load(saved: &SavedQueue) -> Option<Restored> {
        runtime().block_on(saved.load())
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
        let client = FakeClient::new(None);
        let saved = saved(&client, "Andrey");
        let mut queue = Queue::new(vec![song("a"), song("b")], 1, None, Vec::new());
        queue.add(vec![song("x")], None);
        saved.save(&queue, Duration::from_millis(61_500), None);
        written();
        let (restored, position, playlist) = load(&saved).unwrap();
        assert!(playlist.is_none());
        assert_eq!(
            (titles(&restored), position),
            (titles(&queue), Duration::from_millis(61_500))
        );
    }

    #[test]
    fn the_last_of_saves_in_a_row_is_the_one_kept() {
        let client = FakeClient::new(None);
        let saved = saved(&client, "andrey");
        for n in 1..=5 {
            let queue = Queue::new(vec![song(&n.to_string())], 0, None, Vec::new());
            saved.save(&queue, Duration::from_secs(n), None);
        }
        written();
        assert_eq!(load(&saved).unwrap().1, Duration::from_secs(5));
    }

    #[test]
    fn a_playlist_context_and_modes_survive_restarting_and_old_saves_still_load() {
        let client = FakeClient::new(None);
        let saved = saved(&client, "andrey");
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
        written();
        let reopened = self::saved(&client, "andrey");
        let (_, at, restored) = load(&reopened).unwrap();
        assert_eq!(at, Duration::from_secs(12));
        assert_eq!(restored, Some(playlist));
        let old = serde_json::json!({ "queue": queue, "position_ms": 12000 });
        write(
            reopened.store.clone(),
            QUEUE,
            Some(serde_json::to_vec(&old).unwrap()),
        );
        written();
        assert!(load(&reopened).unwrap().2.is_none());
    }

    #[test]
    fn each_profile_has_its_own_queue() {
        let client = FakeClient::new(None);
        saved(&client, "andrey").save(
            &Queue::new(vec![song("a")], 0, None, Vec::new()),
            Duration::ZERO,
            None,
        );
        written();
        assert!(load(&saved(&client, "alice")).is_none());
    }

    #[test]
    fn an_unreadable_value_is_no_queue() {
        let client = FakeClient::new(None);
        let saved = saved(&client, "andrey");
        write(saved.store.clone(), QUEUE, Some(b"not json".to_vec()));
        written();
        assert!(load(&saved).is_none());
    }

    #[test]
    fn a_queue_file_from_before_is_deleted_and_not_loaded() {
        let (client, data) = (FakeClient::new(None), dir("legacy"));
        let key = profile_key("queues", &profile("andrey"));
        let old = Saved {
            queue: Queue::new(vec![song("a")], 0, None, Vec::new()),
            position_ms: 1000,
            playlist: None,
        };
        DataStore::new(data.path())
            .set(&key, Some(&serde_json::to_vec(&old).unwrap()))
            .unwrap();
        let saved = SavedQueue::for_test(&client, Some(DataStore::new(data.path())), "andrey");
        assert!(load(&saved).is_none());
        assert!(DataStore::new(data.path()).get(&key).unwrap().is_none());
    }
}
