//! libfido2 (P7-08): a security key as a portable identity's root signs
//! CTAP2 over a hash the client chooses, SHA-256 of the bytes the root
//! signs, under the key's PIN. The passkey library (webauthn-authenticator-
//! rs) cannot: it hashes its own clientDataJSON and keeps its PIN protocol
//! private. libfido2 takes the hash as it is.
//!
//! From the spikes on the user's Feitian key (`__misc/p7-08-key-spike`):
//! the PIN alone gives UV, and asking for the `uv` option is refused by a
//! key with no built-in UV, so it is never sent. Everything here blocks;
//! callers run it off the main loop.

use std::ffi::{CStr, CString, c_char, c_int};

/// Why the key did not do what was asked, in the kinds the UI acts on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum KeyError {
    /// No key is plugged in, or none can be opened (its udev rule).
    #[error("no security key can be opened")]
    NoKey,
    #[error("the PIN is wrong")]
    WrongPin,
    /// Too many wrong PINs in a row: unplug it and plug it back in.
    #[error("unplug the key and plug it back in")]
    Replug,
    /// No more PIN tries: the key has to be reset, losing what it holds.
    #[error("the key is locked")]
    Locked,
    /// The key has no PIN set; one has to be set with its maker's tool.
    #[error("the key has no PIN")]
    NoPin,
    /// The key holds no credential the request names.
    #[error("no credential for this on the key")]
    NoCredential,
    /// The key has no room for another resident credential.
    #[error("the key is full")]
    Full,
    /// Nobody touched the key in time, or the touch was refused.
    #[error("the key was not touched in time")]
    NotTouched,
    #[error("{0}")]
    Other(String),
}

/// A key plugged in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyDevice {
    /// Its hidraw node, which libfido2 opens.
    pub path: String,
    pub vendor: u16,
    pub product: u16,
    pub name: String,
}

/// What a key says of itself (getInfo), without the PIN.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct KeyInfo {
    pub versions: Vec<String>,
    pub extensions: Vec<String>,
    /// Its options that are on: `rk`, `clientPin`, `credentialMgmtPreview`…
    pub options: Vec<String>,
    /// Resident credentials it still has room for, if it says.
    pub remaining: Option<u64>,
}

impl KeyInfo {
    /// It can root an identity: FIDO2, resident credentials, a PIN set and
    /// credProtect, as P7-08 asks of a key.
    pub fn can_root(&self) -> bool {
        self.versions.iter().any(|v| v.starts_with("FIDO_2"))
            && self.options.iter().any(|o| o == "rk")
            && self.options.iter().any(|o| o == "clientPin")
            && self.extensions.iter().any(|e| e == "credProtect")
    }
}

/// A resident credential just made.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Made {
    pub credential_id: Vec<u8>,
    /// The P-256 public key, SEC1 uncompressed (65 bytes).
    pub public_key: Vec<u8>,
    /// The raw authenticator data the key signed with the hash.
    pub authenticator_data: Vec<u8>,
    /// The attestation signature (DER ECDSA).
    pub signature: Vec<u8>,
}

/// One signature from getAssertion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signed {
    pub credential_id: Vec<u8>,
    /// The raw authenticator data: rpIdHash, flags, counter.
    pub authenticator_data: Vec<u8>,
    /// DER ECDSA over `authenticator_data ‖ client_data_hash`.
    pub signature: Vec<u8>,
    /// The credential's user id, when the request named no credential.
    pub user_handle: Option<Vec<u8>>,
}

#[allow(non_camel_case_types)]
mod ffi {
    use std::ffi::{c_char, c_int};

    #[repr(C)]
    pub struct fido_dev_info_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct fido_dev_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct fido_cbor_info_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct fido_cred_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct fido_assert_t {
        _private: [u8; 0],
    }

    pub const FIDO_OK: c_int = 0;
    pub const FIDO_OPT_FALSE: c_int = 1;
    pub const FIDO_OPT_TRUE: c_int = 2;
    pub const COSE_ES256: c_int = -7;
    pub const FIDO_CRED_PROT_UV_OPTIONAL_WITH_ID: c_int = 0x02;

    // FIDO_ERR_* (fido/err.h).
    pub const FIDO_ERR_ACTION_TIMEOUT: c_int = 0x3a;
    pub const FIDO_ERR_KEY_STORE_FULL: c_int = 0x28;
    pub const FIDO_ERR_NO_CREDENTIALS: c_int = 0x2e;
    pub const FIDO_ERR_OPERATION_DENIED: c_int = 0x27;
    pub const FIDO_ERR_PIN_INVALID: c_int = 0x31;
    pub const FIDO_ERR_PIN_BLOCKED: c_int = 0x32;
    pub const FIDO_ERR_PIN_AUTH_BLOCKED: c_int = 0x34;
    pub const FIDO_ERR_PIN_NOT_SET: c_int = 0x35;
    pub const FIDO_ERR_USER_ACTION_TIMEOUT: c_int = 0x2f;

    unsafe extern "C" {
        pub fn fido_init(flags: c_int);
        pub fn fido_strerr(code: c_int) -> *const c_char;

        pub fn fido_dev_info_new(n: usize) -> *mut fido_dev_info_t;
        pub fn fido_dev_info_free(list: *mut *mut fido_dev_info_t, n: usize);
        pub fn fido_dev_info_manifest(
            list: *mut fido_dev_info_t,
            ilen: usize,
            olen: *mut usize,
        ) -> c_int;
        pub fn fido_dev_info_ptr(list: *const fido_dev_info_t, i: usize) -> *const fido_dev_info_t;
        pub fn fido_dev_info_path(di: *const fido_dev_info_t) -> *const c_char;
        pub fn fido_dev_info_product_string(di: *const fido_dev_info_t) -> *const c_char;
        pub fn fido_dev_info_manufacturer_string(di: *const fido_dev_info_t) -> *const c_char;
        pub fn fido_dev_info_vendor(di: *const fido_dev_info_t) -> i16;
        pub fn fido_dev_info_product(di: *const fido_dev_info_t) -> i16;

        pub fn fido_dev_new() -> *mut fido_dev_t;
        pub fn fido_dev_free(dev: *mut *mut fido_dev_t);
        pub fn fido_dev_open(dev: *mut fido_dev_t, path: *const c_char) -> c_int;
        pub fn fido_dev_close(dev: *mut fido_dev_t) -> c_int;

        pub fn fido_cbor_info_new() -> *mut fido_cbor_info_t;
        pub fn fido_cbor_info_free(ci: *mut *mut fido_cbor_info_t);
        pub fn fido_dev_get_cbor_info(dev: *mut fido_dev_t, ci: *mut fido_cbor_info_t) -> c_int;
        pub fn fido_cbor_info_versions_ptr(ci: *const fido_cbor_info_t) -> *mut *mut c_char;
        pub fn fido_cbor_info_versions_len(ci: *const fido_cbor_info_t) -> usize;
        pub fn fido_cbor_info_extensions_ptr(ci: *const fido_cbor_info_t) -> *mut *mut c_char;
        pub fn fido_cbor_info_extensions_len(ci: *const fido_cbor_info_t) -> usize;
        pub fn fido_cbor_info_options_name_ptr(ci: *const fido_cbor_info_t) -> *mut *mut c_char;
        pub fn fido_cbor_info_options_value_ptr(ci: *const fido_cbor_info_t) -> *const bool;
        pub fn fido_cbor_info_options_len(ci: *const fido_cbor_info_t) -> usize;
        pub fn fido_cbor_info_rk_remaining(ci: *const fido_cbor_info_t) -> i64;

        pub fn fido_cred_new() -> *mut fido_cred_t;
        pub fn fido_cred_free(cred: *mut *mut fido_cred_t);
        pub fn fido_cred_set_type(cred: *mut fido_cred_t, cose_alg: c_int) -> c_int;
        pub fn fido_cred_set_clientdata_hash(
            cred: *mut fido_cred_t,
            hash: *const u8,
            len: usize,
        ) -> c_int;
        pub fn fido_cred_set_rp(
            cred: *mut fido_cred_t,
            id: *const c_char,
            name: *const c_char,
        ) -> c_int;
        pub fn fido_cred_set_user(
            cred: *mut fido_cred_t,
            user_id: *const u8,
            user_id_len: usize,
            name: *const c_char,
            display_name: *const c_char,
            icon: *const c_char,
        ) -> c_int;
        pub fn fido_cred_set_rk(cred: *mut fido_cred_t, rk: c_int) -> c_int;
        pub fn fido_cred_set_prot(cred: *mut fido_cred_t, prot: c_int) -> c_int;
        pub fn fido_dev_make_cred(
            dev: *mut fido_dev_t,
            cred: *mut fido_cred_t,
            pin: *const c_char,
        ) -> c_int;
        pub fn fido_cred_id_ptr(cred: *const fido_cred_t) -> *const u8;
        pub fn fido_cred_id_len(cred: *const fido_cred_t) -> usize;
        pub fn fido_cred_pubkey_ptr(cred: *const fido_cred_t) -> *const u8;
        pub fn fido_cred_pubkey_len(cred: *const fido_cred_t) -> usize;
        pub fn fido_cred_authdata_raw_ptr(cred: *const fido_cred_t) -> *const u8;
        pub fn fido_cred_authdata_raw_len(cred: *const fido_cred_t) -> usize;
        pub fn fido_cred_sig_ptr(cred: *const fido_cred_t) -> *const u8;
        pub fn fido_cred_sig_len(cred: *const fido_cred_t) -> usize;

        pub fn fido_assert_new() -> *mut fido_assert_t;
        pub fn fido_assert_free(assert: *mut *mut fido_assert_t);
        pub fn fido_assert_set_clientdata_hash(
            assert: *mut fido_assert_t,
            hash: *const u8,
            len: usize,
        ) -> c_int;
        pub fn fido_assert_set_rp(assert: *mut fido_assert_t, id: *const c_char) -> c_int;
        pub fn fido_assert_allow_cred(
            assert: *mut fido_assert_t,
            id: *const u8,
            len: usize,
        ) -> c_int;
        pub fn fido_assert_set_up(assert: *mut fido_assert_t, up: c_int) -> c_int;
        pub fn fido_dev_get_assert(
            dev: *mut fido_dev_t,
            assert: *mut fido_assert_t,
            pin: *const c_char,
        ) -> c_int;
        pub fn fido_assert_count(assert: *const fido_assert_t) -> usize;
        pub fn fido_assert_id_ptr(assert: *const fido_assert_t, i: usize) -> *const u8;
        pub fn fido_assert_id_len(assert: *const fido_assert_t, i: usize) -> usize;
        pub fn fido_assert_authdata_raw_ptr(assert: *const fido_assert_t, i: usize) -> *const u8;
        pub fn fido_assert_authdata_raw_len(assert: *const fido_assert_t, i: usize) -> usize;
        pub fn fido_assert_sig_ptr(assert: *const fido_assert_t, i: usize) -> *const u8;
        pub fn fido_assert_sig_len(assert: *const fido_assert_t, i: usize) -> usize;
        pub fn fido_assert_user_id_ptr(assert: *const fido_assert_t, i: usize) -> *const u8;
        pub fn fido_assert_user_id_len(assert: *const fido_assert_t, i: usize) -> usize;

        pub fn fido_dev_get_retry_count(dev: *mut fido_dev_t, retries: *mut c_int) -> c_int;

        pub fn fido_credman_del_dev_rk(
            dev: *mut fido_dev_t,
            id: *const u8,
            len: usize,
            pin: *const c_char,
        ) -> c_int;
    }
}

/// libfido2's own error code as one the UI acts on.
pub fn key_error(code: c_int) -> KeyError {
    use ffi::*;
    match code {
        FIDO_ERR_PIN_INVALID => KeyError::WrongPin,
        FIDO_ERR_PIN_AUTH_BLOCKED => KeyError::Replug,
        FIDO_ERR_PIN_BLOCKED => KeyError::Locked,
        FIDO_ERR_PIN_NOT_SET => KeyError::NoPin,
        FIDO_ERR_NO_CREDENTIALS => KeyError::NoCredential,
        FIDO_ERR_KEY_STORE_FULL => KeyError::Full,
        FIDO_ERR_ACTION_TIMEOUT | FIDO_ERR_USER_ACTION_TIMEOUT | FIDO_ERR_OPERATION_DENIED => {
            KeyError::NotTouched
        }
        other => KeyError::Other(strerr(other)),
    }
}

fn strerr(code: c_int) -> String {
    // SAFETY: fido_strerr returns a static string for any code.
    unsafe { CStr::from_ptr(ffi::fido_strerr(code)) }
        .to_string_lossy()
        .into_owned()
}

fn check(code: c_int) -> Result<(), KeyError> {
    if code == ffi::FIDO_OK {
        Ok(())
    } else {
        Err(key_error(code))
    }
}

fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    // SAFETY: once, before any other call.
    INIT.call_once(|| unsafe { ffi::fido_init(0) });
}

/// `n` bytes from `ptr`, or none when it is null.
///
/// # Safety
/// `ptr` is null or points at `n` readable bytes.
unsafe fn bytes(ptr: *const u8, n: usize) -> Vec<u8> {
    if ptr.is_null() || n == 0 {
        return Vec::new();
    }
    // SAFETY: the caller says so.
    unsafe { std::slice::from_raw_parts(ptr, n) }.to_vec()
}

/// A C string from libfido2, or empty.
///
/// # Safety
/// `ptr` is null or a NUL-terminated string.
unsafe fn text(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the caller says so.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// The FIDO keys plugged in now.
fn keys_here() -> Result<Vec<KeyDevice>, KeyError> {
    const MAX: usize = 16;
    init();
    // SAFETY: a list of MAX entries, filled by manifest, read within the
    // count it gives, then freed with the same size.
    unsafe {
        let list = ffi::fido_dev_info_new(MAX);
        if list.is_null() {
            return Err(KeyError::Other("out of memory".into()));
        }
        let mut found = 0usize;
        let code = ffi::fido_dev_info_manifest(list, MAX, &mut found);
        let mut keys = Vec::new();
        if code == ffi::FIDO_OK {
            for i in 0..found {
                let di = ffi::fido_dev_info_ptr(list, i);
                let maker = text(ffi::fido_dev_info_manufacturer_string(di));
                let product = text(ffi::fido_dev_info_product_string(di));
                keys.push(KeyDevice {
                    path: text(ffi::fido_dev_info_path(di)),
                    vendor: ffi::fido_dev_info_vendor(di) as u16,
                    product: ffi::fido_dev_info_product(di) as u16,
                    name: format!("{maker} {product}").trim().to_owned(),
                });
            }
        }
        let mut list = list;
        ffi::fido_dev_info_free(&mut list, MAX);
        check(code)?;
        Ok(keys)
    }
}

/// An open key, closed and freed when dropped.
struct Dev(*mut ffi::fido_dev_t);

impl Dev {
    fn open(path: &str) -> Result<Self, KeyError> {
        init();
        let path = CString::new(path).map_err(|_| KeyError::NoKey)?;
        // SAFETY: a new handle, opened at a NUL-terminated path; freed by
        // Drop whatever happens.
        unsafe {
            let dev = Dev(ffi::fido_dev_new());
            if dev.0.is_null() {
                return Err(KeyError::Other("out of memory".into()));
            }
            if ffi::fido_dev_open(dev.0, path.as_ptr()) != ffi::FIDO_OK {
                return Err(KeyError::NoKey);
            }
            Ok(dev)
        }
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        // SAFETY: the handle Dev::open made; closing one that did not open
        // is an error libfido2 reports and ignores.
        unsafe {
            ffi::fido_dev_close(self.0);
            ffi::fido_dev_free(&mut self.0);
        }
    }
}

/// Strings from one of getInfo's lists.
///
/// # Safety
/// `ptr` is null or points at `n` NUL-terminated strings.
unsafe fn strings(ptr: *mut *mut c_char, n: usize) -> Vec<String> {
    if ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller says so.
    (0..n).map(|i| unsafe { text(*ptr.add(i)) }).collect()
}

/// What the key at `path` says of itself. No PIN, no touch.
fn info_here(path: &str) -> Result<KeyInfo, KeyError> {
    let dev = Dev::open(path)?;
    // SAFETY: an info struct filled by getInfo, read within the lengths it
    // gives, then freed.
    unsafe {
        let mut ci = ffi::fido_cbor_info_new();
        if ci.is_null() {
            return Err(KeyError::Other("out of memory".into()));
        }
        let code = ffi::fido_dev_get_cbor_info(dev.0, ci);
        let info = (code == ffi::FIDO_OK).then(|| {
            let names = strings(
                ffi::fido_cbor_info_options_name_ptr(ci),
                ffi::fido_cbor_info_options_len(ci),
            );
            let values = ffi::fido_cbor_info_options_value_ptr(ci);
            let options = names
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !values.is_null() && *values.add(*i))
                .map(|(_, name)| name)
                .collect();
            let remaining = ffi::fido_cbor_info_rk_remaining(ci);
            KeyInfo {
                versions: strings(
                    ffi::fido_cbor_info_versions_ptr(ci),
                    ffi::fido_cbor_info_versions_len(ci),
                ),
                extensions: strings(
                    ffi::fido_cbor_info_extensions_ptr(ci),
                    ffi::fido_cbor_info_extensions_len(ci),
                ),
                options,
                remaining: (remaining >= 0).then_some(remaining as u64),
            }
        });
        ffi::fido_cbor_info_free(&mut ci);
        check(code)?;
        Ok(info.unwrap_or_default())
    }
}

/// What makeCredential is asked for.
pub struct MakeRequest<'a> {
    pub rp: &'a str,
    pub user_id: &'a [u8],
    pub user_name: &'a str,
    pub client_data_hash: &'a [u8; 32],
    pub pin: &'a str,
}

/// Makes a resident ES256 credential on the key at `path`, under its PIN,
/// kept out of any listing made without the PIN (credProtect 2). Waits
/// for a touch.
fn make_credential_here(path: &str, request: &MakeRequest<'_>) -> Result<Made, KeyError> {
    let dev = Dev::open(path)?;
    let rp = CString::new(request.rp).map_err(|_| KeyError::Other("a NUL in the RP".into()))?;
    let name =
        CString::new(request.user_name).map_err(|_| KeyError::Other("a NUL in the name".into()))?;
    let pin = CString::new(request.pin).map_err(|_| KeyError::WrongPin)?;
    // SAFETY: a credential set up and made, read within the lengths
    // libfido2 gives, then freed; every pointer passed outlives the call.
    unsafe {
        let mut cred = ffi::fido_cred_new();
        if cred.is_null() {
            return Err(KeyError::Other("out of memory".into()));
        }
        let made = (|| {
            check(ffi::fido_cred_set_type(cred, ffi::COSE_ES256))?;
            check(ffi::fido_cred_set_clientdata_hash(
                cred,
                request.client_data_hash.as_ptr(),
                32,
            ))?;
            check(ffi::fido_cred_set_rp(cred, rp.as_ptr(), std::ptr::null()))?;
            check(ffi::fido_cred_set_user(
                cred,
                request.user_id.as_ptr(),
                request.user_id.len(),
                name.as_ptr(),
                name.as_ptr(),
                std::ptr::null(),
            ))?;
            check(ffi::fido_cred_set_rk(cred, ffi::FIDO_OPT_TRUE))?;
            check(ffi::fido_cred_set_prot(
                cred,
                ffi::FIDO_CRED_PROT_UV_OPTIONAL_WITH_ID,
            ))?;
            // UV comes from the PIN; the uv option is never set.
            check(ffi::fido_dev_make_cred(dev.0, cred, pin.as_ptr()))?;
            let point = bytes(
                ffi::fido_cred_pubkey_ptr(cred),
                ffi::fido_cred_pubkey_len(cred),
            );
            Ok(Made {
                credential_id: bytes(ffi::fido_cred_id_ptr(cred), ffi::fido_cred_id_len(cred)),
                public_key: sec1(&point),
                authenticator_data: bytes(
                    ffi::fido_cred_authdata_raw_ptr(cred),
                    ffi::fido_cred_authdata_raw_len(cred),
                ),
                signature: bytes(ffi::fido_cred_sig_ptr(cred), ffi::fido_cred_sig_len(cred)),
            })
        })();
        ffi::fido_cred_free(&mut cred);
        made
    }
}

/// libfido2's ES256 public key (x ‖ y, 64 bytes) as SEC1 uncompressed.
pub fn sec1(point: &[u8]) -> Vec<u8> {
    match point.len() {
        64 => std::iter::once(0x04).chain(point.iter().copied()).collect(),
        _ => point.to_vec(),
    }
}

/// What getAssertion is asked for. An empty `allowed` lets the key say
/// which of its credentials it has, with their user handles.
pub struct SignRequest<'a> {
    pub rp: &'a str,
    pub client_data_hash: &'a [u8; 32],
    pub allowed: &'a [Vec<u8>],
    pub pin: &'a str,
}

/// Signs `client_data_hash` with the key at `path`, under its PIN, with a
/// touch: one signature per credential it has for the request.
fn get_assertion_here(path: &str, request: &SignRequest<'_>) -> Result<Vec<Signed>, KeyError> {
    let dev = Dev::open(path)?;
    let rp = CString::new(request.rp).map_err(|_| KeyError::Other("a NUL in the RP".into()))?;
    let pin = CString::new(request.pin).map_err(|_| KeyError::WrongPin)?;
    // SAFETY: as make_credential: set up, run, read within libfido2's
    // lengths, freed.
    unsafe {
        let mut assert = ffi::fido_assert_new();
        if assert.is_null() {
            return Err(KeyError::Other("out of memory".into()));
        }
        let signed = (|| {
            check(ffi::fido_assert_set_clientdata_hash(
                assert,
                request.client_data_hash.as_ptr(),
                32,
            ))?;
            check(ffi::fido_assert_set_rp(assert, rp.as_ptr()))?;
            for id in request.allowed {
                check(ffi::fido_assert_allow_cred(assert, id.as_ptr(), id.len()))?;
            }
            check(ffi::fido_assert_set_up(assert, ffi::FIDO_OPT_TRUE))?;
            check(ffi::fido_dev_get_assert(dev.0, assert, pin.as_ptr()))?;
            Ok((0..ffi::fido_assert_count(assert))
                .map(|i| {
                    let user = bytes(
                        ffi::fido_assert_user_id_ptr(assert, i),
                        ffi::fido_assert_user_id_len(assert, i),
                    );
                    Signed {
                        credential_id: bytes(
                            ffi::fido_assert_id_ptr(assert, i),
                            ffi::fido_assert_id_len(assert, i),
                        ),
                        authenticator_data: bytes(
                            ffi::fido_assert_authdata_raw_ptr(assert, i),
                            ffi::fido_assert_authdata_raw_len(assert, i),
                        ),
                        signature: bytes(
                            ffi::fido_assert_sig_ptr(assert, i),
                            ffi::fido_assert_sig_len(assert, i),
                        ),
                        user_handle: (!user.is_empty()).then_some(user),
                    }
                })
                .collect())
        })();
        ffi::fido_assert_free(&mut assert);
        signed
    }
}

/// How many wrong PINs the key at `path` takes before it locks.
fn pin_tries_left_here(path: &str) -> Result<u32, KeyError> {
    let dev = Dev::open(path)?;
    let mut left: c_int = 0;
    // SAFETY: an open device and an int to write.
    check(unsafe { ffi::fido_dev_get_retry_count(dev.0, &mut left) })?;
    Ok(left.max(0) as u32)
}

/// Which of `ids` the key at `path` holds under `rp`, with no PIN and no
/// touch: a getAssertion that names one credential and asks for no user
/// presence answers only whether the key has it. credProtect 2 allows
/// that (it hides a credential from listings, not from its own id).
fn holds_here(path: &str, rp: &str, ids: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, KeyError> {
    let dev = Dev::open(path)?;
    let rp = CString::new(rp).map_err(|_| KeyError::Other("a NUL in the RP".into()))?;
    let hash = [0u8; 32];
    let mut held = Vec::new();
    for id in ids {
        // SAFETY: as get_assertion_here: set up, run, freed.
        let found = unsafe {
            let mut assert = ffi::fido_assert_new();
            if assert.is_null() {
                return Err(KeyError::Other("out of memory".into()));
            }
            let code = (|| {
                check(ffi::fido_assert_set_clientdata_hash(
                    assert,
                    hash.as_ptr(),
                    32,
                ))?;
                check(ffi::fido_assert_set_rp(assert, rp.as_ptr()))?;
                check(ffi::fido_assert_allow_cred(assert, id.as_ptr(), id.len()))?;
                check(ffi::fido_assert_set_up(assert, ffi::FIDO_OPT_FALSE))?;
                check(ffi::fido_dev_get_assert(dev.0, assert, std::ptr::null()))
            })();
            ffi::fido_assert_free(&mut assert);
            code
        };
        match found {
            Ok(()) => held.push(id.clone()),
            Err(KeyError::NoCredential) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(held)
}

/// Which of `ids` the key at `path` holds under `rp`; no PIN, no touch.
pub fn holds(path: &str, rp: &str, ids: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, KeyError> {
    match broker::get() {
        Some(broker) => broker.holds(path, rp, ids),
        None => holds_here(path, rp, ids),
    }
}

/// Deletes resident credential `credential_id` from the key at `path`,
/// under its PIN (credential management). For the hardware tests, which
/// leave the key as they found it.
fn delete_credential_here(path: &str, credential_id: &[u8], pin: &str) -> Result<(), KeyError> {
    let dev = Dev::open(path)?;
    let pin = CString::new(pin).map_err(|_| KeyError::WrongPin)?;
    // SAFETY: an open device, an id slice and a NUL-terminated PIN that
    // outlive the call.
    check(unsafe {
        ffi::fido_credman_del_dev_rk(
            dev.0,
            credential_id.as_ptr(),
            credential_id.len(),
            pin.as_ptr(),
        )
    })
}

/// The FIDO keys plugged in now.
pub fn keys() -> Result<Vec<KeyDevice>, KeyError> {
    match broker::get() {
        Some(broker) => broker.keys(),
        None => keys_here(),
    }
}

/// What the key at `path` says of itself. No PIN, no touch.
pub fn info(path: &str) -> Result<KeyInfo, KeyError> {
    match broker::get() {
        Some(broker) => broker.info(path),
        None => info_here(path),
    }
}

/// Makes a resident ES256 credential on the key at `path`, under its PIN,
/// kept out of any listing made without the PIN (credProtect 2). Waits
/// for a touch.
pub fn make_credential(path: &str, request: &MakeRequest<'_>) -> Result<Made, KeyError> {
    match broker::get() {
        Some(broker) => broker.make_credential(path, request),
        None => make_credential_here(path, request),
    }
}

/// Signs `client_data_hash` with the key at `path`, under its PIN, with a
/// touch: one signature per credential it has for the request.
pub fn get_assertion(path: &str, request: &SignRequest<'_>) -> Result<Vec<Signed>, KeyError> {
    match broker::get() {
        Some(broker) => broker.get_assertion(path, request),
        None => get_assertion_here(path, request),
    }
}

/// How many wrong PINs the key at `path` takes before it locks.
pub fn pin_tries_left(path: &str) -> Result<u32, KeyError> {
    match broker::get() {
        Some(broker) => broker.pin_tries_left(path),
        None => pin_tries_left_here(path),
    }
}

/// Deletes resident credential `credential_id` from the key at `path`,
/// under its PIN (credential management).
pub fn delete_credential(path: &str, credential_id: &[u8], pin: &str) -> Result<(), KeyError> {
    match broker::get() {
        Some(broker) => broker.delete_credential(path, credential_id, pin),
        None => delete_credential_here(path, credential_id, pin),
    }
}

/// The key helper (P7-08): a process forked before the app confines
/// itself, which alone opens the security keys. Landlock lets a process
/// open only the device files there when it confined itself, and a key
/// plugged in later, or plugged in again, is a new one: the helper is
/// confined too, but with every device under /dev open to it, so keys
/// can come and go. It speaks one JSON line per request over a socket
/// pair, and ends when the app does.
pub mod broker {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Mutex, OnceLock};

    use serde::{Deserialize, Serialize};

    use super::{KeyDevice, KeyError, KeyInfo, Made, MakeRequest, SignRequest, Signed};

    #[derive(Debug, Serialize, Deserialize)]
    enum Request {
        Keys,
        Info {
            path: String,
        },
        Make {
            path: String,
            rp: String,
            user_id: Vec<u8>,
            user_name: String,
            client_data_hash: [u8; 32],
            pin: String,
        },
        Sign {
            path: String,
            rp: String,
            client_data_hash: [u8; 32],
            allowed: Vec<Vec<u8>>,
            pin: String,
        },
        Tries {
            path: String,
        },
        Delete {
            path: String,
            credential_id: Vec<u8>,
            pin: String,
        },
        Holds {
            path: String,
            rp: String,
            ids: Vec<Vec<u8>>,
        },
    }

    #[derive(Debug, Serialize, Deserialize)]
    enum Reply {
        Keys(Vec<KeyDevice>),
        Info(KeyInfo),
        Made(Made),
        Signed(Vec<Signed>),
        Tries(u32),
        Held(Vec<Vec<u8>>),
        Done,
    }

    /// The app's end of the helper's socket.
    pub struct Broker(Mutex<BufReader<UnixStream>>);

    static BROKER: OnceLock<Broker> = OnceLock::new();

    /// The helper, if one was started.
    pub fn get() -> Option<&'static Broker> {
        BROKER.get()
    }

    /// Forks the helper. Call before anything starts a thread, and before
    /// the app confines itself: the helper runs `confine` (its own, looser
    /// on /dev) and then serves until the app goes. Returns in the app.
    pub fn start(confine: impl FnOnce()) -> std::io::Result<()> {
        let (app, helper) = UnixStream::pair()?;
        // SAFETY: called single-threaded (see above); the child only
        // confines itself and serves, then exits without unwinding into
        // the app's code.
        match unsafe { libc::fork() } {
            -1 => Err(std::io::Error::last_os_error()),
            0 => {
                drop(app);
                // SAFETY: plain prctl; the helper dies with the app.
                unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
                confine();
                serve(helper);
                // SAFETY: the child's end; no destructors of the app run.
                unsafe { libc::_exit(0) }
            }
            _ => {
                drop(helper);
                let _ = BROKER.set(Broker(Mutex::new(BufReader::new(app))));
                Ok(())
            }
        }
    }

    /// The helper's loop: a request a line, its answer a line, until the
    /// app's end closes.
    fn serve(stream: UnixStream) {
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let answer: Result<Reply, KeyError> = match serde_json::from_str(&line) {
                Ok(request) => handle(request),
                Err(e) => Err(KeyError::Other(format!(
                    "the key helper could not read: {e}"
                ))),
            };
            let Ok(mut bytes) = serde_json::to_vec(&answer) else {
                return;
            };
            bytes.push(b'\n');
            if out.write_all(&bytes).is_err() {
                return;
            }
        }
    }

    fn handle(request: Request) -> Result<Reply, KeyError> {
        Ok(match request {
            Request::Keys => Reply::Keys(super::keys_here()?),
            Request::Info { path } => Reply::Info(super::info_here(&path)?),
            Request::Make {
                path,
                rp,
                user_id,
                user_name,
                client_data_hash,
                pin,
            } => Reply::Made(super::make_credential_here(
                &path,
                &MakeRequest {
                    rp: &rp,
                    user_id: &user_id,
                    user_name: &user_name,
                    client_data_hash: &client_data_hash,
                    pin: &pin,
                },
            )?),
            Request::Sign {
                path,
                rp,
                client_data_hash,
                allowed,
                pin,
            } => Reply::Signed(super::get_assertion_here(
                &path,
                &SignRequest {
                    rp: &rp,
                    client_data_hash: &client_data_hash,
                    allowed: &allowed,
                    pin: &pin,
                },
            )?),
            Request::Tries { path } => Reply::Tries(super::pin_tries_left_here(&path)?),
            Request::Holds { path, rp, ids } => Reply::Held(super::holds_here(&path, &rp, &ids)?),
            Request::Delete {
                path,
                credential_id,
                pin,
            } => {
                super::delete_credential_here(&path, &credential_id, &pin)?;
                Reply::Done
            }
        })
    }

    fn gone() -> KeyError {
        KeyError::Other("the security key helper stopped; restart Spoolframe".into())
    }

    impl Broker {
        fn call(&self, request: &Request) -> Result<Reply, KeyError> {
            let mut stream = self.0.lock().expect("never poisoned");
            let mut bytes = serde_json::to_vec(request).map_err(|_| gone())?;
            bytes.push(b'\n');
            stream.get_mut().write_all(&bytes).map_err(|_| gone())?;
            let mut line = String::new();
            if stream.read_line(&mut line).map_err(|_| gone())? == 0 {
                return Err(gone());
            }
            serde_json::from_str::<Result<Reply, KeyError>>(&line).map_err(|_| gone())?
        }

        pub fn keys(&self) -> Result<Vec<KeyDevice>, KeyError> {
            match self.call(&Request::Keys)? {
                Reply::Keys(keys) => Ok(keys),
                _ => Err(gone()),
            }
        }

        pub fn info(&self, path: &str) -> Result<KeyInfo, KeyError> {
            match self.call(&Request::Info { path: path.into() })? {
                Reply::Info(info) => Ok(info),
                _ => Err(gone()),
            }
        }

        pub fn make_credential(&self, path: &str, r: &MakeRequest<'_>) -> Result<Made, KeyError> {
            match self.call(&Request::Make {
                path: path.into(),
                rp: r.rp.into(),
                user_id: r.user_id.to_vec(),
                user_name: r.user_name.into(),
                client_data_hash: *r.client_data_hash,
                pin: r.pin.into(),
            })? {
                Reply::Made(made) => Ok(made),
                _ => Err(gone()),
            }
        }

        pub fn get_assertion(
            &self,
            path: &str,
            r: &SignRequest<'_>,
        ) -> Result<Vec<Signed>, KeyError> {
            match self.call(&Request::Sign {
                path: path.into(),
                rp: r.rp.into(),
                client_data_hash: *r.client_data_hash,
                allowed: r.allowed.to_vec(),
                pin: r.pin.into(),
            })? {
                Reply::Signed(signed) => Ok(signed),
                _ => Err(gone()),
            }
        }

        pub fn holds(
            &self,
            path: &str,
            rp: &str,
            ids: &[Vec<u8>],
        ) -> Result<Vec<Vec<u8>>, KeyError> {
            match self.call(&Request::Holds {
                path: path.into(),
                rp: rp.into(),
                ids: ids.to_vec(),
            })? {
                Reply::Held(held) => Ok(held),
                _ => Err(gone()),
            }
        }

        pub fn pin_tries_left(&self, path: &str) -> Result<u32, KeyError> {
            match self.call(&Request::Tries { path: path.into() })? {
                Reply::Tries(n) => Ok(n),
                _ => Err(gone()),
            }
        }

        pub fn delete_credential(
            &self,
            path: &str,
            credential_id: &[u8],
            pin: &str,
        ) -> Result<(), KeyError> {
            match self.call(&Request::Delete {
                path: path.into(),
                credential_id: credential_id.to_vec(),
                pin: pin.into(),
            })? {
                Reply::Done => Ok(()),
                _ => Err(gone()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_pin_and_a_locked_key_are_told_apart() {
        assert_eq!(
            (key_error(0x31), key_error(0x34), key_error(0x32)),
            (KeyError::WrongPin, KeyError::Replug, KeyError::Locked)
        );
    }

    #[test]
    fn libfido2s_point_becomes_sec1_uncompressed() {
        let point = [7u8; 64];
        let sec1 = sec1(&point);
        assert_eq!((sec1.len(), sec1[0]), (65, 0x04));
    }

    #[test]
    fn the_spikes_feitian_can_root_an_identity() {
        let feitian = KeyInfo {
            versions: vec!["U2F_V2".into(), "FIDO_2_0".into(), "FIDO_2_1_PRE".into()],
            extensions: vec!["credProtect".into(), "hmac-secret".into()],
            options: vec![
                "rk".into(),
                "up".into(),
                "clientPin".into(),
                "credentialMgmtPreview".into(),
            ],
            remaining: Some(46),
        };
        assert!(feitian.can_root());
    }

    #[test]
    fn a_u2f_only_key_cannot() {
        let u2f = KeyInfo {
            versions: vec!["U2F_V2".into()],
            ..Default::default()
        };
        assert!(!u2f.can_root());
    }

    /// libfido2 is linked and lists keys: none is fine here.
    #[test]
    fn listing_keys_works_with_or_without_one() {
        assert!(keys().is_ok());
    }
}
