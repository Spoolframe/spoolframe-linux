//! Connect the UI's session port to the SDK's playback owner.
use crate::backend::{BackendError, Session};
use async_trait::async_trait;
pub use spoolframe_client_sdk::ManagedPlayback;
use spoolframe_client_sdk::{
    Chunk, LinkMeter, Playback, PlaybackBackend, PlaybackChange, PlaybackOptions, PlaybackQuality,
    PlaybackShare, RemoteMode, Replan, SdkError, Subtitle, SubtitleTimingEvent,
};
use spoolframe_client_sdk::{Edited, PlayFrom, PlaylistCursor, PlaylistSource};
use spoolframe_protocol::playlists::{
    EntryId, PlaySettings, PlaylistEdit, PlaylistEntry, PlaylistNext, PlaylistPage,
};
use spoolframe_protocol::{BitmapSubtitles, PlaybackHandle, Selection};
use spoolframe_types::MediaId;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

pub fn backend(session: Arc<dyn Session>) -> Arc<dyn PlaybackBackend> {
    Arc::new(Adapter(session))
}
struct Adapter(Arc<dyn Session>);
#[async_trait]
impl PlaybackBackend for Adapter {
    async fn start(
        &self,
        item: &MediaId,
        selection: Option<Selection>,
        options: PlaybackOptions,
    ) -> Result<Playback, SdkError> {
        if options.bitmap != BitmapSubtitles::default() {
            return Err(SdkError::InvalidInput(
                "this client uses the default bitmap subtitle mode".into(),
            ));
        }
        self.0
            .start_playback(item, selection, options)
            .await
            .map_err(error)
    }
    async fn read(&self, h: &PlaybackHandle, o: u64, n: u64) -> Result<Vec<u8>, SdkError> {
        self.0.read(h, o, n).await.map_err(error)
    }
    async fn read_chunk(
        &self,
        h: &PlaybackHandle,
        e: u32,
        o: u64,
        n: u64,
    ) -> Result<Chunk, SdkError> {
        self.0.read_chunk(h, e, o, n).await.map_err(error)
    }
    async fn seek(&self, h: &PlaybackHandle, p: Duration) -> Result<u32, SdkError> {
        self.0.seek_playback(h, p).await.map_err(error)
    }
    async fn subtitle(
        &self,
        _: &PlaybackHandle,
        _: Option<u32>,
        _: &Replan,
    ) -> Result<PlaybackChange, SdkError> {
        Err(SdkError::InvalidInput(
            "this player changes tracks by replacing playback".into(),
        ))
    }
    async fn progress(&self, h: &PlaybackHandle, p: Duration) -> Result<(), SdkError> {
        self.0.update_progress(h, p).await.map_err(error)
    }
    async fn stop(&self, h: &PlaybackHandle) -> Result<(), SdkError> {
        self.0.stop_playback(h).await.map_err(error)
    }
    /// Planned again for the quality the viewer chose (P6-06).
    async fn change_quality(
        &self,
        h: &PlaybackHandle,
        quality: PlaybackQuality,
        replan: &Replan,
    ) -> Result<PlaybackChange, SdkError> {
        default_bitmap(replan.bitmap)?;
        self.0
            .change_quality(h, quality, replan.position)
            .await
            .map_err(error)
    }
    /// Planned again for a slow link (P6-06), which the direct reader asks
    /// for when it stalls; the SDK swaps the new plan in.
    async fn limit(
        &self,
        h: &PlaybackHandle,
        link_bitrate: u64,
        replan: &Replan,
    ) -> Result<PlaybackChange, SdkError> {
        default_bitmap(replan.bitmap)?;
        self.0
            .limit_playback(h, link_bitrate, replan.position)
            .await
            .map_err(error)
    }
    async fn set_track_override(
        &self,
        item: &MediaId,
        selection: Selection,
    ) -> Result<(), SdkError> {
        self.0
            .set_track_override(item, selection)
            .await
            .map_err(error)
    }
    async fn read_subtitle(&self, h: &PlaybackHandle) -> Result<Subtitle, SdkError> {
        let (format, bytes) = self.0.read_subtitle(h).await.map_err(error)?;
        Ok(Subtitle { format, bytes })
    }
    // The session port has none of these yet: the player's own reader
    // measures the link, and a paired item's route is asked by handle
    // (`Session::remote_mode`). As before the SDK made them required.
    fn link_meter(&self) -> Option<Arc<LinkMeter>> {
        None
    }
    fn shares(&self) -> Option<broadcast::Receiver<PlaybackShare>> {
        None
    }
    fn subtitle_timings(&self) -> Option<(String, broadcast::Receiver<SubtitleTimingEvent>)> {
        None
    }
    fn route(&self) -> Option<RemoteMode> {
        None
    }
}

/// Follows playlist `id` from `playing`, the entry that plays already,
/// with `settings`, or the server's while they are not known: the server
/// was told of the entry when it started, or, from a saved queue, is not,
/// since playing it would move the user's place and deal a new shuffled
/// round. What comes up is read from the server's place.
pub async fn follow_playlist(
    session: Arc<dyn Session>,
    id: &str,
    playing: PlaylistNext,
    settings: Option<PlaySettings>,
) -> Result<PlaylistCursor, SdkError> {
    let settings = match settings {
        Some(settings) => settings,
        None => {
            session
                .playlist(id, 0, 1)
                .await
                .map_err(playlist_error)?
                .settings
        }
    };
    let from = PlayFrom::Entry(playing.entry);
    let source = Arc::new(Playlists {
        session,
        playing: Mutex::new(Some(playing)),
    });
    // An entry to start from: nothing is picked at random.
    PlaylistCursor::start(source, id, from, settings, |_| 0).await
}

/// The session's playlist calls, for a [`PlaylistCursor`]; its first
/// play is answered with the entry playing, not sent.
struct Playlists {
    session: Arc<dyn Session>,
    playing: Mutex<Option<PlaylistNext>>,
}

#[async_trait]
impl PlaylistSource for Playlists {
    async fn page(&self, id: &str, offset: u32, limit: u32) -> Result<PlaylistPage, SdkError> {
        let page = self.session.playlist(id, offset, limit).await;
        page.map_err(playlist_error)
    }

    async fn play(
        &self,
        id: &str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> Result<Option<PlaylistNext>, SdkError> {
        let playing = self.playing.lock().expect("never poisoned").take();
        if let Some(playing) = playing {
            return Ok(Some(playing));
        }
        let next = self.session.play_playlist(id, from, settings).await;
        next.map_err(playlist_error)
    }

    async fn next_after(&self, id: &str, after: EntryId) -> Result<Option<PlaylistNext>, SdkError> {
        let next = self.session.next_in_playlist(id, after).await;
        next.map_err(playlist_error)
    }

    async fn upcoming(&self, id: &str, limit: u32) -> Result<Vec<PlaylistEntry>, SdkError> {
        let upcoming = self.session.upcoming_in_playlist(id, limit).await;
        upcoming.map_err(playlist_error)
    }

    async fn save_settings(&self, id: &str, settings: PlaySettings) -> Result<(), SdkError> {
        let saved = self.session.set_playlist_settings(id, settings).await;
        saved.map_err(playlist_error)
    }

    /// The session says the new revision only: the entries the adds
    /// made are read again by whoever needs them.
    async fn edit(
        &self,
        id: &str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> Result<Edited, SdkError> {
        let revision = self.session.edit_playlist(id, revision, edits).await;
        Ok(Edited {
            revision: revision.map_err(playlist_error)?,
            added: None,
        })
    }
}

/// This client shows bitmap subtitles the default way.
fn default_bitmap(bitmap: BitmapSubtitles) -> Result<(), SdkError> {
    if bitmap != BitmapSubtitles::default() {
        return Err(SdkError::InvalidInput(
            "this client uses the default bitmap subtitle mode".into(),
        ));
    }
    Ok(())
}
fn error(e: BackendError) -> SdkError {
    match e {
        BackendError::PlaybackExpired => SdkError::PlaybackExpired,
        BackendError::PlaybackRestarted { epoch } => SdkError::PlaybackRestarted { epoch },
        BackendError::ProfileLocked => SdkError::ProfileLocked,
        BackendError::SignInRequired => SdkError::SignInRequired,
        BackendError::Busy => SdkError::Busy,
        BackendError::CannotPlay(reasons) => SdkError::CannotPlay { reasons },
        BackendError::Unavailable(message) => SdkError::Unavailable(message),
        BackendError::Unreachable(message) => SdkError::Unreachable(message),
        BackendError::TimedOut(message) => SdkError::TimedOut(message),
        BackendError::PlaybackStopped => SdkError::PlaybackStopped,
        BackendError::InvalidInput(message) => SdkError::InvalidInput(message),
        BackendError::NotFound(message) => SdkError::NotFound(message),
        // The SDK's subtitle_file asks again only on this one.
        BackendError::SubtitleNotReady(done_percent) => SdkError::SubtitleNotReady { done_percent },
        other => SdkError::Server(other.to_string()),
    }
}

/// [`error`], and the playlist refusals a [`PlaylistCursor`] acts on: a
/// server that cannot say what comes up is read page by page instead.
fn playlist_error(e: BackendError) -> SdkError {
    match e {
        BackendError::Unsupported(request) => SdkError::NotOnServer { request },
        BackendError::PlaylistConflict { revision } => SdkError::PlaylistConflict { revision },
        BackendError::PlaylistReadOnly => SdkError::PlaylistReadOnly,
        other => error(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Client;
    use crate::fake::FakeClient;
    use spoolframe_protocol::PlaybackPlan;

    /// A direct play the link is too slow for is planned again (P6-06):
    /// through this adapter the SDK swaps the conversion in, and the
    /// direct reader's next read is told to start a new reader.
    /// The viewer's quality choice reaches the server through this
    /// adapter (P6-06): a bitrate converts, Original plays the file again.
    #[test]
    fn a_chosen_quality_plans_the_playback_again() {
        let (_dir, path) = crate::testing::file_with("quality", &[0; 4096]);
        crate::testing::runtime().block_on(choose_quality(path));
    }

    async fn choose_quality(path: std::path::PathBuf) {
        let client = FakeClient::new(Some(path));
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let item = crate::testing::first_movie_id(&session).await;
        let playback = ManagedPlayback::open(backend(session), item, None, Default::default())
            .await
            .unwrap();
        let capped = playback
            .change_quality(PlaybackQuality::Bitrate(8_000_000), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(capped.plan, PlaybackPlan::Converted { .. }),
            "{capped:?}"
        );
        assert_eq!(playback.quality(), PlaybackQuality::Bitrate(8_000_000));
        let original = playback
            .change_quality(PlaybackQuality::Original, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(original.plan, PlaybackPlan::Direct { .. }),
            "{original:?}"
        );
    }

    /// The fake's All, played from its top: its id and the entry playing.
    async fn all_playing(session: &Arc<dyn Session>) -> (String, PlaylistNext) {
        let list = session.playlists().await.unwrap().playlists;
        let all = list.into_iter().find(|p| p.name == "All").unwrap();
        let first = session.play_playlist(&all.id, None, PlaySettings::default());
        (all.id.clone(), first.await.unwrap().unwrap())
    }

    /// Restored from a saved queue, or started already: following it
    /// moves nothing on the server, and what comes up is its own.
    #[test]
    fn following_the_entry_playing_leaves_the_servers_place() {
        crate::testing::runtime().block_on(async {
            let client = FakeClient::new(None);
            let session = client.session(&FakeClient::profile("andrey")).unwrap();
            let (id, first) = all_playing(&session).await;
            let later = session.playlist(&id, 0, 3).await.unwrap().entries[2].entry;
            // Played on, on another device.
            let elsewhere = session.play_playlist(&id, Some(later), PlaySettings::default());
            elsewhere.await.unwrap();
            let cursor = follow_playlist(
                session.clone(),
                &id,
                first.clone(),
                Some(Default::default()),
            )
            .await
            .unwrap();
            assert_eq!(cursor.current().map(|e| e.entry), Some(first.entry));
            let place = session.playlist(&id, 0, 1).await.unwrap().place;
            assert_eq!(place, Some(later));
        });
    }

    /// Restored from a queue saved before the modes were kept: they are
    /// the server's.
    #[test]
    fn a_playlist_followed_with_its_modes_unknown_plays_with_the_servers() {
        crate::testing::runtime().block_on(async {
            let client = FakeClient::new(None);
            let session = client.session(&FakeClient::profile("andrey")).unwrap();
            let (id, first) = all_playing(&session).await;
            let shuffled = PlaySettings {
                shuffle: true,
                ..PlaySettings::default()
            };
            session.set_playlist_settings(&id, shuffled).await.unwrap();
            let cursor = follow_playlist(session, &id, first, None).await.unwrap();
            assert!(cursor.settings().shuffle);
        });
    }

    /// A server older than `UpcomingInPlaylist` refuses it as unsupported:
    /// the cursor reads the playlist's pages instead.
    #[test]
    fn an_unsupported_call_is_one_the_server_does_not_know() {
        assert_eq!(
            playlist_error(BackendError::Unsupported("UpcomingInPlaylist".into())),
            SdkError::NotOnServer {
                request: "UpcomingInPlaylist".into()
            }
        );
    }

    #[test]
    fn a_direct_play_on_a_slow_link_becomes_a_conversion() {
        let (_dir, path) = crate::testing::file_with("limit", &[0; 4096]);
        crate::testing::runtime().block_on(slow_link(path));
    }

    async fn slow_link(path: std::path::PathBuf) {
        let client = FakeClient::new(Some(path));
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let item = crate::testing::first_movie_id(&session).await;
        let playback = ManagedPlayback::open(backend(session), item, None, Default::default())
            .await
            .unwrap();
        let direct = playback.snapshot();
        assert!(matches!(direct.plan, PlaybackPlan::Direct { .. }));

        let next = playback.limit(1_000_000).await.unwrap();
        assert!(
            matches!(next.plan, PlaybackPlan::Converted { .. }),
            "{next:?}"
        );
        assert_eq!(playback.snapshot().epoch, next.epoch);
        assert_eq!(
            playback.read_chunk(&direct, 0, 1).await,
            Err(SdkError::PlaybackRestarted { epoch: next.epoch })
        );
    }

    /// A subtitle the server is still reading as text stays one through
    /// this adapter, so the SDK's `subtitle_file` asks again rather than
    /// giving up on a server error.
    #[test]
    fn a_subtitle_still_being_read_reaches_the_sdk_as_not_ready() {
        assert_eq!(
            error(BackendError::SubtitleNotReady(50)),
            SdkError::SubtitleNotReady { done_percent: 50 }
        );
    }

    /// What the screen shows (PGPU-06) reaches the server with each start,
    /// so a change to Show HDR applies at the next play, not the next
    /// connection.
    #[test]
    fn the_display_reaches_the_server_with_the_start() {
        let (_dir, path) = crate::testing::file_with("display", &[0; 4096]);
        crate::testing::runtime().block_on(display_reaches(path));
    }

    async fn display_reaches(path: std::path::PathBuf) {
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = crate::testing::first_movie_id(&session).await;
        let sdr = spoolframe_types::DisplayCapabilities {
            bit_depth: None,
            hdr: Some(Default::default()),
            video_codecs: None,
            height: None,
            frame_rate: None,
        };
        let options = PlaybackOptions {
            display: Some(sdr.clone()),
            ..Default::default()
        };
        ManagedPlayback::open(backend(session), item, None, options)
            .await
            .unwrap();
        assert_eq!(client.displays_asked(&profile), vec![Some(sdr)]);
    }
}
