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

use crate::backend::Reachability;
use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Chunk, Client, ConnectionPath,
    ConnectionState, DeviceInfo, DeviceSignIn, DeviceSignInRequest, HomeScreen, Invite, PersonId,
    PinProof, PinState, PlaybackHandle, PlaybackPlan, PlaybackStart, PlexLink, Previews, Profile,
    ProfileSignIn, SearchRequest, SearchResults, Selection, Server, ServerKey, Session,
};
use crate::backend::{
    EntryId, ItemDetail, Kinds, PlaySettings, PlaylistEdit, PlaylistList, PlaylistNext,
    PlaylistPage, PlaylistSummary, PlaylistUpdate, ShareCandidatePage, Visibility,
};
use crate::backend::{
    FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyInfo, PasskeyRequest,
    TrackPreferencesInfo, Trust, TrustedProfile,
};
use crate::backend::{InviteKind, InviteOrigin, InviteSpec, InviteSummary, MintedInvite};
use crate::backend::{SubtitleDownloaded, SubtitleSearch, SubtitleTiming};
use crate::storage::Storage;

fn invite_kind(kind: sdk::InviteKind) -> InviteKind {
    match kind {
        sdk::InviteKind::CreateUser => InviteKind::CreateUser,
        sdk::InviteKind::PortableAccount => InviteKind::PortableAccount,
        sdk::InviteKind::Federation => InviteKind::Federation,
        _ => InviteKind::Unknown,
    }
}

/// The SDK's invite to make; an unknown kind is never made, so it cannot
/// come from here.
fn sdk_invite_spec(spec: InviteSpec) -> sdk::InviteSpec {
    sdk::InviteSpec {
        kind: match spec.kind {
            InviteKind::CreateUser | InviteKind::Unknown => sdk::InviteKind::CreateUser,
            InviteKind::PortableAccount => sdk::InviteKind::PortableAccount,
            InviteKind::Federation => sdk::InviteKind::Federation,
        },
        note: spec.note,
        uses: spec.uses,
        expires_in: spec.expires_in,
        role: spec.role,
    }
}

fn minted_invite(made: sdk::MintedInvite) -> MintedInvite {
    MintedInvite {
        id: made.id,
        kind: invite_kind(made.kind),
        code: made.code,
        link: made.link,
        share_link: made.share_link,
        copy_string: made.copy_string,
        expires_at: Some(made.expires_at),
        uses: Some(made.uses),
    }
}

/// The SDK lists only invites that still work: none is revoked.
fn invite_summary(invite: sdk::InviteSummary) -> InviteSummary {
    InviteSummary {
        id: invite.id,
        kind: invite_kind(invite.kind),
        note: invite.note,
        uses: Some(invite.uses),
        used: invite.used,
        expires_at: Some(invite.expires_at),
        created_by: invite.created_by.display_name,
        created_by_username: invite.created_by.username,
        created_from: match invite.created_from {
            sdk::InviteOrigin::WebAdmin => InviteOrigin::WebAdmin,
            sdk::InviteOrigin::Cli => InviteOrigin::Cli,
            sdk::InviteOrigin::Device {
                name,
                platform,
                this_device,
            } => InviteOrigin::Device {
                name,
                platform,
                this_device,
            },
            _ => InviteOrigin::Unknown,
        },
        revoked: false,
    }
}

/// The identity a session is a guest with (P7-06). The server names a
/// guest's session by its identity, `portable:` and its hex, and an
/// account's by `local:`. A guest's own `portable` list is empty (P7-09),
/// so the identity is read from the id.
fn guest_identity(
    user: &sdk::UserSummary,
) -> Option<spoolframe_protocol::portable::PortableUserId> {
    user.user_id
        .strip_prefix("portable:")
        .and_then(|hex| hex.parse().ok())
}

/// A tie as the UI keeps it: the server's Unix seconds as times.
fn tie(tie: sdk::PortableTie) -> crate::backend::Tie {
    let at = |secs: i64| std::time::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64);
    crate::backend::Tie {
        id: tie.id,
        tied_at: at(tie.tied_at),
        last_used_at: tie.last_used_at.map(at),
    }
}

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
        E::ServerIdChanged { saved, now, .. } => BackendError::ServerIdChanged { saved, now },
        E::Busy => BackendError::Busy,
        E::WanStreamsFull { max } => BackendError::WanStreamsFull { max },
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
        E::UpdateRequired { server_protocol } => BackendError::UpdateRequired { server_protocol },
        E::NotAnInvite => BackendError::NotAnInvite,
        E::InviteInvalid => BackendError::InviteInvalid,
        E::InviteExpired => BackendError::InviteExpired,
        E::InviteExhausted => BackendError::InviteExhausted,
        E::UsernameTaken => BackendError::UsernameTaken,
        E::AdminRequired => BackendError::AdminOnly,
        E::LastAdmin => BackendError::LastAdmin,
        E::PasswordChangeRequired => BackendError::PasswordChangeRequired,
        E::Unsupported { feature } => BackendError::Unsupported(feature),
        E::PlaylistReadOnly => BackendError::PlaylistReadOnly,
        E::KindNotAllowed { kind } => BackendError::KindNotAllowed(kind),
        E::PlaylistConflict { revision } => BackendError::PlaylistConflict { revision },
        E::NotAllowed => BackendError::NotAllowed,
        E::ImageTooLarge {
            max_bytes,
            max_pixels,
        } => BackendError::ImageTooLarge {
            max_bytes,
            max_pixels,
        },
        E::ImageUnreadable => BackendError::ImageUnreadable,
        E::Server(message) => server_error(message),
        E::UnknownServer(id) => BackendError::NotFound(id),
        E::UnknownIdentity(_) | E::Credential(_) | E::NotThisDevice | E::DeviceKeyRevoked => {
            BackendError::NotPaired
        }
        other => BackendError::Other(other.to_string()),
    }
}

/// The SDK's 0.12.0 has no variants of its own for PX-02's two subtitle
/// refusals and passes them on as `SdkError::Server` with the protocol
/// error's message, so they are told apart by it here. A quota's site and
/// reset time are lost on the way: the message has only whose it is.
fn server_error(message: String) -> BackendError {
    const NO_SOURCE: &str = "this server has no subtitle source set up";
    const QUOTA: &str = "no subtitle downloads left today (";
    if message == NO_SOURCE {
        return BackendError::NoSubtitleSource;
    }
    if let Some(whose) = message
        .strip_prefix(QUOTA)
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let whose = match whose {
            "Account" => spoolframe_types::QuotaOf::Account,
            "User" => spoolframe_types::QuotaOf::User,
            _ => spoolframe_types::QuotaOf::Unknown,
        };
        return BackendError::QuotaExhausted {
            whose,
            site: None,
            resets_at: None,
        };
    }
    BackendError::Other(sdk::SdkError::Server(message).to_string())
}

/// The SDK's connection states, in the UI's words.
/// A link's state with why the SDK stopped dialling, if it did: a
/// disconnected link it stopped is [`ConnectionState::Stopped`].
pub fn link_state(state: sdk::ConnectionState, stopped: Option<sdk::SdkError>) -> ConnectionState {
    match (state, stopped) {
        (sdk::ConnectionState::Disconnected, Some(why)) => {
            ConnectionState::Stopped(backend_error(why))
        }
        (state, _) => connection_state(state),
    }
}

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
        server_id: entry.server_id,
        advertised: entry
            .advertised_addrs
            .iter()
            .map(SocketAddr::to_string)
            .collect(),
        relays: entry.relays,
        advertised_at: entry.advertised_at,
        use_relays: entry.use_relays,
        relays_edited: entry.relays_edited,
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
    identities: crate::identity::Identities,
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
        // Keys on this computer through libfido2, and their names beside
        // the app's data (P7-08).
        let identities = crate::identity::Identities::new(
            client.portable(),
            Some(client.clone()),
            crate::paths::data_dir().map(|d| d.join("identity-keys.json")),
            Arc::new(|prompt, held| {
                Arc::new(crate::security_key::Fido2SecurityKey::new(prompt).avoiding(held))
                    as Arc<dyn sdk::SecurityKey>
            }),
            Some(handle.clone()),
        )
        .with_eraser(Arc::new(|prompt, ids| {
            crate::security_key::Fido2SecurityKey::new(prompt)
                .erase(&ids)
                .map_err(crate::security_key::sdk_error)
        }))
        .with_probe(Arc::new(|ids| {
            crate::security_key::plugged_in_holding(&ids).map_err(crate::security_key::sdk_error)
        }));
        Self {
            identities,
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

    fn read_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Invite, BackendError>> {
        Box::pin(async move {
            let (client, link) = (self.client.clone(), link.to_owned());
            let info =
                on_runtime(&self.handle, async move { client.read_invite(&link).await }).await?;
            Ok(Invite {
                key: ServerKey(info.endpoint_id),
                fingerprint: info.fingerprint,
                name: info.name,
                addresses: info.addrs.iter().map(SocketAddr::to_string).collect(),
                relays: info.relays,
                has_code: info.has_code,
                expires_at: info.expires_at,
                role: info.role,
                saved: info.saved,
            })
        })
    }

    fn connect_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (client, link) = (self.client.clone(), link.to_owned());
            on_runtime(
                &self.handle,
                async move { client.connect_invite(&link).await },
            )
            .await
            .map(server)
        })
    }

    fn identities(&self) -> crate::identity::Identities {
        self.identities.clone()
    }

    fn reachability<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Reachability, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move { client.reachability(&id).await }).await
        })
    }

    fn set_server_relays<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Option<bool>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move {
                client.set_server_relays(&id, relays).await
            })
            .await?;
            self.servers_changed.send_replace(());
            Ok(())
        })
    }

    fn set_server_relay_urls<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Vec<String>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            on_runtime(&self.handle, async move {
                client.set_server_relay_urls(&id, relays).await
            })
            .await?;
            self.servers_changed.send_replace(());
            Ok(())
        })
    }

    fn save_invite_server<'a>(
        &'a self,
        link: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (client, link) = (self.client.clone(), link.to_owned());
            let entry = on_runtime(&self.handle, async move {
                client.save_invite_server(&link).await
            })
            .await?;
            self.servers_changed.send_replace(());
            Ok(server(entry))
        })
    }

    fn redeem_invite<'a>(
        &'a self,
        link: &'a str,
        username: &'a str,
        display_name: Option<&'a str>,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let client = self.client.clone();
            let (link, username, display_name, password) = (
                link.to_owned(),
                username.to_owned(),
                display_name.map(str::to_owned),
                password.to_owned(),
            );
            let session = on_runtime(&self.handle, async move {
                client
                    .redeem_invite(&link, &username, display_name.as_deref(), &password)
                    .await
            })
            .await?;
            self.servers_changed.send_replace(());
            Ok(self.remember(&session))
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
            // Every profile kept here and not among the sessions is offered
            // to sign in again: ended ones, and one signed in since
            // `profiles` answered. All count as kept (C7).
            for saved in saved {
                if !profiles.iter().any(|p| p.username == saved.username) {
                    // Offered the way they last signed in, as far as it
                    // can be offered again here.
                    let sign_in = match (saved.method, saved.portable) {
                        (Some(sdk::SignInMethod::Passkey), _) => ProfileSignIn::Passkey,
                        (Some(sdk::SignInMethod::Portable), Some(identity)) => {
                            ProfileSignIn::Portable(identity)
                        }
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

    fn sign_in_portable<'a>(
        &'a self,
        key: &'a ServerKey,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let (client, id) = (self.client.clone(), key.0.clone());
            let session = on_runtime(&self.handle, async move {
                client.sign_in_portable(&id, identity).await
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
                    forward.send_replace(link_state(watch.current(), watch.stopped()));
                    while let Some(state) = watch.changed().await {
                        forward.send_replace(link_state(state, watch.stopped()));
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

    fn playlist_events(&self) -> tokio::sync::broadcast::Receiver<crate::backend::PlaylistEvent> {
        self.client.playlist_events()
    }

    fn subtitle_timing_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::backend::SubtitleTimingEvent> {
        self.client.subtitle_timing_events()
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

    fn item_detail<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<ItemDetail, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move {
            let detail = s.item(&id).await?;
            Ok(ItemDetail {
                item: detail.item,
                children: detail.children,
                playlists: detail.playlists,
                liked: detail.liked,
                liked_children: detail.liked_children,
            })
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
        options: sdk::PlaybackOptions,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        let id = id.clone();
        Box::pin(
            self.call(move |s| async move { s.start_playback_with(&id, selection, options).await }),
        )
    }

    fn change_quality<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        quality: sdk::PlaybackQuality,
        position: Duration,
    ) -> BoxFuture<'a, Result<sdk::PlaybackChange, BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move {
            s.change_quality(&handle, quality, Default::default(), position)
                .await
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

    fn link_rate(&self) -> Option<u64> {
        self.session.link_rate()
    }

    fn limit_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        link_bitrate: u64,
        position: Duration,
    ) -> BoxFuture<'a, Result<sdk::PlaybackChange, BackendError>> {
        let handle = handle.clone();
        Box::pin(self.call(move |s| async move {
            s.limit_playback(&handle, link_bitrate, Default::default(), position)
                .await
        }))
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
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferencesInfo, BackendError>> {
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

    fn is_admin(&self) -> bool {
        self.session.user().role.is_admin()
    }

    fn password_change_required(&self) -> bool {
        self.session.password_change_required()
    }

    fn change_password<'a>(
        &'a self,
        current: &'a str,
        new: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let (current, new) = (current.to_owned(), new.to_owned());
        Box::pin(self.call(move |s| async move { s.change_password(&current, &new).await }))
    }

    fn create_invite(&self, spec: InviteSpec) -> BoxFuture<'_, Result<MintedInvite, BackendError>> {
        let spec = sdk_invite_spec(spec);
        Box::pin(async move {
            self.call(|s| async move { s.create_invite(spec).await })
                .await
                .map(minted_invite)
        })
    }

    fn invites(&self) -> BoxFuture<'_, Result<Vec<InviteSummary>, BackendError>> {
        Box::pin(async move {
            self.call(|s| async move { s.invites().await })
                .await
                .map(|invites| invites.into_iter().map(invite_summary).collect())
        })
    }

    fn revoke_invite<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.revoke_invite(&id).await }))
    }

    fn guest_identity(&self) -> Option<spoolframe_protocol::portable::PortableUserId> {
        guest_identity(self.session.user())
    }

    fn tie_info(&self) -> BoxFuture<'_, Result<Vec<crate::backend::Tie>, BackendError>> {
        Box::pin(async move {
            self.call(|s| async move { s.tie_info().await })
                .await
                .map(|ties| ties.into_iter().map(tie).collect())
        })
    }

    fn tie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(move |s| async move { s.tie(identity).await }))
    }

    fn untie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(move |s| async move { s.untie(identity).await }))
    }

    fn leave_server(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        // The SDK drops this profile itself once the server has let go,
        // and forgets the server with its last profile here.
        Box::pin(self.call(|s| async move { s.leave_server().await }))
    }

    fn can_leave(&self) -> bool {
        self.session.can_leave()
    }

    fn can_leave_now(&self) -> BoxFuture<'_, Result<bool, BackendError>> {
        Box::pin(self.call(|s| async move { s.can_leave_now().await }))
    }

    fn has_playlists(&self) -> bool {
        self.session.features().playlists
    }

    fn has_liked(&self) -> bool {
        self.session.features().liked
    }

    fn like<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        let items = items.to_vec();
        Box::pin(self.call(move |s| async move { s.like(&items).await }))
    }

    fn unlike<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        let items = items.to_vec();
        Box::pin(self.call(move |s| async move { s.unlike(&items).await }))
    }

    fn has_last_viewed(&self) -> bool {
        self.session.features().last_viewed
    }

    fn has_playlist_sharing(&self) -> bool {
        self.session.features().playlist_sharing
    }

    fn has_play_history(&self) -> bool {
        self.session.features().play_history
    }

    fn has_playlist_art(&self) -> bool {
        self.session.features().playlist_art
    }

    fn has_subtitle_sync(&self) -> bool {
        self.session.features().subtitle_sync
    }

    fn has_subtitle_search(&self) -> bool {
        self.session.features().subtitle_search
    }

    fn search_subtitles<'a>(
        &'a self,
        id: &'a MediaId,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<SubtitleSearch, BackendError>> {
        let (id, language) = (id.clone(), language.map(str::to_owned));
        Box::pin(
            self.call(move |s| async move { s.search_subtitles(&id, language.as_deref()).await }),
        )
    }

    fn download_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        result: &'a str,
    ) -> BoxFuture<'a, Result<SubtitleDownloaded, BackendError>> {
        let (id, result) = (id.clone(), result.to_owned());
        Box::pin(self.call(move |s| async move { s.download_subtitle(&id, &result).await }))
    }

    fn sync_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        let id = id.clone();
        Box::pin(self.call(move |s| async move { s.sync_subtitle(&id, track).await }))
    }

    fn use_original_subtitle_timing<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        let id = id.clone();
        Box::pin(
            self.call(move |s| async move { s.use_original_subtitle_timing(&id, track).await }),
        )
    }

    fn set_subtitle_offset<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
        offset_ms: i64,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        let id = id.clone();
        Box::pin(
            self.call(move |s| async move { s.set_subtitle_offset(&id, track, offset_ms).await }),
        )
    }

    fn remove_subtitle_download<'a>(
        &'a self,
        id: &'a MediaId,
        download: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let (id, download) = (id.clone(), download.to_owned());
        Box::pin(
            self.call(move |s| async move { s.remove_subtitle_download(&id, &download).await }),
        )
    }

    fn share_candidates<'a>(
        &'a self,
        id: &'a str,
        query: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<ShareCandidatePage, BackendError>> {
        let (id, query) = (id.to_owned(), query.to_owned());
        Box::pin(
            self.call(move |s| async move { s.share_candidates(&id, &query, offset, limit).await }),
        )
    }

    fn recently_played(
        &self,
        kinds: Option<Kinds>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'_, Result<(Vec<Item>, u32), BackendError>> {
        Box::pin(self.call(move |s| async move {
            let page = s.recently_played(kinds, offset, limit).await?;
            Ok((page.items, page.total))
        }))
    }

    fn playlists(&self) -> BoxFuture<'_, Result<PlaylistList, BackendError>> {
        Box::pin(self.call(|s| async move { s.playlists().await }))
    }

    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.playlist(&id, offset, limit).await }))
    }

    fn create_playlist<'a>(
        &'a self,
        name: &'a str,
        kinds: Kinds,
        visibility: Visibility,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        let name = name.to_owned();
        Box::pin(
            self.call(move |s| async move { s.create_playlist(&name, kinds, visibility).await }),
        )
    }

    fn update_playlist<'a>(
        &'a self,
        id: &'a str,
        update: PlaylistUpdate,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.update_playlist(&id, update).await }))
    }

    fn edit_playlist<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> BoxFuture<'a, Result<u64, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.edit_playlist(&id, revision, edits).await }))
    }

    fn delete_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.delete_playlist(&id).await }))
    }

    fn set_playlist_art<'a>(
        &'a self,
        id: &'a str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<ArtworkId, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.set_playlist_art(&id, bytes).await }))
    }

    fn remove_playlist_art<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.remove_playlist_art(&id).await }))
    }

    fn pin_playlist<'a>(
        &'a self,
        id: &'a str,
        position: u32,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.pin_playlist(&id, position).await }))
    }

    fn unpin_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.unpin_playlist(&id).await }))
    }

    fn dismiss_pin_notes(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(self.call(|s| async move { s.dismiss_pin_notes().await }))
    }

    fn play_playlist<'a>(
        &'a self,
        id: &'a str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.play_playlist(&id, from, settings).await }))
    }

    fn next_in_playlist<'a>(
        &'a self,
        id: &'a str,
        after: EntryId,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        let id = id.to_owned();
        Box::pin(self.call(move |s| async move { s.next_in_playlist(&id, after).await }))
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

    fn forget_device<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        let device = device.to_owned();
        Box::pin(self.call(move |s| async move { s.forget_device(&device).await }))
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

    fn id(byte: u8) -> spoolframe_protocol::portable::PortableUserId {
        spoolframe_protocol::portable::PortableUserId::from_bytes([byte; 32]).unwrap()
    }

    fn user(user_id: &str, portable: Option<u8>) -> sdk::UserSummary {
        sdk::UserSummary {
            user_id: user_id.into(),
            username: "alice".into(),
            display_name: "Alice".into(),
            role: spoolframe_types::Role::Consumer,
            locked: false,
            pin: None,
            portable: portable
                .map(|b| sdk::PortableTie {
                    id: id(b),
                    tied_at: 0,
                    last_used_at: None,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn a_session_signed_in_as_the_identity_is_a_guest_with_it() {
        // A guest's own list is empty (P7-09): the id names the identity.
        let guest = user(&format!("portable:{}", "07".repeat(32)), None);
        assert_eq!(guest_identity(&guest), Some(id(7)));
    }

    #[test]
    fn an_account_tied_to_an_identity_is_no_guest() {
        assert_eq!(guest_identity(&user("local:0192f1c4", Some(7))), None);
    }

    /// PX-46: off the home network, the server plays only so many at once.
    #[test]
    fn too_many_streams_outside_the_home_says_how_many_and_what_to_do() {
        assert_eq!(
            backend_error(sdk::SdkError::WanStreamsFull { max: 2 }).to_string(),
            "Too many people are watching from outside the server’s home right now (it \
             allows 2 at once). Try again when someone stops, or watch at home."
        );
    }

    /// An invite made with the server's CLI names no username; the list
    /// shows its display name.
    #[test]
    fn a_cli_invite_is_listed_by_its_makers_name_without_a_username() {
        let listed = invite_summary(sdk::InviteSummary {
            id: "inv".into(),
            kind: sdk::InviteKind::CreateUser,
            note: None,
            role: spoolframe_types::Role::Consumer,
            uses: 2,
            used: 1,
            expires_at: std::time::UNIX_EPOCH,
            created_at: std::time::UNIX_EPOCH,
            created_by: sdk::InviteCreator {
                display_name: "server CLI".into(),
                username: None,
            },
            created_from: sdk::InviteOrigin::Cli,
        });
        assert_eq!(
            (
                listed.created_by,
                listed.created_by_username,
                listed.created_from,
                listed.uses,
                listed.revoked
            ),
            (
                "server CLI".to_owned(),
                None,
                InviteOrigin::Cli,
                Some(2),
                false
            )
        );
    }

    /// A kind or a place from a newer server is listed as unknown, not
    /// taken for one this build knows.
    #[test]
    fn a_newer_servers_kind_and_origin_are_unknown() {
        assert_eq!(invite_kind(sdk::InviteKind::Unknown), InviteKind::Unknown);
    }

    #[test]
    fn not_being_the_admin_is_admin_only() {
        assert_eq!(
            backend_error(sdk::SdkError::AdminRequired),
            BackendError::AdminOnly
        );
    }

    /// Not an Other: the app greys Leave out for it rather than toasting.
    #[test]
    fn the_last_admin_leaving_is_last_admin() {
        assert_eq!(
            backend_error(sdk::SdkError::LastAdmin),
            BackendError::LastAdmin
        );
    }

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
    fn a_changed_server_id_keeps_both_ids() {
        assert_eq!(
            backend_error(sdk::SdkError::ServerIdChanged {
                endpoint_id: "k".into(),
                saved: "k7f3q2m4x9pq8z2a".into(),
                now: "w4n8b2r6t3kq7x5m".into(),
            }),
            BackendError::ServerIdChanged {
                saved: "k7f3q2m4x9pq8z2a".into(),
                now: "w4n8b2r6t3kq7x5m".into(),
            }
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
    fn playlist_refusals_keep_their_kind() {
        use spoolframe_protocol::playlists::ItemKind;
        assert_eq!(
            [
                sdk::SdkError::Unsupported {
                    feature: "spoolframe:playlists".into()
                },
                sdk::SdkError::PlaylistReadOnly,
                sdk::SdkError::KindNotAllowed {
                    kind: ItemKind::Movie
                },
                sdk::SdkError::PlaylistConflict { revision: 9 },
            ]
            .map(backend_error),
            [
                BackendError::Unsupported("spoolframe:playlists".into()),
                BackendError::PlaylistReadOnly,
                BackendError::KindNotAllowed(ItemKind::Movie),
                BackendError::PlaylistConflict { revision: 9 },
            ]
        );
    }

    #[test]
    fn cover_refusals_keep_their_kind() {
        assert_eq!(
            [
                sdk::SdkError::NotAllowed,
                sdk::SdkError::ImageTooLarge {
                    max_bytes: 20 << 20,
                    max_pixels: 40_000_000,
                },
                sdk::SdkError::ImageUnreadable,
            ]
            .map(backend_error),
            [
                BackendError::NotAllowed,
                BackendError::ImageTooLarge {
                    max_bytes: 20 << 20,
                    max_pixels: 40_000_000,
                },
                BackendError::ImageUnreadable,
            ]
        );
    }

    #[test]
    fn a_picture_too_large_says_the_limits_in_mb_and_megapixels() {
        let refused = BackendError::ImageTooLarge {
            max_bytes: 20 << 20,
            max_pixels: 40_000_000,
        };
        assert_eq!(
            refused.to_string(),
            "That picture is too large: at most 20 MB and 40 megapixels."
        );
    }

    /// The protocol's messages for PX-02's refusals, as the SDK passes
    /// them on (`ServerError`'s Display in spoolframe-protocol 0.12.0).
    #[test]
    fn a_server_without_a_subtitle_site_is_told_apart() {
        let message = spoolframe_protocol::Error::NoSubtitleSource.to_string();
        assert_eq!(
            backend_error(sdk::SdkError::Server(message)),
            BackendError::NoSubtitleSource
        );
    }

    #[test]
    fn used_up_downloads_say_whose() {
        let quota = |whose| {
            spoolframe_protocol::Error::QuotaExhausted {
                whose,
                resets_at: Some(1),
                site: Some("subdl".into()),
            }
            .to_string()
        };
        let whose = |message| match backend_error(sdk::SdkError::Server(message)) {
            BackendError::QuotaExhausted { whose, .. } => Some(whose),
            _ => None,
        };
        assert_eq!(
            [
                whose(quota(spoolframe_types::QuotaOf::Account)),
                whose(quota(spoolframe_types::QuotaOf::User))
            ],
            [
                Some(spoolframe_types::QuotaOf::Account),
                Some(spoolframe_types::QuotaOf::User)
            ]
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
    fn a_refused_protocol_says_to_update_with_the_servers_revision() {
        assert_eq!(
            backend_error(sdk::SdkError::UpdateRequired {
                server_protocol: Some(9)
            }),
            BackendError::UpdateRequired {
                server_protocol: Some(9)
            }
        );
    }

    /// Each refusal of an invite stays its own kind: the join flow draws
    /// one screen per kind (P6-01).
    #[test]
    fn invite_refusals_keep_their_kind() {
        for (sdk, backend) in [
            (sdk::SdkError::NotAnInvite, BackendError::NotAnInvite),
            (sdk::SdkError::InviteInvalid, BackendError::InviteInvalid),
            (sdk::SdkError::InviteExpired, BackendError::InviteExpired),
            (
                sdk::SdkError::InviteExhausted,
                BackendError::InviteExhausted,
            ),
            (sdk::SdkError::UsernameTaken, BackendError::UsernameTaken),
        ] {
            assert_eq!(backend_error(sdk), backend);
        }
    }

    /// A link the SDK stopped dialling says why, in place of a plain
    /// disconnect, so the app does not show it as about to reconnect.
    #[test]
    fn a_link_the_sdk_stopped_says_why() {
        assert_eq!(
            link_state(
                sdk::ConnectionState::Disconnected,
                Some(sdk::SdkError::UpdateRequired {
                    server_protocol: None
                })
            ),
            ConnectionState::Stopped(BackendError::UpdateRequired {
                server_protocol: None
            })
        );
        assert_eq!(
            link_state(sdk::ConnectionState::Disconnected, None),
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
