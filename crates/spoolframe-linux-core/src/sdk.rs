//! The backend on a real server: [`Client`] and [`Session`] over
//! `spoolframe-client-sdk` (P1-15, P1-19). The traits were shaped after the
//! SDK and carry the same protocol types, so this mostly passes calls
//! through and maps errors.
//!
//! The SDK runs its own tasks (mDNS discovery, a reconnect supervisor per
//! server) on the tokio runtime it was opened in. Every call here runs on
//! that runtime too, through its handle, so it does not matter which
//! runtime awaits the call.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use spoolframe_client_sdk as sdk;
use spoolframe_types::{
    ArtworkId, Item, Library, LibraryId, MediaId, MediaTracks, SubtitleFormat, TrackPreferences,
};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Client, ConnectionState, HomeScreen,
    PlaybackHandle, PlaybackPlan, PlaybackStart, Profile, Selection, Server, ServerKey, Session,
};
use crate::storage::Storage;

/// The SDK's errors, in the kinds the UI acts on.
pub fn backend_error(error: sdk::SdkError) -> BackendError {
    use sdk::SdkError as E;
    match error {
        E::Unreachable(what) => BackendError::Unreachable(what),
        E::InvalidCredentials => BackendError::WrongCredentials,
        E::RateLimited { retry_after } => BackendError::RateLimited(retry_after),
        E::SignInRequired => BackendError::SignInRequired,
        E::CannotPlay { reasons } => BackendError::CannotPlay(reasons),
        E::PlaybackExpired => BackendError::PlaybackExpired,
        E::NotFound(what) => BackendError::NotFound(what),
        E::NotPinned(id) => BackendError::NotPinned(id),
        E::IdentityChanged { saved, now } => BackendError::IdentityChanged { saved, now },
        // The server's own "unavailable" (Plex or a media mount down)
        // arrives as a server error with its message.
        E::Server(message) => match message.strip_prefix("unavailable: ") {
            Some(why) => BackendError::Unavailable(why.to_owned()),
            None => BackendError::Other(format!("server: {message}")),
        },
        other => BackendError::Other(other.to_string()),
    }
}

/// The SDK's connection states, in the UI's words.
pub fn connection_state(state: sdk::ConnectionState) -> ConnectionState {
    match state {
        sdk::ConnectionState::Disconnected => ConnectionState::Closed,
        sdk::ConnectionState::Connecting => ConnectionState::Connecting,
        sdk::ConnectionState::Connected => ConnectionState::Connected,
        sdk::ConnectionState::Reconnecting { attempt } => ConnectionState::Reconnecting { attempt },
    }
}

fn server(entry: sdk::ServerEntry) -> Server {
    Server {
        key: ServerKey(entry.endpoint_id),
        name: entry.name,
        addresses: entry.addrs.iter().map(SocketAddr::to_string).collect(),
        pinned: entry.pinned,
        fingerprint: entry.fingerprint,
        verified: entry.verified,
        on_network: entry.on_network,
    }
}

fn profile(session: &sdk::Session) -> Profile {
    let user = session.user();
    Profile {
        server: ServerKey(session.server()),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
    }
}

/// The app's storage as the SDK's: four pass-throughs.
pub struct SdkStorage(pub Storage);

#[async_trait]
impl sdk::Storage for SdkStorage {
    async fn secret(&self, key: &str) -> Result<Option<Vec<u8>>, sdk::StorageError> {
        self.0
            .secret(key)
            .await
            .map_err(|e| sdk::StorageError(e.to_string()))
    }

    async fn set_secret(&self, key: &str, value: Option<Vec<u8>>) -> Result<(), sdk::StorageError> {
        self.0
            .set_secret(key, value)
            .await
            .map_err(|e| sdk::StorageError(e.to_string()))
    }

    async fn data(&self, key: &str) -> Result<Option<Vec<u8>>, sdk::StorageError> {
        self.0
            .data(key)
            .await
            .map_err(|e| sdk::StorageError(e.to_string()))
    }

    async fn set_data(&self, key: &str, value: Option<Vec<u8>>) -> Result<(), sdk::StorageError> {
        self.0
            .set_data(key, value)
            .await
            .map_err(|e| sdk::StorageError(e.to_string()))
    }
}

/// Signed-in sessions by (server, username). `Client::session` is
/// synchronous and the SDK hands sessions out only from `profiles` and
/// `sign_in`, so those fill this; a session that turns out to be gone is
/// taken out.
type Sessions = Arc<Mutex<HashMap<(ServerKey, String), sdk::Session>>>;

/// Runs `call` on the SDK's runtime and maps its error. A task that
/// panicked or was cancelled is an error too, never a hang.
async fn on_runtime<T, F>(handle: &Handle, call: F) -> Result<T, BackendError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, sdk::SdkError>> + Send + 'static,
{
    match handle.spawn(call).await {
        Ok(result) => result.map_err(backend_error),
        Err(e) => Err(BackendError::Other(format!("the call did not finish: {e}"))),
    }
}

pub struct SdkClient {
    client: sdk::Client,
    handle: Handle,
    sessions: Sessions,
    states: Mutex<HashMap<ServerKey, watch::Sender<ConnectionState>>>,
    servers_changed: watch::Sender<()>,
}

impl SdkClient {
    /// `handle` is the runtime `client` was opened in; it must outlive this.
    pub fn new(client: sdk::Client, handle: Handle) -> Self {
        let servers_changed = watch::Sender::new(());
        let mut changes = client.watch_servers();
        let forward = servers_changed.clone();
        handle.spawn(async move {
            while changes.changed().await {
                forward.send_replace(());
            }
        });
        Self {
            client,
            handle,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            states: Mutex::new(HashMap::new()),
            servers_changed,
        }
    }

    fn remember(&self, session: &sdk::Session) -> Profile {
        let profile = profile(session);
        self.sessions.lock().expect("never poisoned").insert(
            (profile.server.clone(), profile.username.clone()),
            session.clone(),
        );
        profile
    }
}

impl Client for SdkClient {
    fn servers(&self) -> BoxFuture<'_, Result<Vec<Server>, BackendError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let entries = on_runtime(&self.handle, async move { client.servers().await }).await?;
            Ok(entries.into_iter().map(server).collect())
        })
    }

    fn add_address<'a>(
        &'a self,
        key: &'a ServerKey,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let address: SocketAddr = address.parse().map_err(|_| {
                BackendError::Other(format!(
                    "{address:?} is not an address; write it as 192.168.1.20:7420"
                ))
            })?;
            let client = self.client.clone();
            let id = key.0.clone();
            // The SDK lists an address-only server once it has connected:
            // connecting checks the key and learns the name and fingerprint.
            // Nothing is saved until the user has checked the fingerprint.
            let entry = on_runtime(&self.handle, async move {
                client.add_address(&id, vec![address])?;
                client.connect(&id).await
            })
            .await?;
            Ok(server(entry))
        })
    }

    fn connect<'a>(&'a self, key: &'a ServerKey) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move { client.connect(&id).await })
                .await
                .map(server)
        })
    }

    fn pin<'a>(
        &'a self,
        key: &'a ServerKey,
        verified: bool,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            let entry =
                on_runtime(&self.handle, async move { client.pin(&id, verified).await }).await?;
            self.servers_changed.send_replace(());
            Ok(server(entry))
        })
    }

    fn trust_new_identity<'a>(
        &'a self,
        saved: &'a ServerKey,
        now: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (client, old, new) = (self.client.clone(), saved.0.clone(), now.0.clone());
            let entry = on_runtime(&self.handle, async move {
                client.trust_new_identity(&old, &new).await
            })
            .await?;
            // The old key's sessions are gone with it.
            self.sessions
                .lock()
                .expect("never poisoned")
                .retain(|(server, _), _| server != saved);
            self.servers_changed.send_replace(());
            Ok(server(entry))
        })
    }

    fn profiles<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let id = key.0.clone();
            let sessions =
                on_runtime(&self.handle, async move { client.profiles(&id).await }).await?;
            // Sessions the SDK no longer has were ended; drop them here too.
            self.sessions
                .lock()
                .expect("never poisoned")
                .retain(|(server, _), _| server != key);
            Ok(sessions.iter().map(|s| self.remember(s)).collect())
        })
    }

    fn sign_in<'a>(
        &'a self,
        key: &'a ServerKey,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let (id, username, password) =
                (key.0.clone(), username.to_owned(), password.to_owned());
            let session = on_runtime(&self.handle, async move {
                client.sign_in(&id, &username, &password).await
            })
            .await?;
            Ok(self.remember(&session))
        })
    }

    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError> {
        let key = (profile.server.clone(), profile.username.clone());
        let session = self
            .sessions
            .lock()
            .expect("never poisoned")
            .get(&key)
            .cloned()
            .ok_or(BackendError::SignInRequired)?;
        Ok(Arc::new(SdkSession {
            session,
            handle: self.handle.clone(),
            sessions: self.sessions.clone(),
            key,
        }))
    }

    fn connection(&self, key: &ServerKey) -> watch::Receiver<ConnectionState> {
        let mut states = self.states.lock().expect("never poisoned");
        if let Some(sender) = states.get(key) {
            return sender.subscribe();
        }
        let sender = watch::Sender::new(ConnectionState::Closed);
        let receiver = sender.subscribe();
        match self.client.state(&key.0) {
            Ok(mut watch) => {
                let forward = sender.clone();
                self.handle.spawn(async move {
                    forward.send_replace(connection_state(watch.current()));
                    while let Some(state) = watch.changed().await {
                        forward.send_replace(connection_state(state));
                    }
                });
            }
            // A server the SDK does not know stays Closed.
            Err(e) => tracing::warn!(error = %e, server = %key.short(), "no connection state"),
        }
        states.insert(key.clone(), sender);
        receiver
    }

    fn servers_changed(&self) -> watch::Receiver<()> {
        self.servers_changed.subscribe()
    }
}

pub struct SdkSession {
    session: sdk::Session,
    handle: Handle,
    sessions: Sessions,
    key: (ServerKey, String),
}

impl SdkSession {
    /// Runs a call on the session; if the session turns out to be gone,
    /// forgets it, so "Who's watching" no longer offers it.
    async fn call<T, F, Fut>(&self, call: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(sdk::Session) -> Fut,
        Fut: Future<Output = Result<T, sdk::SdkError>> + Send + 'static,
    {
        let result = on_runtime(&self.handle, call(self.session.clone())).await;
        if matches!(result, Err(BackendError::SignInRequired)) {
            self.sessions
                .lock()
                .expect("never poisoned")
                .remove(&self.key);
        }
        result
    }
}

impl Session for SdkSession {
    fn home(&self) -> BoxFuture<'_, Result<HomeScreen, BackendError>> {
        Box::pin(self.call(|s| async move { s.home().await }))
    }

    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>> {
        Box::pin(self.call(|s| async move { s.libraries().await }))
    }

    fn items<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        let library = library.clone();
        Box::pin(self.call(move |s| async move {
            let page = s.items(&library, offset, limit).await?;
            Ok((page.items, page.total))
        }))
    }

    fn item<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(Item, Vec<Item>), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move {
            let detail = s.item(&id).await?;
            Ok((detail.item, detail.children))
        }))
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<Item>, BackendError>> {
        let query = query.to_owned();
        Box::pin(self.call(move |s| async move { s.search(&query, limit).await }))
    }

    fn tracks<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<MediaTracks, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.tracks(&id).await }))
    }

    fn playback_plan<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<(PlaybackPlan, Option<ChosenTracks>), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move {
            let plan = s.playback_plan(&id, selection).await?;
            Ok((plan.plan, plan.chosen))
        }))
    }

    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move {
            let playback = s.start_playback(&id, selection).await?;
            Ok(PlaybackStart {
                handle: playback.handle,
                plan: playback.plan,
                size: playback.size,
                chosen: playback.chosen,
            })
        }))
    }

    fn read<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move { s.read(&handle, offset, length).await }))
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move {
            let subtitle = s.read_subtitle(&handle).await?;
            Ok((subtitle.format, subtitle.bytes))
        }))
    }

    fn artwork<'a>(
        &'a self,
        id: &'a ArtworkId,
        size: ArtworkSize,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.artwork(&id, size).await }))
    }

    fn update_progress<'a>(
        &'a self,
        id: &'a MediaId,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.update_progress(&id, position).await }))
    }

    fn mark_watched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.mark_watched(&id).await }))
    }

    fn mark_unwatched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.mark_unwatched(&id).await }))
    }

    fn hide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.hide_from_continue(&id).await }))
    }

    fn unhide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.unhide_from_continue(&id).await }))
    }
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferences, BackendError>> {
        Box::pin(self.call(|s| async move { s.track_preferences().await }))
    }

    fn set_track_preferences<'a>(
        &'a self,
        preferences: &'a TrackPreferences,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let preferences = preferences.clone();
        Box::pin(self.call(move |s| async move { s.set_track_preferences(&preferences).await }))
    }

    fn set_track_override<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Selection,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.set_track_override(&id, selection).await }))
    }

    fn clear_track_override<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.clear_track_override(&id).await }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_credentials_keep_their_meaning() {
        assert_eq!(
            backend_error(sdk::SdkError::InvalidCredentials),
            BackendError::WrongCredentials
        );
    }

    #[test]
    fn a_dropped_session_asks_for_sign_in() {
        assert_eq!(
            backend_error(sdk::SdkError::SignInRequired),
            BackendError::SignInRequired
        );
    }

    #[test]
    fn the_servers_unavailable_becomes_unavailable() {
        assert_eq!(
            backend_error(sdk::SdkError::Server("unavailable: Plex is down".into())),
            BackendError::Unavailable("Plex is down".into())
        );
    }

    #[test]
    fn another_server_error_keeps_its_message() {
        assert_eq!(
            backend_error(sdk::SdkError::Server("internal server error".into())),
            BackendError::Other("server: internal server error".into())
        );
    }

    #[test]
    fn rate_limits_keep_their_wait() {
        let wait = Duration::from_secs(30);
        assert_eq!(
            backend_error(sdk::SdkError::RateLimited { retry_after: wait }),
            BackendError::RateLimited(wait)
        );
    }

    #[test]
    fn a_disconnected_server_shows_as_closed() {
        assert_eq!(
            connection_state(sdk::ConnectionState::Disconnected),
            ConnectionState::Closed
        );
    }

    #[test]
    fn reconnecting_keeps_its_attempt() {
        assert_eq!(
            connection_state(sdk::ConnectionState::Reconnecting { attempt: 3 }),
            ConnectionState::Reconnecting { attempt: 3 }
        );
    }
}
