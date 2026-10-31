//! Stand-ins for this device and a user's account on a server: a
//! [`MockClient`] (its servers, profiles and sessions, a sign-in another
//! device approves, the relays it saves, its downloads' lock) and a
//! [`MockAccount`] session (leaving, ties, passkeys, invites, devices).
//! Each answers with what its test gave it, the last answer again once
//! the others are used, and keeps what it was sent. Anything not given
//! answers as the trait defaults do: `Unsupported`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use spoolframe_protocol::portable::PortableUserId;
use spoolframe_types::Item;

use crate::backend::{
    BackendError, BoxFuture, Client, DeviceInfo, DeviceSignIn, DownloadPhase, DownloadSource,
    DownloadState, DownloadTracks, FinishPasskeyRegistration, InviteSpec, InviteSummary,
    LocalDownload, LocalWatch, MintedInvite, PasskeyInfo, Profile, Server, ServerKey, Session, Tie,
};
use crate::identity::Identities;

/// Answers given in order; the last one stays.
struct Answers<T>(Mutex<VecDeque<T>>);

impl<T> Default for Answers<T> {
    fn default() -> Self {
        Self(Mutex::new(VecDeque::new()))
    }
}

impl<T: Clone> Answers<T> {
    fn extend(&self, answers: impl IntoIterator<Item = T>) {
        self.0.lock().unwrap().extend(answers);
    }

    fn next(&self) -> Option<T> {
        let mut answers = self.0.lock().unwrap();
        if answers.len() > 1 {
            answers.pop_front()
        } else {
            answers.front().cloned()
        }
    }
}

impl<T: Clone + Send + 'static> Answers<Result<T, BackendError>> {
    /// The next answer, or `Unsupported` for a call no test answered.
    fn answer(&self, call: &str) -> BoxFuture<'static, Result<T, BackendError>> {
        let answer = self
            .next()
            .unwrap_or_else(|| Err(BackendError::Unsupported(call.into())));
        Box::pin(async move { answer })
    }
}

/// How a sign-in another device approves ends, each time it is waited on.
#[derive(Debug, Clone)]
pub enum Approval {
    /// Approved as `profile`, remembered here as the approver chose.
    Approved {
        profile: Profile,
        remembered: Option<bool>,
    },
    Expired,
    Denied,
    /// Never answered: the wait lasts until it is dropped.
    Waiting,
}

/// One relay setting saved for a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayEdit {
    /// Through relays or not; `None` follows the app's setting.
    Use(ServerKey, Option<bool>),
    /// The user's own list; empty gives the server's back.
    Urls(ServerKey, Vec<String>),
}

/// This device, answering from canned data.
pub struct MockClient {
    identities: Identities,
    servers: Answers<Result<Vec<Server>, BackendError>>,
    profiles: Vec<Profile>,
    sessions: HashMap<String, Arc<dyn Session>>,
    web_url: Option<String>,
    approval: Option<Approval>,
    relay_edits: Mutex<Vec<RelayEdit>>,
    locked: AtomicBool,
    unlock_answers: Answers<Result<(), BackendError>>,
}

impl Default for MockClient {
    fn default() -> Self {
        Self {
            identities: crate::identity::in_memory(),
            servers: Answers::default(),
            profiles: Vec::new(),
            sessions: HashMap::new(),
            web_url: None,
            approval: None,
            relay_edits: Mutex::default(),
            locked: AtomicBool::new(false),
            unlock_answers: Answers::default(),
        }
    }
}

impl MockClient {
    /// No servers or profiles, and an identity store in memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// What `servers` lists.
    pub fn with_servers(self, servers: Vec<Server>) -> Self {
        self.servers.extend([Ok(servers)]);
        self
    }

    /// The profiles kept here: `profiles` lists a server's.
    pub fn with_profiles(mut self, profiles: Vec<Profile>) -> Self {
        self.profiles.extend(profiles);
        self
    }

    /// `session` is the one behind `username`'s profile.
    pub fn with_session(mut self, username: &str, session: Arc<dyn Session>) -> Self {
        self.sessions.insert(username.to_owned(), session);
        self
    }

    /// A sign-in another device approves begins with code `BCDF-GHJK`,
    /// approved on `web_url` (none for a server without a web address),
    /// and ends as `approval` each time it is waited on.
    pub fn with_device_sign_in(mut self, web_url: Option<&str>, approval: Approval) -> Self {
        self.web_url = web_url.map(str::to_owned);
        self.approval = Some(approval);
        self
    }

    /// The profile shown asks for its PIN before its downloads show,
    /// until `unlock_downloads_offline` is answered `Ok`; `answers` are
    /// its answers, in order.
    pub fn with_locked_downloads(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.locked.store(true, Ordering::SeqCst);
        self.unlock_answers.extend(answers);
        self
    }

    /// Every relay setting saved, in order.
    pub fn relay_edits(&self) -> Vec<RelayEdit> {
        self.relay_edits.lock().unwrap().clone()
    }

    fn relays_saved(&self, edit: RelayEdit) -> BoxFuture<'static, Result<(), BackendError>> {
        self.relay_edits.lock().unwrap().push(edit);
        Box::pin(async { Ok(()) })
    }
}

impl Client for MockClient {
    fn servers(&self) -> BoxFuture<'_, Result<Vec<Server>, BackendError>> {
        self.servers.answer("servers")
    }

    fn identities(&self) -> Identities {
        self.identities.clone()
    }

    fn set_server_relays<'a>(
        &'a self,
        server: &'a ServerKey,
        use_relays: Option<bool>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.relays_saved(RelayEdit::Use(server.clone(), use_relays))
    }

    fn set_server_relay_urls<'a>(
        &'a self,
        server: &'a ServerKey,
        relays: Vec<String>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.relays_saved(RelayEdit::Urls(server.clone(), relays))
    }

    fn profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        let kept = self
            .profiles
            .iter()
            .filter(|p| p.server == *server)
            .cloned()
            .collect();
        Box::pin(async move { Ok(kept) })
    }

    fn begin_device_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        remember: bool,
    ) -> BoxFuture<'a, Result<DeviceSignIn, BackendError>> {
        let begun = match self.approval {
            Some(_) => Ok(DeviceSignIn {
                server: server.clone(),
                display_code: "BCDF-GHJK".into(),
                remember,
                web_url: self.web_url.clone(),
                expires_in: Duration::from_secs(10 * 60),
                request: Arc::new(()),
            }),
            None => Err(BackendError::Unsupported("begin_device_sign_in".into())),
        };
        Box::pin(async move { begun })
    }

    fn await_device_sign_in<'a>(
        &'a self,
        _sign_in: &'a DeviceSignIn,
    ) -> BoxFuture<'a, Result<(Profile, Option<bool>), BackendError>> {
        let approval = self.approval.clone();
        Box::pin(async move {
            match approval {
                Some(Approval::Approved {
                    profile,
                    remembered,
                }) => Ok((profile, remembered)),
                Some(Approval::Expired) => Err(BackendError::SignInExpired),
                Some(Approval::Denied) => Err(BackendError::SignInDenied),
                Some(Approval::Waiting) => std::future::pending().await,
                None => Err(BackendError::Unsupported("await_device_sign_in".into())),
            }
        })
    }

    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError> {
        self.sessions
            .get(&profile.username)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(profile.username.clone()))
    }

    fn download_profile_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    fn unlock_downloads_offline<'a>(
        &'a self,
        _pin: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let answer = self
            .unlock_answers
            .next()
            .unwrap_or_else(|| Err(BackendError::Unsupported("unlock_downloads_offline".into())));
        if answer.is_ok() {
            self.locked.store(false, Ordering::SeqCst);
        }
        Box::pin(async move { answer })
    }
}

/// A user's session, answering for their account from canned data.
pub struct MockAccount {
    guest: Option<PortableUserId>,
    can_leave: bool,
    can_leave_now: Answers<Result<bool, BackendError>>,
    leave_answers: Answers<Result<(), BackendError>>,
    left: Mutex<u32>,
    ties: Answers<Result<Vec<Tie>, BackendError>>,
    tie_answers: Answers<Result<(), BackendError>>,
    tied: Mutex<Vec<PortableUserId>>,
    untie_answers: Answers<Result<(), BackendError>>,
    untied: Mutex<Vec<PortableUserId>>,
    passkeys: Answers<Result<Vec<PasskeyInfo>, BackendError>>,
    save_answers: Answers<Result<(), BackendError>>,
    rename_answers: Answers<Result<(), BackendError>>,
    renamed: Mutex<Vec<(String, String)>>,
    invites: Answers<Result<Vec<InviteSummary>, BackendError>>,
    create_answers: Answers<Result<MintedInvite, BackendError>>,
    revoke_answers: Answers<Result<(), BackendError>>,
    revoked: Mutex<Vec<String>>,
    devices: Answers<Result<Vec<DeviceInfo>, BackendError>>,
    trust_answers: Answers<Result<(), BackendError>>,
    trusted: Mutex<Vec<Trusted>>,
}

/// One `set_device_trust` the session was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trusted {
    pub device: Option<String>,
    pub trusted: bool,
    pub auto_login: bool,
}

impl Default for MockAccount {
    fn default() -> Self {
        Self {
            guest: None,
            // Anyone but the last admin may leave.
            can_leave: true,
            can_leave_now: Answers::default(),
            leave_answers: Answers::default(),
            left: Mutex::default(),
            ties: Answers::default(),
            tie_answers: Answers::default(),
            tied: Mutex::default(),
            untie_answers: Answers::default(),
            untied: Mutex::default(),
            passkeys: Answers::default(),
            save_answers: Answers::default(),
            rename_answers: Answers::default(),
            renamed: Mutex::default(),
            invites: Answers::default(),
            create_answers: Answers::default(),
            revoke_answers: Answers::default(),
            revoked: Mutex::default(),
            devices: Answers::default(),
            trust_answers: Answers::default(),
            trusted: Mutex::default(),
        }
    }
}

impl MockAccount {
    /// An account that may leave, with nothing else answered.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server's last admin, as the session and the server say:
    /// greyed out from the start, and refused if it leaves all the same.
    pub fn last_admin(self) -> Self {
        self.can_leave_now.extend([Ok(false)]);
        self.leave_answers.extend([Err(BackendError::LastAdmin)]);
        Self {
            can_leave: false,
            ..self
        }
    }

    /// A guest here with identity `id`, with no account.
    pub fn guest(self, id: PortableUserId) -> Self {
        Self {
            guest: Some(id),
            ..self
        }
    }

    /// `leave_server`'s answers, in order.
    pub fn with_leave_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.leave_answers.extend(answers);
        self
    }

    /// What `tie_info` lists, one list a call, in order.
    pub fn with_ties(self, lists: impl IntoIterator<Item = Vec<Tie>>) -> Self {
        self.ties.extend(lists.into_iter().map(Ok));
        self
    }

    /// `tie`'s answers, in order.
    pub fn with_tie_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.tie_answers.extend(answers);
        self
    }

    /// `untie`'s answers, in order.
    pub fn with_untie_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.untie_answers.extend(answers);
        self
    }

    /// What `passkeys` lists, one list a call, in order.
    pub fn with_passkeys(self, lists: impl IntoIterator<Item = Vec<PasskeyInfo>>) -> Self {
        self.passkeys.extend(lists.into_iter().map(Ok));
        self
    }

    /// `finish_passkey_registration`'s answers, in order.
    pub fn with_passkey_saves(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.save_answers.extend(answers);
        self
    }

    /// `rename_passkey`'s answers, in order.
    pub fn with_rename_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.rename_answers.extend(answers);
        self
    }

    /// What `invites` lists, as answers, in order.
    pub fn with_invites(
        self,
        answers: impl IntoIterator<Item = Result<Vec<InviteSummary>, BackendError>>,
    ) -> Self {
        self.invites.extend(answers);
        self
    }

    /// `create_invite`'s answers, in order.
    pub fn with_created_invites(
        self,
        answers: impl IntoIterator<Item = Result<MintedInvite, BackendError>>,
    ) -> Self {
        self.create_answers.extend(answers);
        self
    }

    /// `revoke_invite`'s answers, in order.
    pub fn with_revoke_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.revoke_answers.extend(answers);
        self
    }

    /// What `devices` lists, one list a call, in order.
    pub fn with_devices(self, lists: impl IntoIterator<Item = Vec<DeviceInfo>>) -> Self {
        self.devices.extend(lists.into_iter().map(Ok));
        self
    }

    /// `set_device_trust`'s answers, in order.
    pub fn with_trust_answers(
        self,
        answers: impl IntoIterator<Item = Result<(), BackendError>>,
    ) -> Self {
        self.trust_answers.extend(answers);
        self
    }

    /// How many times `leave_server` was sent.
    pub fn left(&self) -> u32 {
        *self.left.lock().unwrap()
    }

    /// The identities `tie` was sent, in order.
    pub fn tied(&self) -> Vec<PortableUserId> {
        self.tied.lock().unwrap().clone()
    }

    /// The identities `untie` was sent, in order.
    pub fn untied(&self) -> Vec<PortableUserId> {
        self.untied.lock().unwrap().clone()
    }

    /// Every `rename_passkey` sent, as (id, name), in order.
    pub fn renamed(&self) -> Vec<(String, String)> {
        self.renamed.lock().unwrap().clone()
    }

    /// The invites `revoke_invite` was sent, in order.
    pub fn revoked(&self) -> Vec<String> {
        self.revoked.lock().unwrap().clone()
    }

    /// Every `set_device_trust` sent, in order.
    pub fn trusted(&self) -> Vec<Trusted> {
        self.trusted.lock().unwrap().clone()
    }
}

impl Session for MockAccount {
    fn guest_identity(&self) -> Option<PortableUserId> {
        self.guest
    }

    fn can_leave(&self) -> bool {
        self.can_leave
    }

    fn can_leave_now(&self) -> BoxFuture<'_, Result<bool, BackendError>> {
        let answer = self.can_leave_now.next().unwrap_or(Ok(self.can_leave));
        Box::pin(async move { answer })
    }

    fn leave_server(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        *self.left.lock().unwrap() += 1;
        self.leave_answers.answer("leave_server")
    }

    fn tie_info(&self) -> BoxFuture<'_, Result<Vec<Tie>, BackendError>> {
        self.ties.answer("tie_info")
    }

    fn tie(&self, identity: PortableUserId) -> BoxFuture<'_, Result<(), BackendError>> {
        self.tied.lock().unwrap().push(identity);
        self.tie_answers.answer("tie")
    }

    fn untie(&self, identity: PortableUserId) -> BoxFuture<'_, Result<(), BackendError>> {
        self.untied.lock().unwrap().push(identity);
        self.untie_answers.answer("untie")
    }

    fn passkeys(&self) -> BoxFuture<'_, Result<Vec<PasskeyInfo>, BackendError>> {
        self.passkeys.answer("passkeys")
    }

    fn finish_passkey_registration(
        &self,
        _made: FinishPasskeyRegistration,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        self.save_answers.answer("finish_passkey_registration")
    }

    fn rename_passkey<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.renamed
            .lock()
            .unwrap()
            .push((id.to_owned(), name.to_owned()));
        self.rename_answers.answer("rename_passkey")
    }

    fn invites(&self) -> BoxFuture<'_, Result<Vec<InviteSummary>, BackendError>> {
        self.invites.answer("invites")
    }

    fn create_invite(
        &self,
        _spec: InviteSpec,
    ) -> BoxFuture<'_, Result<MintedInvite, BackendError>> {
        self.create_answers.answer("create_invite")
    }

    fn revoke_invite<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.revoked.lock().unwrap().push(id.to_owned());
        self.revoke_answers.answer("revoke_invite")
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceInfo>, BackendError>> {
        self.devices.answer("devices")
    }

    fn set_device_trust<'a>(
        &'a self,
        device: Option<&'a str>,
        trusted: bool,
        auto_login: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.trusted.lock().unwrap().push(Trusted {
            device: device.map(str::to_owned),
            trusted,
            auto_login,
        });
        self.trust_answers.answer("set_device_trust")
    }
}

/// The key of the server tests call Home Media.
pub fn home() -> ServerKey {
    ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into())
}

/// Server `name` at `key`: pinned and verified, reached on this network,
/// with no addresses, relays or server id.
pub fn server(key: ServerKey, name: &str) -> Server {
    Server {
        key,
        name: name.into(),
        addresses: Vec::new(),
        pinned: true,
        fingerprint: String::new(),
        verified: true,
        on_network: true,
        verified_at: None,
        last_connected_at: None,
        server_id: None,
        advertised: Vec::new(),
        relays: Vec::new(),
        advertised_at: None,
        use_relays: None,
        relays_edited: false,
    }
}

/// `username`'s profile on Home Media, shown as `display_name`, open
/// without asking.
pub fn profile(username: &str, display_name: &str) -> Profile {
    Profile {
        server: home(),
        username: username.into(),
        display_name: display_name.into(),
        locked: false,
        sign_in: Default::default(),
        trust: None,
    }
}

/// Download `key` of `item` for andrey on server `home`: the original
/// with every track, waiting on the server, never watched here.
pub fn local_download(key: &str, item: Item) -> LocalDownload {
    LocalDownload {
        key: key.into(),
        server: "home".into(),
        user_id: "andrey".into(),
        username: "andrey".into(),
        item,
        show: None,
        season: None,
        markers: Vec::new(),
        artwork: Vec::new(),
        tracks: Default::default(),
        source: DownloadSource::Original,
        chosen: DownloadTracks::All,
        file: None,
        phase: DownloadPhase::Preparing {
            server: DownloadState::Waiting,
        },
        received: 0,
        size: None,
        expected_size: None,
        file_path: None,
        file_url: None,
        watch: LocalWatch::default(),
        note: None,
        created_at: UNIX_EPOCH,
    }
}
