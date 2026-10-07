//! Passkeys on a security key or a phone (P3-19), as the platform's
//! passkey API would do them on a system that has one.
//!
//! Linux has no passkey API, so the app talks to the authenticator itself
//! through webauthn-authenticator-rs: CTAP2 over USB to a key, or the
//! hybrid (caBLE) flow to a phone, which scans a QR code, proves it is
//! near over Bluetooth, and answers through a websocket tunnel. It does what the server expects
//! of an app: the relying party is the server's own (`<server id>.spoolframe.app`, PX-14) and
//! the client data names its origin, both as the server's request gives them. The bytes go to
//! and from the server as the key gives and takes them.
//!
//! A ceremony blocks while the key waits for a touch or a PIN, so it runs
//! on a thread of its own ([`sign_in`], [`register`]). It tells the UI what
//! to show through a [`Prompter`], and asks it for the key's PIN there.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use webauthn_authenticator_rs::AuthenticatorBackend;
use webauthn_authenticator_rs::cable::connect_cable_authenticator;
use webauthn_authenticator_rs::ctap2::select_one_device;
use webauthn_authenticator_rs::error::{CtapError, WebauthnCError};
use webauthn_authenticator_rs::transport::{Token, Transport};
use webauthn_authenticator_rs::types::{CableRequestType, CableState, EnrollSampleStatus};
use webauthn_authenticator_rs::ui::UiCallback;
use webauthn_authenticator_rs::usb::USBTransport;
use webauthn_rs_proto::{
    AllowCredentials, AttestationConveyancePreference, AuthenticatorAttachment,
    AuthenticatorSelectionCriteria, PubKeyCredParams, PublicKeyCredential,
    PublicKeyCredentialCreationOptions, PublicKeyCredentialDescriptor,
    PublicKeyCredentialRequestOptions, RegisterPublicKeyCredential, RelyingParty,
    ResidentKeyRequirement, User, UserVerificationPolicy,
};

use crate::backend::{
    FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyRequest,
};

/// How long a key has to be inserted, and then touched.
const WAIT: Duration = Duration::from_secs(60);

/// How long a phone has to scan the code and answer.
const PHONE_WAIT: Duration = Duration::from_secs(120);

/// Where the passkey is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Key,
    Phone,
}

/// Wrong PINs tried again on the same key before giving up on the
/// ceremony. The key counts its own tries and locks itself; this only
/// bounds one dialog.
const PIN_TRIES: u32 = 8;

/// What the dialog shows while a ceremony runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// "Insert or tap your security key".
    WaitingForKey,
    /// "Touch your key": its light blinks.
    Touch,
    /// The key is working.
    Processing,
    /// The phone flow's code to scan: a `FIDO:/…` URL for a QR code.
    Scan(String),
    /// The phone scanned it; Bluetooth and the tunnel are connecting.
    Connecting,
    /// The phone has the request: "Confirm on your phone".
    ConfirmOnPhone,
}

/// The dialog, as a ceremony sees it. Called from the ceremony's thread.
pub trait Prompter: Send + Sync {
    fn show(&self, prompt: Prompt);

    /// The key's own PIN, typed by the user; `None` if they gave up.
    /// `wrong` says the last one was refused. Blocks until answered.
    fn pin(&self, wrong: bool) -> Option<String>;
}

/// A key whose makeCredential always asks for a passkey it can find
/// again (`"rk": true`), and whose sign-ins keep the user handle.
///
/// webauthn-authenticator-rs 0.6.1-dev drops the creation options'
/// `resident_key`: its CTAP 2.0 `perform_register` sends `uv` and nothing
/// else, so keys made passkeys they could not find without being told
/// their id. Signing in names no passkeys (a username is not asked for),
/// and the key answered "no credentials". Its PIN and verification code
/// is private, so rather than redo it, every command passes through here
/// and makeCredential gains the option on its way to the key.
///
/// Its perform_auth also returns no user handle ("TODO"), which a sign-in
/// that names no passkeys needs: it is how the server knows whose passkey
/// signed. The key's getAssertion answer has it (key 4, the user's `id`),
/// so it is kept here on the way back, for [`UserHandle::fill`].
#[derive(Debug)]
struct Discoverable<T> {
    token: T,
    user: UserHandle,
}

fn discoverable<T: Token>(token: T, user: UserHandle) -> Discoverable<T> {
    Discoverable { token, user }
}

/// The user handle in the key's last getAssertion answer.
#[derive(Debug, Clone, Default)]
struct UserHandle(Arc<std::sync::Mutex<Option<Vec<u8>>>>);

impl UserHandle {
    /// A sign-in's answer with the key's user handle, where the library
    /// left it out.
    fn fill(&self, answer: Answer) -> Answer {
        match answer {
            Answer::Signed(mut credential) if credential.response.user_handle.is_none() => {
                credential.response.user_handle = self.0.lock().expect("never poisoned").take();
                Answer::Signed(credential)
            }
            other => other,
        }
    }

    /// Keeps the user handle from a getAssertion answer.
    fn keep(&self, answer: &[u8]) {
        use serde_cbor_2::Value;
        const USER: i128 = 4;
        let Ok(Value::Map(map)) = serde_cbor_2::from_slice::<Value>(answer) else {
            return;
        };
        if let Some(Value::Map(user)) = map.get(&Value::Integer(USER))
            && let Some(Value::Bytes(id)) = user.get(&Value::Text("id".to_owned()))
        {
            *self.0.lock().expect("never poisoned") = Some(id.clone());
        }
    }
}

#[async_trait::async_trait]
impl<T: Token> Token for Discoverable<T> {
    type Id = T::Id;

    fn has_button(&self) -> bool {
        self.token.has_button()
    }

    fn get_transport(&self) -> webauthn_rs_proto::AuthenticatorTransport {
        self.token.get_transport()
    }

    async fn transmit_raw<U>(&mut self, cbor: &[u8], ui: &U) -> Result<Vec<u8>, WebauthnCError>
    where
        U: UiCallback,
    {
        const GET_ASSERTION: u8 = 0x02;
        let request = with_resident_key(cbor)?;
        let answer = self.token.transmit_raw(&request, ui).await?;
        if cbor.first() == Some(&GET_ASSERTION) {
            self.user.keep(&answer);
        }
        Ok(answer)
    }

    async fn cancel(&mut self) -> Result<(), WebauthnCError> {
        self.token.cancel().await
    }

    async fn init(&mut self) -> Result<(), WebauthnCError> {
        self.token.init().await
    }

    async fn close(&mut self) -> Result<(), WebauthnCError> {
        self.token.close().await
    }
}

/// `cbor` with `"rk": true` among its options when it is makeCredential
/// (command 0x01, options under key 7); any other command as it is.
fn with_resident_key(cbor: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, WebauthnCError> {
    use serde_cbor_2::Value;
    const MAKE_CREDENTIAL: u8 = 0x01;
    const OPTIONS: i128 = 7;
    let Some((&MAKE_CREDENTIAL, body)) = cbor.split_first() else {
        return Ok(cbor.into());
    };
    let Value::Map(mut request) =
        serde_cbor_2::from_slice(body).map_err(|_| WebauthnCError::Cbor)?
    else {
        return Err(WebauthnCError::Cbor);
    };
    let options = request
        .entry(Value::Integer(OPTIONS))
        .or_insert_with(|| Value::Map(Default::default()));
    let Value::Map(options) = options else {
        return Err(WebauthnCError::Cbor);
    };
    options.insert(Value::Text("rk".to_owned()), Value::Bool(true));
    let mut out = vec![MAKE_CREDENTIAL];
    out.extend(serde_cbor_2::to_vec(&Value::Map(request)).map_err(|_| WebauthnCError::Cbor)?);
    Ok(out.into())
}

/// Why a ceremony ended without a passkey.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The key cannot keep a passkey it can find again: it has no room
    /// left (`full`), or keeps none at all.
    #[error("the key cannot keep the passkey")]
    CannotKeep { full: bool },
    /// The user went back or cancelled.
    #[error("cancelled")]
    Cancelled,
    /// No key was inserted, or touched, in time.
    #[error("no key was touched in time")]
    TimedOut,
    /// Too many wrong PINs: the key has to be reset with its maker's tool.
    #[error("the key is locked")]
    Locked,
    /// Three wrong PINs in a row: the key takes no more until it is
    /// unplugged and plugged back in.
    #[error("unplug the key and plug it back in")]
    Replug,
    /// The key holds no passkey for this server.
    #[error("no passkey for this server on the key")]
    NoPasskey,
    /// Making one: the key already holds a passkey for this user here.
    #[error("the key already has a passkey for this account")]
    AlreadyMade,
    /// No key can be opened: none plugged in, or no permission to open
    /// it (its udev rule).
    #[error("no security key can be opened")]
    NoDevice,
    /// The phone flow needs Bluetooth, which is off, missing or not
    /// allowed.
    #[error("Bluetooth is off")]
    NoBluetooth,
    #[error("{0}")]
    Other(String),
}

/// Signs in with a passkey on a key or a phone: what to send the server
/// for `request`. Blocks.
pub fn sign_in(
    request: &PasskeyRequest,
    via: Via,
    prompter: Arc<dyn Prompter>,
) -> Result<FinishPasskeySignIn, KeyError> {
    match run(via, prompter, sign_in_op(request)?)? {
        Answer::Signed(credential) => Ok(signed(credential)),
        Answer::Made(_) => Err(KeyError::Other("the authenticator made a passkey".into())),
    }
}

/// Makes a passkey on a key or a phone for `creation`, named `name` for
/// the server. Blocks.
pub fn register(
    creation: &PasskeyCreation,
    name: &str,
    via: Via,
    prompter: Arc<dyn Prompter>,
) -> Result<FinishPasskeyRegistration, KeyError> {
    match run(via, prompter, register_op(creation)?)? {
        Answer::Made(credential) => made(credential, name),
        Answer::Signed(_) => Err(KeyError::Other("the authenticator signed in".into())),
    }
}

/// What a ceremony asks of the authenticator, and the origin its client
/// data names.
enum Op {
    SignIn(PublicKeyCredentialRequestOptions, url::Url),
    Register(PublicKeyCredentialCreationOptions, url::Url),
}

/// A sign-in for the server's request: its relying party and its origin.
fn sign_in_op(request: &PasskeyRequest) -> Result<Op, KeyError> {
    Ok(Op::SignIn(
        request_options(request),
        origin(&request.origin)?,
    ))
}

/// A passkey to make for the server's request, as [`sign_in_op`].
fn register_op(creation: &PasskeyCreation) -> Result<Op, KeyError> {
    Ok(Op::Register(
        creation_options(creation),
        origin(&creation.origin)?,
    ))
}

enum Answer {
    Signed(PublicKeyCredential),
    Made(RegisterPublicKeyCredential),
}

/// Runs `op` on any authenticator: a key's or a phone's.
fn perform(
    authenticator: &mut impl AuthenticatorBackend,
    op: &Op,
    wait: Duration,
) -> Result<Answer, WebauthnCError> {
    let wait = wait.as_millis() as u32;
    match op {
        Op::SignIn(options, origin) => authenticator
            .perform_auth(origin.clone(), options.clone(), wait)
            .map(Answer::Signed),
        Op::Register(options, origin) => authenticator
            .perform_register(origin.clone(), options.clone(), wait)
            .map(Answer::Made),
    }
}

/// A tokio runtime for the library: the USB layer sleeps on its timer and
/// the tunnel is a tokio websocket, so worker threads drive them while
/// this thread blocks.
fn runtime() -> Result<tokio::runtime::Runtime, KeyError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| KeyError::Other(e.to_string()))
}

fn run(via: Via, prompter: Arc<dyn Prompter>, op: Op) -> Result<Answer, KeyError> {
    match via {
        Via::Key => with_key(prompter, &op),
        Via::Phone => with_phone(prompter, &op),
    }
}

/// The server's sign-in request as WebAuthn options. With no passkeys
/// listed it is a discoverable sign-in: the key offers any it holds for
/// the server's relying party, so only this server's.
pub fn request_options(request: &PasskeyRequest) -> PublicKeyCredentialRequestOptions {
    PublicKeyCredentialRequestOptions {
        challenge: request.challenge.clone(),
        timeout: Some(WAIT.as_millis() as u32),
        rp_id: request.rp_id.clone(),
        allow_credentials: request
            .allow_credential_ids
            .iter()
            .map(|id| AllowCredentials {
                type_: "public-key".to_owned(),
                id: id.clone(),
                transports: None,
            })
            .collect(),
        user_verification: UserVerificationPolicy::Required,
        hints: None,
        extensions: None,
    }
}

/// The server's registration as WebAuthn options: a discoverable passkey
/// (so a sign-in needs no username) with user verification, on a key, not
/// one this user has here already.
pub fn creation_options(creation: &PasskeyCreation) -> PublicKeyCredentialCreationOptions {
    let exclude: Vec<PublicKeyCredentialDescriptor> = creation
        .exclude_credential_ids
        .iter()
        .map(|id| PublicKeyCredentialDescriptor {
            type_: "public-key".to_owned(),
            id: id.clone(),
            transports: None,
        })
        .collect();
    PublicKeyCredentialCreationOptions {
        rp: RelyingParty {
            name: "Spoolframe".to_owned(),
            id: creation.rp_id.clone(),
        },
        user: User {
            id: creation.user_id.clone(),
            name: creation.user_name.clone(),
            display_name: creation.display_name.clone(),
        },
        challenge: creation.challenge.clone(),
        // ES256, EdDSA, RS256: what keys make, best first.
        pub_key_cred_params: [-7, -8, -257]
            .into_iter()
            .map(|alg| PubKeyCredParams {
                type_: "public-key".to_owned(),
                alg,
            })
            .collect(),
        timeout: Some(WAIT.as_millis() as u32),
        exclude_credentials: (!exclude.is_empty()).then_some(exclude),
        authenticator_selection: Some(AuthenticatorSelectionCriteria {
            authenticator_attachment: Some(AuthenticatorAttachment::CrossPlatform),
            resident_key: Some(ResidentKeyRequirement::Required),
            require_resident_key: true,
            user_verification: UserVerificationPolicy::Required,
        }),
        hints: None,
        attestation: Some(AttestationConveyancePreference::None),
        attestation_formats: None,
        extensions: None,
    }
}

/// What a key signed, as the server takes it. The user handle is None
/// when the authenticator gave none (a phone's sign-in, or a passkey asked
/// for by id): the server finds whose it is by the credential id.
pub fn signed(credential: PublicKeyCredential) -> FinishPasskeySignIn {
    let response = credential.response;
    FinishPasskeySignIn {
        credential_id: credential.raw_id,
        authenticator_data: response.authenticator_data,
        client_data_json: response.client_data_json,
        signature: response.signature,
        // None, never empty: the server refuses an empty handle, and finds
        // the owner by the credential id without one (a phone's sign-in).
        user_handle: response.user_handle.filter(|h| !h.is_empty()),
    }
}

/// The id the server names an app passkey by: its credential id,
/// base64url with no padding.
pub fn passkey_id(credential_id: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(credential_id.len().div_ceil(3) * 4);
    for chunk in credential_id.chunks(3) {
        let bits = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((bits >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// What a key made, as the server keeps it.
///
/// The library leaves the id empty for a CTAP2 key ("parsing out the real
/// ID is complicated"), so it is read from the attestation itself: renaming
/// the passkey from here names it by that id.
pub fn made(
    credential: RegisterPublicKeyCredential,
    name: &str,
) -> Result<FinishPasskeyRegistration, KeyError> {
    let attestation_object = credential.response.attestation_object;
    let credential_id = match credential.raw_id {
        id if !id.is_empty() => id,
        _ => attested_credential_id(&attestation_object)
            .ok_or_else(|| KeyError::Other("the key's answer names no passkey".to_owned()))?,
    };
    Ok(FinishPasskeyRegistration {
        credential_id,
        attestation_object,
        client_data_json: credential.response.client_data_json,
        name: name.to_owned(),
    })
}

/// The credential id in an attestation object's authData (WebAuthn §6.1):
/// after the relying party's hash (32 bytes), the flags (1), the counter
/// (4) and the AAGUID (16), a 2-byte length and the id. None without
/// attested credential data.
fn attested_credential_id(attestation_object: &[u8]) -> Option<Vec<u8>> {
    use serde_cbor_2::Value;
    const ATTESTED: u8 = 0x40;
    let Value::Map(map) = serde_cbor_2::from_slice(attestation_object).ok()? else {
        return None;
    };
    let Some(Value::Bytes(data)) = map.get(&Value::Text("authData".to_owned())) else {
        return None;
    };
    if data.get(32)? & ATTESTED == 0 {
        return None;
    }
    let length = u16::from_be_bytes([*data.get(53)?, *data.get(54)?]) as usize;
    data.get(55..55 + length).map(<[u8]>::to_vec)
}

/// A key's refusal in the dialog's terms.
pub fn key_error(error: &WebauthnCError) -> KeyError {
    match error {
        WebauthnCError::Cancelled => KeyError::Cancelled,
        WebauthnCError::NoHidDevices | WebauthnCError::NoSelectedToken => KeyError::NoDevice,
        WebauthnCError::NoBluetoothAdapter
        | WebauthnCError::PermissionDenied
        | WebauthnCError::BluetoothError(_) => KeyError::NoBluetooth,
        WebauthnCError::Ctap(ctap) => match ctap {
            CtapError::Ctap2PinBlocked => KeyError::Locked,
            CtapError::Ctap2PinAuthBlocked => KeyError::Replug,
            CtapError::Ctap2NoCredentials => KeyError::NoPasskey,
            CtapError::Ctap2CredentialExcluded => KeyError::AlreadyMade,
            CtapError::Ctap2KeyStoreFull => KeyError::CannotKeep { full: true },
            CtapError::Ctap2UnsupportedOption => KeyError::CannotKeep { full: false },
            CtapError::Ctap2UserActionTimeout => KeyError::TimedOut,
            CtapError::Ctap2KeepAliveCancel
            | CtapError::Ctap2OperationDenied
            | CtapError::Ctap2NotAllowed => KeyError::Cancelled,
            other => KeyError::Other(format!("the key refused: {other:?}")),
        },
        other => KeyError::Other(other.to_string()),
    }
}

/// The origin the server gave, `https://<server id>.spoolframe.app`.
fn origin(origin: &str) -> Result<url::Url, KeyError> {
    url::Url::parse(origin)
        .map_err(|_| KeyError::Other(format!("the server gave no origin to sign for: {origin:?}")))
}

/// Waits for a key, then runs `op` on it, asking for the PIN again while
/// it is refused.
fn with_key(prompter: Arc<dyn Prompter>, op: &Op) -> Result<Answer, KeyError> {
    let runtime = runtime()?;
    let _entered = runtime.enter();
    let ui = Ui::new(prompter.clone());
    let user = UserHandle::default();
    prompter.show(Prompt::WaitingForKey);
    let transport = runtime
        .block_on(USBTransport::new())
        .map_err(|e| key_error(&e))?;
    let mut key = runtime
        .block_on(async {
            use futures::StreamExt;
            use webauthn_authenticator_rs::transport::TokenEvent;
            let stream = transport
                .watch()
                .await?
                .map(|event| match event {
                    TokenEvent::Added(token) => {
                        TokenEvent::Added(discoverable(token, user.clone()))
                    }
                    TokenEvent::Removed(id) => TokenEvent::Removed(id),
                    TokenEvent::EnumerationComplete => TokenEvent::EnumerationComplete,
                })
                .boxed();
            tokio::time::timeout(WAIT, select_one_device(stream, &ui))
                .await
                .map_err(|_| WebauthnCError::Ctap(CtapError::Ctap2UserActionTimeout))
        })
        .map_err(|e| key_error(&e))?
        .ok_or(KeyError::NoDevice)?;
    prompter.show(Prompt::Touch);
    for _ in 0..PIN_TRIES {
        match perform(&mut key, op, WAIT) {
            Err(WebauthnCError::Ctap(CtapError::Ctap2PinInvalid)) => ui.refused(),
            result => return result.map(|a| user.fill(a)).map_err(|e| key_error(&e)),
        }
    }
    Err(KeyError::Other("too many wrong PINs".to_owned()))
}

/// Shows the code for a phone to scan, waits for it near by Bluetooth and
/// through the tunnel, then runs `op` on it. The phone takes one request
/// and closes.
fn with_phone(prompter: Arc<dyn Prompter>, op: &Op) -> Result<Answer, KeyError> {
    let runtime = runtime()?;
    let _entered = runtime.enter();
    let ui = Ui::new(prompter);
    let request_type = match op {
        Op::SignIn(..) => CableRequestType::GetAssertion,
        Op::Register(..) => CableRequestType::DiscoverableMakeCredential,
    };
    let mut phone = runtime
        .block_on(async {
            tokio::time::timeout(PHONE_WAIT, connect_cable_authenticator(request_type, &ui))
                .await
                .map_err(|_| WebauthnCError::Ctap(CtapError::Ctap2UserActionTimeout))?
        })
        .map_err(|e| key_error(&e))?;
    perform(&mut phone, op, PHONE_WAIT).map_err(|e| key_error(&e))
}

/// The library's UI callbacks, onto a [`Prompter`]. Fingerprint enrolling
/// has nothing to show here.
struct Ui {
    prompter: Arc<dyn Prompter>,
    /// The last PIN was refused: the next ask says so.
    wrong: std::sync::atomic::AtomicBool,
}

impl Ui {
    fn new(prompter: Arc<dyn Prompter>) -> Self {
        Self {
            prompter,
            wrong: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn refused(&self) {
        self.wrong.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl fmt::Debug for Ui {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ui").finish_non_exhaustive()
    }
}

impl UiCallback for Ui {
    fn request_pin(&self) -> Option<String> {
        let wrong = self.wrong.swap(false, std::sync::atomic::Ordering::SeqCst);
        self.prompter.pin(wrong)
    }

    fn request_touch(&self) {
        self.prompter.show(Prompt::Touch);
    }

    fn processing(&self) {
        self.prompter.show(Prompt::Processing);
    }

    fn fingerprint_enrollment_feedback(
        &self,
        _remaining_samples: u32,
        _feedback: Option<EnrollSampleStatus>,
    ) {
    }

    fn cable_qr_code(&self, _request_type: CableRequestType, url: String) {
        self.prompter.show(Prompt::Scan(url));
    }

    fn dismiss_qr_code(&self) {
        self.prompter.show(Prompt::Connecting);
    }

    fn cable_status_update(&self, state: CableState) {
        self.prompter.show(match state {
            CableState::WaitingForAuthenticatorResponse => Prompt::ConfirmOnPhone,
            CableState::Processing => Prompt::Processing,
            _ => Prompt::Connecting,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_passkey_is_named_by_its_credential_id_in_base64url() {
        assert_eq!(
            (
                passkey_id(&[1, 2, 3]),
                passkey_id(&[0xfb, 0xff]),
                passkey_id(&[0])
            ),
            ("AQID".to_owned(), "-_8".to_owned(), "AA".to_owned())
        );
    }

    /// authData as a key makes it: the relying party's hash, the flags
    /// (user present, verified, attested credential data), the counter,
    /// the AAGUID, then the credential id and a key.
    pub(super) fn auth_data(credential_id: &[u8]) -> Vec<u8> {
        let mut data = vec![0xAB; 32];
        data.push(0x45);
        data.extend([0, 0, 0, 1]);
        data.extend([0x11; 16]);
        data.extend((credential_id.len() as u16).to_be_bytes());
        data.extend(credential_id);
        data.extend([0xA0]);
        data
    }

    /// An attestation object ("none") around `auth_data`.
    pub(super) fn attestation_object(auth_data: Vec<u8>) -> Vec<u8> {
        use serde_cbor_2::Value;
        let map = std::collections::BTreeMap::from([
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (
                Value::Text("attStmt".into()),
                Value::Map(Default::default()),
            ),
            (Value::Text("authData".into()), Value::Bytes(auth_data)),
        ]);
        serde_cbor_2::to_vec(&Value::Map(map)).unwrap()
    }

    /// What the library hands back for a CTAP2 key: no id of its own.
    fn made_by_a_key(credential_id: &[u8]) -> RegisterPublicKeyCredential {
        RegisterPublicKeyCredential {
            id: String::new(),
            raw_id: Vec::new(),
            type_: "packed".into(),
            extensions: Default::default(),
            response: webauthn_rs_proto::AuthenticatorAttestationResponseRaw {
                attestation_object: attestation_object(auth_data(credential_id)),
                client_data_json: Vec::new(),
                transports: None,
            },
        }
    }

    /// A CTAP2 key on the wire, as `Token` sees one: it keeps a passkey
    /// only when makeCredential asks for one it can find again (`rk`), and
    /// a sign-in with no passkeys listed finds only those. Like the user's
    /// key, it has its own user verification, so no PIN is asked.
    #[derive(Debug, Default)]
    struct FakeKey {
        /// makeCredential's options, as each arrived.
        asked: Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, bool>>>>,
        /// Kept passkeys: (credential id, user handle).
        kept: Vec<(Vec<u8>, Vec<u8>)>,
        /// It has no room for another passkey.
        full: bool,
    }

    fn text(s: &str) -> serde_cbor_2::Value {
        serde_cbor_2::Value::Text(s.to_owned())
    }

    fn int(n: i128) -> serde_cbor_2::Value {
        serde_cbor_2::Value::Integer(n)
    }

    fn reply(
        entries: Vec<(serde_cbor_2::Value, serde_cbor_2::Value)>,
    ) -> Result<Vec<u8>, WebauthnCError> {
        let map = serde_cbor_2::Value::Map(entries.into_iter().collect());
        Ok(serde_cbor_2::to_vec(&map).unwrap())
    }

    #[async_trait::async_trait]
    impl webauthn_authenticator_rs::transport::Token for FakeKey {
        type Id = ();

        fn get_transport(&self) -> webauthn_rs_proto::AuthenticatorTransport {
            webauthn_rs_proto::AuthenticatorTransport::Usb
        }

        async fn transmit_raw<U>(&mut self, cbor: &[u8], _ui: &U) -> Result<Vec<u8>, WebauthnCError>
        where
            U: UiCallback,
        {
            use serde_cbor_2::Value;
            let request = match cbor.get(1..) {
                Some(rest) if !rest.is_empty() => match serde_cbor_2::from_slice(rest).unwrap() {
                    Value::Map(map) => map,
                    _ => panic!("a command that is no map"),
                },
                _ => Default::default(),
            };
            match cbor[0] {
                // authenticatorGetInfo
                0x04 => reply(vec![
                    (int(1), Value::Array(vec![text("FIDO_2_0")])),
                    (int(3), Value::Bytes(vec![0x11; 16])),
                    (
                        int(4),
                        Value::Map(
                            [("rk", true), ("up", true), ("uv", true)]
                                .into_iter()
                                .map(|(k, v)| (text(k), Value::Bool(v)))
                                .collect(),
                        ),
                    ),
                ]),
                // authenticatorMakeCredential
                0x01 => {
                    let options: std::collections::BTreeMap<String, bool> =
                        match request.get(&int(7)) {
                            Some(Value::Map(o)) => o
                                .iter()
                                .filter_map(|(k, v)| match (k, v) {
                                    (Value::Text(k), Value::Bool(v)) => Some((k.clone(), *v)),
                                    _ => None,
                                })
                                .collect(),
                            _ => Default::default(),
                        };
                    self.asked.lock().unwrap().push(options.clone());
                    let resident = options.get("rk") == Some(&true);
                    if resident && self.full {
                        return Err(WebauthnCError::Ctap(CtapError::Ctap2KeyStoreFull));
                    }
                    let id = vec![0x5A; if resident { 16 } else { 96 }];
                    if resident {
                        let Some(Value::Map(user)) = request.get(&int(3)) else {
                            panic!("no user")
                        };
                        let Some(Value::Bytes(handle)) = user.get(&text("id")) else {
                            panic!("no user id")
                        };
                        self.kept.push((id.clone(), handle.clone()));
                    }
                    reply(vec![
                        (int(1), text("none")),
                        (int(2), Value::Bytes(auth_data(&id))),
                        (int(3), Value::Map(Default::default())),
                    ])
                }
                // authenticatorGetAssertion
                0x02 => {
                    let listed =
                        matches!(request.get(&int(3)), Some(Value::Array(a)) if !a.is_empty());
                    let Some((id, handle)) = self.kept.first().filter(|_| !listed).cloned() else {
                        return Err(WebauthnCError::Ctap(CtapError::Ctap2NoCredentials));
                    };
                    let mut data = vec![0xAB; 32];
                    data.push(0x05);
                    data.extend([0, 0, 0, 2]);
                    reply(vec![
                        (
                            int(1),
                            Value::Map(
                                [
                                    (text("type"), text("public-key")),
                                    (text("id"), Value::Bytes(id)),
                                ]
                                .into_iter()
                                .collect(),
                            ),
                        ),
                        (int(2), Value::Bytes(data)),
                        (int(3), Value::Bytes(vec![0x30; 70])),
                        (
                            int(4),
                            Value::Map([(text("id"), Value::Bytes(handle))].into_iter().collect()),
                        ),
                    ])
                }
                other => panic!("the fake key has no command {other:#x}"),
            }
        }

        async fn cancel(&mut self) -> Result<(), WebauthnCError> {
            Ok(())
        }

        async fn init(&mut self) -> Result<(), WebauthnCError> {
            Ok(())
        }

        async fn close(&mut self) -> Result<(), WebauthnCError> {
            Ok(())
        }
    }

    struct Silent;

    impl Prompter for Silent {
        fn show(&self, _: Prompt) {}

        fn pin(&self, _: bool) -> Option<String> {
            None
        }
    }

    /// The relying party and origin of a server whose id is
    /// `homemediaserver2`.
    const RP: &str = "homemediaserver2.spoolframe.app";
    const ORIGIN: &str = "https://homemediaserver2.spoolframe.app";

    fn creation() -> PasskeyCreation {
        PasskeyCreation {
            rp_id: RP.into(),
            origin: ORIGIN.into(),
            challenge: vec![9; 32],
            user_id: b"u-andoriyu".to_vec(),
            user_name: "andoriyu".into(),
            display_name: "Andrey".into(),
            exclude_credential_ids: Vec::new(),
        }
    }

    /// Runs `ops` on `key` as a key plugged in here is run: wrapped, and
    /// with the user handle filled in.
    fn on_key(key: FakeKey, ops: &[Op]) -> Vec<Result<Answer, WebauthnCError>> {
        let runtime = runtime().unwrap();
        let _entered = runtime.enter();
        let ui = Ui::new(Arc::new(Silent));
        let user = UserHandle::default();
        let mut key = runtime
            .block_on(webauthn_authenticator_rs::ctap2::CtapAuthenticator::new(
                discoverable(key, user.clone()),
                &ui,
            ))
            .expect("the fake key answers GetInfo");
        ops.iter()
            .map(|op| perform(&mut key, op, WAIT).map(|a| user.fill(a)))
            .collect()
    }

    #[test]
    fn a_passkey_made_on_a_key_is_asked_to_be_findable() {
        let key = FakeKey::default();
        let asked = key.asked.clone();
        let _ = on_key(key, &[register_op(&creation()).unwrap()]);
        let asked = asked.lock().unwrap().clone();
        assert_eq!(
            asked.first().and_then(|o| o.get("rk")),
            Some(&true),
            "makeCredential's options: {asked:?}"
        );
    }

    #[test]
    fn a_passkey_made_here_signs_in_with_no_passkeys_listed() {
        let answers = on_key(
            FakeKey::default(),
            &[
                register_op(&creation()).unwrap(),
                sign_in_op(&request(Vec::new())).unwrap(),
            ],
        );
        let mut answers = answers.into_iter();
        let Some(Ok(Answer::Made(made_))) = answers.next() else {
            panic!("the key made no passkey")
        };
        let made_ = made(made_, "thumb").unwrap();
        let Some(Ok(Answer::Signed(credential))) = answers.next() else {
            panic!("the passkey made here does not sign in unlisted")
        };
        assert_eq!(signed(credential).credential_id, made_.credential_id);
    }

    #[test]
    fn a_sign_in_with_no_passkeys_listed_says_whose_passkey_it_is() {
        let mut answers = on_key(
            FakeKey::default(),
            &[
                register_op(&creation()).unwrap(),
                sign_in_op(&request(Vec::new())).unwrap(),
            ],
        )
        .into_iter();
        let _made = answers.next();
        let Some(Ok(Answer::Signed(credential))) = answers.next() else {
            panic!("no sign-in")
        };
        assert_eq!(signed(credential).user_handle, Some(creation().user_id));
    }

    #[test]
    fn an_empty_user_handle_goes_as_none() {
        let credential = PublicKeyCredential {
            id: String::new(),
            raw_id: vec![1],
            response: webauthn_rs_proto::AuthenticatorAssertionResponseRaw {
                authenticator_data: vec![0; 37],
                client_data_json: Vec::new(),
                signature: vec![0x30],
                user_handle: Some(Vec::new()),
            },
            extensions: Default::default(),
            type_: "public-key".into(),
        };
        assert_eq!(signed(credential).user_handle, None);
    }

    #[test]
    fn the_client_data_names_the_servers_origin() {
        let mut answers = on_key(
            FakeKey::default(),
            &[
                register_op(&creation()).unwrap(),
                sign_in_op(&request(Vec::new())).unwrap(),
            ],
        )
        .into_iter();
        let Some(Ok(Answer::Made(credential))) = answers.next() else {
            panic!("no passkey made")
        };
        let made = String::from_utf8(made(credential, "thumb").unwrap().client_data_json).unwrap();
        let Some(Ok(Answer::Signed(credential))) = answers.next() else {
            panic!("no sign-in")
        };
        let signed = String::from_utf8(signed(credential).client_data_json).unwrap();
        // As the server reads it: a URL, so a trailing slash is the same.
        let origin_of = |json: &str| {
            let at = json.find("\"origin\":\"").unwrap() + 10;
            url::Url::parse(&json[at..at + json[at..].find('"').unwrap()]).unwrap()
        };
        let server = url::Url::parse(ORIGIN).unwrap();
        assert_eq!(
            (origin_of(&made), origin_of(&signed)),
            (server.clone(), server)
        );
    }

    #[test]
    fn a_key_with_no_room_says_so() {
        let full = FakeKey {
            full: true,
            ..FakeKey::default()
        };
        let answers = on_key(full, &[register_op(&creation()).unwrap()]);
        let Some(Err(error)) = answers.into_iter().next() else {
            panic!("a full key made a passkey")
        };
        assert_eq!(key_error(&error), KeyError::CannotKeep { full: true });
    }

    #[test]
    fn a_keys_passkey_is_named_by_the_id_in_its_attestation() {
        let id = vec![0x5A; 96];
        let made = made(made_by_a_key(&id), "thumb").unwrap();
        assert_eq!(made.credential_id, id);
    }

    #[test]
    fn an_answer_with_no_credential_in_it_is_refused() {
        let mut credential = made_by_a_key(&[1, 2, 3]);
        credential.response.attestation_object = attestation_object(vec![0; 37]);
        assert!(made(credential, "thumb").is_err());
    }

    fn request(allow: Vec<Vec<u8>>) -> PasskeyRequest {
        PasskeyRequest {
            rp_id: RP.into(),
            origin: ORIGIN.into(),
            challenge: vec![7; 32],
            allow_credential_ids: allow,
        }
    }

    #[test]
    fn a_sign_in_asks_for_the_servers_relying_party_with_verification() {
        let options = request_options(&request(Vec::new()));
        assert_eq!(
            (
                options.rp_id.as_str(),
                options.user_verification,
                options.allow_credentials.len()
            ),
            (RP, UserVerificationPolicy::Required, 0)
        );
    }

    #[test]
    fn a_users_passkeys_are_the_only_ones_allowed() {
        let options = request_options(&request(vec![vec![1, 2], vec![3]]));
        let ids: Vec<Vec<u8>> = options
            .allow_credentials
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec![vec![1, 2], vec![3]]);
    }

    #[test]
    fn a_passkey_is_made_discoverable_for_the_servers_relying_party() {
        let options = creation_options(&PasskeyCreation {
            exclude_credential_ids: vec![vec![4]],
            ..creation()
        });
        let selection = options.authenticator_selection.unwrap();
        assert_eq!(
            (
                options.rp.id.as_str(),
                selection.resident_key,
                options.exclude_credentials.map(|e| e.len()),
            ),
            (RP, Some(ResidentKeyRequirement::Required), Some(1))
        );
    }

    #[test]
    fn no_bluetooth_adapter_reads_as_bluetooth_off() {
        assert_eq!(
            key_error(&WebauthnCError::NoBluetoothAdapter),
            KeyError::NoBluetooth
        );
    }

    #[test]
    fn a_locked_key_and_a_key_without_a_passkey_are_told_apart() {
        assert_eq!(
            (
                key_error(&WebauthnCError::Ctap(CtapError::Ctap2PinBlocked)),
                key_error(&WebauthnCError::Ctap(CtapError::Ctap2NoCredentials)),
            ),
            (KeyError::Locked, KeyError::NoPasskey)
        );
    }
}
