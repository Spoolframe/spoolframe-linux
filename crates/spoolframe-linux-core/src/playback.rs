//! Connect the UI's session port to the SDK's playback owner.
use crate::backend::{BackendError, Session};
use async_trait::async_trait;
pub use spoolframe_client_sdk::ManagedPlayback;
use spoolframe_client_sdk::{
    Chunk, Playback, PlaybackBackend, PlaybackChange, PlaybackOptions, SdkError,
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
            .start_playback(item, selection, options.start)
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
        BackendError::InvalidInput(message) => SdkError::InvalidInput(message),
        BackendError::NotFound(message) => SdkError::NotFound(message),
        other => SdkError::Server(other.to_string()),
    }
}
