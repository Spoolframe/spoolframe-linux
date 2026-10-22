//! The fake device's downloads (PX-38): what the SDK keeps on disk, in
//! memory. They live on the client, not the server, so they list, play
//! and delete after the server has gone, as the real ones do. The rules
//! are the SDK's: a download started again is the same one, pause needs
//! one under way, resume one paused, retry one failed (or under way), and
//! a finished download is deleted, never cancelled. Nothing moves by
//! itself: a test moves it with `FakeClient::advance_download`,
//! `FakeClient::finish_download` and `FakeClient::fail_download`.
//!
//! As in SDK 0.14.1 (PX-60), a file is the device's: the profiles that
//! downloaded it hold it, each with its own watch, and the app's shown
//! profile (`Client::set_download_profile`) sees only what it holds. A
//! profile whose PIN is asked for here starts locked
//! (`FakeClient::require_offline_pin`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use spoolframe_types::{Container, Item, Marker, MediaTracks};
use tokio::sync::broadcast;

use crate::backend::{
    BackendError, DownloadEvent, DownloadFailure, DownloadFile, DownloadPhase, DownloadSource,
    DownloadSpace, DownloadState, DownloadTracks, FileTracks, LocalDownload, LocalWatch,
    SyncedWatch,
};
use spoolframe_protocol::downloads::OfflineWatchOutcome;

/// When the fake says every download was started, Unix seconds.
pub const STARTED_AT: i64 = 1_760_000_200;

/// The size a fake download says its file is, until it is finished with
/// a real file.
pub const FILE_SIZE: u64 = 1_000_000;

/// The room the fake device has.
pub const SPACE_AVAILABLE: u64 = 50_000_000_000;

/// What a session asks to be downloaded.
pub(in crate::fake) struct Start {
    pub server: String,
    pub username: String,
    pub item: Item,
    pub show: Option<Item>,
    pub season: Option<Item>,
    pub markers: Vec<Marker>,
    pub tracks: MediaTracks,
    pub source: DownloadSource,
    pub chosen: DownloadTracks,
}

pub(in crate::fake) struct Store {
    /// Oldest first, each with the profiles that hold it.
    downloads: Vec<Held>,
    next: u64,
    events: broadcast::Sender<DownloadEvent>,
    /// The profile the app shows (PX-60); every profile's downloads are
    /// listed before there is one.
    shown: Option<Shown>,
    /// The profiles whose PIN is asked for on this device, by server and
    /// user.
    locks: HashMap<(String, String), Lock>,
}

/// One file on the device and who holds it: a film two profiles
/// downloaded is one file, and each has its own watch state.
struct Held {
    download: LocalDownload,
    /// User ids, oldest hold first, each with its watch.
    holders: Vec<(String, LocalWatch)>,
}

struct Shown {
    server: String,
    user_id: String,
    unlocked: bool,
}

/// A profile's lock offline, as the SDK keeps it.
#[derive(Default)]
struct Lock {
    /// The PIN that last unlocked it online here; none, nothing to check.
    pin: Option<String>,
    /// Wrong PINs in a row.
    wrong: u32,
    /// The next try waits: the fake has no clock, so a test says when
    /// the wait is over ([`Store::wait_out`]).
    waiting: bool,
}

/// Wrong PINs in a row before each next try waits, and before the
/// profile stays locked until an online unlock: the SDK's.
const FREE_TRIES: u32 = 3;
const MOST_WRONG: u32 = 10;

impl Store {
    pub fn new() -> Self {
        Store {
            downloads: Vec::new(),
            next: 1,
            events: broadcast::channel(64).0,
            shown: None,
            locks: HashMap::new(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.events.subscribe()
    }

    /// The app shows `server`'s `user_id`: another profile than before
    /// starts locked, if its PIN is asked for here.
    pub fn set_profile(&mut self, server: &str, user_id: &str) {
        if self
            .shown
            .as_ref()
            .is_some_and(|s| s.server == server && s.user_id == user_id)
        {
            return;
        }
        self.shown = Some(Shown {
            server: server.into(),
            user_id: user_id.into(),
            unlocked: false,
        });
    }

    pub fn locked(&self) -> bool {
        self.shown.as_ref().is_some_and(|s| {
            !s.unlocked
                && self
                    .locks
                    .contains_key(&(s.server.clone(), s.user_id.clone()))
        })
    }

    /// `server`'s `user_id` asks for their PIN on this device; `pin` last
    /// unlocked them online here, if any did.
    pub fn require_pin(&mut self, server: &str, user_id: &str, pin: Option<&str>) {
        self.locks.insert(
            (server.into(), user_id.into()),
            Lock {
                pin: pin.map(str::to_owned),
                ..Lock::default()
            },
        );
    }

    /// `pin` unlocked `server`'s `user_id` online: kept to check offline,
    /// the wrong ones forgotten, and the profile open if it is shown.
    pub fn unlocked_online(&mut self, server: &str, user_id: &str, pin: &str) {
        self.locks.insert(
            (server.into(), user_id.into()),
            Lock {
                pin: Some(pin.into()),
                ..Lock::default()
            },
        );
        if let Some(shown) = self.shown.as_mut()
            && shown.server == server
            && shown.user_id == user_id
        {
            shown.unlocked = true;
        }
    }

    /// Unlocks the profile shown with `pin`, with the SDK's answers.
    pub fn unlock_offline(&mut self, pin: &str) -> Result<(), BackendError> {
        let Some(shown) = self.shown.as_mut() else {
            return Err(BackendError::InvalidInput("no profile is shown".into()));
        };
        let Some(lock) = self
            .locks
            .get_mut(&(shown.server.clone(), shown.user_id.clone()))
        else {
            shown.unlocked = true;
            return Ok(());
        };
        if lock.wrong >= MOST_WRONG {
            return Err(BackendError::SignInRequired);
        }
        if lock.waiting {
            return Err(BackendError::RateLimited(wait_after(lock.wrong)));
        }
        let Some(right) = lock.pin.as_deref() else {
            return Err(BackendError::NoOfflinePin);
        };
        if right == pin {
            lock.wrong = 0;
            shown.unlocked = true;
            return Ok(());
        }
        lock.wrong += 1;
        lock.waiting = lock.wrong >= FREE_TRIES;
        if lock.wrong >= MOST_WRONG {
            Err(BackendError::SignInRequired)
        } else {
            Err(BackendError::WrongCredentials)
        }
    }

    /// The wait after the last wrong PIN is over.
    pub fn wait_out(&mut self) {
        for lock in self.locks.values_mut() {
            lock.waiting = false;
        }
    }

    pub fn list(&self) -> Vec<LocalDownload> {
        self.downloads
            .iter()
            .filter(|h| self.sees(h))
            .map(|h| self.view(h))
            .collect()
    }

    pub fn get(&self, key: &str) -> Result<LocalDownload, BackendError> {
        let at = self.find(key)?;
        Ok(self.view(&self.downloads[at]))
    }

    /// The device's room: every profile's files count.
    pub fn space(&self) -> DownloadSpace {
        DownloadSpace {
            available: SPACE_AVAILABLE,
            used: self.downloads.iter().map(|h| h.download.received).sum(),
        }
    }

    /// Download `start`: waiting its turn on the server. The same item,
    /// source and tracks on the same server are the one file: another
    /// user starting it holds it too.
    pub fn start(&mut self, start: Start) -> LocalDownload {
        let chosen = start.chosen.normalized();
        let same = |h: &Held| {
            h.download.server == start.server
                && h.download.item.id() == start.item.id()
                && h.download.source == start.source
                && h.download.chosen == chosen
        };
        if let Some(at) = self.downloads.iter().position(same) {
            let held = &mut self.downloads[at];
            if !held.holders.iter().any(|(u, _)| *u == start.username) {
                held.holders
                    .push((start.username.clone(), watch_of(&start.item)));
                self.tell(at);
            }
            return view_for(&self.downloads[at], &start.username);
        }
        let (audio, subtitles) = match &chosen {
            DownloadTracks::Chosen { audio, subtitles } => (audio.clone(), subtitles.clone()),
            _ => (
                (0..start.tracks.audio.len() as u32).collect(),
                (0..start.tracks.subtitles.len() as u32).collect(),
            ),
        };
        let watch = watch_of(&start.item);
        let download = LocalDownload {
            key: format!("download-{}", self.next),
            server: start.server,
            user_id: start.username.clone(),
            username: start.username.clone(),
            watch: watch.clone(),
            item: start.item,
            show: start.show,
            season: start.season,
            markers: start.markers,
            artwork: Vec::new(),
            tracks: start.tracks,
            source: start.source,
            chosen,
            file: Some(DownloadFile {
                container: Container::Matroska,
                tracks: FileTracks { audio, subtitles },
            }),
            phase: waiting(),
            received: 0,
            size: None,
            // As a server says when it prepares one: about the file's size.
            expected_size: Some(FILE_SIZE),
            file_path: None,
            file_url: None,
            note: None,
            created_at: STARTED_AT,
        };
        self.next += 1;
        self.downloads.push(Held {
            download: download.clone(),
            holders: vec![(start.username, watch)],
        });
        self.tell(self.downloads.len() - 1);
        download
    }

    pub fn pause(&mut self, key: &str) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            if !is_active(&d.phase) {
                return Err(not(key, "under way"));
            }
            d.phase = DownloadPhase::Paused;
            Ok(())
        })
    }

    pub fn resume(&mut self, key: &str) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            if d.phase != DownloadPhase::Paused {
                return Err(not(key, "paused"));
            }
            d.phase = waiting();
            d.note = None;
            Ok(())
        })
    }

    pub fn retry(&mut self, key: &str) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            let failed = matches!(d.phase, DownloadPhase::Failed { .. });
            if !failed && !is_active(&d.phase) {
                return Err(not(key, "failed"));
            }
            if failed {
                d.phase = waiting();
            }
            d.note = None;
            Ok(())
        })
    }

    pub fn cancel(&mut self, key: &str) -> Result<(), BackendError> {
        if self.get(key)?.phase == DownloadPhase::Done {
            return Err(BackendError::InvalidInput(format!(
                "download {key} is finished: delete it"
            )));
        }
        self.delete(key)
    }

    /// Lets go of the shown profile's hold on `key`: the file goes with
    /// the last hold, and with no profile shown it goes at once.
    pub fn delete(&mut self, key: &str) -> Result<(), BackendError> {
        let at = self.find(key)?;
        let held = &mut self.downloads[at];
        match &self.shown {
            Some(shown) => held.holders.retain(|(u, _)| *u != shown.user_id),
            None => held.holders.clear(),
        }
        if held.holders.is_empty() {
            self.downloads.remove(at);
        }
        // Nobody listening is fine: nothing is shown.
        let _ = self.events.send(DownloadEvent::Removed { key: key.into() });
        Ok(())
    }

    pub fn position(&mut self, key: &str, at: Duration) -> Result<LocalDownload, BackendError> {
        self.watched(key, Some(at.as_millis() as u64), None)
    }

    pub fn mark(&mut self, key: &str, watched: bool) -> Result<LocalDownload, BackendError> {
        self.watched(key, None, Some(watched))
    }

    /// The shown profile's watch of `key` changes; with none shown, the
    /// first holder's.
    fn watched(
        &mut self,
        key: &str,
        position_ms: Option<u64>,
        watched: Option<bool>,
    ) -> Result<LocalDownload, BackendError> {
        let at = self.find(key)?;
        let user = self.shown.as_ref().map(|s| s.user_id.clone());
        let holders = &mut self.downloads[at].holders;
        let watch = match user {
            Some(user) => holders.iter_mut().find(|(u, _)| *u == user),
            None => holders.first_mut(),
        };
        if let Some((_, watch)) = watch {
            *watch = LocalWatch {
                position_ms,
                watched,
                at: STARTED_AT,
                pending: true,
            };
        }
        self.tell(at);
        Ok(self.view(&self.downloads[at]))
    }

    /// The transfer got to `received` bytes.
    pub fn advance(&mut self, key: &str, received: u64) -> Result<LocalDownload, BackendError> {
        self.move_on(key, |d| {
            d.phase = DownloadPhase::Transferring;
            d.size.get_or_insert(FILE_SIZE);
            d.received = received;
        })
    }

    /// The file is whole, at `path` when there is one to play.
    pub fn finish(
        &mut self,
        key: &str,
        path: Option<PathBuf>,
    ) -> Result<LocalDownload, BackendError> {
        self.move_on(key, |d| {
            let size = path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map_or(FILE_SIZE, |m| m.len());
            d.phase = DownloadPhase::Done;
            d.size = Some(size);
            d.received = size;
            d.file_url = path
                .as_ref()
                .and_then(|p| url::Url::from_file_path(p).ok())
                .map(String::from);
            d.file_path = path;
        })
    }

    /// Sends `username`'s offline watching on `server`: all of it is
    /// written, as the server does for state newer than its own.
    pub fn sync(&mut self, server: &str, username: &str) -> Vec<SyncedWatch> {
        let mut synced = Vec::new();
        for at in 0..self.downloads.len() {
            let held = &mut self.downloads[at];
            if held.download.server != server {
                continue;
            }
            let Some((_, watch)) = held
                .holders
                .iter_mut()
                .find(|(u, w)| u == username && w.pending)
            else {
                continue;
            };
            watch.pending = false;
            synced.push(SyncedWatch {
                item: held.download.item.id().clone(),
                outcome: OfflineWatchOutcome::Applied,
            });
            self.tell(at);
        }
        synced
    }

    pub fn fail(
        &mut self,
        key: &str,
        reason: &str,
        cause: DownloadFailure,
    ) -> Result<LocalDownload, BackendError> {
        self.move_on(key, |d| {
            d.phase = DownloadPhase::Failed {
                reason: reason.into(),
                cause,
            };
        })
    }

    /// Where `key` is, if the shown profile may see it.
    fn find(&self, key: &str) -> Result<usize, BackendError> {
        if self.locked() {
            return Err(BackendError::ProfileLocked);
        }
        self.downloads
            .iter()
            .position(|h| h.download.key == key && self.sees(h))
            .ok_or_else(|| missing(key))
    }

    /// The shown profile holds it on its server, unlocked; any, with no
    /// profile shown.
    fn sees(&self, held: &Held) -> bool {
        match &self.shown {
            None => true,
            Some(shown) => {
                !self.locked()
                    && held.download.server == shown.server
                    && held.holders.iter().any(|(u, _)| *u == shown.user_id)
            }
        }
    }

    /// `held` as the shown profile has it: their watch.
    fn view(&self, held: &Held) -> LocalDownload {
        match &self.shown {
            Some(shown) => view_for(held, &shown.user_id),
            None => view_for(held, &held.download.user_id),
        }
    }

    /// The shown profile's change of `key`.
    fn change(
        &mut self,
        key: &str,
        change: impl FnOnce(&mut LocalDownload) -> Result<(), BackendError>,
    ) -> Result<LocalDownload, BackendError> {
        let at = self.find(key)?;
        change(&mut self.downloads[at].download)?;
        self.tell(at);
        Ok(self.view(&self.downloads[at]))
    }

    /// The SDK moved `key` on, whoever is shown.
    fn move_on(
        &mut self,
        key: &str,
        change: impl FnOnce(&mut LocalDownload),
    ) -> Result<LocalDownload, BackendError> {
        let at = self
            .downloads
            .iter()
            .position(|h| h.download.key == key)
            .ok_or_else(|| missing(key))?;
        change(&mut self.downloads[at].download);
        self.tell(at);
        Ok(self.view(&self.downloads[at]))
    }

    /// The download at `at` changed: told only to a profile that sees
    /// it, as the SDK's events are.
    fn tell(&self, at: usize) {
        let held = &self.downloads[at];
        if self.sees(held) {
            // Nobody listening is fine: nothing is shown.
            let _ = self.events.send(DownloadEvent::Changed(self.view(held)));
        }
    }
}

/// `held` with `user`'s watch.
fn view_for(held: &Held, user: &str) -> LocalDownload {
    let mut download = held.download.clone();
    if let Some((_, watch)) = held.holders.iter().find(|(u, _)| u == user) {
        download.watch = watch.clone();
    }
    download
}

/// How long the next PIN waits after `wrong` in a row: 2 s, doubling,
/// at most 5 min, as the SDK's.
fn wait_after(wrong: u32) -> Duration {
    let over = wrong.saturating_sub(FREE_TRIES);
    Duration::from_secs(2u64 << over.min(16)).min(Duration::from_secs(300))
}

fn waiting() -> DownloadPhase {
    DownloadPhase::Preparing {
        server: DownloadState::Waiting,
    }
}

fn is_active(phase: &DownloadPhase) -> bool {
    matches!(
        phase,
        DownloadPhase::Preparing { .. } | DownloadPhase::Transferring
    )
}

fn missing(key: &str) -> BackendError {
    BackendError::NotFound(format!("download {key}"))
}

fn not(key: &str, what: &str) -> BackendError {
    BackendError::InvalidInput(format!("download {key} is not {what}"))
}

/// The server's watch state of `item`, as of the download.
fn watch_of(item: &Item) -> LocalWatch {
    let playback = match item {
        Item::Movie(m) => Some(&m.playback),
        Item::Episode(e) => Some(&e.playback),
        _ => None,
    };
    LocalWatch {
        position_ms: playback
            .and_then(|p| p.position)
            .map(|p| p.as_millis() as u64),
        watched: playback.map(|p| p.watched),
        at: STARTED_AT,
        pending: false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_types::{MediaId, MediaKind};

    use super::*;
    use crate::backend::{Client, OriginalDownload, Session};
    use crate::fake::{FakeClient, media};
    use crate::testing::{file_with, ready};

    fn alien() -> MediaId {
        media(MediaKind::Movie, "1")
    }

    fn andrey(client: &FakeClient) -> Arc<dyn Session> {
        client.session(&FakeClient::profile("andrey")).unwrap()
    }

    fn start(session: &dyn Session) -> LocalDownload {
        ready(session.start_download(&alien(), DownloadSource::Original, DownloadTracks::All))
            .unwrap()
    }

    fn phases(events: &mut broadcast::Receiver<DownloadEvent>) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(match event {
                DownloadEvent::Changed(d) => format!("{:?} {}", d.phase, d.received),
                DownloadEvent::Removed { key } => format!("removed {key}"),
            });
        }
        seen
    }

    #[test]
    fn a_download_starts_waiting_moves_and_ends_done() {
        let (_dir, path) = file_with("download", b"stand-in file");
        let client = FakeClient::new(Some(path));
        let mut events = client.download_events();
        let download = start(andrey(&client).as_ref());
        client.advance_download(&download.key, 400_000);
        client.finish_download(&download.key);
        assert_eq!(
            phases(&mut events),
            [
                "Preparing { server: Waiting } 0",
                "Transferring 400000",
                "Done 13"
            ]
        );
    }

    #[test]
    fn a_finished_download_plays_the_file_the_fake_serves() {
        let (_dir, path) = file_with("download", b"stand-in file");
        let client = FakeClient::new(Some(path.clone()));
        let key = start(andrey(&client).as_ref()).key;
        client.finish_download(&key);
        let done = ready(client.download(&key)).unwrap();
        assert_eq!(
            (
                done.file_path,
                done.file_url.map(|u| u.starts_with("file:///")),
                done.size
            ),
            (Some(path), Some(true), Some(13))
        );
    }

    #[test]
    fn a_download_names_the_item_and_the_tracks_asked_for() {
        let client = FakeClient::new(None);
        let chosen = DownloadTracks::Chosen {
            audio: vec![2, 0, 2],
            subtitles: vec![1],
        };
        let download = ready(andrey(&client).start_download(
            &alien(),
            DownloadSource::Profile {
                name: "Mobile".into(),
            },
            chosen,
        ))
        .unwrap();
        assert_eq!(
            (
                download.item.id().clone(),
                download.chosen,
                download.file.map(|f| f.tracks)
            ),
            (
                alien(),
                DownloadTracks::Chosen {
                    audio: vec![0, 2],
                    subtitles: vec![1]
                },
                Some(FileTracks {
                    audio: vec![0, 2],
                    subtitles: vec![1]
                })
            )
        );
    }

    #[test]
    fn starting_the_same_download_again_is_the_same_download() {
        let client = FakeClient::new(None);
        let session = andrey(&client);
        let (first, again) = (start(session.as_ref()), start(session.as_ref()));
        assert_eq!((first.key, ready(client.downloads()).len()), (again.key, 1));
    }

    #[test]
    fn without_the_feature_nothing_is_offered_or_sent() {
        let client = FakeClient::new(None).without_downloads();
        let session = andrey(&client);
        let unsupported = Some(BackendError::Unsupported("spoolframe:downloads".into()));
        assert_eq!(
            (
                session.has_downloads(),
                ready(session.download_options(&alien())).err(),
                ready(session.start_download(
                    &alien(),
                    DownloadSource::Original,
                    DownloadTracks::All
                ))
                .err(),
                ready(session.sync_download_watch()).err(),
            ),
            (false, unsupported.clone(), unsupported.clone(), unsupported)
        );
    }

    #[test]
    fn the_downloads_list_when_the_server_has_gone() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.go_away();
        assert_eq!(
            (
                ready(client.downloads())
                    .into_iter()
                    .map(|d| d.key)
                    .collect::<Vec<_>>(),
                ready(client.download(&key)).map(|d| d.key)
            ),
            (vec![key.clone()], Ok(key))
        );
    }

    #[test]
    fn a_finished_download_is_deleted_not_cancelled() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.finish_download(&key);
        let mut events = client.download_events();
        let refused = ready(client.cancel_download(&key));
        let listed = ready(client.downloads()).len();
        ready(client.delete_download(&key)).unwrap();
        assert_eq!(
            (
                refused.is_err(),
                listed,
                ready(client.downloads()).len(),
                phases(&mut events)
            ),
            (true, 1, 0, vec![format!("removed {key}")])
        );
    }

    #[test]
    fn cancelling_one_under_way_takes_it_off_the_device() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.advance_download(&key, 10);
        ready(client.cancel_download(&key)).unwrap();
        assert!(ready(client.downloads()).is_empty());
    }

    #[test]
    fn an_unknown_download_is_not_found() {
        let client = FakeClient::new(None);
        assert_eq!(
            ready(client.pause_download("nope")).err(),
            Some(BackendError::NotFound("download nope".into()))
        );
    }

    #[test]
    fn pause_resume_and_retry_follow_the_phase() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        let resume_early = ready(client.resume_download(&key)).is_err();
        let retry_waiting = ready(client.retry_download(&key)).map(|d| d.phase);
        let paused = ready(client.pause_download(&key)).map(|d| d.phase);
        let retry_paused = ready(client.retry_download(&key)).is_err();
        let resumed = ready(client.resume_download(&key)).map(|d| d.phase);
        client.fail_download(
            &key,
            "the conversion failed",
            DownloadFailure::ServerStoppedConverting,
        );
        let retried = ready(client.retry_download(&key)).map(|d| d.phase);
        let waiting = DownloadPhase::Preparing {
            server: DownloadState::Waiting,
        };
        assert_eq!(
            (
                resume_early,
                retry_waiting,
                paused,
                retry_paused,
                resumed,
                retried
            ),
            (
                true,
                Ok(waiting.clone()),
                Ok(DownloadPhase::Paused),
                true,
                Ok(waiting.clone()),
                Ok(waiting)
            )
        );
    }

    #[test]
    fn a_failed_download_says_why() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.fail_download(&key, "the file is gone", DownloadFailure::SourceMissing);
        assert_eq!(
            ready(client.download(&key)).map(|d| d.phase),
            Ok(DownloadPhase::Failed {
                reason: "the file is gone".into(),
                cause: DownloadFailure::SourceMissing,
            })
        );
    }

    #[test]
    fn watching_offline_is_kept_until_it_is_synced() {
        let client = FakeClient::new(None);
        let session = andrey(&client);
        let key = start(session.as_ref()).key;
        let kept = ready(client.record_download_position(&key, Duration::from_secs(90))).unwrap();
        let synced = ready(session.sync_download_watch()).unwrap();
        let after = ready(client.download(&key)).unwrap();
        assert_eq!(
            (
                (kept.watch.position_ms, kept.watch.pending),
                synced.len(),
                after.watch.pending,
                ready(session.sync_download_watch()).unwrap().len()
            ),
            ((Some(90_000), true), 1, false, 0)
        );
    }

    #[test]
    fn marking_watched_keeps_no_position() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        let watch = ready(client.set_download_watched(&key, true))
            .unwrap()
            .watch;
        assert_eq!(
            (watch.watched, watch.position_ms, watch.pending),
            (Some(true), None, true)
        );
    }

    #[test]
    fn the_options_tick_the_first_audio_and_every_subtitle() {
        let client = FakeClient::new(None);
        let options = ready(andrey(&client).download_options(&alien())).unwrap();
        assert_eq!(
            (
                options.preferred,
                options.profiles,
                options.original,
                options.left_out
            ),
            (
                DownloadTracks::Chosen {
                    audio: vec![0],
                    subtitles: (0..options.tracks.subtitles.len() as u32).collect()
                },
                vec!["Mobile".to_owned(), "Mobile (H.264)".to_owned()],
                OriginalDownload::AsIs,
                Vec::new()
            )
        );
    }

    #[test]
    fn music_and_tracks_that_do_not_exist_are_refused() {
        let client = FakeClient::new(None);
        let session = andrey(&client);
        let track = media(MediaKind::Track, "40");
        let beyond = DownloadTracks::Chosen {
            audio: vec![9],
            subtitles: Vec::new(),
        };
        assert!(matches!(
            (
                ready(session.start_download(
                    &track,
                    DownloadSource::Original,
                    DownloadTracks::All
                )),
                ready(session.start_download(&alien(), DownloadSource::Original, beyond)),
            ),
            (
                Err(BackendError::InvalidInput(_)),
                Err(BackendError::InvalidInput(_))
            )
        ));
    }

    #[test]
    fn an_episode_keeps_its_show_and_season() {
        let client = FakeClient::new(None);
        let episode = media(MediaKind::Episode, "65");
        let download = ready(andrey(&client).start_download(
            &episode,
            DownloadSource::Original,
            DownloadTracks::All,
        ))
        .unwrap();
        assert_eq!(
            (download.show.is_some(), download.season.is_some()),
            (true, true)
        );
    }

    fn bob(client: &FakeClient) -> Arc<dyn Session> {
        client.session(&FakeClient::profile("bob")).unwrap()
    }

    fn server() -> String {
        FakeClient::profile("andrey").server.0
    }

    fn keys(client: &FakeClient) -> Vec<String> {
        ready(client.downloads())
            .into_iter()
            .map(|d| d.key)
            .collect()
    }

    #[test]
    fn the_list_follows_the_profile_shown() {
        let client = FakeClient::new(None);
        let andreys = start(andrey(&client).as_ref()).key;
        let bobs = ready(bob(&client).start_download(
            &media(MediaKind::Episode, "65"),
            DownloadSource::Original,
            DownloadTracks::All,
        ))
        .unwrap()
        .key;
        let everyone = keys(&client);
        client.set_download_profile(&server(), "andrey");
        let andrey_sees = keys(&client);
        client.set_download_profile(&server(), "bob");
        assert_eq!(
            (everyone, andrey_sees, keys(&client)),
            (
                vec![andreys.clone(), bobs.clone()],
                vec![andreys],
                vec![bobs]
            )
        );
    }

    #[test]
    fn another_profiles_download_is_not_found() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.set_download_profile(&server(), "bob");
        assert_eq!(
            ready(client.download(&key)).err(),
            Some(BackendError::NotFound(format!("download {key}")))
        );
    }

    #[test]
    fn another_profiles_changes_are_not_told() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.set_download_profile(&server(), "bob");
        let mut events = client.download_events();
        client.advance_download(&key, 10);
        assert!(phases(&mut events).is_empty());
    }

    #[test]
    fn a_file_two_profiles_downloaded_stays_for_the_other() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        let again = start(bob(&client).as_ref()).key;
        client.set_download_profile(&server(), "andrey");
        ready(client.delete_download(&key)).unwrap();
        let andrey_sees = keys(&client);
        client.set_download_profile(&server(), "bob");
        assert_eq!(
            (again == key, andrey_sees, keys(&client)),
            (true, Vec::<String>::new(), vec![key])
        );
    }

    #[test]
    fn each_profile_has_its_own_watch_of_a_shared_file() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        start(bob(&client).as_ref());
        client.set_download_profile(&server(), "andrey");
        ready(client.record_download_position(&key, Duration::from_secs(90))).unwrap();
        client.set_download_profile(&server(), "bob");
        let bobs = ready(client.download(&key)).unwrap().watch;
        assert!(!bobs.pending);
    }

    fn locked_andrey(pin: Option<&str>) -> (FakeClient, String) {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.require_offline_pin(&FakeClient::profile("andrey").server, "andrey", pin);
        client.set_download_profile(&server(), "andrey");
        (client, key)
    }

    #[test]
    fn a_locked_profile_lists_nothing_and_refuses_each_key() {
        let (client, key) = locked_andrey(Some("1234"));
        assert_eq!(
            (
                client.download_profile_locked(),
                keys(&client),
                ready(client.download(&key)).err()
            ),
            (
                true,
                Vec::<String>::new(),
                Some(BackendError::ProfileLocked)
            )
        );
    }

    #[test]
    fn the_right_pin_unlocks_offline_and_lists() {
        let (client, key) = locked_andrey(Some("1234"));
        ready(client.unlock_downloads_offline("1234")).unwrap();
        assert_eq!(
            (client.download_profile_locked(), keys(&client)),
            (false, vec![key])
        );
    }

    #[test]
    fn showing_another_profile_and_back_locks_again() {
        let (client, _) = locked_andrey(Some("1234"));
        ready(client.unlock_downloads_offline("1234")).unwrap();
        client.set_download_profile(&server(), "andrey");
        let same_again = client.download_profile_locked();
        client.set_download_profile(&server(), "bob");
        client.set_download_profile(&server(), "andrey");
        assert_eq!(
            (same_again, client.download_profile_locked()),
            (false, true)
        );
    }

    #[test]
    fn with_no_pin_kept_here_nothing_can_be_checked() {
        let (client, _) = locked_andrey(None);
        assert_eq!(
            ready(client.unlock_downloads_offline("1234")).err(),
            Some(BackendError::NoOfflinePin)
        );
    }

    #[test]
    fn after_three_wrong_pins_the_next_try_waits() {
        let (client, _) = locked_andrey(Some("1234"));
        let tries: Vec<_> = (0..4)
            .map(|_| ready(client.unlock_downloads_offline("0000")).err())
            .collect();
        client.wait_out_offline_pin();
        let after_waiting = ready(client.unlock_downloads_offline("1234"));
        assert_eq!(
            (tries, after_waiting),
            (
                vec![
                    Some(BackendError::WrongCredentials),
                    Some(BackendError::WrongCredentials),
                    Some(BackendError::WrongCredentials),
                    Some(BackendError::RateLimited(Duration::from_secs(2))),
                ],
                Ok(())
            )
        );
    }

    #[test]
    fn ten_wrong_pins_lock_it_until_an_online_unlock() {
        let (client, _) = locked_andrey(Some("1234"));
        let mut last = None;
        for _ in 0..10 {
            client.wait_out_offline_pin();
            last = ready(client.unlock_downloads_offline("0000")).err();
        }
        client.wait_out_offline_pin();
        assert_eq!(
            (last, ready(client.unlock_downloads_offline("1234")).err()),
            (
                Some(BackendError::SignInRequired),
                Some(BackendError::SignInRequired)
            )
        );
    }

    #[test]
    fn an_online_unlock_opens_the_downloads_and_keeps_the_pin() {
        let client = FakeClient::new(None);
        let alice = client.session(&FakeClient::profile("alice")).unwrap();
        client.require_offline_pin(&FakeClient::profile("alice").server, "alice", None);
        client.set_download_profile(&server(), "alice");
        let before = client.download_profile_locked();
        ready(alice.unlock(crate::fake::FAKE_PIN)).unwrap();
        let after = client.download_profile_locked();
        client.set_download_profile(&server(), "bob");
        client.set_download_profile(&server(), "alice");
        assert_eq!(
            (
                before,
                after,
                ready(client.unlock_downloads_offline(crate::fake::FAKE_PIN))
            ),
            (true, false, Ok(()))
        );
    }

    #[test]
    fn the_room_counts_every_profiles_files() {
        let client = FakeClient::new(None);
        let key = start(andrey(&client).as_ref()).key;
        client.advance_download(&key, 400);
        client.set_download_profile(&server(), "bob");
        assert_eq!(ready(client.download_space()).unwrap().used, 400);
    }
}
