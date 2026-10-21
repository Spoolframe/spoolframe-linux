//! The fake device's downloads (PX-38): what the SDK keeps on disk, in
//! memory. They live on the client, not the server, so they list, play
//! and delete after the server has gone, as the real ones do. The rules
//! are the SDK's: a download started again is the same one, pause needs
//! one under way, resume one paused, retry one failed (or under way), and
//! a finished download is deleted, never cancelled. Nothing moves by
//! itself: a test moves it with `FakeClient::advance_download`,
//! `FakeClient::finish_download` and `FakeClient::fail_download`.

use std::path::PathBuf;
use std::time::Duration;

use spoolframe_types::{Container, Item, Marker, MediaTracks};
use tokio::sync::broadcast;

use crate::backend::{
    BackendError, DownloadEvent, DownloadFile, DownloadPhase, DownloadSource, DownloadSpace,
    DownloadState, DownloadTracks, FileTracks, LocalDownload, LocalWatch, SyncedWatch,
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
    /// Oldest first.
    downloads: Vec<LocalDownload>,
    next: u64,
    events: broadcast::Sender<DownloadEvent>,
}

impl Store {
    pub fn new() -> Self {
        Store {
            downloads: Vec::new(),
            next: 1,
            events: broadcast::channel(64).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.events.subscribe()
    }

    pub fn list(&self) -> Vec<LocalDownload> {
        self.downloads.clone()
    }

    pub fn get(&self, key: &str) -> Result<LocalDownload, BackendError> {
        self.downloads
            .iter()
            .find(|d| d.key == key)
            .cloned()
            .ok_or_else(|| missing(key))
    }

    pub fn space(&self) -> DownloadSpace {
        DownloadSpace {
            available: SPACE_AVAILABLE,
            used: self.downloads.iter().map(|d| d.received).sum(),
        }
    }

    /// Download `start`: waiting its turn on the server. The same item,
    /// source and tracks for the same user are the one download.
    pub fn start(&mut self, start: Start) -> LocalDownload {
        let chosen = start.chosen.normalized();
        let same = |d: &&LocalDownload| {
            d.user_id == start.username
                && d.server == start.server
                && d.item.id() == start.item.id()
                && d.source == start.source
                && d.chosen == chosen
        };
        if let Some(found) = self.downloads.iter().find(same) {
            return found.clone();
        }
        let (audio, subtitles) = match &chosen {
            DownloadTracks::Chosen { audio, subtitles } => (audio.clone(), subtitles.clone()),
            _ => (
                (0..start.tracks.audio.len() as u32).collect(),
                (0..start.tracks.subtitles.len() as u32).collect(),
            ),
        };
        let download = LocalDownload {
            key: format!("download-{}", self.next),
            server: start.server,
            user_id: start.username.clone(),
            username: start.username,
            watch: watch_of(&start.item),
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
            file_path: None,
            file_url: None,
            note: None,
            created_at: STARTED_AT,
        };
        self.next += 1;
        self.downloads.push(download.clone());
        self.tell(&download);
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

    pub fn delete(&mut self, key: &str) -> Result<(), BackendError> {
        let at = self
            .downloads
            .iter()
            .position(|d| d.key == key)
            .ok_or_else(|| missing(key))?;
        self.downloads.remove(at);
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

    fn watched(
        &mut self,
        key: &str,
        position_ms: Option<u64>,
        watched: Option<bool>,
    ) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            d.watch = LocalWatch {
                position_ms,
                watched,
                at: STARTED_AT,
                pending: true,
            };
            Ok(())
        })
    }

    /// The transfer got to `received` bytes.
    pub fn advance(&mut self, key: &str, received: u64) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            d.phase = DownloadPhase::Transferring;
            d.size.get_or_insert(FILE_SIZE);
            d.received = received;
            Ok(())
        })
    }

    /// The file is whole, at `path` when there is one to play.
    pub fn finish(
        &mut self,
        key: &str,
        path: Option<PathBuf>,
    ) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
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
            Ok(())
        })
    }

    /// Sends `username`'s offline watching: all of it is written, as the
    /// server does for state newer than its own.
    pub fn sync(&mut self, username: &str) -> Vec<SyncedWatch> {
        let keys: Vec<String> = self
            .downloads
            .iter()
            .filter(|d| d.user_id == username && d.watch.pending)
            .map(|d| d.key.clone())
            .collect();
        keys.iter()
            .filter_map(|key| {
                self.change(key, |d| {
                    d.watch.pending = false;
                    Ok(())
                })
                .ok()
            })
            .map(|d| SyncedWatch {
                item: d.item.id().clone(),
                outcome: OfflineWatchOutcome::Applied,
            })
            .collect()
    }

    pub fn fail(&mut self, key: &str, reason: &str) -> Result<LocalDownload, BackendError> {
        self.change(key, |d| {
            d.phase = DownloadPhase::Failed {
                reason: reason.into(),
            };
            Ok(())
        })
    }

    fn change(
        &mut self,
        key: &str,
        change: impl FnOnce(&mut LocalDownload) -> Result<(), BackendError>,
    ) -> Result<LocalDownload, BackendError> {
        let download = self
            .downloads
            .iter_mut()
            .find(|d| d.key == key)
            .ok_or_else(|| missing(key))?;
        change(download)?;
        let download = download.clone();
        self.tell(&download);
        Ok(download)
    }

    fn tell(&self, download: &LocalDownload) {
        // Nobody listening is fine: nothing is shown.
        let _ = self.events.send(DownloadEvent::Changed(download.clone()));
    }
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
        client.fail_download(&key, "the conversion failed");
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
        client.fail_download(&key, "the file is gone");
        assert_eq!(
            ready(client.download(&key)).map(|d| d.phase),
            Ok(DownloadPhase::Failed {
                reason: "the file is gone".into()
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
                vec!["Mobile".to_owned()],
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
}
