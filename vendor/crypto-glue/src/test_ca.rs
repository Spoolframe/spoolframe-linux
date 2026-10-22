use std::time::{Duration, SystemTime};

use x509_cert::builder::{Builder, CertificateBuilder, Profile, RequestBuilder};
use x509_cert::der::asn1::Ia5String;
use x509_cert::der::Encode;
use x509_cert::name::Name;
use x509_cert::request::CertReq;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::{Time, Validity};

use x509_cert::certificate::CertificateInner;

use x509_cert::ext::pkix::{
    constraints::name::GeneralSubtree,
    constraints::BasicConstraints,
    crl::dp::DistributionPoint,
    crl::CrlDistributionPoints,
    name::{DistributionPointName, GeneralName},
    AuthorityKeyIdentifier, ExtendedKeyUsage, KeyUsage, KeyUsages, NameConstraints, SubjectAltName,
    SubjectKeyIdentifier,
};

use x509_cert::spki::DecodePublicKey;

use std::str::FromStr;

use p384::ecdsa::{signature::Verifier, VerifyingKey};
use p384::ecdsa::{DerSignature, SigningKey};

use crate::x509::uuid_to_serial;
use uuid::Uuid;

pub(crate) fn build_test_ca_root(
    not_before: Time,
    not_after: Time,
) -> (SigningKey, CertificateInner) {
    let mut rng = rand::thread_rng();

    let root_serial_uuid = Uuid::new_v4();
    let serial_number = uuid_to_serial(root_serial_uuid);

    let validity = Validity {
        not_before,
        not_after,
    };

    let profile = Profile::Root;
    let root_subject = Name::from_str("CN=Oh no he is writing a CA,O=Pls Help,C=AU").unwrap();

    let signing_key = SigningKey::random(&mut rng);
    let verifying_key = VerifyingKey::from(&signing_key); // Serialize with `::to_encoded_point()`
    let pub_key = SubjectPublicKeyInfoOwned::from_key(verifying_key).expect("get rsa pub key");

    let mut builder = CertificateBuilder::new(
        profile,
        serial_number,
        validity,
        root_subject.clone(),
        pub_key.clone(),
        &signing_key,
    )
    .expect("Create certificate");

    let dist_points = vec![DistributionPoint {
        distribution_point: Some(DistributionPointName::FullName(vec![
            GeneralName::UniformResourceIdentifier(
                "https://example.com/crl".to_string().try_into().unwrap(),
            ),
        ])),
        reasons: None,
        crl_issuer: None,
    }];

    let crl_extension = CrlDistributionPoints(dist_points);

    builder
        .add_extension(&crl_extension)
        .expect("Unable to add extension");

    let cert = builder.build_with_rng::<DerSignature>(&mut rng).unwrap();

    // let cert_der = cert.to_der().unwrap();
    println!("{:?}", cert);

    let cert_bytes = cert.tbs_certificate.to_der().unwrap();

    let byte_sig: &[u8] = cert.signature.as_bytes().unwrap();
    let cert_sig = DerSignature::try_from(byte_sig).unwrap();
    assert!(verifying_key.verify(&cert_bytes, &cert_sig).is_ok());

    // For a root cert we must validate
    //
    //   Basic Constraints: critical
    //     CA: True
    let (critical, basic_constraints) = cert
        .tbs_certificate
        .get::<BasicConstraints>()
        .expect("failed to get extensions")
        .expect("basic constraints not present");

    assert!(critical);
    eprintln!("{:?}", basic_constraints);

    assert!(basic_constraints.ca);
    assert!(basic_constraints.path_len_constraint.is_none());

    //   Key Usage: critical
    //     Certificate Sign
    //     CRL Sign

    let (critical, key_usage) = cert
        .tbs_certificate
        .get::<KeyUsage>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert!(critical);

    eprintln!("{:?}", key_usage);
    let expected_key_usages = KeyUsages::KeyCertSign | KeyUsages::CRLSign;
    assert_eq!(key_usage, expected_key_usages.into());

    //   Subject Key ID
    //     (Should be sha1 of the public key?)
    let (_, ca_subject_key_id) = cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", ca_subject_key_id);

    //   Validity
    assert_eq!(
        cert.tbs_certificate.validity.not_before.to_unix_duration(),
        validity.not_before.to_unix_duration()
    );
    assert_eq!(
        cert.tbs_certificate.validity.not_after.to_unix_duration(),
        validity.not_after.to_unix_duration()
    );

    //   Issuer == Subject

    assert_eq!(cert.tbs_certificate.issuer, cert.tbs_certificate.subject,);

    //   Serial Number - We have to drop the first byte.
    println!("{:?}", &cert.tbs_certificate.serial_number.as_bytes()[1..]);
    println!("{:?}", root_serial_uuid.as_bytes());
    let verify_serial =
        Uuid::from_slice(&cert.tbs_certificate.serial_number.as_bytes()[1..]).unwrap();

    assert_eq!(root_serial_uuid, verify_serial);

    // CRL
    //   It's there. Trust me.

    (signing_key, cert)
}

pub(crate) fn build_test_ca_int(
    not_before: Time,
    not_after: Time,
    root_signing_key: &SigningKey,
    root_ca_cert: &CertificateInner,
) -> (SigningKey, CertificateInner) {
    let mut rng = rand::thread_rng();

    let root_verifying_key = VerifyingKey::from(root_signing_key); // Serialize with `::to_encoded_point()`

    let (_, root_subject_key_id) = root_ca_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    let int_serial_uuid = Uuid::new_v4();

    let mut serial_bytes: [u8; 17] = [0; 17];
    serial_bytes[0] = 0x01;
    {
        let update_bytes = &mut serial_bytes[1..];
        update_bytes.copy_from_slice(int_serial_uuid.as_bytes());
    }

    println!("{:?}", serial_bytes);
    let serial_number = SerialNumber::new(&serial_bytes).unwrap();

    let validity = Validity {
        not_before,
        not_after,
    };

    let profile = Profile::SubCA {
        issuer: root_ca_cert.tbs_certificate.subject.clone(),
        path_len_constraint: Some(0),
    };
    let int_subject = Name::from_str("CN=Oh no its an intermediate,C=AU").unwrap();

    let int_signing_key = SigningKey::random(&mut rng);
    let int_verifying_key = VerifyingKey::from(&int_signing_key); // Serialize with `::to_encoded_point()`
    let int_pub_key =
        SubjectPublicKeyInfoOwned::from_key(int_verifying_key).expect("get rsa pub key");

    let mut builder = CertificateBuilder::new(
        profile,
        serial_number,
        validity,
        int_subject.clone(),
        int_pub_key.clone(),
        root_signing_key,
    )
    .expect("Create certificate");

    let dist_points = vec![DistributionPoint {
        distribution_point: Some(DistributionPointName::FullName(vec![
            GeneralName::UniformResourceIdentifier(
                "https://example.com/int/crl"
                    .to_string()
                    .try_into()
                    .unwrap(),
            ),
        ])),
        reasons: None,
        crl_issuer: None,
    }];

    let crl_extension = CrlDistributionPoints(dist_points);

    builder
        .add_extension(&crl_extension)
        .expect("Unable to add extension");

    let name_constraint_extension = NameConstraints {
        permitted_subtrees: Some(vec![GeneralSubtree {
            base: GeneralName::DnsName("example.com".to_string().try_into().unwrap()),
            minimum: 0,
            maximum: None,
        }]),
        excluded_subtrees: None,
    };

    builder
        .add_extension(&name_constraint_extension)
        .expect("Unable to add extension");

    let int_cert = builder.build_with_rng::<DerSignature>(&mut rng).unwrap();

    // let cert_der = int_cert.to_der().unwrap();
    println!("{:?}", int_cert);

    let cert_bytes = int_cert.tbs_certificate.to_der().unwrap();

    let byte_sig: &[u8] = int_cert.signature.as_bytes().unwrap();
    let cert_sig = DerSignature::try_from(byte_sig).unwrap();
    assert!(root_verifying_key.verify(&cert_bytes, &cert_sig).is_ok());

    // Intermediate:
    //   Basic Constraints: critical
    //     CA:TRUE
    //     pathlen:0  // indicates no subordinate CA's

    let (critical, basic_constraints) = int_cert
        .tbs_certificate
        .get::<BasicConstraints>()
        .expect("failed to get extensions")
        .expect("basic constraints not present");

    assert!(critical);

    eprintln!("{:?}", basic_constraints);

    assert!(basic_constraints.ca);
    assert_eq!(basic_constraints.path_len_constraint, Some(0));

    //   Key Usage: critical
    //     Digital Signature
    //     Certificate Sign
    //     CRL Sign

    let (critical, key_usage) = int_cert
        .tbs_certificate
        .get::<KeyUsage>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert!(critical);

    eprintln!("{:?}", key_usage);
    let expected_key_usages = KeyUsages::KeyCertSign | KeyUsages::CRLSign;
    assert_eq!(key_usage, expected_key_usages.into());

    //   "Name Constraints": https://datatracker.ietf.org/doc/html/rfc5280#section-4.2.1.10
    //      Only needed for "servers".

    let (critical, _name_constraints) = int_cert
        .tbs_certificate
        .get::<NameConstraints>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert!(critical);
    // Otherwise, it's there, trust me bro.

    //   Authority Key ID
    //     (Should be sha1 of the signer public key)
    let (_, authority_key_id) = int_cert
        .tbs_certificate
        .get::<AuthorityKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", authority_key_id);

    assert_eq!(
        authority_key_id.key_identifier.as_ref().unwrap(),
        root_subject_key_id.as_ref()
    );

    //   Subject Key ID
    let (_, int_subject_key_id) = int_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", int_subject_key_id);

    //   Validity
    assert_eq!(
        int_cert
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration(),
        validity.not_before.to_unix_duration()
    );
    assert_eq!(
        int_cert
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration(),
        validity.not_after.to_unix_duration()
    );

    //   Issuer == Subject of Authority Key
    assert_eq!(
        int_cert.tbs_certificate.issuer,
        root_ca_cert.tbs_certificate.subject,
    );

    assert_eq!(int_cert.tbs_certificate.subject, int_subject);

    //
    //   Serial Number
    println!(
        "{:?}",
        &int_cert.tbs_certificate.serial_number.as_bytes()[1..]
    );
    println!("{:?}", int_serial_uuid.as_bytes());
    let verify_serial =
        Uuid::from_slice(&int_cert.tbs_certificate.serial_number.as_bytes()[1..]).unwrap();

    assert_eq!(int_serial_uuid, verify_serial);

    //   CRL
    //   It's there. Trust me.

    (int_signing_key, int_cert)
}

pub(crate) fn build_test_csr(subject: &Name) -> (SigningKey, CertReq) {
    let mut rng = rand::thread_rng();

    let client_signing_key = SigningKey::random(&mut rng);
    let client_verifying_key = VerifyingKey::from(&client_signing_key);

    // Serialize with `::to_encoded_point()`
    // let int_pub_key =
    // SubjectPublicKeyInfoOwned::from_key(int_verifying_key).expect("get rsa pub key");

    let builder = RequestBuilder::new(subject.clone(), &client_signing_key)
        .expect("Create certificate request");

    let client_cert_req = builder.build_with_rng::<DerSignature>(&mut rng).unwrap();

    let client_cert_req_der = client_cert_req.to_der().unwrap();
    println!("{:?}", client_cert_req_der);

    // First, extract the public key from the cert and use it to self-verify

    // Need to check the algorithm ID in future.
    let spki = &client_cert_req.info.public_key;

    println!("--> {:?}", &spki);

    let extracted_public_key = VerifyingKey::from_public_key_der(
        // spki.subject_public_key.as_bytes().unwrap()
        spki.to_der().unwrap().as_slice(),
    )
    .expect("Unable to parse key bytes");
    assert_eq!(extracted_public_key, client_verifying_key);

    let req_bytes = client_cert_req.info.to_der().unwrap();

    let byte_sig: &[u8] = client_cert_req.signature.as_bytes().unwrap();
    let client_cert_req_sig = DerSignature::try_from(byte_sig).unwrap();
    assert!(extracted_public_key
        .verify(&req_bytes, &client_cert_req_sig)
        .is_ok());
    assert!(client_verifying_key
        .verify(&req_bytes, &client_cert_req_sig)
        .is_ok());

    (client_signing_key, client_cert_req)
}

pub(crate) fn test_ca_sign_client_csr(
    not_before: Time,
    not_after: Time,
    cert_req: &CertReq,
    ca_signing_key: &SigningKey,
    ca_cert: &CertificateInner,
) -> CertificateInner {
    let mut rng = rand::thread_rng();

    // The process of issuance at this point really is up to "what do we want to copy from the
    // csr and what don't we?".

    // ------------------------

    let client_serial_uuid = Uuid::new_v4();

    let mut serial_bytes: [u8; 17] = [0; 17];
    serial_bytes[0] = 0x01;
    {
        let update_bytes = &mut serial_bytes[1..];
        update_bytes.copy_from_slice(client_serial_uuid.as_bytes());
    }

    println!("{:?}", serial_bytes);
    let serial_number = SerialNumber::new(&serial_bytes).unwrap();

    let validity = Validity {
        not_before,
        not_after,
    };

    let profile = Profile::Leaf {
        issuer: ca_cert.tbs_certificate.subject.clone(),
        enable_key_agreement: false,
        enable_key_encipherment: true,
        include_subject_key_identifier: true,
    };

    let client_cert_subject = cert_req.info.subject.clone();

    let spki = &cert_req.info.public_key;

    println!("--> {:?}", &spki);

    let mut builder = CertificateBuilder::new(
        profile,
        serial_number,
        validity,
        client_cert_subject.clone(),
        spki.clone(),
        ca_signing_key,
    )
    .expect("Create certificate");

    let eku_extension = ExtendedKeyUsage(vec![const_oid::db::rfc5280::ID_KP_CLIENT_AUTH]);

    builder
        .add_extension(&eku_extension)
        .expect("Unable to add extension");

    let alt_name = Name::from_str("ENTRYUUID=cb98d3d3-efcc-4675-ad40-435f6280d41b").unwrap();

    let san = SubjectAltName(vec![GeneralName::DirectoryName(alt_name)]);

    builder
        .add_extension(&san)
        .expect("Unable to add extension");

    let client_cert = builder.build_with_rng::<DerSignature>(&mut rng).unwrap();

    // let client_cert_der = client_cert.to_der().unwrap();
    println!("{:?}", client_cert);

    // Client Leaf Cert
    //   Basic Constraints: critical
    //     CA:FALSE

    let (critical, basic_constraints) = client_cert
        .tbs_certificate
        .get::<BasicConstraints>()
        .expect("failed to get extensions")
        .expect("basic constraints not present");

    assert!(critical);
    eprintln!("{:?}", basic_constraints);

    assert!(!basic_constraints.ca);

    //   Key Usage: critical
    //     Digital Signature
    //     Non Repudiation
    //     Key Encipherment

    let (critical, key_usage) = client_cert
        .tbs_certificate
        .get::<KeyUsage>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert!(critical);

    eprintln!("{:?}", key_usage);
    let expected_key_usages =
        KeyUsages::DigitalSignature | KeyUsages::NonRepudiation | KeyUsages::KeyEncipherment;
    assert_eq!(key_usage, expected_key_usages.into());

    //   Extended Key Usage
    //     TLS Web Client Authentication

    let (_, key_usage) = client_cert
        .tbs_certificate
        .get::<ExtendedKeyUsage>()
        .expect("failed to get extensions")
        .expect("extended key usage not present");

    assert_eq!(key_usage.0, vec![const_oid::db::rfc5280::ID_KP_CLIENT_AUTH]);

    //   Authority Key ID
    //     (Should be sha256 of the signer public key)

    let (_, authority_key_id) = client_cert
        .tbs_certificate
        .get::<AuthorityKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", authority_key_id);

    //   Subject Key ID
    let (_, int_subject_key_id) = ca_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert_eq!(
        authority_key_id.key_identifier.as_ref().unwrap(),
        int_subject_key_id.as_ref()
    );

    //   Subject Key ID
    let (_, client_subject_key_id) = client_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", client_subject_key_id);

    //   Validity
    assert_eq!(
        client_cert
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration(),
        validity.not_before.to_unix_duration()
    );
    assert_eq!(
        client_cert
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration(),
        validity.not_after.to_unix_duration()
    );

    //   Issuer == Subject of Authority Key
    assert_eq!(
        client_cert.tbs_certificate.issuer,
        ca_cert.tbs_certificate.subject
    );

    //
    //  Must have
    //   Subject Alternative Name

    //   Serial Number
    println!(
        "{:?}",
        &client_cert.tbs_certificate.serial_number.as_bytes()[1..]
    );
    println!("{:?}", client_serial_uuid.as_bytes());
    let verify_serial =
        Uuid::from_slice(&client_cert.tbs_certificate.serial_number.as_bytes()[1..]).unwrap();

    assert_eq!(client_serial_uuid, verify_serial);
    //   Subject
    assert_eq!(client_cert_subject, client_cert.tbs_certificate.subject);

    client_cert
}

pub(crate) fn test_ca_sign_server_csr(
    not_before: Time,
    not_after: Time,
    cert_req: &CertReq,
    ca_signing_key: &SigningKey,
    ca_cert: &CertificateInner,
) -> CertificateInner {
    let mut rng = rand::thread_rng();

    // The process of issuance at this point really is up to "what do we want to copy from the
    // csr and what don't we?".

    // ------------------------

    let server_serial_uuid = Uuid::new_v4();

    let mut serial_bytes: [u8; 17] = [0; 17];
    serial_bytes[0] = 0x01;
    {
        let update_bytes = &mut serial_bytes[1..];
        update_bytes.copy_from_slice(server_serial_uuid.as_bytes());
    }

    println!("{:?}", serial_bytes);
    let serial_number = SerialNumber::new(&serial_bytes).unwrap();

    let validity = Validity {
        not_before,
        not_after,
    };

    let profile = Profile::Leaf {
        issuer: ca_cert.tbs_certificate.subject.clone(),
        enable_key_agreement: true,
        enable_key_encipherment: true,
        include_subject_key_identifier: true,
    };

    let server_cert_subject = cert_req.info.subject.clone();

    let spki = &cert_req.info.public_key;

    println!("--> {:?}", &spki);

    let mut builder = CertificateBuilder::new(
        profile,
        serial_number,
        validity,
        server_cert_subject.clone(),
        spki.clone(),
        ca_signing_key,
    )
    .expect("Create certificate");

    let eku_extension = ExtendedKeyUsage(vec![const_oid::db::rfc5280::ID_KP_SERVER_AUTH]);

    builder
        .add_extension(&eku_extension)
        .expect("Unable to add extension");

    let alt_name = Ia5String::new("localhost").unwrap();

    let san = SubjectAltName(vec![GeneralName::DnsName(alt_name)]);

    builder
        .add_extension(&san)
        .expect("Unable to add extension");

    let server_cert = builder.build_with_rng::<DerSignature>(&mut rng).unwrap();

    // let server_cert_der = server_cert.to_der().unwrap();
    println!("{:?}", server_cert);

    // VALIDATION NOW

    // Server Leaf Cert
    //   Basic Constraints: critical
    //     CA:FALSE

    let (critical, basic_constraints) = server_cert
        .tbs_certificate
        .get::<BasicConstraints>()
        .expect("failed to get extensions")
        .expect("basic constraints not present");

    assert!(critical);
    eprintln!("{:?}", basic_constraints);

    assert!(!basic_constraints.ca);

    //   Key Usage: critical
    //     Digital Signature,
    //     Non Repudiation,
    //     Key Encipherment,
    //     Key Agreement

    let (critical, key_usage) = server_cert
        .tbs_certificate
        .get::<KeyUsage>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert!(critical);

    eprintln!("{:?}", key_usage);
    let expected_key_usages = KeyUsages::DigitalSignature
        | KeyUsages::NonRepudiation
        | KeyUsages::KeyAgreement
        | KeyUsages::KeyEncipherment;
    assert_eq!(key_usage, expected_key_usages.into());

    //   Extended Key Usage
    //     TLS Web Server Authentication

    let (_, key_usage) = server_cert
        .tbs_certificate
        .get::<ExtendedKeyUsage>()
        .expect("failed to get extensions")
        .expect("extended key usage not present");

    assert_eq!(key_usage.0, vec![const_oid::db::rfc5280::ID_KP_SERVER_AUTH]);

    //   Authority Key ID
    //     (Should be sha256 of the signer public key)

    let (_, authority_key_id) = server_cert
        .tbs_certificate
        .get::<AuthorityKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", authority_key_id);

    //   Subject Key ID
    let (_, int_subject_key_id) = ca_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    assert_eq!(
        authority_key_id.key_identifier.as_ref().unwrap(),
        int_subject_key_id.as_ref()
    );

    //   Subject Key ID
    let (_, server_subject_key_id) = server_cert
        .tbs_certificate
        .get::<SubjectKeyIdentifier>()
        .expect("failed to get extensions")
        .expect("key usage not present");

    eprintln!("{:?}", server_subject_key_id);

    //   Validity
    assert_eq!(
        server_cert
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration(),
        validity.not_before.to_unix_duration()
    );
    assert_eq!(
        server_cert
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration(),
        validity.not_after.to_unix_duration()
    );

    //   Issuer == Subject of Authority Key
    assert_eq!(
        server_cert.tbs_certificate.issuer,
        ca_cert.tbs_certificate.subject
    );

    //
    //  Must have
    //   Subject Alternative Name

    //   Serial Number
    println!(
        "{:?}",
        &server_cert.tbs_certificate.serial_number.as_bytes()[1..]
    );
    println!("{:?}", server_serial_uuid.as_bytes());
    let verify_serial =
        Uuid::from_slice(&server_cert.tbs_certificate.serial_number.as_bytes()[1..]).unwrap();

    assert_eq!(server_serial_uuid, verify_serial);
    //   Subject
    assert_eq!(server_cert_subject, server_cert.tbs_certificate.subject);

    server_cert
}

#[test]
fn test_ca_build_process() {
    let now = SystemTime::now();
    let not_before = Time::try_from(now).unwrap();
    let not_after = Time::try_from(now + Duration::new(3600, 0)).unwrap();

    let (root_signing_key, root_ca_cert) = build_test_ca_root(not_before, not_after);

    // =========================================================================================

    let (int_signing_key, int_ca_cert) =
        build_test_ca_int(not_before, not_after, &root_signing_key, &root_ca_cert);

    // =========================================================================================

    let subject = Name::from_str("CN=multi pass").unwrap();

    let (_client_key, client_csr) = build_test_csr(&subject);

    let _client_cert = test_ca_sign_client_csr(
        not_before,
        not_after,
        &client_csr,
        &int_signing_key,
        &int_ca_cert,
    );

    // =========================================================================================

    let subject = Name::from_str("CN=localhost").unwrap();

    let (_server_key, server_csr) = build_test_csr(&subject);

    let _server_cert = test_ca_sign_server_csr(
        not_before,
        not_after,
        &server_csr,
        &int_signing_key,
        &int_ca_cert,
    );

    // Server Leaf Cert
    //   Basic Constraints: critical
    //     CA:FALSE
    //   Key Usage: critical
    //     Digital Signature,
    //     Non Repudiation,
    //     Key Encipherment,
    //     Key Agreement
    //   Extended Key Usage
    //     TLS Web Server Authentication
    //   Authority Key ID
    //     (Should be sha256 of the signer public key)
    //   Subject Key ID
    //   Validity
    //   Issuer == Subject of Authority Key
    //
    //  Must have
    //   Subject Alternative Name
    //     DNS: <hostname>
    //   Serial Number
    //   Subject
}
