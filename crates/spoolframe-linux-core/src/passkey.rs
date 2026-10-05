//! Passkeys on a security key or a phone (P3-19), as the platform's
//! passkey API would do them on a system that has one.
//!
//! Linux has no passkey API, so the app talks to the authenticator itself
//! through webauthn-authenticator-rs: CTAP2 over USB to a key, or the
//! hybrid (caBLE) flow to a phone, which scans a QR code, proves it is
//! near over Bluetooth, and answers through a websocket tunnel. It does what the server expects
//! of an app: the relying party is [`APP_RP_ID`] on every server, and the
//! client data names [`APP_ORIGIN`]. The bytes go to and from the server
//! as the key gives and takes them.
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
use webauthn_authenticator_rs::transport::Transport;
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
    APP_ORIGIN, APP_RP_ID, FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation,
    PasskeyRequest,
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

/// Why a ceremony ended without a passkey.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
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
    match run(via, prompter, Op::SignIn(request_options(request)))? {
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
    match run(via, prompter, Op::Register(creation_options(creation)))? {
        Answer::Made(credential) => Ok(made(credential, name)),
        Answer::Signed(_) => Err(KeyError::Other("the authenticator signed in".into())),
    }
}

/// What a ceremony asks of the authenticator.
enum Op {
    SignIn(PublicKeyCredentialRequestOptions),
    Register(PublicKeyCredentialCreationOptions),
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
        Op::SignIn(options) => authenticator
            .perform_auth(origin(), options.clone(), wait)
            .map(Answer::Signed),
        Op::Register(options) => authenticator
            .perform_register(origin(), options.clone(), wait)
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
/// [`APP_RP_ID`].
pub fn request_options(request: &PasskeyRequest) -> PublicKeyCredentialRequestOptions {
    PublicKeyCredentialRequestOptions {
        challenge: request.challenge.clone(),
        timeout: Some(WAIT.as_millis() as u32),
        rp_id: APP_RP_ID.to_owned(),
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
            id: APP_RP_ID.to_owned(),
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

/// What a key signed, as the server takes it. A key leaves the user
/// handle out when the passkey was asked for by id; the server knows
/// whose it is then.
pub fn signed(credential: PublicKeyCredential) -> FinishPasskeySignIn {
    let response = credential.response;
    FinishPasskeySignIn {
        credential_id: credential.raw_id,
        authenticator_data: response.authenticator_data,
        client_data_json: response.client_data_json,
        signature: response.signature,
        user_handle: response.user_handle.unwrap_or_default(),
    }
}

/// What a key made, as the server keeps it.
pub fn made(credential: RegisterPublicKeyCredential, name: &str) -> FinishPasskeyRegistration {
    FinishPasskeyRegistration {
        credential_id: credential.raw_id,
        attestation_object: credential.response.attestation_object,
        client_data_json: credential.response.client_data_json,
        name: name.to_owned(),
    }
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
            CtapError::Ctap2UserActionTimeout => KeyError::TimedOut,
            CtapError::Ctap2KeepAliveCancel
            | CtapError::Ctap2OperationDenied
            | CtapError::Ctap2NotAllowed => KeyError::Cancelled,
            other => KeyError::Other(format!("the key refused: {other:?}")),
        },
        other => KeyError::Other(other.to_string()),
    }
}

fn origin() -> url::Url {
    url::Url::parse(APP_ORIGIN).expect("APP_ORIGIN is a URL")
}

/// Waits for a key, then runs `op` on it, asking for the PIN again while
/// it is refused.
fn with_key(prompter: Arc<dyn Prompter>, op: &Op) -> Result<Answer, KeyError> {
    let runtime = runtime()?;
    let _entered = runtime.enter();
    let ui = Ui::new(prompter.clone());
    prompter.show(Prompt::WaitingForKey);
    let transport = runtime
        .block_on(USBTransport::new())
        .map_err(|e| key_error(&e))?;
    let mut key = runtime
        .block_on(async {
            let stream = transport.watch().await?;
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
            result => return result.map_err(|e| key_error(&e)),
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
        Op::SignIn(_) => CableRequestType::GetAssertion,
        Op::Register(_) => CableRequestType::DiscoverableMakeCredential,
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

    fn request(allow: Vec<Vec<u8>>) -> PasskeyRequest {
        PasskeyRequest {
            challenge: vec![7; 32],
            allow_credential_ids: allow,
        }
    }

    #[test]
    fn a_sign_in_asks_for_the_apps_relying_party_with_verification() {
        let options = request_options(&request(Vec::new()));
        assert_eq!(
            (
                options.rp_id.as_str(),
                options.user_verification,
                options.allow_credentials.len()
            ),
            (APP_RP_ID, UserVerificationPolicy::Required, 0)
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
    fn a_passkey_is_made_discoverable_for_the_apps_relying_party() {
        let options = creation_options(&PasskeyCreation {
            challenge: vec![1; 32],
            user_id: vec![9; 16],
            user_name: "andrey on Home Media".into(),
            display_name: "Andrey on Home Media".into(),
            exclude_credential_ids: vec![vec![4]],
        });
        let selection = options.authenticator_selection.unwrap();
        assert_eq!(
            (
                options.rp.id.as_str(),
                selection.resident_key,
                options.exclude_credentials.map(|e| e.len()),
            ),
            (APP_RP_ID, Some(ResidentKeyRequirement::Required), Some(1))
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
