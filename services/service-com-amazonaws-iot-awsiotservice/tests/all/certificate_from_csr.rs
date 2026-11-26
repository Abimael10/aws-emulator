// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `CreateCertificateFromCsr` and the `DescribeCertificate`
//! reader that proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: an operation documented to issue a certificate OVER A CALLER'S
//! CERTIFICATE SIGNING REQUEST reads the subject and the public key it certifies out of
//! that request, signs the result with real cryptography, records the certificate under
//! the identity its own encoding determines, and refuses a request it cannot read or whose
//! key the operation does not admit instead of recording a certificate the caller never
//! asked for. Here that means `CreateCertificateFromCsr` answers an X.509 certificate
//! whose subject and `SubjectPublicKeyInfo` are the request's own, whose issuer is the
//! service's own authority (the caller keeps the private half, so the certificate is not
//! self-signed) with a `sha256WithRSAEncryption` signature that verifies under that one
//! authority key for every certificate it issues, whose `certificateId` is the SHA-256
//! digest of its own DER, and whose ARN is
//! `arn:aws:iot:<region>:<account>:cert/<certificateId>`; `DescribeCertificate` reports
//! that record back; reusing one request yields a DISTINCT certificate; and material that
//! is not a PKCS#10 certification request, or that carries a key outside the documented
//! set, is `InvalidRequestException` (400) and records nothing.
//!
//! The tables span what that rule generalises over rather than one request: the key the
//! request carries (RSA at two lengths, and an EC key on each of the three documented
//! curves), the subject it names, the three forms `setAsActive` takes on the wire (absent
//! — the modelled `default false` — explicitly `false`, explicitly `true`,
//! `smithy_query(query="operation",operation="CreateCertificateFromCsr")`), the region the
//! call is signed for, and several shapes of material that is not a request this operation
//! can issue over.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::signature::KeyPair as _;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    service_state, CERTIFICATE_MODE_DEFAULT, CERTIFICATE_STATUS_ACTIVE,
    CERTIFICATE_STATUS_INACTIVE, FIRST_CERTIFICATE_CUSTOMER_VERSION, MIN_CSR_RSA_KEY_BITS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;
use x509_cert::attr::AttributeTypeAndValue;
use x509_cert::der::asn1::{Any, BitString, SetOfVec};
use x509_cert::der::pem::LineEnding;
use x509_cert::der::{Decode, DecodePem, Encode, EncodePem, Tag as DerTag};
use x509_cert::name::{Name, RdnSequence, RelativeDistinguishedName};
use x509_cert::request::{CertReq, CertReqInfo, Version as CertReqVersion};
use x509_cert::spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};
use x509_cert::Certificate;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// `sha256WithRSAEncryption` (RFC 8017 appendix C) — the signature an RSA-keyed request is
/// signed under here, and the one every certificate this service issues carries.
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
/// `ecdsa-with-SHA256`, `ecdsa-with-SHA384` and `ecdsa-with-SHA512` (RFC 5758 section 3.2)
/// — the signatures an EC-keyed request on P-256, P-384 and P-521 is signed under.
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
const ECDSA_WITH_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
/// `rsaEncryption` and `id-ecPublicKey` — the two key algorithms a crafted key below names
/// (RFC 8017 appendix C, RFC 5480 section 2.1.1).
const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `id-Ed25519` (RFC 8410 section 3) — a key type the operation's documented set does not
/// name.
const ED25519: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.101.112");
/// `secp256k1` (SEC 2 section 2.4.1) — a named curve outside the three the operation
/// documents.
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");
/// `id-at-commonName` and `id-at-organizationName` (RFC 5280 appendix A).
const COMMON_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
const ORGANIZATION: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.10");

/// The length, in bytes, of a signature made by an RSA-2048 key: a PKCS #1 signature is
/// exactly as long as the key's modulus, so this is what the issuing authority's own
/// signature on every certificate it signs must measure.
const RSA_2048_SIGNATURE_BYTES: usize = 256;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// `region`: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }

    /// The error shape the answer names, as a client reads it: the `x-amzn-errortype`
    /// header up to the `:` that introduces the shape's documentation URI.
    fn code(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .map(|value| value.split(':').next().unwrap_or(value))
    }

    /// A member of the reported `CertificateDescription`.
    fn member(&self, name: &str) -> Option<&Value> {
        self.body
            .get("certificateDescription")
            .and_then(|description| description.get(name))
    }

    /// A string member of the reported `CertificateDescription`.
    fn member_str(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it. The engine is kept so a case can read the
/// issuing authority's PUBLIC key out of this service's own state — the key a client
/// verifies an issued certificate under, which no operation publishes.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path: &str, region: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(value.to_string().into_bytes())
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateCertificateFromCsr`: `POST /certificates`, with the request in the body and
    /// `setAsActive` bound as an `@httpQuery` parameter.
    async fn create(&self, csr: &str, set_as_active: Option<bool>, region: &str) -> Reply {
        let path = match set_as_active {
            None => "/certificates".to_string(),
            Some(value) => format!("/certificates?setAsActive={value}"),
        };
        self.call(
            "POST",
            &path,
            region,
            Some(&json!({"certificateSigningRequest": csr})),
        )
        .await
    }

    async fn describe(&self, certificate_id: &str, region: &str) -> Reply {
        self.call(
            "GET",
            &format!("/certificates/{certificate_id}"),
            region,
            None,
        )
        .await
    }

    /// How many certificates the account holds, as `ListCertificates` reports them.
    async fn certificate_count(&self) -> usize {
        let listed = self.call("GET", "/certificates", REGION, None).await;
        assert_eq!(
            listed.status, 200,
            "ListCertificates must succeed, got {listed:?}"
        );
        listed
            .body
            .get("certificates")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    }

    /// The PKCS#1 `RSAPublicKey` of the authority this service signs requested
    /// certificates with, or `None` while it has signed none.
    fn authority_verification_key(&self) -> Option<Vec<u8>> {
        let state = service_state(&*self.engine).expect("the IoT state store is attached");
        let der = state
            .lock()
            .issuing_certificate_authority
            .as_ref()
            .map(|authority| authority.verification_key_der())?;
        Some(
            SubjectPublicKeyInfoOwned::from_der(&der)
                .expect("the authority's verification key is a SubjectPublicKeyInfo")
                .subject_public_key
                .raw_bytes()
                .to_vec(),
        )
    }
}

/// A key a case builds its certification request over: the `SubjectPublicKeyInfo` the
/// request certifies, the algorithm identifier the request is signed under, and the
/// private half when the case holds one.
struct RequestKey {
    public_key: SubjectPublicKeyInfoOwned,
    signature_algorithm: AlgorithmIdentifierOwned,
    signer: Option<Signer>,
}

/// The private half of a case's key, as the two libraries sign with it.
enum Signer {
    /// An RSA key pair, PKCS#8 DER.
    Rsa(Vec<u8>),
    /// An ECDSA key pair over one of the NIST curves.
    Ecdsa(aws_lc_rs::signature::EcdsaKeyPair),
}

impl Signer {
    /// `message` signed by this key — a real signature, so a case's request is one a
    /// verifier accepts as the proof of possession a certification request is.
    fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            Self::Rsa(pkcs8) => {
                let pair = aws_lc_rs::signature::RsaKeyPair::from_pkcs8(pkcs8)
                    .expect("the RSA key pair parses");
                let mut signature = vec![0u8; pair.public_modulus_len()];
                pair.sign(
                    &aws_lc_rs::signature::RSA_PKCS1_SHA256,
                    &aws_lc_rs::rand::SystemRandom::new(),
                    message,
                    &mut signature,
                )
                .expect("the RSA signature is made");
                signature
            }
            Self::Ecdsa(pair) => pair
                .sign(&aws_lc_rs::rand::SystemRandom::new(), message)
                .expect("the ECDSA signature is made")
                .as_ref()
                .to_vec(),
        }
    }
}

/// An RSA key pair of `size` from `aws-lc-rs`, as a request key.
fn rsa_request_key(size: aws_lc_rs::rsa::KeySize) -> RequestKey {
    let private_key =
        aws_lc_rs::rsa::PrivateDecryptingKey::generate(size).expect("an RSA key pair");
    let spki = private_key
        .public_key()
        .as_der()
        .expect("the public key encodes")
        .as_ref()
        .to_vec();
    RequestKey {
        public_key: SubjectPublicKeyInfoOwned::from_der(&spki).expect("a SubjectPublicKeyInfo"),
        signature_algorithm: AlgorithmIdentifierOwned {
            oid: SHA256_WITH_RSA,
            // RSASSA-PKCS1-v1_5 takes an explicit NULL parameter (RFC 8017 appendix C).
            parameters: Some(Any::new(DerTag::Null, Vec::new()).expect("a NULL parameter")),
        },
        signer: Some(Signer::Rsa(
            private_key
                .as_der()
                .expect("the private key encodes")
                .as_ref()
                .to_vec(),
        )),
    }
}

/// An ECDSA key pair on `curve` from `aws-lc-rs`, signed under `signature_algorithm`.
fn ecdsa_request_key(
    curve: &'static aws_lc_rs::signature::EcdsaSigningAlgorithm,
    signature_algorithm: ObjectIdentifier,
) -> RequestKey {
    let pair = aws_lc_rs::signature::EcdsaKeyPair::generate(curve).expect("an ECDSA key pair");
    let spki = pair
        .public_key()
        .as_der()
        .expect("the public key encodes")
        .as_ref()
        .to_vec();
    RequestKey {
        public_key: SubjectPublicKeyInfoOwned::from_der(&spki).expect("a SubjectPublicKeyInfo"),
        signature_algorithm: AlgorithmIdentifierOwned {
            oid: signature_algorithm,
            // An ECDSA signature algorithm takes no parameters (RFC 5758 section 3.2).
            parameters: None,
        },
        signer: Some(Signer::Ecdsa(pair)),
    }
}

/// A request key this test CRAFTS rather than generates: the `SubjectPublicKeyInfo` is a
/// well-formed encoding of a key of the named type, and no private half goes with it.
///
/// It is how a case offers a key the operation must refuse: `aws-lc-rs` declines to
/// generate an RSA key shorter than the documented minimum at all, and the key TYPE is
/// read out of the request's own encoding before any signature is, so a request whose
/// signature is filler is refused for its key either way.
fn crafted_request_key(public_key: SubjectPublicKeyInfoOwned) -> RequestKey {
    RequestKey {
        public_key,
        signature_algorithm: AlgorithmIdentifierOwned {
            oid: SHA256_WITH_RSA,
            parameters: Some(Any::new(DerTag::Null, Vec::new()).expect("a NULL parameter")),
        },
        signer: None,
    }
}

/// The `SubjectPublicKeyInfo` of an RSA key whose modulus is `bits` bits long, with the
/// public exponent 65537 (F4, RFC 8017 appendix C).
fn rsa_public_key_of(bits: usize) -> SubjectPublicKeyInfoOwned {
    assert_eq!(bits % 8, 0, "case: a crafted modulus is whole bytes");
    let mut modulus = vec![0xab_u8; bits / 8];
    // The top bit set makes the modulus exactly `bits` long, and an odd last byte makes it
    // odd as a product of two primes is.
    modulus[0] = 0xc1;
    let last = modulus.len() - 1;
    modulus[last] = 0xa9;
    let key = pkcs1::RsaPublicKey {
        modulus: pkcs1::UintRef::new(&modulus).expect("a positive modulus"),
        public_exponent: pkcs1::UintRef::new(&[0x01, 0x00, 0x01]).expect("a positive exponent"),
    };
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: RSA_ENCRYPTION,
            parameters: Some(Any::new(DerTag::Null, Vec::new()).expect("a NULL parameter")),
        },
        subject_public_key: BitString::from_bytes(&key.to_der().expect("the RSA key encodes"))
            .expect("whole bytes"),
    }
}

/// The `SubjectPublicKeyInfo` of an EC key on `curve`, as an uncompressed point of
/// `point_bytes` bytes (RFC 5480 section 2.2).
fn ec_public_key_on(curve: ObjectIdentifier, point_bytes: usize) -> SubjectPublicKeyInfoOwned {
    let mut point = vec![0x7f_u8; point_bytes];
    point[0] = 0x04;
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: EC_PUBLIC_KEY,
            parameters: Some(
                Any::from_der(&curve.to_der().expect("the curve encodes"))
                    .expect("the curve is a parameter"),
            ),
        },
        subject_public_key: BitString::from_bytes(&point).expect("whole bytes"),
    }
}

/// The `SubjectPublicKeyInfo` of an Ed25519 key: 32 bytes and no parameters (RFC 8410
/// section 3).
fn ed25519_public_key() -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: ED25519,
            parameters: None,
        },
        subject_public_key: BitString::from_bytes(&[0x3b_u8; 32]).expect("whole bytes"),
    }
}

/// The X.500 distinguished name a case's request asks a certificate for.
fn distinguished_name(common_name: &str) -> Name {
    Name::from(RdnSequence(vec![
        relative_distinguished_name(ORGANIZATION, "A Device Fleet"),
        relative_distinguished_name(COMMON_NAME, common_name),
    ]))
}

fn relative_distinguished_name(
    attribute: ObjectIdentifier,
    value: &str,
) -> RelativeDistinguishedName {
    let pair = AttributeTypeAndValue {
        oid: attribute,
        value: Any::new(DerTag::Utf8String, value.as_bytes()).expect("a UTF8String"),
    };
    RelativeDistinguishedName::from(SetOfVec::try_from(vec![pair]).expect("one attribute"))
}

/// The PEM PKCS#10 certification request for `common_name` over `key` — the material a
/// case sends as `certificateSigningRequest`.
fn certification_request(common_name: &str, key: &RequestKey) -> String {
    let info = CertReqInfo {
        version: CertReqVersion::V1,
        subject: distinguished_name(common_name),
        public_key: key.public_key.clone(),
        attributes: SetOfVec::new(),
    };
    let signature = key.signer.as_ref().map_or_else(
        // A case that holds no private half sends filler where the signature goes: the
        // request is still a well-formed `CertificationRequest`.
        || vec![0x5a_u8; 64],
        |signer| signer.sign(&info.to_der().expect("the request info encodes")),
    );
    CertReq {
        info,
        algorithm: key.signature_algorithm.clone(),
        signature: BitString::from_bytes(&signature).expect("whole bytes"),
    }
    .to_pem(LineEnding::LF)
    .expect("the certification request encodes as PEM")
}

/// One accepted request: a label, the key it is built over, the subject it names, the
/// `setAsActive` query parameter it carries (or none), the region it is signed for, and the
/// certificate status that resolves to.
struct Case {
    label: &'static str,
    key: RequestKey,
    common_name: &'static str,
    set_as_active: Option<bool>,
    region: &'static str,
    status: &'static str,
}

/// The table every case list below is built from. It spans what the rule generalises over:
/// each key type the operation documents — "an RSA key with a length of at least 2048 bits
/// or an ECC key from NIST P-256, NIST P-384, or NIST P-521 curves"
/// (`smithy_query(query="doc",shape="CreateCertificateFromCsr")`) — at more than one RSA
/// length, several subjects, the three forms `setAsActive` takes on the wire, and the
/// region the call is signed for.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "RSA 2048, no setAsActive at all",
            key: rsa_request_key(aws_lc_rs::rsa::KeySize::Rsa2048),
            common_name: "sensor-01",
            set_as_active: None,
            region: REGION,
            status: CERTIFICATE_STATUS_INACTIVE,
        },
        Case {
            label: "RSA 3072, setAsActive=true",
            key: rsa_request_key(aws_lc_rs::rsa::KeySize::Rsa3072),
            common_name: "sensor-02",
            set_as_active: Some(true),
            region: REGION,
            status: CERTIFICATE_STATUS_ACTIVE,
        },
        Case {
            label: "EC P-256, setAsActive=false",
            key: ecdsa_request_key(
                &aws_lc_rs::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
                ECDSA_WITH_SHA256,
            ),
            common_name: "gateway-eu",
            set_as_active: Some(false),
            region: "eu-west-1",
            status: CERTIFICATE_STATUS_INACTIVE,
        },
        Case {
            label: "EC P-384, setAsActive=true in another region",
            key: ecdsa_request_key(
                &aws_lc_rs::signature::ECDSA_P384_SHA384_ASN1_SIGNING,
                ECDSA_WITH_SHA384,
            ),
            common_name: "gateway-ap",
            set_as_active: Some(true),
            region: "ap-southeast-2",
            status: CERTIFICATE_STATUS_ACTIVE,
        },
        Case {
            label: "EC P-521, no setAsActive at all",
            key: ecdsa_request_key(
                &aws_lc_rs::signature::ECDSA_P521_SHA512_ASN1_SIGNING,
                ECDSA_WITH_SHA512,
            ),
            common_name: "controller-521",
            set_as_active: None,
            region: REGION,
            status: CERTIFICATE_STATUS_INACTIVE,
        },
    ]
}

/// The ARN a certificate issued in `region` must carry.
fn expected_arn(region: &str, certificate_id: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// Epoch seconds now, for bounding a reported timestamp.
fn now_epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Parse `pem` as an X.509 certificate, independently of the encoder the service used.
fn parse_certificate(case: &str, pem: &str) -> Certificate {
    Certificate::from_pem(pem).unwrap_or_else(|why| {
        panic!("case {case:?}: the certificatePem must parse: {why} ({pem})")
    })
}

#[tokio::test]
async fn a_certificate_is_issued_over_the_key_and_subject_the_request_carries() {
    let emulator = Emulator::new();

    for case in cases() {
        let label = case.label;
        let csr = certification_request(case.common_name, &case.key);
        let issued = emulator.create(&csr, case.set_as_active, case.region).await;
        assert_eq!(
            issued.status, 200,
            "case {label:?}: CreateCertificateFromCsr must succeed, got {issued:?}"
        );

        let certificate_id = issued.text("certificateId").unwrap_or_else(|| {
            panic!("case {label:?}: the response must carry a certificateId, got {issued:?}")
        });
        let certificate_pem = issued.text("certificatePem").unwrap_or_else(|| {
            panic!("case {label:?}: the response must carry a certificatePem, got {issued:?}")
        });
        let certificate = parse_certificate(label, &certificate_pem);

        // The certificate certifies the SUBJECT and the PUBLIC KEY the request carried,
        // read out of the request rather than chosen by the service.
        let expected_subject = distinguished_name(case.common_name).to_string();
        let subject = certificate.tbs_certificate.subject.to_string();
        assert_eq!(
            subject, expected_subject,
            "case {label:?}: expected the request's subject {expected_subject}, got {subject}"
        );
        let certified_key = certificate
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .unwrap_or_else(|why| panic!("case {label:?}: the certified key re-encodes: {why}"));
        let requested_key = case
            .key
            .public_key
            .to_der()
            .unwrap_or_else(|why| panic!("case {label:?}: the requested key re-encodes: {why}"));
        assert_eq!(
            certified_key, requested_key,
            "case {label:?}: the certificate must certify the request's own public key"
        );

        // The id is the SHA-256 digest of the certificate's own DER, hex encoded, so it
        // satisfies the modelled `length 64..64, pattern ^(0x)?[a-fA-F0-9]+$` and names the
        // certificate that was actually issued.
        let der = certificate
            .to_der()
            .unwrap_or_else(|why| panic!("case {label:?}: the certificate re-encodes: {why}"));
        let digest = hex::encode(Sha256::digest(&der));
        assert_eq!(
            certificate_id, digest,
            "case {label:?}: expected the certificateId to be the certificate's SHA-256 digest \
             {digest}, got {certificate_id}"
        );
        assert_eq!(
            certificate_id.len(),
            64,
            "case {label:?}: expected a 64-character certificateId, got {certificate_id}"
        );
        assert!(
            certificate_id
                .chars()
                .all(|character| character.is_ascii_hexdigit()),
            "case {label:?}: expected a hex certificateId, got {certificate_id}"
        );
        assert_eq!(
            issued.text("certificateArn").as_deref(),
            Some(expected_arn(case.region, &certificate_id).as_str()),
            "case {label:?}: expected the certificate ARN for region {}, got {issued:?}",
            case.region
        );

        // The caller keeps the private half, so the certificate is signed by the service's
        // own authority and not by the key it certifies: the issuer is another name, and
        // nothing in the answer carries key material of the caller's.
        let issuer = certificate.tbs_certificate.issuer.to_string();
        assert_ne!(
            issuer, subject,
            "case {label:?}: a certificate issued over a request is not self-signed, got \
             issuer {issuer}"
        );
        let rendered = issued.body.to_string();
        assert!(
            !rendered.contains("PRIVATE KEY"),
            "case {label:?}: this operation issues no private key, got {issued:?}"
        );
    }
}

#[tokio::test]
async fn every_issued_certificate_verifies_under_the_one_issuing_authority() {
    let emulator = Emulator::new();

    // The signature on each certificate is a real `sha256WithRSAEncryption` signature by
    // the service's issuing authority, and ONE authority signs them all: the key that
    // verifies the first certificate verifies every later one.
    let mut issuers = Vec::new();
    for case in cases() {
        let label = case.label;
        let csr = certification_request(case.common_name, &case.key);
        let issued = emulator.create(&csr, case.set_as_active, case.region).await;
        assert_eq!(
            issued.status, 200,
            "case {label:?}: CreateCertificateFromCsr must succeed, got {issued:?}"
        );
        let certificate = parse_certificate(label, &issued.text("certificatePem").unwrap_or_default());

        let verification_key = emulator.authority_verification_key().unwrap_or_else(|| {
            panic!("case {label:?}: an issued certificate means an authority has signed it")
        });
        let tbs = certificate
            .tbs_certificate
            .to_der()
            .unwrap_or_else(|why| panic!("case {label:?}: the TBSCertificate re-encodes: {why}"));
        let signature = certificate
            .signature
            .as_bytes()
            .unwrap_or_else(|| panic!("case {label:?}: the signature is whole bytes"));
        assert_eq!(
            signature.len(),
            RSA_2048_SIGNATURE_BYTES,
            "case {label:?}: expected an RSA-2048 signature of {RSA_2048_SIGNATURE_BYTES} bytes, \
             got {}",
            signature.len()
        );
        let verifier = aws_lc_rs::signature::UnparsedPublicKey::new(
            &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
            &verification_key,
        );
        verifier.verify(&tbs, signature).unwrap_or_else(|why| {
            panic!(
                "case {label:?}: the certificate's signature must verify under the issuing \
                 authority: {why}"
            )
        });
        assert_eq!(
            certificate.signature_algorithm.oid,
            SHA256_WITH_RSA,
            "case {label:?}: expected a sha256WithRSAEncryption signature, got {:?}",
            certificate.signature_algorithm.oid
        );
        issuers.push(certificate.tbs_certificate.issuer.to_string());
    }

    let first = issuers.first().cloned().unwrap_or_default();
    for issuer in &issuers {
        assert_eq!(
            issuer, &first,
            "every certificate issued over a signing request carries one issuer: expected \
             {first}, got {issuer}"
        );
    }
}

#[tokio::test]
async fn describe_certificate_reports_the_certificate_the_request_produced() {
    let emulator = Emulator::new();

    for case in cases() {
        let label = case.label;
        let csr = certification_request(case.common_name, &case.key);
        let before = now_epoch_seconds();
        let issued = emulator.create(&csr, case.set_as_active, case.region).await;
        assert_eq!(
            issued.status, 200,
            "case {label:?}: CreateCertificateFromCsr must succeed, got {issued:?}"
        );
        let certificate_id = issued.text("certificateId").unwrap_or_else(|| {
            panic!("case {label:?}: the response must carry a certificateId, got {issued:?}")
        });

        let read = emulator.describe(&certificate_id, case.region).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: DescribeCertificate of the issued id must succeed, got {read:?}"
        );
        assert_eq!(
            read.member_str("certificateId"),
            Some(certificate_id.as_str()),
            "case {label:?}: the description must report the issued id, got {read:?}"
        );
        assert_eq!(
            read.member_str("certificateArn"),
            Some(expected_arn(case.region, &certificate_id).as_str()),
            "case {label:?}: the description must report the issued ARN, got {read:?}"
        );
        assert_eq!(
            read.member_str("certificatePem"),
            issued.text("certificatePem").as_deref(),
            "case {label:?}: the description must report the issued certificate, got {read:?}"
        );
        assert_eq!(
            read.member_str("status"),
            Some(case.status),
            "case {label:?}: expected status {:?}, got {read:?}",
            case.status
        );
        assert_eq!(
            read.member_str("ownedBy"),
            Some(ACCOUNT_ID),
            "case {label:?}: the description must report the owning account, got {read:?}"
        );
        assert_eq!(
            read.member_str("certificateMode"),
            Some(CERTIFICATE_MODE_DEFAULT),
            "case {label:?}: a certificate this service issued is in DEFAULT mode, got {read:?}"
        );
        assert_eq!(
            read.member("customerVersion").and_then(Value::as_i64),
            Some(i64::from(FIRST_CERTIFICATE_CUSTOMER_VERSION)),
            "case {label:?}: expected customer version {FIRST_CERTIFICATE_CUSTOMER_VERSION}, \
             got {read:?}"
        );
        assert!(
            read.member_str("generationId")
                .is_some_and(|id| !id.is_empty()),
            "case {label:?}: the description must report a generationId, got {read:?}"
        );

        // `creationDate` is the instant of the issue, and nothing has updated the
        // certificate since, so `lastModifiedDate` is that same instant.
        let created_at = read
            .member("creationDate")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                panic!("case {label:?}: the description reported no creationDate: {read:?}")
            });
        let after = now_epoch_seconds();
        assert!(
            (before - 1.0..=after + 1.0).contains(&created_at),
            "case {label:?}: creationDate {created_at} is outside [{before}, {after}], got {read:?}"
        );
        assert_eq!(
            read.member("lastModifiedDate").and_then(Value::as_f64),
            Some(created_at),
            "case {label:?}: a certificate nothing has updated was last modified when it was \
             issued ({created_at}), got {read:?}"
        );

        // The reported validity is the validity the certificate itself carries, to the
        // second.
        let certificate = parse_certificate(label, &issued.text("certificatePem").unwrap_or_default());
        let validity = read.member("validity").cloned().unwrap_or(Value::Null);
        for (member, expected) in [
            ("notBefore", certificate.tbs_certificate.validity.not_before),
            ("notAfter", certificate.tbs_certificate.validity.not_after),
        ] {
            let reported = validity
                .get(member)
                .and_then(Value::as_f64)
                .unwrap_or_else(|| {
                    panic!("case {label:?}: the description reported no validity.{member}: {read:?}")
                });
            let expected = expected.to_unix_duration().as_secs_f64();
            assert!(
                (reported - expected).abs() < 1.0,
                "case {label:?}: expected validity.{member} {expected}, got {reported}"
            );
        }
    }
}

#[tokio::test]
async fn reusing_one_signing_request_issues_a_distinct_certificate() {
    let emulator = Emulator::new();

    // "Reusing the same certificate signing request (CSR) results in a distinct
    // certificate" — and both certificates stay readable, over the same subject and key.
    for case in cases() {
        let label = case.label;
        let csr = certification_request(case.common_name, &case.key);
        let first = emulator.create(&csr, case.set_as_active, case.region).await;
        let second = emulator.create(&csr, case.set_as_active, case.region).await;
        for (which, reply) in [("first", &first), ("second", &second)] {
            assert_eq!(
                reply.status, 200,
                "case {label:?}: the {which} CreateCertificateFromCsr must succeed, got {reply:?}"
            );
        }
        let first_id = first.text("certificateId").unwrap_or_default();
        let second_id = second.text("certificateId").unwrap_or_default();
        assert_ne!(
            first_id, second_id,
            "case {label:?}: reusing one signing request must issue a distinct certificate, got \
             {first_id} twice"
        );
        assert_ne!(
            first.text("certificatePem"),
            second.text("certificatePem"),
            "case {label:?}: the second certificate must be its own material, got the first \
             again"
        );

        let expected_subject = distinguished_name(case.common_name).to_string();
        for (which, id) in [("first", &first_id), ("second", &second_id)] {
            let read = emulator.describe(id, case.region).await;
            assert_eq!(
                read.status, 200,
                "case {label:?}: the {which} certificate must stay readable, got {read:?}"
            );
            let subject = parse_certificate(label, read.member_str("certificatePem").unwrap_or(""))
                .tbs_certificate
                .subject
                .to_string();
            assert_eq!(
                subject, expected_subject,
                "case {label:?}: the {which} certificate must certify the request's subject \
                 {expected_subject}, got {subject}"
            );
        }
    }
}

/// The message AWS IoT refuses material it cannot read as a certification request with:
/// one fixed sentence with no part taken from the request.
fn unreadable_message() -> String {
    "Invalid CSR format".to_string()
}

/// The message it refuses a readable request whose public key the operation does not admit
/// with, worded from the rule the operation states.
fn unsupported_key_message() -> String {
    format!(
        "Invalid certificate signing request: the public key must be an RSA key of at least \
         {MIN_CSR_RSA_KEY_BITS} bits or an ECC key from the NIST P-256, NIST P-384 or NIST P-521 \
         curves"
    )
}

#[tokio::test]
async fn material_that_is_no_certification_request_is_refused_and_records_nothing() {
    let emulator = Emulator::new();

    // One certificate exists first, so the refusals below are about the material and not
    // about an empty store.
    let accepted = certification_request(
        "sensor-00",
        &rsa_request_key(aws_lc_rs::rsa::KeySize::Rsa2048),
    );
    let issued = emulator.create(&accepted, Some(true), REGION).await;
    assert_eq!(
        issued.status, 200,
        "the accepted request must succeed, got {issued:?}"
    );
    let held = emulator.certificate_count().await;

    // A certificate is not a certification request, so the PEM of the certificate just
    // issued is material of the right FORM and the wrong structure.
    let a_certificate = issued.text("certificatePem").unwrap_or_default();
    let truncated = {
        let mut lines: Vec<&str> = accepted.lines().collect();
        lines.truncate(2);
        lines.join("\n")
    };
    let unreadable: Vec<(String, String)> = vec![
        ("not a PEM block at all".to_string(), "csr".to_string()),
        (
            "whitespace only".to_string(),
            "   \n  \n".to_string(),
        ),
        (
            "a PEM certificate rather than a request".to_string(),
            a_certificate,
        ),
        ("a truncated PEM block".to_string(), truncated),
        (
            "a PEM block of another label".to_string(),
            "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n".to_string(),
        ),
    ];
    for (label, material) in unreadable {
        let refused = emulator.create(&material, Some(true), REGION).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: material that is no certification request must be refused 400, \
             got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("InvalidRequestException"),
            "case {label:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            unreadable_message(),
            "case {label:?}: expected the unreadable-request message, got {refused:?}"
        );
        assert!(
            refused.text("certificateId").is_none(),
            "case {label:?}: a refusal reports no certificate, got {refused:?}"
        );
    }

    assert_eq!(
        emulator.certificate_count().await,
        held,
        "a refused request records nothing: the account still holds {held} certificate(s)"
    );
}

#[tokio::test]
async fn a_key_outside_the_documented_set_is_refused_and_records_nothing() {
    let emulator = Emulator::new();

    let accepted = certification_request(
        "sensor-00",
        &rsa_request_key(aws_lc_rs::rsa::KeySize::Rsa2048),
    );
    assert_eq!(
        emulator.create(&accepted, Some(true), REGION).await.status,
        200,
        "the accepted request must succeed"
    );
    let held = emulator.certificate_count().await;

    // Each case is a well-formed certification request whose KEY is outside "an RSA key
    // with a length of at least 2048 bits or an ECC key from NIST P-256, NIST P-384, or
    // NIST P-521 curves": RSA under the documented minimum (at the bound below it and well
    // below it), an EC key on a curve outside the three, and a key of another type
    // altogether.
    let refused_keys: Vec<(String, SubjectPublicKeyInfoOwned)> = vec![
        (
            "RSA 1024".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS / 2),
        ),
        (
            "RSA one byte under the minimum".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS - 8),
        ),
        (
            "EC secp256k1".to_string(),
            ec_public_key_on(SECP256K1, 65),
        ),
        ("Ed25519".to_string(), ed25519_public_key()),
    ];
    for (label, public_key) in refused_keys {
        let material = certification_request("sensor-x", &crafted_request_key(public_key));
        let refused = emulator.create(&material, Some(true), REGION).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: a key outside the documented set must be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("InvalidRequestException"),
            "case {label:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            unsupported_key_message(),
            "case {label:?}: expected the unsupported-key message, got {refused:?}"
        );
        assert!(
            refused.text("certificatePem").is_none(),
            "case {label:?}: a refusal reports no certificate, got {refused:?}"
        );
    }

    // A key AT the documented minimum is admitted, so what the refusals above pin is the
    // bound and not the key type.
    let at_the_bound = certification_request(
        "sensor-at-the-bound",
        &rsa_request_key(aws_lc_rs::rsa::KeySize::Rsa2048),
    );
    let issued = emulator.create(&at_the_bound, Some(true), REGION).await;
    assert_eq!(
        issued.status, 200,
        "an RSA key of exactly {MIN_CSR_RSA_KEY_BITS} bits must be admitted, got {issued:?}"
    );
    assert_eq!(
        emulator.certificate_count().await,
        held + 1,
        "only the admitted request added a certificate"
    );
}
