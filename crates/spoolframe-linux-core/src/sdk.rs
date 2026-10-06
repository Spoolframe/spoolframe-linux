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
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Chunk, Client, ConnectionPath,
    ConnectionState, DeviceInfo, DeviceSignIn, DeviceSignInRequest, HomeScreen, PersonId, PinProof,
    PinState, PlaybackHandle, PlaybackPlan, PlaybackStart, PlexLink, Previews, Profile,
    ProfileSignIn, SearchRequest, SearchResults, Selection, Server, ServerKey, Session,
};
use crate::backend::{
    FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyInfo, PasskeyRequest,
    Trust, TrustedProfile,
};
use crate::storage::Storage;

/// The SDK's errors, in the kinds the UI acts on.
pub fn backend_error(error: sdk::SdkError) -> BackendError {
    use sdk::SdkError as E;
    match error {
        E::Unreachable(what) => BackendError::Unreachable(what),
        E::TimedOut(what) => BackendError::TimedOut(what),
        E::PlaybackStopped => BackendError::PlaybackStopped,
        E::InvalidCredentials => BackendError::WrongCredentials,
        E::NoPassword => BackendError::NoPassword,
        E::RateLimited { retry_after } => BackendError::RateLimited(retry_after),
        E::SignInRequired => BackendError::SignInRequired,
        E::CannotPlay { reasons } => BackendError::CannotPlay(reasons),
        E::PlaybackExpired => BackendError::PlaybackExpired,
        E::NotFound(what) => BackendError::NotFound(what),
        E::NotPinned(id) => BackendError::NotPinned(id),
        E::IdentityChanged { saved, now } => BackendError::IdentityChanged { saved, now },
        E::Busy => BackendError::Busy,
        E::ProfileLocked => BackendError::ProfileLocked,
        E::InvalidInput(why) => BackendError::InvalidInput(why),
        E::PlaybackRestarted { epoch } => BackendError::PlaybackRestarted { epoch },
        E::SubtitleNotReady { done_percent } => BackendError::SubtitleNotReady(done_percent),
        E::NotSetUp => BackendError::NotSetUp,
        E::PreviewsNotReady { done_percent } => BackendError::PreviewsNotReady(done_percent),
        E::NoPreviews => BackendError::NoPreviews,
        E::SignInDenied => BackendError::SignInDenied,
        E::SignInExpired => BackendError::SignInExpired,
        E::PlaybackFailed(why) => BackendError::Other(format!("the conversion failed: {why}")),
        E::Unavailable(message) => BackendError::Unavailable(message),
        E::RecentSignInRequired => BackendError::RecentSignInRequired,
        E::ServerFull => BackendError::ServerFull,
        E::LastSignInMethod => BackendError::LastSignInMethod,
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

/// The SDK's connection path, in the UI's words.
pub fn connection_path(path: sdk::ConnectionPath) -> ConnectionPath {
    match path {
        sdk::ConnectionPath::Lan { addr } => ConnectionPath::Lan(addr),
        sdk::ConnectionPath::Direct { addr } => ConnectionPath::Direct(addr),
        sdk::ConnectionPath::Relay => ConnectionPath::Relay,
        sdk::ConnectionPath::Unknown => ConnectionPath::Unknown,
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
        verified_at: entry.verified_at,
        last_connected_at: entry.last_connected_at,
    }
}

fn profile(session: &sdk::Session) -> Profile {
    let user = session.user();
    Profile {
        server: ServerKey(session.server()),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        locked: user.locked,
        sign_in: ProfileSignIn::Automatic,
        trust: None,
    }
}

/// `profiles` with the remembered ones of `trusted` (P4-01) folded in by
/// username: one with a session keeps opening with it; one without opens
/// by resuming; one only remembered is added.
pub fn with_trust(
    mut profiles: Vec<Profile>,
    trusted: Vec<TrustedProfile>,
    server: &ServerKey,
) -> Vec<Profile> {
    for remembered in trusted {
        let mut trust = Trust {
            user_id: remembered.user_id,
            pin_required: remembered.pin_required,
            auto_login: remembered.auto_login,
            by_passkey: false,
        };
        match profiles
            .iter_mut()
            .find(|p| p.username == remembered.username)
        {
            Some(profile) => {
                trust.by_passkey = profile.sign_in == ProfileSignIn::Passkey;
                if profile.sign_in != ProfileSignIn::Automatic {
                    profile.sign_in = ProfileSignIn::Resume;
                }
                profile.trust = Some(trust);
            }
            None => profiles.push(Profile {
                server: server.clone(),
                username: remembered.username,
                display_name: remembered.display_name,
                locked: false,
                sign_in: ProfileSignIn::Resume,
                trust: Some(trust),
            }),
        }
    }
    profiles
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

/// [`on_runtime`] for a call that stops when its future is dropped: a
/// wait the UI gives up on must not go on asking the server.
async fn on_runtime_until_dropped<T, F>(handle: &Handle, call: F) -> Result<T, BackendError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, sdk::SdkError>> + Send + 'static,
{
    struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
    impl<T> Drop for AbortOnDrop<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut task = AbortOnDrop(handle.spawn(call));
    match (&mut task.0).await {
        Ok(result) => result.map_err(backend_error),
        Err(e) => Err(BackendError::Other(format!("the call did not finish: {e}"))),
    }
}

pub struct SdkClient {
    client: sdk::Client,
    handle: Handle,
    sessions: Sessions,
    states: Mutex<HashMap<ServerKey, watch::Sender<ConnectionState>>>,
    sign_outs: Mutex<HashMap<ServerKey, watch::Sender<u64>>>,
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
            sign_outs: Mutex::new(HashMap::new()),
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

    fn forget<'a>(&'a self, key: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move { client.forget(&id).await }).await?;
            self.sessions
                .lock()
                .expect("never poisoned")
                .retain(|(server, _), _| server != key);
            self.servers_changed.send_replace(());
            Ok(())
        })
    }

    fn resume(&self) -> BoxFuture<'_, ()> {
        let client = self.client.clone();
        Box::pin(async move {
            let _ = on_runtime(&self.handle, async move {
                client.resume().await;
                Ok::<_, sdk::SdkError>(())
            })
            .await;
        })
    }

    fn connection_path(&self, key: &ServerKey) -> ConnectionPath {
        // A malformed id has no connection either.
        self.client
            .connection_path(&key.0)
            .map_or(ConnectionPath::Unknown, connection_path)
    }

    fn forget_profile<'a>(
        &'a self,
        key: &'a ServerKey,
        username: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (client, id, name) = (self.client.clone(), key.0.clone(), username.to_owned());
            on_runtime(&self.handle, async move {
                client.forget_profile(&id, &name).await
            })
            .await?;
            self.sessions
                .lock()
                .expect("never poisoned")
                .remove(&(key.clone(), username.to_owned()));
            Ok(())
        })
    }

    fn sign_out<'a>(&'a self, key: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move {
                for session in client.profiles(&id).await? {
                    session.sign_out().await?;
                }
                // Those whose sessions had ended have no session to sign
                // out; their names go too.
                for saved in client.saved_profiles(&id).await? {
                    client.forget_profile(&id, &saved.username).await?;
                }
                Ok::<_, sdk::SdkError>(())
            })
            .await?;
            self.sessions
                .lock()
                .expect("never poisoned")
                .retain(|(server, _), _| server != key);
            Ok(())
        })
    }

    fn profiles<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let id = key.0.clone();
            let (sessions, saved, trusted) = on_runtime(&self.handle, async move {
                let sessions = client.profiles(&id).await?;
                let saved = client.saved_profiles(&id).await?;
                // Asked of the server; without an answer the picker still
                // shows what the device keeps.
                let trusted = client.trusted_profiles(&id).await.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "cannot list the remembered profiles");
                    Vec::new()
                });
                Ok::<_, sdk::SdkError>((sessions, saved, trusted))
            })
            .await?;
            // Sessions the SDK no longer has were ended; drop them here too.
            self.sessions
                .lock()
                .expect("never poisoned")
                .retain(|(server, _), _| server != key);
            let mut profiles: Vec<Profile> = sessions.iter().map(|s| self.remember(s)).collect();
            // Ended sessions are kept by name, to sign in again.
            for saved in saved {
                if saved.sign_in == sdk::ProfileSignIn::Password
                    && !profiles.iter().any(|p| p.username == saved.username)
                {
                    // Offered the way they last signed in, as far as it
                    // can be offered again here.
                    let sign_in = match saved.method {
                        Some(sdk::SignInMethod::Passkey) => ProfileSignIn::Passkey,
                        _ => ProfileSignIn::Password,
                    };
                    profiles.push(Profile {
                        server: key.clone(),
                        username: saved.username,
                        display_name: saved.display_name,
                        locked: false,
                        sign_in,
                        trust: None,
                    });
                }
            }
            Ok(with_trust(profiles, trusted, key))
        })
    }

    fn sign_in<'a>(
        &'a self,
        key: &'a ServerKey,
        username: &'a str,
        password: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let (id, username, password) =
                (key.0.clone(), username.to_owned(), password.to_owned());
            let session = on_runtime(&self.handle, async move {
                client.sign_in(&id, &username, &password, remember).await
            })
            .await?;
            Ok(self.remember(&session))
        })
    }

    fn trusted_profiles<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<TrustedProfile>, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(
                &self.handle,
                async move { client.trusted_profiles(&id).await },
            )
            .await
        })
    }

    fn resume_profile<'a>(
        &'a self,
        key: &'a ServerKey,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let (client, id, user) = (self.client.clone(), key.0.clone(), user_id.to_owned());
            let session = on_runtime(&self.handle, async move {
                client.resume_profile(&id, &user).await
            })
            .await?;
            Ok(self.remember(&session))
        })
    }

    fn begin_passkey_sign_in<'a>(
        &'a self,
        key: &'a ServerKey,
        username: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PasskeyRequest, BackendError>> {
        Box::pin(async move {
            let (client, id, username) = (
                self.client.clone(),
                key.0.clone(),
                username.map(str::to_owned),
            );
            on_runtime(&self.handle, async move {
                client.begin_passkey_sign_in(&id, username.as_deref()).await
            })
            .await
        })
    }

    fn finish_passkey_sign_in<'a>(
        &'a self,
        key: &'a ServerKey,
        signed: FinishPasskeySignIn,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            let session = on_runtime(&self.handle, async move {
                client.finish_passkey_sign_in(&id, signed, remember).await
            })
            .await?;
            Ok(self.remember(&session))
        })
    }

    fn begin_device_sign_in<'a>(
        &'a self,
        key: &'a ServerKey,
        remember: bool,
    ) -> BoxFuture<'a, Result<DeviceSignIn, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            let begun = on_runtime(&self.handle, async move {
                client.begin_device_sign_in(&id, remember).await
            })
            .await?;
            Ok(DeviceSignIn {
                server: key.clone(),
                remember: begun.remember,
                display_code: begun.display_code.clone(),
                web_url: begun.web_url.clone(),
                expires_in: begun.expires_in,
                request: Arc::new(begun),
            })
        })
    }

    fn await_device_sign_in<'a>(
        &'a self,
        sign_in: &'a DeviceSignIn,
    ) -> BoxFuture<'a, Result<(Profile, Option<bool>), BackendError>> {
        Box::pin(async move {
            let begun = sign_in
                .request
                .downcast_ref::<sdk::DeviceSignIn>()
                .ok_or_else(|| BackendError::Other("a sign-in begun elsewhere".into()))?
                .clone();
            let client = self.client.clone();
            let session = on_runtime_until_dropped(&self.handle, async move {
                client.await_device_sign_in(&begun).await
            })
            .await?;
            let remembered = session.remembered();
            Ok((self.remember(&session), remembered))
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
            client: self.client.clone(),
            session,
            handle: self.handle.clone(),
            sessions: self.sessions.clone(),
            key,
        }))
    }

    fn signed_out(&self, key: &ServerKey) -> watch::Receiver<u64> {
        let mut sign_outs = self.sign_outs.lock().expect("never poisoned");
        if let Some(sender) = sign_outs.get(key) {
            return sender.subscribe();
        }
        let sender = watch::Sender::new(0);
        let receiver = sender.subscribe();
        match self.client.signed_out(&key.0) {
            Ok(mut watch) => {
                let forward = sender.clone();
                self.handle.spawn(async move {
                    while watch.next().await.is_some() {
                        forward.send_modify(|n| *n += 1);
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "cannot watch for sign-outs"),
        }
        sign_outs.insert(key.clone(), sender);
        receiver
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
    client: sdk::Client,
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
    fn activate(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        let client = self.client.clone();
        Box::pin(self.call(move |s| async move { client.activate_profile(&s).await.map(|_| ()) }))
    }

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
        request: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<SearchResults, BackendError>> {
        let request = request.clone();
        Box::pin(self.call(move |s| async move { s.search(request).await }))
    }

    fn person_items<'a>(
        &'a self,
        person: &'a PersonId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        let person = person.clone();
        Box::pin(self.call(move |s| async move {
            s.person_items(&person, offset, limit)
                .await
                .map(|page| (page.items, page.total))
        }))
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
        start: Option<Duration>,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move {
            let options = sdk::PlaybackOptions {
                start,
                ..Default::default()
            };
            s.start_playback_with(&id, selection, options).await
        }))
    }

    fn read_chunk<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        epoch: u32,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Chunk, BackendError>> {
        let handle = handle.clone();
        Box::pin(
            self.call(move |s| async move { s.read_chunk(&handle, epoch, offset, length).await }),
        )
    }

    fn seek_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<u32, BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move { s.seek_playback(&handle, position).await }))
    }

    fn stop_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move { s.stop_playback(&handle).await }))
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

    fn web_url(&self) -> Option<String> {
        self.session.web_url()
    }

    fn connection_path(&self) -> ConnectionPath {
        connection_path(self.session.connection_path())
    }

    fn previews<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<Previews, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.previews(&id).await }))
    }

    fn update_progress<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move { s.update_progress(&handle, position).await }))
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

    fn unlock<'a>(&'a self, pin: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let pin = pin.to_owned();
        Box::pin(self.call(move |s| async move { s.unlock(&pin).await }))
    }

    fn pin(&self) -> BoxFuture<'_, Result<Option<PinState>, BackendError>> {
        Box::pin(self.call(|s| async move { s.whoami().await.map(|user| user.pin) }))
    }

    fn set_pin<'a>(
        &'a self,
        pin: &'a str,
        proof: Option<PinProof>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let pin = pin.to_owned();
        Box::pin(self.call(move |s| async move { s.set_pin(&pin, proof).await }))
    }

    fn clear_pin(&self, proof: PinProof) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(move |s| async move { s.clear_pin(proof).await }))
    }

    fn plex_link(&self) -> BoxFuture<'_, Result<Option<PlexLink>, BackendError>> {
        Box::pin(self.call(|s| async move { s.plex_link().await }))
    }

    fn confirm_password<'a>(
        &'a self,
        password: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let password = password.to_owned();
        Box::pin(self.call(move |s| async move { s.confirm_password(&password).await }))
    }

    fn begin_confirm_passkey(&self) -> BoxFuture<'_, Result<PasskeyRequest, BackendError>> {
        Box::pin(self.call(|s| async move { s.begin_confirm_passkey().await }))
    }

    fn finish_confirm_passkey(
        &self,
        signed: FinishPasskeySignIn,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(move |s| async move { s.finish_confirm_passkey(signed).await }))
    }

    fn begin_passkey_registration(&self) -> BoxFuture<'_, Result<PasskeyCreation, BackendError>> {
        Box::pin(self.call(|s| async move { s.begin_passkey_registration().await }))
    }

    fn finish_passkey_registration(
        &self,
        made: FinishPasskeyRegistration,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(move |s| async move { s.finish_passkey_registration(made).await }))
    }

    fn passkeys(&self) -> BoxFuture<'_, Result<Vec<PasskeyInfo>, BackendError>> {
        Box::pin(self.call(|s| async move { s.passkeys().await }))
    }

    fn rename_passkey<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let (id, name) = (id.to_owned(), name.to_owned());
        Box::pin(self.call(move |s| async move { s.rename_passkey(&id, &name).await }))
    }

    fn remove_passkey<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.remove_passkey(&id).await }))
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceInfo>, BackendError>> {
        Box::pin(self.call(|s| async move { s.devices().await }))
    }

    fn set_pin_required<'a>(
        &'a self,
        device: Option<&'a str>,
        required: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let device = device.map(str::to_owned);
        Box::pin(
            self.call(
                move |s| async move { s.set_pin_required(device.as_deref(), required).await },
            ),
        )
    }
    fn lookup_device_sign_in<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<DeviceSignInRequest, BackendError>> {
        let code = code.to_owned();
        Box::pin(self.call(move |s| async move { s.lookup_device_sign_in(&code).await }))
    }

    fn approve_device_sign_in<'a>(
        &'a self,
        code: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let code = code.to_owned();
        Box::pin(self.call(move |s| async move { s.approve_device_sign_in(&code, remember).await }))
    }

    fn set_device_trust<'a>(
        &'a self,
        device: Option<&'a str>,
        trusted: bool,
        auto_login: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let device = device.map(str::to_owned);
        Box::pin(self.call(move |s| async move {
            s.set_device_trust(device.as_deref(), trusted, auto_login)
                .await
        }))
    }

    fn sign_out(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(|s| async move { s.sign_out().await }))
    }

    fn deny_device_sign_in<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let code = code.to_owned();
        Box::pin(self.call(move |s| async move { s.deny_device_sign_in(&code).await }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_profile_last_in_by_passkey_still_says_passkey() {
        let server = ServerKey("home".into());
        let saved = Profile {
            server: server.clone(),
            username: "sam".into(),
            display_name: "Sam".into(),
            locked: false,
            sign_in: ProfileSignIn::Passkey,
            trust: None,
        };
        let remembered = TrustedProfile {
            user_id: "u-sam".into(),
            username: "sam".into(),
            display_name: "Sam".into(),
            pin_required: false,
            auto_login: false,
        };
        let merged = with_trust(vec![saved], vec![remembered], &server);
        assert!(merged[0].trust.as_ref().is_some_and(|t| t.by_passkey));
    }

    #[test]
    fn previews_not_ready_or_none_keep_their_meaning() {
        assert_eq!(
            [
                backend_error(sdk::SdkError::PreviewsNotReady { done_percent: 40 }),
                backend_error(sdk::SdkError::NoPreviews)
            ],
            [BackendError::PreviewsNotReady(40), BackendError::NoPreviews]
        );
    }

    #[test]
    fn a_server_not_set_up_keeps_its_meaning() {
        assert_eq!(
            backend_error(sdk::SdkError::NotSetUp),
            BackendError::NotSetUp
        );
    }

    #[test]
    fn a_refused_or_expired_device_sign_in_keeps_its_meaning() {
        assert_eq!(
            [
                backend_error(sdk::SdkError::SignInDenied),
                backend_error(sdk::SdkError::SignInExpired)
            ],
            [BackendError::SignInDenied, BackendError::SignInExpired]
        );
    }

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
            backend_error(sdk::SdkError::Unavailable("Plex is down".into())),
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
