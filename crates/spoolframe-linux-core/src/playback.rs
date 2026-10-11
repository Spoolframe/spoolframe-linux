//! Connect the UI's session port to the SDK's playback owner.
use crate::backend::{BackendError, Session};
use async_trait::async_trait;
pub use spoolframe_client_sdk::ManagedPlayback;
use spoolframe_client_sdk::{
    Chunk, Playback, PlaybackBackend, PlaybackChange, PlaybackOptions, PlaybackQuality, SdkError,
};
use spoolframe_protocol::{BitmapSubtitles, PlaybackHandle, Selection};
use spoolframe_types::MediaId;
use std::{sync::Arc, time::Duration};

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
        _: BitmapSubtitles,
        _: Duration,
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
        bitmap: BitmapSubtitles,
        position: Duration,
    ) -> Result<PlaybackChange, SdkError> {
        if bitmap != BitmapSubtitles::default() {
            return Err(SdkError::InvalidInput(
                "this client uses the default bitmap subtitle mode".into(),
            ));
        }
        self.0
            .change_quality(h, quality, position)
            .await
            .map_err(error)
    }
    /// Planned again for a slow link (P6-06), which the direct reader asks
    /// for when it stalls; the SDK swaps the new plan in.
    async fn limit(
        &self,
        h: &PlaybackHandle,
        link_bitrate: u64,
        bitmap: BitmapSubtitles,
        position: Duration,
    ) -> Result<PlaybackChange, SdkError> {
        if bitmap != BitmapSubtitles::default() {
            return Err(SdkError::InvalidInput(
                "this client uses the default bitmap subtitle mode".into(),
            ));
        }
        self.0
            .limit_playback(h, link_bitrate, position)
            .await
            .map_err(error)
    }
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
        other => SdkError::Server(other.to_string()),
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
