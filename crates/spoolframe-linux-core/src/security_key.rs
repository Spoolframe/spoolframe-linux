//! The SDK's [`SecurityKey`] over libfido2 ([`crate::fido2`]), P7-08: a
//! FIDO2 key plugged into this computer as a portable identity's root.
//!
//! One [`Fido2SecurityKey`] serves one root action, which may ask the key
//! more than once (create with a backup asks two keys; adding a root asks
//! the new key and an old one). It asks for the PIN through a
//! [`PinPrompt`] the first time and keeps it for the rest of the action
//! only; it is wiped when the action's key is dropped. The SDK never sees
//! it, and the key's own `uv` option is never sent (the PIN gives UV).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use spoolframe_client_sdk::{KeyAssertion, RootKey, SdkError, SecurityKey, SecurityKeyError};
use spoolframe_protocol::portable::{HANDLE_LEN, RootSignature};
use zeroize::Zeroizing;

use crate::fido2::{self, KeyError, MakeRequest, SignRequest};

/// What the PIN dialog says: which key, and after a wrong PIN how many
/// tries the key has left (LinuxKeyPrompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAsk {
    /// The key's name as it gives it ("FT FIDO KB").
    pub key: String,
    /// The PIN before was wrong.
    pub wrong: bool,
    /// Wrong PINs the key takes before it locks, when it says.
    pub tries_left: Option<u32>,
}

/// How the app asks the person: for the key's PIN, to touch it, and to
/// plug one in. Called off the main loop; an implementation blocks until
/// it has an answer.
pub trait PinPrompt: Send + Sync {
    /// The key's PIN. `None`: they cancelled.
    fn pin(&self, ask: &PinAsk) -> Option<String>;
    /// The key named `key` is waiting for a touch.
    fn touch(&self, key: &str);
    /// No key the action can use is plugged in: ask for one. Called again
    /// while it waits; `false` once they cancelled.
    fn plug_in(&self) -> bool {
        false
    }
    /// The action moved on to its next key: what to say on the touch page
    /// from now on.
    fn step(&self, _step: &Step) {}
}

/// One key's part in an action of several (Add a key: the new key, then
/// one that approves it), for the prompt's touch page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// "Step 1 of 2".
    pub caption: String,
    /// "Plug in the new key and touch it".
    pub title: String,
    pub line: String,
}

/// `prompt` saying `step` whenever the key it serves waits for a touch.
pub fn stepped(prompt: Arc<dyn PinPrompt>, step: Step) -> Arc<dyn PinPrompt> {
    Arc::new(Stepped { prompt, step })
}

struct Stepped {
    prompt: Arc<dyn PinPrompt>,
    step: Step,
}

impl PinPrompt for Stepped {
    fn pin(&self, ask: &PinAsk) -> Option<String> {
        self.prompt.step(&self.step);
        self.prompt.pin(ask)
    }
    fn touch(&self, key: &str) {
        self.prompt.step(&self.step);
        self.prompt.touch(key);
    }
    fn plug_in(&self) -> bool {
        self.prompt.step(&self.step);
        self.prompt.plug_in()
    }
    fn step(&self, step: &Step) {
        self.prompt.step(step);
    }
}

/// Wrong PINs in a row before the action gives up and says so. The key
/// itself stops after three until it is plugged in again.
const PIN_TRIES: u32 = 3;

/// How long an action waits for a key to be plugged in.
const PLUG_IN_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The keys on this computer as the SDK's [`SecurityKey`], for one root
/// action. Creating with a backup registers each new root on a key the
/// action has not used yet, waiting for one to be plugged in; signing
/// tries each key plugged in until one holds the credential, so the keys
/// can be plugged in together or one after another. Each key's PIN is
/// asked for once in the action.
#[derive(Clone)]
pub struct Fido2SecurityKey {
    prompt: Arc<dyn PinPrompt>,
    /// Each key's PIN, by its device path, for this action. Shared with
    /// the blocking thread each call runs on.
    pins: Arc<Mutex<HashMap<String, Zeroizing<String>>>>,
    /// The keys this action registered a root on.
    taken: Arc<Mutex<Vec<String>>>,
    /// Credentials (by id) a key a new root goes on must not hold: the
    /// identity's roots, so an added key is another key.
    avoid: Vec<Vec<u8>>,
}

impl Fido2SecurityKey {
    pub fn new(prompt: Arc<dyn PinPrompt>) -> Self {
        Self {
            prompt,
            pins: Arc::default(),
            taken: Arc::default(),
            avoid: Vec::new(),
        }
    }

    /// A new root goes only on a key holding none of `ids`.
    pub fn avoiding(mut self, ids: Vec<Vec<u8>>) -> Self {
        self.avoid = ids;
        self
    }

    /// Whether `key` may take a new root: it holds none of `avoid`. A key
    /// that will not say is taken at its word.
    fn fresh(&self, key: &fido2::KeyDevice) -> bool {
        self.avoid.is_empty()
            || fido2::holds(&key.path, spoolframe_client_sdk::PORTABLE_RP, &self.avoid)
                .map_or(true, |held| held.is_empty())
    }

    /// `key`'s PIN kept for this action, or asked for.
    fn pin(&self, key: &fido2::KeyDevice, again: bool) -> Result<Zeroizing<String>, KeyError> {
        let mut pins = self.pins.lock().expect("never poisoned");
        if !again && let Some(pin) = pins.get(&key.path) {
            return Ok(pin.clone());
        }
        let ask = PinAsk {
            key: key.name.clone(),
            wrong: again,
            tries_left: again
                .then(|| fido2::pin_tries_left(&key.path).ok())
                .flatten(),
        };
        let pin = Zeroizing::new(
            self.prompt
                .pin(&ask)
                .ok_or_else(|| KeyError::Other("cancelled".into()))?,
        );
        pins.insert(key.path.clone(), pin.clone());
        Ok(pin)
    }

    /// Runs `call` with `key`'s PIN, asking again while it is wrong.
    fn with_pin<T>(
        &self,
        key: &fido2::KeyDevice,
        call: &impl Fn(&str, &str) -> Result<T, KeyError>,
    ) -> Result<T, KeyError> {
        let mut again = false;
        for _ in 0..PIN_TRIES {
            let pin = self.pin(key, again)?;
            self.prompt.touch(&key.name);
            match call(&key.path, &pin) {
                Err(KeyError::WrongPin) => again = true,
                other => return other,
            }
        }
        Err(KeyError::WrongPin)
    }

    /// The keys plugged in now that `pick` takes, waiting while there are
    /// none for as long as the person does.
    fn wait_for(
        &self,
        pick: impl Fn(&fido2::KeyDevice) -> bool,
    ) -> Result<Vec<fido2::KeyDevice>, KeyError> {
        let started = std::time::Instant::now();
        loop {
            let keys: Vec<_> = rooting_keys()?.into_iter().filter(|k| pick(k)).collect();
            if !keys.is_empty() {
                return Ok(keys);
            }
            if started.elapsed() > PLUG_IN_WAIT || !self.prompt.plug_in() {
                return Err(KeyError::NoKey);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    /// Runs `call` on a key this action has not registered on, and takes
    /// it.
    fn with_new_key<T>(
        &self,
        call: impl Fn(&str, &str) -> Result<T, KeyError>,
    ) -> Result<T, KeyError> {
        let taken = self.taken.lock().expect("never poisoned").clone();
        let key = self
            .wait_for(|k| !taken.contains(&k.path) && self.fresh(k))?
            .remove(0);
        let made = self.with_pin(&key, &call)?;
        self.taken.lock().expect("never poisoned").push(key.path);
        Ok(made)
    }

    /// Runs `call` on each key plugged in until one answers with what it
    /// holds; none of them: waits for another key. A key that says, with
    /// no PIN or touch, that it holds none of `allowed` is passed over, so
    /// its PIN is not asked for nothing (Add a key has the new key plugged
    /// in beside the one that approves it).
    fn with_holding_key<T>(
        &self,
        rp: &str,
        allowed: &[Vec<u8>],
        call: impl Fn(&str, &str) -> Result<T, KeyError>,
    ) -> Result<T, KeyError> {
        let mut tried: Vec<String> = Vec::new();
        loop {
            let keys = self.wait_for(|k| {
                !tried.contains(&k.path)
                    && (allowed.is_empty()
                        || fido2::holds(&k.path, rp, allowed).map_or(true, |h| !h.is_empty()))
            })?;
            for key in keys {
                match self.with_pin(&key, &call) {
                    Err(KeyError::NoCredential) => tried.push(key.path),
                    other => return other,
                }
            }
            if !self.prompt.plug_in() {
                return Err(KeyError::NoCredential);
            }
        }
    }
}

impl Fido2SecurityKey {
    /// Deletes the credentials `ids` from every key plugged in that holds
    /// them, under each key's PIN: what removing an identity from its keys
    /// takes. How many were deleted.
    pub fn erase(&self, ids: &[Vec<u8>]) -> Result<usize, KeyError> {
        let mut deleted = 0;
        for key in self.wait_for(|_| true)? {
            for id in ids {
                match self.with_pin(&key, &|path, pin| fido2::delete_credential(path, id, pin)) {
                    Ok(()) => deleted += 1,
                    Err(KeyError::NoCredential) => {}
                    Err(other) => return Err(other),
                }
            }
        }
        Ok(deleted)
    }
}

/// Which of `ids` the keys plugged in hold under the portable RP, with no
/// PIN and no touch.
pub fn plugged_in_holding(ids: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, KeyError> {
    let mut held = Vec::new();
    for key in rooting_keys()? {
        for id in fido2::holds(&key.path, spoolframe_client_sdk::PORTABLE_RP, ids)? {
            if !held.contains(&id) {
                held.push(id);
            }
        }
    }
    Ok(held)
}

/// The keys plugged in that can root an identity (FIDO2, resident
/// credentials, a PIN, credProtect).
fn rooting_keys() -> Result<Vec<fido2::KeyDevice>, KeyError> {
    Ok(fido2::keys()?
        .into_iter()
        .filter(|key| fido2::info(&key.path).is_ok_and(|info| info.can_root()))
        .collect())
}

/// The transport's error as the SDK's.
pub fn sdk_error(error: KeyError) -> SdkError {
    let error = match error {
        KeyError::WrongPin => SecurityKeyError::WrongPin,
        KeyError::Replug => SecurityKeyError::Replug,
        KeyError::Locked => SecurityKeyError::Locked,
        KeyError::NoPin => SecurityKeyError::NoPin,
        KeyError::NoCredential => SecurityKeyError::NoCredential,
        KeyError::Full => SecurityKeyError::Full,
        KeyError::NotTouched => SecurityKeyError::NotTouched,
        KeyError::NoKey => SecurityKeyError::Other("no security key is plugged in".into()),
        KeyError::Other(why) => SecurityKeyError::Other(why),
    };
    error.into()
}

/// A libfido2 assertion as the SDK's.
fn key_assertion(signed: fido2::Signed) -> KeyAssertion {
    KeyAssertion {
        credential_id: signed.credential_id,
        signature: RootSignature::Ctap {
            authenticator_data: signed.authenticator_data,
            signature: signed.signature,
        },
        user_handle: signed
            .user_handle
            .and_then(|handle| <[u8; HANDLE_LEN]>::try_from(handle).ok()),
    }
}

/// Runs a blocking key call off the async runtime.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, KeyError> + Send + 'static,
) -> Result<T, SdkError> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|e| sdk_error(KeyError::Other(e.to_string())))?
        .map_err(sdk_error)
}

#[async_trait]
impl SecurityKey for Fido2SecurityKey {
    async fn register(
        &self,
        rp: &str,
        handle: [u8; HANDLE_LEN],
        name: &str,
    ) -> Result<RootKey, SdkError> {
        let this = self.clone();
        let (rp, name) = (rp.to_owned(), name.to_owned());
        let made = blocking(move || {
            this.with_new_key(|path, pin| {
                // The attestation is not checked (a root proves itself by
                // signing the genesis), so the hash it signs is the handle.
                fido2::make_credential(
                    path,
                    &MakeRequest {
                        rp: &rp,
                        user_id: &handle,
                        user_name: &name,
                        client_data_hash: &handle,
                        pin,
                    },
                )
            })
        })
        .await?;
        let public_key = made.public_key.as_slice().try_into().map_err(|_| {
            sdk_error(KeyError::Other(format!(
                "the key's public key is {} bytes, not a P-256 point",
                made.public_key.len()
            )))
        })?;
        Ok(RootKey::SecurityKey {
            public_key,
            credential_id: made.credential_id,
        })
    }

    async fn assert(
        &self,
        rp: &str,
        client_data_hash: [u8; 32],
        allowed: &[Vec<u8>],
    ) -> Result<Vec<KeyAssertion>, SdkError> {
        let this = self.clone();
        let (rp, allowed) = (rp.to_owned(), allowed.to_vec());
        let signed = blocking(move || {
            this.with_holding_key(&rp, &allowed, |path, pin| {
                fido2::get_assertion(
                    path,
                    &SignRequest {
                        rp: &rp,
                        client_data_hash: &client_data_hash,
                        allowed: &allowed,
                        pin,
                    },
                )
            })
        })
        .await?;
        if signed.is_empty() {
            return Err(sdk_error(KeyError::NoCredential));
        }
        Ok(signed.into_iter().map(key_assertion).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use spoolframe_protocol::portable::PORTABLE_RP;

    use super::*;

    /// What the prompt was told, in order.
    #[derive(Default)]
    struct Heard(Mutex<Vec<String>>);
    impl PinPrompt for Heard {
        fn pin(&self, _ask: &PinAsk) -> Option<String> {
            None
        }
        fn touch(&self, key: &str) {
            self.0.lock().unwrap().push(format!("touch {key}"));
        }
        fn step(&self, step: &Step) {
            self.0.lock().unwrap().push(step.caption.clone());
        }
    }

    #[test]
    fn a_stepped_prompt_says_its_step_before_the_touch() {
        let heard = Arc::new(Heard::default());
        let step = Step {
            caption: "Step 2 of 2".into(),
            title: "Now touch Keyring key".into(),
            line: String::new(),
        };
        stepped(heard.clone(), step).touch("FT FIDO KB");
        assert_eq!(
            *heard.0.lock().unwrap(),
            ["Step 2 of 2", "touch FT FIDO KB"]
        );
    }

    #[test]
    fn a_wrong_pin_reaches_the_sdk_as_one() {
        assert!(matches!(
            sdk_error(KeyError::WrongPin),
            SdkError::SecurityKey(SecurityKeyError::WrongPin)
        ));
    }

    #[test]
    fn no_key_says_to_plug_one_in() {
        let SdkError::SecurityKey(SecurityKeyError::Other(why)) = sdk_error(KeyError::NoKey) else {
            panic!("not a key error");
        };
        assert!(why.starts_with("no security key"), "{why}");
    }

    #[test]
    fn a_handle_is_kept_only_at_its_length() {
        let signed = |handle: Vec<u8>| fido2::Signed {
            credential_id: vec![1],
            authenticator_data: vec![2],
            signature: vec![3],
            user_handle: Some(handle),
        };
        assert_eq!(
            (
                key_assertion(signed(vec![7; 32])).user_handle,
                key_assertion(signed(vec![7; 16])).user_handle
            ),
            (Some([7; 32]), None)
        );
    }

    /// The PIN from the environment, counting how often it is asked for.
    struct EnvPin(AtomicU32);

    impl PinPrompt for EnvPin {
        fn pin(&self, ask: &PinAsk) -> Option<String> {
            eprintln!("PIN for {} asked", ask.key);
            self.0.fetch_add(1, Ordering::SeqCst);
            std::env::var("SPOOLFRAME_TEST_KEY_PIN").ok()
        }
        fn touch(&self, key: &str) {
            eprintln!("touch {key}");
        }
    }

    /// The user's key as a root, end to end: a credential made, an
    /// assertion over SHA-256 of a message that the SDK's own verifier
    /// accepts, the handle given back with an empty allow list, the PIN
    /// asked for once, and the credential deleted again. Needs the key
    /// plugged in, its PIN in `SPOOLFRAME_TEST_KEY_PIN`, and three touches:
    ///
    /// ```sh
    /// SPOOLFRAME_TEST_KEY_PIN=… cargo test -p spoolframe-linux-core \
    ///   --lib -- --ignored a_real_key_roots
    /// ```
    #[test]
    #[ignore = "needs a FIDO2 key, its PIN and three touches"]
    fn a_real_key_roots_and_signs_what_the_sdk_verifies() {
        use sha2::Digest;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let prompt = Arc::new(EnvPin(AtomicU32::new(0)));
        let key = Fido2SecurityKey::new(prompt.clone());
        let handle = [0x5a; HANDLE_LEN];
        let root = runtime
            .block_on(key.register(PORTABLE_RP, handle, "spoolframe test"))
            .unwrap();
        let RootKey::SecurityKey { credential_id, .. } = &root else {
            panic!("not a key root");
        };
        let credential_id = credential_id.clone();
        let message = b"spoolframe identity genesis v1\0a test";
        let hash: [u8; 32] = sha2::Sha256::digest(message).into();
        let named = runtime
            .block_on(key.assert(PORTABLE_RP, hash, std::slice::from_ref(&credential_id)))
            .unwrap();
        let any = runtime
            .block_on(key.assert(PORTABLE_RP, hash, &[]))
            .unwrap();
        let held = plugged_in_holding(&[credential_id.clone(), vec![9; 32]]).unwrap();
        let pin = std::env::var("SPOOLFRAME_TEST_KEY_PIN").unwrap();
        let path = rooting_keys().unwrap().remove(0).path;
        fido2::delete_credential(&path, &credential_id, &pin).unwrap();
        let gone = plugged_in_holding(std::slice::from_ref(&credential_id)).unwrap();

        assert_eq!(
            held,
            vec![credential_id.clone()],
            "the probe finds it, without a PIN"
        );
        assert!(gone.is_empty(), "the probe no longer finds it once deleted");

        assert!(
            root.verify(message, &named[0].signature),
            "the SDK refuses it"
        );
        assert!(
            any.iter()
                .any(|a| a.credential_id == credential_id && a.user_handle == Some(handle)),
            "{any:?}"
        );
        assert_eq!(
            prompt.0.load(Ordering::SeqCst),
            1,
            "the PIN is asked for once"
        );
    }
}
