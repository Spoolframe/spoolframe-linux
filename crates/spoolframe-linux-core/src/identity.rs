//! A portable identity rooted in security keys (P7-08), for Preferences ›
//! Identity: what this computer knows of it, and the root actions, over
//! the SDK's [`Portable`]. The SDK keeps the identity; this keeps what it
//! does not: each key's name ("Keyring key"), its maker and when it was
//! added, for the tab's list.
//!
//! The same code runs for the app and its tests: tests give it an SDK
//! `Portable` over memory and the SDK's software key, so only the relay
//! (pairing by code, recovery from a server) is missing there.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use spoolframe_client_sdk::portable::IssuedCredential;
use spoolframe_client_sdk::{
    IdentityRecord, KeyAssertion, Portable, PortableIdentity, RelayAnswer, RelayOffer,
    RelayPairing, RootKey, RootSigner, SdkError, SecurityKey,
};
use spoolframe_protocol::portable::{DevicePublicKey, HANDLE_LEN, PortableUserId};

use crate::security_key::PinPrompt;

/// A key's name and where it came from, kept on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLabel {
    pub name: String,
    /// "FT FIDO KB", as the key says.
    pub maker: Option<String>,
    /// Unix seconds.
    pub added_at: i64,
}

/// One of the identity's security keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRoot {
    pub root: RootKey,
    /// `None` for a key added on another device.
    pub label: Option<KeyLabel>,
}

/// One device the identity's keys paired from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub credential: IssuedCredential,
    pub this_computer: bool,
}

/// What Preferences › Identity shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityView {
    pub identity: PortableIdentity,
    pub fingerprint: String,
    pub keys: Vec<KeyRoot>,
    pub devices: Vec<Device>,
    /// When this computer's sign-in ends, if it is in its last 14 days.
    pub renewal_due: Option<i64>,
}

/// A key to act with, for one root action: libfido2's on this computer,
/// or the tests' software key. A new root it makes goes only on a key
/// holding none of the credentials (by id) given.
pub type KeyMaker =
    Arc<dyn Fn(Arc<dyn PinPrompt>, Vec<Vec<u8>>) -> Arc<dyn SecurityKey> + Send + Sync>;

/// Deletes credentials (by id) from the keys plugged in, asking through
/// the prompt; how many it deleted. Blocks. Only real keys have one.
pub type KeyEraser =
    Arc<dyn Fn(Arc<dyn PinPrompt>, Vec<Vec<u8>>) -> Result<usize, SdkError> + Send + Sync>;

/// Which of these credentials (by id) the keys plugged in hold, with no
/// PIN and no touch. Blocks. Only real keys have one.
pub type KeyProbe = Arc<dyn Fn(Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>, SdkError> + Send + Sync>;

/// The identity side of this computer.
#[derive(Clone)]
pub struct Identities {
    portable: Portable,
    /// The SDK client, for pairing by code and recovery from a server;
    /// none in tests.
    client: Option<spoolframe_client_sdk::Client>,
    labels: Arc<Labels>,
    key: KeyMaker,
    erase: Option<KeyEraser>,
    probe: Option<KeyProbe>,
    /// The SDK's runtime, which its client and storage run on; none in
    /// tests, which run where they are.
    runtime: Option<tokio::runtime::Handle>,
}

impl Identities {
    pub fn new(
        portable: Portable,
        client: Option<spoolframe_client_sdk::Client>,
        labels: Option<PathBuf>,
        key: KeyMaker,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Self {
        Self {
            portable,
            client,
            labels: Arc::new(Labels::load(labels)),
            key,
            erase: None,
            probe: None,
            runtime,
        }
    }

    /// With a way to erase credentials from real keys.
    pub fn with_eraser(mut self, erase: KeyEraser) -> Self {
        self.erase = Some(erase);
        self
    }

    /// With a way to tell which credentials the keys plugged in hold.
    pub fn with_probe(mut self, probe: KeyProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Which of `keys` the keys plugged in now hold: `None` where that
    /// cannot be told (the tests' software key, or a key that will not
    /// say).
    pub async fn plugged_in(&self, keys: &[RootKey]) -> Option<Vec<bool>> {
        let probe = self.probe.clone()?;
        let ids: Vec<Vec<u8>> = keys
            .iter()
            .map(|root| match root {
                RootKey::SecurityKey { credential_id, .. } => credential_id.clone(),
                _ => Vec::new(),
            })
            .collect();
        let asked = ids.clone();
        let held = tokio::task::spawn_blocking(move || probe(asked))
            .await
            .ok()?
            .ok()?;
        Some(
            ids.iter()
                .map(|id| !id.is_empty() && held.contains(id))
                .collect(),
        )
    }

    /// Whether keys can be erased here (real keys only).
    pub fn can_erase(&self) -> bool {
        self.erase.is_some()
    }

    /// Takes identity `id` off this computer: its entry and anything secret
    /// of it here, and its keys' names. Its keys still hold it, so it can
    /// be recovered or imported again.
    pub async fn forget(&self, id: PortableUserId) -> Result<(), SdkError> {
        let roots = self
            .portable
            .roots(id)
            .await
            .map(|r| r.members)
            .unwrap_or_default();
        self.portable.forget(id).await?;
        for root in &roots {
            self.labels.forget(root);
        }
        Ok(())
    }

    /// Erases identity `id`'s credentials from the keys plugged in, under
    /// their PINs; then they can no longer act for it. How many it erased.
    pub async fn erase_from_keys(
        &self,
        id: PortableUserId,
        prompt: Arc<dyn PinPrompt>,
    ) -> Result<usize, SdkError> {
        let erase = self
            .erase
            .clone()
            .ok_or_else(|| SdkError::Unavailable("no keys to erase here".into()))?;
        let ids: Vec<Vec<u8>> = self
            .portable
            .roots(id)
            .await?
            .members
            .into_iter()
            .filter_map(|root| match root {
                RootKey::SecurityKey { credential_id, .. } => Some(credential_id),
                _ => None,
            })
            .collect();
        tokio::task::spawn_blocking(move || erase(prompt, ids))
            .await
            .map_err(|e| SdkError::Unavailable(e.to_string()))?
    }

    /// Runs `action` with these identities on the SDK's runtime, as every
    /// call of the SDK's client runs: `action` owns what it uses (its
    /// keys, as `Arc`s), so it can move there.
    pub async fn run<T: Send + 'static>(
        &self,
        action: impl FnOnce(Identities) -> crate::backend::BoxFuture<'static, Result<T, SdkError>>
        + Send
        + 'static,
    ) -> Result<T, SdkError> {
        let this = self.clone();
        match &self.runtime {
            Some(runtime) => runtime
                .spawn(action(this))
                .await
                .map_err(|e| SdkError::Unavailable(e.to_string()))?,
            None => action(this).await,
        }
    }

    /// Whether people with their own identity may sign in here by a code
    /// (LinuxPrefsPortablePolicy). Off unless turned on: then this
    /// computer shows no code and refuses before asking any server.
    pub async fn allow_guests(&self) -> Result<bool, SdkError> {
        self.portable.allow_guests().await
    }

    pub async fn set_allow_guests(&self, allowed: bool) -> Result<(), SdkError> {
        self.portable.set_allow_guests(allowed).await
    }

    /// A key for one root action, asking through `prompt`.
    pub fn key(&self, prompt: Arc<dyn PinPrompt>) -> Arc<dyn SecurityKey> {
        (self.key)(prompt, Vec::new())
    }

    /// A key to add as a root of `view`'s identity: one that holds none of
    /// its roots yet.
    pub fn new_key(&self, prompt: Arc<dyn PinPrompt>, view: &IdentityView) -> Arc<dyn SecurityKey> {
        let held = view
            .keys
            .iter()
            .filter_map(|k| match &k.root {
                RootKey::SecurityKey { credential_id, .. } => Some(credential_id.clone()),
                _ => None,
            })
            .collect();
        (self.key)(prompt, held)
    }

    /// The key-rooted identity on this computer, if it has one.
    pub async fn current(&self) -> Result<Option<IdentityView>, SdkError> {
        let Some(identity) = self
            .portable
            .list()
            .await?
            .into_iter()
            .find(|i| i.key_rooted)
        else {
            return Ok(None);
        };
        self.view(identity).await.map(Some)
    }

    async fn view(&self, identity: PortableIdentity) -> Result<IdentityView, SdkError> {
        let id = identity.id;
        let roots = self.portable.roots(id).await?;
        let keys = roots
            .members
            .into_iter()
            .map(|root| KeyRoot {
                label: self.labels.get(&root),
                root,
            })
            .collect();
        let own = self.portable.device_key(id).await.ok();
        let devices = self
            .portable
            .issued(id)
            .await?
            .into_iter()
            .filter(|c| c.revoked_at.is_none())
            .map(|credential| Device {
                this_computer: Some(credential.device_key) == own,
                credential,
            })
            .collect();
        Ok(IdentityView {
            fingerprint: id.fingerprint(),
            renewal_due: self.portable.renewal_due(id).await?,
            identity,
            keys,
            devices,
        })
    }

    /// A new identity called `name`, rooted in `keys` (the first, then
    /// any backup), each named as given; this computer is paired with it
    /// by the first key, one more touch.
    pub async fn create(
        &self,
        name: &str,
        keys: &[(Arc<dyn SecurityKey>, KeyLabel)],
    ) -> Result<PortableUserId, SdkError> {
        let refs: Vec<&dyn SecurityKey> = keys.iter().map(|(k, _)| k.as_ref()).collect();
        let id = self.portable.create_with_keys(&refs, name).await?;
        // The genesis lists the roots in the order the keys were given.
        let roots = self.portable.roots(id).await?;
        for (root, (_, label)) in roots.members.iter().zip(keys) {
            self.labels.set(root, label.clone());
        }
        self.portable
            .pair_self(id, RootSigner::Key(keys[0].0.as_ref()))
            .await?;
        Ok(id)
    }

    /// Pairs this computer, or renews it, with a touch of any root.
    pub async fn pair_self(
        &self,
        id: PortableUserId,
        key: &dyn SecurityKey,
    ) -> Result<(), SdkError> {
        self.portable
            .pair_self(id, RootSigner::Key(key))
            .await
            .map(|_| ())
    }

    /// Adds `new_key` as a root, named `label`; an existing root signs.
    pub async fn add_key(
        &self,
        id: PortableUserId,
        signer: &dyn SecurityKey,
        new_key: &dyn SecurityKey,
        label: KeyLabel,
    ) -> Result<(), SdkError> {
        let before = self.portable.roots(id).await?.members;
        self.portable
            .add_root(id, RootSigner::Key(signer), new_key, &label.name)
            .await?;
        let after = self.portable.roots(id).await?.members;
        if let Some(added) = after.iter().find(|r| !before.contains(r)) {
            self.labels.set(added, label);
        }
        Ok(())
    }

    /// Removes `root`; another root, or it, signs. The last one stays.
    pub async fn remove_key(
        &self,
        id: PortableUserId,
        signer: &dyn SecurityKey,
        root: &RootKey,
    ) -> Result<(), SdkError> {
        self.portable
            .remove_root(id, RootSigner::Key(signer), root)
            .await?;
        self.labels.forget(root);
        Ok(())
    }

    /// Stops `device` acting as the identity, on every server that hears.
    pub async fn revoke(
        &self,
        id: PortableUserId,
        signer: &dyn SecurityKey,
        device: DevicePublicKey,
    ) -> Result<(), SdkError> {
        self.portable
            .revoke_by(id, RootSigner::Key(signer), device)
            .await
            .map(|_| ())
    }

    /// The identity's record as a file's bytes: no secret in it.
    pub async fn export(&self, id: PortableUserId) -> Result<Vec<u8>, SdkError> {
        let record = self.portable.identity_record(id).await?;
        serde_json::to_vec_pretty(&record).map_err(|e| SdkError::InvalidInput(e.to_string()))
    }

    /// The record in a file's bytes, read without acting on it.
    pub fn read_record(bytes: &[u8]) -> Result<IdentityRecord, SdkError> {
        serde_json::from_slice(bytes)
            .map_err(|_| SdkError::InvalidInput("that file isn't a Spoolframe identity".into()))
    }

    /// Takes `record` (a file's, or a server's) as this computer's
    /// identity `name`, then pairs this computer with any of its keys.
    pub async fn import(
        &self,
        record: IdentityRecord,
        name: &str,
        key: &dyn SecurityKey,
    ) -> Result<PortableUserId, SdkError> {
        let id = self.portable.import_record(record, name).await?;
        self.pair_self(id, key).await?;
        Ok(id)
    }

    fn client(&self) -> Result<&spoolframe_client_sdk::Client, SdkError> {
        self.client
            .as_ref()
            .ok_or_else(|| SdkError::Unavailable("no server here to relay through".into()))
    }

    /// What server `server` keeps of identity `user`, for recovery.
    pub async fn fetch_record(
        &self,
        server: &str,
        user: PortableUserId,
    ) -> Result<IdentityRecord, SdkError> {
        self.client()?.fetch_identity_record(server, user).await
    }

    /// Recovers the identity `key` holds a root of from `server`, which
    /// knows it, as this computer's identity `name`; then pairs this
    /// computer with the same key. Two touches: one finds the identity on
    /// the key, one pairs.
    pub async fn recover(
        &self,
        server: &str,
        key: &dyn SecurityKey,
        name: &str,
    ) -> Result<PortableUserId, SdkError> {
        let id = self.client()?.recover_with_key(server, key, name).await?;
        self.pair_self(id, key).await?;
        Ok(id)
    }

    /// The device side of pairing by QR (P7-03), beside the code: a
    /// phone that holds the identity scans [`Pairing::link`]. Shared, so
    /// its waits can run on the SDK's runtime while the dialog keeps it;
    /// cancel it when the dialog goes.
    pub async fn start_pairing(
        &self,
        name: &str,
    ) -> Result<Arc<spoolframe_client_sdk::Pairing>, SdkError> {
        self.client()?.start_pairing(name).await.map(Arc::new)
    }

    /// The device side of pairing by code: a request held on `server`.
    pub async fn offer_pairing(&self, server: &str, name: &str) -> Result<RelayPairing, SdkError> {
        self.client()?.offer_relay_pairing(server, name).await
    }

    pub async fn collect_pairing(
        &self,
        pairing: &RelayPairing,
    ) -> Result<Option<RelayAnswer>, SdkError> {
        self.client()?.collect_relay_pairing(pairing).await
    }

    /// Keeps `answer`: this computer is now one of the identity's
    /// devices. The identity's id, to sign in with.
    pub async fn accept_pairing(&self, answer: RelayAnswer) -> Result<PortableUserId, SdkError> {
        self.client()?
            .accept_relay_pairing(answer)
            .await
            .map(|paired| paired.identity)
    }

    /// The key side: the request a device holds on `server` under `code`.
    pub async fn fetch_pairing(&self, server: &str, code: &str) -> Result<RelayOffer, SdkError> {
        self.client()?.fetch_relay_pairing(server, code).await
    }

    pub async fn answer_pairing(
        &self,
        offer: &RelayOffer,
        id: PortableUserId,
        key: &dyn SecurityKey,
    ) -> Result<(), SdkError> {
        self.client()?
            .answer_relay_pairing(offer, id, RootSigner::Key(key), Some(offer.name()))
            .await
    }
}

/// Each key's name, by its credential id, in a file of this computer's.
struct Labels {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, KeyLabel>>,
}

fn label_key(root: &RootKey) -> Option<String> {
    match root {
        RootKey::SecurityKey { credential_id, .. } => {
            Some(credential_id.iter().map(|b| format!("{b:02x}")).collect())
        }
        _ => None,
    }
}

impl Labels {
    fn load(path: Option<PathBuf>) -> Self {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn get(&self, root: &RootKey) -> Option<KeyLabel> {
        let key = label_key(root)?;
        self.map.lock().expect("never poisoned").get(&key).cloned()
    }

    fn set(&self, root: &RootKey, label: KeyLabel) {
        if let Some(key) = label_key(root) {
            self.map.lock().expect("never poisoned").insert(key, label);
            self.save();
        }
    }

    fn forget(&self, root: &RootKey) {
        if let Some(key) = label_key(root) {
            self.map.lock().expect("never poisoned").remove(&key);
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let map = self.map.lock().expect("never poisoned");
        if let Ok(bytes) = serde_json::to_vec_pretty(&*map)
            && let Err(e) = std::fs::write(path, bytes)
        {
            tracing::warn!(error = %e, path = %path.display(), "cannot keep the keys' names");
        }
    }
}

/// The SDK's software key, shared by every action, as one physical key
/// is: for tests, and for the fake.
pub struct SharedSoftKey(pub Arc<spoolframe_client_sdk::SoftSecurityKey>);

#[async_trait]
impl SecurityKey for SharedSoftKey {
    async fn register(
        &self,
        rp: &str,
        handle: [u8; HANDLE_LEN],
        name: &str,
    ) -> Result<RootKey, SdkError> {
        self.0.register(rp, handle, name).await
    }

    async fn assert(
        &self,
        rp: &str,
        client_data_hash: [u8; 32],
        allowed: &[Vec<u8>],
    ) -> Result<Vec<KeyAssertion>, SdkError> {
        self.0.assert(rp, client_data_hash, allowed).await
    }
}

/// An `Identities` over memory with one software key, for tests and the
/// fake: every action uses the same key.
pub fn in_memory() -> Identities {
    let soft = Arc::new(spoolframe_client_sdk::SoftSecurityKey::default());
    Identities::new(
        Portable::new(Arc::new(spoolframe_client_sdk::MemoryStorage::default())),
        None,
        None,
        Arc::new(move |_prompt, _held| {
            Arc::new(SharedSoftKey(soft.clone())) as Arc<dyn SecurityKey>
        }),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ready;

    struct NoPrompt;
    impl PinPrompt for NoPrompt {
        fn pin(&self, _ask: &crate::security_key::PinAsk) -> Option<String> {
            None
        }
        fn touch(&self, _key: &str) {}
    }

    fn label(name: &str) -> KeyLabel {
        KeyLabel {
            name: name.into(),
            maker: Some("FT FIDO KB".into()),
            added_at: 1_791_000_000,
        }
    }

    fn created(identities: &Identities) -> PortableUserId {
        let key = identities.key(Arc::new(NoPrompt));
        ready(identities.create("Alice", &[(key, label("Keyring key"))])).unwrap()
    }

    #[test]
    fn a_computer_with_no_identity_has_none_to_show() {
        assert_eq!(ready(in_memory().current()).unwrap(), None);
    }

    #[test]
    fn a_created_identity_is_rooted_in_its_named_key_and_pairs_this_computer() {
        let identities = in_memory();
        let id = created(&identities);
        let view = ready(identities.current()).unwrap().unwrap();
        assert_eq!(
            (
                view.identity.id,
                view.identity.key_rooted,
                view.keys
                    .iter()
                    .map(|k| k.label.clone().map(|l| l.name))
                    .collect::<Vec<_>>(),
                view.devices
                    .iter()
                    .map(|d| d.this_computer)
                    .collect::<Vec<_>>()
            ),
            (id, true, vec![Some("Keyring key".to_owned())], vec![true])
        );
    }

    #[test]
    fn the_last_key_cannot_be_removed() {
        let identities = in_memory();
        let id = created(&identities);
        let view = ready(identities.current()).unwrap().unwrap();
        let key = identities.key(Arc::new(NoPrompt));
        assert!(ready(identities.remove_key(id, key.as_ref(), &view.keys[0].root)).is_err());
    }

    #[test]
    fn an_added_key_is_listed_by_its_name() {
        let identities = in_memory();
        let id = created(&identities);
        let key = identities.key(Arc::new(NoPrompt));
        ready(identities.add_key(id, key.as_ref(), key.as_ref(), label("Drawer key"))).unwrap();
        let names: Vec<_> = ready(identities.current())
            .unwrap()
            .unwrap()
            .keys
            .into_iter()
            .filter_map(|k| k.label.map(|l| l.name))
            .collect();
        assert_eq!(names, ["Keyring key", "Drawer key"]);
    }

    #[test]
    fn a_key_to_add_avoids_the_keys_already_roots() {
        let held = Arc::new(Mutex::new(Vec::new()));
        let seen = held.clone();
        let soft = Arc::new(spoolframe_client_sdk::SoftSecurityKey::default());
        let identities = Identities::new(
            Portable::new(Arc::new(spoolframe_client_sdk::MemoryStorage::default())),
            None,
            None,
            Arc::new(move |_prompt, avoid: Vec<Vec<u8>>| {
                *seen.lock().unwrap() = avoid;
                Arc::new(SharedSoftKey(soft.clone())) as Arc<dyn SecurityKey>
            }),
            None,
        );
        created(&identities);
        let view = ready(identities.current()).unwrap().unwrap();
        identities.new_key(Arc::new(NoPrompt), &view);
        let RootKey::SecurityKey { credential_id, .. } = &view.keys[0].root else {
            panic!("not a key root");
        };
        assert_eq!(*held.lock().unwrap(), std::slice::from_ref(credential_id));
    }

    #[test]
    fn a_forgotten_identity_is_gone_from_this_computer() {
        let identities = in_memory();
        let id = created(&identities);
        ready(identities.forget(id)).unwrap();
        assert_eq!(ready(identities.current()).unwrap(), None);
    }

    #[test]
    fn a_forgotten_identity_comes_back_from_its_record() {
        let identities = in_memory();
        let id = created(&identities);
        let record = Identities::read_record(&ready(identities.export(id)).unwrap()).unwrap();
        ready(identities.forget(id)).unwrap();
        let key = identities.key(Arc::new(NoPrompt));
        assert_eq!(
            ready(identities.import(record, "Alice", key.as_ref())).unwrap(),
            id
        );
    }

    #[test]
    fn sign_ins_with_other_identities_are_off_until_turned_on() {
        let identities = in_memory();
        let before = ready(identities.allow_guests()).unwrap();
        ready(identities.set_allow_guests(true)).unwrap();
        assert_eq!(
            (before, ready(identities.allow_guests()).unwrap()),
            (false, true)
        );
    }

    #[test]
    fn the_tests_software_key_cannot_be_erased() {
        assert!(!in_memory().can_erase());
    }

    #[test]
    fn an_exported_record_reads_back_as_the_same_identity() {
        let identities = in_memory();
        let id = created(&identities);
        let bytes = ready(identities.export(id)).unwrap();
        assert_eq!(Identities::read_record(&bytes).unwrap().user, id);
    }

    #[test]
    fn a_file_that_is_not_a_record_says_so() {
        assert!(matches!(
            Identities::read_record(b"{\"hello\": 1}"),
            Err(SdkError::InvalidInput(_))
        ));
    }

    #[test]
    fn recovering_needs_a_server() {
        let identities = in_memory();
        let key = identities.key(Arc::new(NoPrompt));
        assert!(matches!(
            ready(identities.recover("k", key.as_ref(), "Alice")),
            Err(SdkError::Unavailable(_))
        ));
    }

    #[test]
    fn pairing_by_qr_needs_the_sdk_client() {
        assert!(matches!(
            ready(in_memory().start_pairing("Desk")),
            Err(SdkError::Unavailable(_))
        ));
    }

    #[test]
    fn pairing_by_code_needs_a_server() {
        assert!(matches!(
            ready(in_memory().offer_pairing("k", "Desk")),
            Err(SdkError::Unavailable(_))
        ));
    }
}
