// SPDX-License-Identifier: CC0-1.0

//! The sentence `CreateCertificateFromCsr` refuses a READABLE certification request whose
//! public key violates the operation's key-strength constraint with, over the wire.
//!
//! The rule under test: when the `certificateSigningRequest` a caller submits parses as a
//! PKCS#10 certification request but its public key is outside the set the operation
//! documents — "The CSR must include a public key that is either an RSA key with a length
//! of at least 2048 bits or an ECC key from NIST P-256, NIST P-384, or NIST P-521 curves"
//! (`smithy_query(query="doc",shape="CreateCertificateFromCsr")`) — AWS IoT answers HTTP
//! 400 `InvalidRequestException` with ONE fixed sentence naming the class of the violation,
//! which has no variable part: neither the member that carried the request, nor the key type
//! or key size submitted, nor the shortest admitted key length or the admitted curve names
//! appears in it. So every key outside that set earns the SAME message, whichever way it
//! falls outside, and that sentence is NOT the one an unreadable request earns.
//!
//! The table below spans what the rule generalises over — the key's TYPE (RSA, EC, and key
//! types the set names at all: Ed25519, X25519, DSA), its SIZE (RSA well under the
//! documented floor, and one byte under it), and its CURVE (named curves outside the three:
//! secp256k1, secp224r1, brainpoolP256r1) — and the admitted keys are driven too (RSA at
//! the documented floor and above it, an EC key on each of the three documented curves), so
//! what the rows pin is the refusal's wording and the bound it is taken at, not a handler
//! that refuses every request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MIN_CSR_RSA_KEY_BITS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;
use x509_cert::attr::AttributeTypeAndValue;
use x509_cert::der::asn1::{Any, BitString, SetOfVec};
use x509_cert::der::pem::LineEnding;
use x509_cert::der::{Decode, Encode, EncodePem, Tag as DerTag};
use x509_cert::name::{Name, RdnSequence, RelativeDistinguishedName};
use x509_cert::request::{CertReq, CertReqInfo, Version as CertReqVersion};
use x509_cert::spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};

/// The region the suite signs for.
const REGION: &str = "us-east-1";

/// The one sentence AWS IoT states this refusal in: the whole message is fixed text for the
/// class, so nothing of the request reaches it.
const CSR_VIOLATES_CONSTRAINTS: &str = "CSR violates constraints";

/// The sentence the OTHER refusal branch of this operation states — material that is no
/// certification request at all — which the key-strength refusal must not be confused with.
const INVALID_CSR_FORMAT: &str = "Invalid CSR format";

/// `rsaEncryption` (RFC 8017 appendix C) and `id-ecPublicKey` (RFC 5480 section 2.1.1): the
/// two key algorithms the documented set names.
const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `sha256WithRSAEncryption` (RFC 8017 appendix C) — the algorithm a crafted request names
/// where its signature goes.
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
/// The three curves the operation documents, each by the identifier RFC 5480 section
/// 2.1.1.1 gives it: `secp256r1` (NIST P-256), `secp384r1` (P-384), `secp521r1` (P-521).
const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const SECP521R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.35");
/// Named curves OUTSIDE those three: `secp256k1` (SEC 2 section 2.4.1), `secp224r1` (RFC
/// 5480 section 2.1.1.1) and `brainpoolP256r1` (RFC 5639 section 4.1).
const SECP256K1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");
const SECP224R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.33");
const BRAINPOOL_P256R1: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.36.3.3.2.8.1.1.7");
/// `id-Ed25519` and `id-X25519` (RFC 8410 section 3), and `id-dsa` (RFC 3279 section
/// 2.3.2): key types the documented set does not name at all.
const ED25519: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.101.112");
const X25519: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.101.110");
const DSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10040.4.1");
/// `id-at-commonName` and `id-at-organizationName` (RFC 5280 appendix A).
const COMMON_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
const ORGANIZATION: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.10");

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never
/// checked — the harness turns request authentication off, as an unauthenticated emulator
/// runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The error shape the answer names, as a client reads it: the `x-amzn-errortype`
    /// header up to the `:` that introduces the shape's documentation URI.
    fn code(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .map(|value| value.split(':').next().unwrap_or(value))
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default().to_string()
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }
}

/// The emulator under test: one engine with the IoT service mounted on it, reached the way
/// the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(value.to_string().into_bytes())
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(REGION))
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

    /// `CreateCertificateFromCsr`: `POST /certificates`, the request in the body.
    async fn create(&self, csr: &str) -> Reply {
        self.call(
            "POST",
            "/certificates?setAsActive=true",
            Some(&json!({"certificateSigningRequest": csr})),
        )
        .await
    }

    /// How many certificates the account holds, as `ListCertificates` reports them.
    async fn certificate_count(&self) -> usize {
        let listed = self.call("GET", "/certificates", None).await;
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
}

/// The `SubjectPublicKeyInfo` of an RSA key whose modulus is `bits` bits long, with the
/// public exponent 65537 (F4, RFC 8017 appendix C).
///
/// The key is CRAFTED rather than generated: `aws-lc-rs` declines to generate an RSA key
/// below the documented floor at all, and the key is read out of the request's own encoding
/// before any signature is, so a crafted modulus of a chosen length is what pins the bound.
fn rsa_public_key_of(bits: usize) -> SubjectPublicKeyInfoOwned {
    assert_eq!(bits % 8, 0, "a crafted modulus is whole bytes");
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

/// The `SubjectPublicKeyInfo` of a key of another algorithm altogether, carrying `bytes` of
/// key material and no algorithm parameters (as RFC 8410 section 3 encodes a Curve25519
/// key).
fn public_key_of_algorithm(algorithm: ObjectIdentifier, bytes: usize) -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: algorithm,
            parameters: None,
        },
        subject_public_key: BitString::from_bytes(&vec![0x3b_u8; bytes]).expect("whole bytes"),
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

/// The PEM PKCS#10 certification request for `common_name` over `public_key` — a readable
/// `CertificationRequest`, so what decides its fate is the key it carries.
fn certification_request(common_name: &str, public_key: SubjectPublicKeyInfoOwned) -> String {
    let info = CertReqInfo {
        version: CertReqVersion::V1,
        subject: distinguished_name(common_name),
        public_key,
        attributes: SetOfVec::new(),
    };
    CertReq {
        info,
        algorithm: AlgorithmIdentifierOwned {
            oid: SHA256_WITH_RSA,
            parameters: Some(Any::new(DerTag::Null, Vec::new()).expect("a NULL parameter")),
        },
        // No private half goes with a crafted key, so filler stands where the signature
        // goes: the request is still a well-formed `CertificationRequest`.
        signature: BitString::from_bytes(&[0x5a_u8; 64]).expect("whole bytes"),
    }
    .to_pem(LineEnding::LF)
    .expect("the certification request encodes as PEM")
}

/// The keys that fall OUTSIDE the documented set, each labelled by how it falls outside.
fn keys_outside_the_documented_set() -> Vec<(String, SubjectPublicKeyInfoOwned)> {
    vec![
        (
            "RSA 512, far under the documented floor".to_string(),
            rsa_public_key_of(512),
        ),
        (
            "RSA 1024, half the documented floor".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS / 2),
        ),
        (
            "RSA one byte under the documented floor".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS - 8),
        ),
        (
            "EC secp256k1, a curve outside the three".to_string(),
            ec_public_key_on(SECP256K1, 65),
        ),
        (
            "EC secp224r1, a shorter curve outside the three".to_string(),
            ec_public_key_on(SECP224R1, 57),
        ),
        (
            "EC brainpoolP256r1, a curve of another family".to_string(),
            ec_public_key_on(BRAINPOOL_P256R1, 65),
        ),
        (
            "Ed25519, a key type the set does not name".to_string(),
            public_key_of_algorithm(ED25519, 32),
        ),
        (
            "X25519, an agreement key rather than a signing key".to_string(),
            public_key_of_algorithm(X25519, 32),
        ),
        (
            "DSA, a key type the set does not name".to_string(),
            public_key_of_algorithm(DSA, 128),
        ),
    ]
}

/// The keys the documented set admits: RSA at the floor and above it, and an EC key on each
/// of the three curves it names.
fn keys_inside_the_documented_set() -> Vec<(String, SubjectPublicKeyInfoOwned)> {
    vec![
        (
            "RSA at exactly the documented floor".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS),
        ),
        (
            "RSA one byte over the documented floor".to_string(),
            rsa_public_key_of(MIN_CSR_RSA_KEY_BITS + 8),
        ),
        (
            "RSA 4096, well over the documented floor".to_string(),
            rsa_public_key_of(4096),
        ),
        (
            "EC NIST P-256".to_string(),
            ec_public_key_on(SECP256R1, 65),
        ),
        (
            "EC NIST P-384".to_string(),
            ec_public_key_on(SECP384R1, 97),
        ),
        (
            "EC NIST P-521".to_string(),
            ec_public_key_on(SECP521R1, 133),
        ),
    ]
}

/// The fragments the sentence must NOT carry: it recites neither the rule that was broken
/// nor anything of the request, so the member name, the floor, the key algorithms and the
/// curve names all stay out of it.
const FORBIDDEN_FRAGMENTS: [&str; 8] = [
    "certificateSigningRequest",
    "2048",
    "RSA",
    "ECC",
    "P-256",
    "P-384",
    "P-521",
    "NIST",
];

#[tokio::test]
async fn every_key_outside_the_documented_set_is_refused_with_one_fixed_sentence() {
    let emulator = Emulator::new();

    for (label, public_key) in keys_outside_the_documented_set() {
        let material = certification_request("sensor-outside", public_key);
        let refused = emulator.create(&material).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected status 400, got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("InvalidRequestException"),
            "case {label:?}: expected error code InvalidRequestException, got {refused:?}"
        );
        let message = refused.message();
        assert_eq!(
            message, CSR_VIOLATES_CONSTRAINTS,
            "case {label:?}: expected message {CSR_VIOLATES_CONSTRAINTS:?}, got {message:?}"
        );
        for fragment in FORBIDDEN_FRAGMENTS {
            assert!(
                !message.contains(fragment),
                "case {label:?}: the message must carry no variable part ({fragment:?}), got \
                 {message:?}"
            );
        }
        assert!(
            refused.text("certificateId").is_none(),
            "case {label:?}: a refusal reports no certificate, got {refused:?}"
        );
    }

    assert_eq!(
        emulator.certificate_count().await,
        0,
        "a refused request records nothing"
    );
}

#[tokio::test]
async fn a_readable_request_on_an_admitted_key_is_issued_a_certificate() {
    let emulator = Emulator::new();

    // The contrast rows: the refusal above is earned by the KEY, at the bound the operation
    // documents, and not by every request this operation is handed.
    for (index, (label, public_key)) in keys_inside_the_documented_set().into_iter().enumerate() {
        let material = certification_request(&format!("sensor-inside-{index}"), public_key);
        let issued = emulator.create(&material).await;
        assert_eq!(
            issued.status, 200,
            "case {label:?}: an admitted key must be issued a certificate, got {issued:?}"
        );
        assert!(
            issued.text("certificateId").is_some(),
            "case {label:?}: the issued certificate must be reported, got {issued:?}"
        );
    }
}

#[tokio::test]
async fn the_key_refusal_is_not_the_unreadable_request_refusal() {
    let emulator = Emulator::new();

    // Two distinct refusal classes of one operation: a client that branches on the sentence
    // to tell "the key is not admitted" from "this is no certification request" must see two
    // different sentences.
    let weak_key = certification_request(
        "sensor-weak",
        rsa_public_key_of(MIN_CSR_RSA_KEY_BITS / 2),
    );
    let on_the_key = emulator.create(&weak_key).await;
    let unreadable = emulator.create("not-a-certification-request").await;

    for (label, reply) in [("weak key", &on_the_key), ("unreadable", &unreadable)] {
        assert_eq!(
            reply.status, 400,
            "case {label:?}: expected status 400, got {reply:?}"
        );
        assert_eq!(
            reply.code(),
            Some("InvalidRequestException"),
            "case {label:?}: expected error code InvalidRequestException, got {reply:?}"
        );
    }
    assert_eq!(
        on_the_key.message(),
        CSR_VIOLATES_CONSTRAINTS,
        "case \"weak key\": expected message {CSR_VIOLATES_CONSTRAINTS:?}, got {on_the_key:?}"
    );
    assert_eq!(
        unreadable.message(),
        INVALID_CSR_FORMAT,
        "case \"unreadable\": expected message {INVALID_CSR_FORMAT:?}, got {unreadable:?}"
    );
}
