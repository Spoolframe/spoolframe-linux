use crate::{
    ecdsa_p256::{EcdsaP256PublicKey, EcdsaP256Signature, EcdsaP256VerifyingKey},
    ecdsa_p384::{EcdsaP384PublicKey, EcdsaP384Signature, EcdsaP384VerifyingKey},
    s256::{Sha256, Sha256Output},
    traits::{Digest, EncodeDer, OwnedToRef, Verifier},
};
use tracing::error;

#[cfg(feature = "rsa")]
use crate::rsa::{RS256PublicKey, RS256Signature, RS256VerifyingKey};

pub use self::chain::{X509Store, X509VerificationError};
pub use self::display::X509Display;
pub use self::serialise::{pkeyb64, x509b64};
pub use const_oid::db as oiddb;
pub use const_oid::{AssociatedOid, ObjectIdentifier};
pub use der::asn1::{BitString, GeneralizedTime, Ia5String, OctetString};
pub use x509_cert::builder::RequestBuilder as CertificateRequestBuilder;
pub use x509_cert::builder::{Builder, CertificateBuilder, Profile};
pub use x509_cert::certificate::{Certificate, Version};
pub use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName, OtherName};
pub use x509_cert::ext::pkix::{
    crl::dp::DistributionPoint, crl::CrlDistributionPoints, AccessDescription,
    AuthorityInfoAccessSyntax, AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage,
    KeyUsage, KeyUsages, SubjectAltName, SubjectKeyIdentifier,
};
pub use x509_cert::name::Name;
pub use x509_cert::request::CertReq as CertificateRequest;
pub use x509_cert::serial_number::SerialNumber;
pub use x509_cert::spki::{
    AlgorithmIdentifier, SignatureBitStringEncoding, SubjectPublicKeyInfoOwned,
};
pub use x509_cert::time::{Time, Validity};

mod chain;
mod display;
mod serialise;

pub fn uuid_to_serial(serial_uuid: uuid::Uuid) -> SerialNumber {
    let mut serial_bytes: [u8; 17] = [0; 17];
    // The first byte must be a value else if the leading byte is a null then
    // der is unhappy.
    serial_bytes[0] = 0x01;
    let update_bytes = &mut serial_bytes[1..];
    update_bytes.copy_from_slice(serial_uuid.as_bytes());

    #[allow(clippy::expect_used)]
    SerialNumber::new(&serial_bytes).expect("Failed to create serial number from uuid")
}

pub fn x509_digest_public_key_sha256(certificate: &Certificate) -> Option<Sha256Output> {
    let public_key_bytes = certificate
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()?;

    let mut hasher = Sha256::new();
    hasher.update(public_key_bytes);
    Some(hasher.finalize())
}

pub fn x509_digest_cert_sha256(certificate: &Certificate) -> Result<Sha256Output, der::Error> {
    let mut hasher = Sha256::new();
    hasher.update(certificate.to_der()?);
    Ok(hasher.finalize())
}

pub fn x509_verify_signature(
    data: &[u8],
    signature: &[u8],
    certificate: &Certificate,
) -> Result<(), X509VerificationError> {
    let subject_public_key_info = certificate
        .tbs_certificate
        .subject_public_key_info
        .owned_to_ref();

    match subject_public_key_info.algorithm.oids() {
        Ok((oiddb::rfc5912::ID_EC_PUBLIC_KEY, Some(oiddb::rfc5912::SECP_256_R_1))) => {
            let signature = EcdsaP256Signature::from_der(signature)
                .map_err(|_err| X509VerificationError::DerSignatureInvalid)?;

            let verifier = EcdsaP256PublicKey::try_from(subject_public_key_info)
                .map(EcdsaP256VerifyingKey::from)
                .map_err(|_err| X509VerificationError::VerifyingKeyFromSpki)?;

            verifier
                .verify(data, &signature)
                .map_err(|_err| X509VerificationError::SignatureVerificationFailed)?;
        }
        Ok((oiddb::rfc5912::ID_EC_PUBLIC_KEY, Some(oiddb::rfc5912::SECP_384_R_1))) => {
            let signature = EcdsaP384Signature::from_der(signature)
                .map_err(|_err| X509VerificationError::DerSignatureInvalid)?;

            let verifier = EcdsaP384PublicKey::try_from(subject_public_key_info)
                .map(EcdsaP384VerifyingKey::from)
                .map_err(|_err| X509VerificationError::VerifyingKeyFromSpki)?;

            verifier
                .verify(data, &signature)
                .map_err(|_err| X509VerificationError::SignatureVerificationFailed)?;
        }
        #[cfg(feature = "rsa")]
        Ok((oiddb::rfc5912::SHA_256_WITH_RSA_ENCRYPTION, None)) => {
            let signature = RS256Signature::try_from(signature)
                .map_err(|_err| X509VerificationError::DerSignatureInvalid)?;

            let verifier = RS256PublicKey::try_from(subject_public_key_info)
                .map(RS256VerifyingKey::new)
                .map_err(|_err| X509VerificationError::VerifyingKeyFromSpki)?;

            verifier
                .verify(data, &signature)
                .map_err(|_err| X509VerificationError::SignatureVerificationFailed)?;
        }
        algo_oids => {
            error!(?algo_oids);
            return Err(X509VerificationError::SignatureAlgorithmNotImplemented);
        }
    }

    Ok(())
}
