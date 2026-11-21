// SPDX-License-Identifier: CC0-1.0

//! The SSH public keys an IAM user holds, end to end over the awsQuery wire:
//! `UploadSSHPublicKey` writes one, `ListSSHPublicKeys` reports what the account holds and
//! `DeleteSSHPublicKey` takes one away.
//!
//! The rules under test:
//!
//! 1. an upload takes the public key MATERIAL in either of the encodings AWS names — "the
//!    public key must be encoded in ssh-rsa format or PEM format. The minimum bit-length of
//!    the public key is 2048 bits" (`UploadSSHPublicKey$SSHPublicKeyBody`) — identifies it
//!    by a minted `SSHPublicKeyId`, fingerprints it as "the MD5 message digest of the SSH
//!    public key" (`SSHPublicKey$Fingerprint`) and holds it `Active`; and what it stored is
//!    what a later read reports. The two refusals the operation models are told apart by
//!    the READING of the material: a body no reader of either encoding can take an RSA
//!    public key out of is in neither of them, which is `UnrecognizedPublicKeyEncoding`,
//!    while a body that IS such a key but not one the upload takes is `InvalidPublicKey` —
//!    and each refusal carries one fixed sentence for its whole class.
//! 2. the list "returns information about the SSH public keys associated with the specified
//!    IAM user. If none exists, the operation returns an empty list" — so it reports exactly
//!    the keys that user holds, pages over them under `MaxItems`, and refuses a name the
//!    account holds no user under with `NoSuchEntity` (404).
//! 3. "deletes the specified SSH public key" (`DeleteSSHPublicKey`) — the key leaves the
//!    user's collection, so every later reader of it stops reporting the key and its
//!    material can be associated again, while a delete naming no key THAT user holds
//!    removes nothing and is refused with the operation's one modelled error,
//!    `NoSuchEntity` (404).
//! 4. "sets the status of an IAM user's SSH public key" (`UpdateSSHPublicKey`) — the named
//!    key's `Status` becomes the value the request carries and nothing else about the key
//!    moves, so every later read reports the new status beside the key the upload stored.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: which
//! encoding carries the key, how long the key is, whether the line carries a comment, which
//! way the material fails, how many keys a user holds, how many a page asks for, and which
//! of a user's keys a delete names. The
//! material itself is built by the crates that implement those encodings, so no row depends
//! on a blob written out by hand.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour of a key that was PERSISTED. The awsQuery harness is the one
//! `tests/all/user_lifecycle.rs` owns and the signed-caller fixture the one
//! `tests/all/identity_policy_enforcement.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    MAX_SSH_PUBLIC_KEYS_PER_USER, MIN_SSH_PUBLIC_KEY_BITS,
};

use crate::identity_policy_enforcement::{principal, statement};
// The read a deleted key must stop being served by is `tests/all/ssh_public_key_reads.rs`'s,
// which owns the rest of what a read reports.
use crate::ssh_public_key_reads::get;
use crate::user_lifecycle::{call, call_signed_by, element, elements, error_code, router};

/// The bounds AWS publishes for an uploaded key, written out here rather than read from the
/// implementation: "the minimum bit-length of the public key is 2048 bits"
/// (`UploadSSHPublicKey$SSHPublicKeyBody`) and the "SSH public keys per IAM user: 5" quota
/// of AWS's "IAM and STS quotas" page. The suite also pins that the service's own constants
/// ARE these values, so a row written against them is a row about the published bound.
pub(crate) const PUBLISHED_MINIMUM_BITS: usize = 2048;
const PUBLISHED_KEYS_PER_USER: usize = 5;

/// RULE: the bounds the rows below are written against are the ones AWS publishes.
#[test]
fn the_published_bounds_are_the_ones_this_service_takes() {
    assert_eq!(
        MIN_SSH_PUBLIC_KEY_BITS, PUBLISHED_MINIMUM_BITS,
        "expected the published minimum key length {PUBLISHED_MINIMUM_BITS}, got \
         {MIN_SSH_PUBLIC_KEY_BITS}"
    );
    assert_eq!(
        MAX_SSH_PUBLIC_KEYS_PER_USER, PUBLISHED_KEYS_PER_USER,
        "expected the published per-user quota {PUBLISHED_KEYS_PER_USER}, got \
         {MAX_SSH_PUBLIC_KEYS_PER_USER}"
    );
}

/// The public exponent every key below carries: 65537, the exponent `ssh-keygen` writes.
const EXPONENT: [u8; 3] = [0x01, 0x00, 0x01];

/// A modulus of `bits` bits, fixed for a given `seed`.
///
/// The top bit is set so the integer really is that long and the last byte is odd, which is
/// what an RSA modulus is; nothing here depends on the integer being a product of primes,
/// because what the upload reads off the material is its STRUCTURE and its length.
pub(crate) fn modulus(bits: usize, seed: u8) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..bits / 8)
        .map(|at| seed.wrapping_mul(31).wrapping_add(at as u8) | 0x11)
        .collect();
    if let Some(first) = bytes.first_mut() {
        *first |= 0x80;
    }
    if let Some(last) = bytes.last_mut() {
        *last |= 0x01;
    }
    bytes
}

/// The one-line OpenSSH public key for `modulus`, as `ssh-keygen` writes one: the key type,
/// the base64 key blob and the comment, written by the crate that implements the format.
pub(crate) fn openssh_line(modulus: &[u8], comment: &str) -> String {
    let rsa = ::ssh_key::public::RsaPublicKey {
        e: ::ssh_key::Mpint::from_positive_bytes(&EXPONENT).expect("a positive exponent"),
        n: ::ssh_key::Mpint::from_positive_bytes(modulus).expect("a positive modulus"),
    };
    ::ssh_key::PublicKey::new(
        ::ssh_key::public::KeyData::Rsa(rsa),
        comment.to_string(),
    )
    .to_openssh()
    .expect("an OpenSSH public key line")
}

/// The PKCS#1 `RSAPublicKey` body for `modulus`, which both PEM forms below carry.
fn pkcs1_body(modulus: &[u8]) -> Vec<u8> {
    use ::pkcs1::der::Encode as _;

    ::pkcs1::RsaPublicKey {
        modulus: ::pkcs1::der::asn1::UintRef::new(modulus).expect("a positive modulus"),
        public_exponent: ::pkcs1::der::asn1::UintRef::new(&EXPONENT).expect("an exponent"),
    }
    .to_der()
    .expect("a PKCS#1 RSA public key")
}

/// The X.509 `SubjectPublicKeyInfo` PEM for `modulus` — what `ssh-keygen -e -m PKCS8`
/// writes, the PEM form of a public key.
pub(crate) fn spki_pem(modulus: &[u8]) -> String {
    use ::pkcs1::der::EncodePem as _;

    let body = pkcs1_body(modulus);
    ::pkcs8::spki::SubjectPublicKeyInfo {
        algorithm: ::pkcs8::spki::AlgorithmIdentifier {
            oid: ::pkcs1::ALGORITHM_OID,
            parameters: Some(::pkcs1::der::asn1::AnyRef::NULL),
        },
        subject_public_key: ::pkcs1::der::asn1::BitStringRef::new(0, &body)
            .expect("a bit string"),
    }
    .to_pem(::pkcs1::der::pem::LineEnding::LF)
    .expect("a SubjectPublicKeyInfo PEM")
}

/// The PKCS#1 `RSAPublicKey` PEM for `modulus` — what `ssh-keygen -e -m PEM` writes.
pub(crate) fn pkcs1_pem(modulus: &[u8]) -> String {
    use ::pkcs1::der::EncodePem as _;

    ::pkcs1::RsaPublicKey {
        modulus: ::pkcs1::der::asn1::UintRef::new(modulus).expect("a positive modulus"),
        public_exponent: ::pkcs1::der::asn1::UintRef::new(&EXPONENT).expect("an exponent"),
    }
    .to_pem(::pkcs1::der::pem::LineEnding::LF)
    .expect("a PKCS#1 RSA public key PEM")
}

/// One row of the accepted-material table: the body an upload carries, under the encoding
/// and key length it is written in.
struct Accepted {
    case: &'static str,
    body: String,
}

/// Rows spanning both encodings the upload names, the published minimum key length and one
/// above it, a line with a comment and one without, and the two PEM labels a public key is
/// written under.
fn accepted() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "an ssh-rsa line at the published minimum length",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 1), ""),
        },
        Accepted {
            case: "an ssh-rsa line carrying a comment",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 2), "developer@workstation"),
        },
        Accepted {
            case: "an ssh-rsa line twice the minimum length",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS * 2, 3), ""),
        },
        Accepted {
            case: "a SubjectPublicKeyInfo PEM",
            body: spki_pem(&modulus(PUBLISHED_MINIMUM_BITS, 4)),
        },
        Accepted {
            case: "a PKCS#1 RSA public key PEM",
            body: pkcs1_pem(&modulus(PUBLISHED_MINIMUM_BITS, 5)),
        },
        Accepted {
            case: "a SubjectPublicKeyInfo PEM twice the minimum length",
            body: spki_pem(&modulus(PUBLISHED_MINIMUM_BITS * 2, 6)),
        },
    ]
}

/// RULE 1 and RULE 2 over every accepted encoding: the upload reports the stored key, and
/// the list reports exactly what the upload stored.
#[tokio::test]
async fn an_uploaded_key_is_reported_by_the_list_that_holds_it() {
    let router = router();

    for (row, case) in accepted().into_iter().enumerate() {
        let user_name = format!("keyholder{row}");
        create_user(&router, case.case, &user_name).await;

        let (status, body) = upload(&router, &user_name, &case.body).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the upload to be served, got {status}: {body}",
            case.case
        );
        let ssh_public_key_id = element(&body, "SSHPublicKeyId");
        let fingerprint = element(&body, "Fingerprint");
        assert!(
            ssh_public_key_id.starts_with("APKA") && ssh_public_key_id.len() >= 20,
            "case {:?}: expected a minted public key id, got {ssh_public_key_id:?}",
            case.case
        );
        assert_eq!(
            element(&body, "Status"),
            "Active",
            "case {:?}: expected an uploaded key to be Active, got {body}",
            case.case
        );
        assert_eq!(
            element(&body, "UserName"),
            user_name,
            "case {:?}: expected the key to name its user, got {body}",
            case.case
        );
        assert_eq!(
            element(&body, "SSHPublicKeyBody"),
            case.body,
            "case {:?}: expected the body to be reported as it arrived, got {body}",
            case.case
        );
        assert_fingerprint(case.case, &fingerprint);

        let (status, listed) = list(&router, &[("UserName", &user_name)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the list to be served, got {status}: {listed}",
            case.case
        );
        assert_eq!(
            elements(&listed, "SSHPublicKeyId"),
            vec![ssh_public_key_id.clone()],
            "case {:?}: expected the list to report the stored key, got {listed}",
            case.case
        );
        assert_eq!(
            elements(&listed, "Status"),
            vec!["Active".to_string()],
            "case {:?}: expected the stored status, got {listed}",
            case.case
        );
        assert_eq!(
            element(&listed, "UploadDate"),
            element(&body, "UploadDate"),
            "case {:?}: expected the stored upload date, got {listed}",
            case.case
        );
        assert_eq!(
            element(&listed, "IsTruncated"),
            "false",
            "case {:?}: one key fits one page, got {listed}",
            case.case
        );
    }
}

/// RULE 1's invariant: the fingerprint is a function of the KEY and of nothing else, so one
/// key fingerprints alike however it was encoded and two different keys fingerprint
/// differently.
#[tokio::test]
async fn one_key_fingerprints_alike_in_every_encoding() {
    let router = router();
    let first = modulus(PUBLISHED_MINIMUM_BITS, 11);
    let second = modulus(PUBLISHED_MINIMUM_BITS, 12);

    // The SAME key, written three ways, and a different key beside it.
    let rows: &[(&str, String, bool)] = &[
        ("the key as an ssh-rsa line", openssh_line(&first, ""), true),
        (
            "the same key as an ssh-rsa line with a comment",
            openssh_line(&first, "a comment changes no key"),
            true,
        ),
        (
            "the same key as a SubjectPublicKeyInfo PEM",
            spki_pem(&first),
            true,
        ),
        (
            "the same key as a PKCS#1 PEM",
            pkcs1_pem(&first),
            true,
        ),
        ("another key entirely", openssh_line(&second, ""), false),
    ];

    let mut expected: Option<String> = None;
    for (row, (case, body, same_key)) in rows.iter().enumerate() {
        // One user per row: a repeat of the same key on one user is a duplicate, which the
        // suite below is about.
        let user_name = format!("fingerprinted{row}");
        create_user(&router, case, &user_name).await;
        let (status, uploaded) = upload(&router, &user_name, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the upload to be served, got {status}: {uploaded}"
        );
        let fingerprint = element(&uploaded, "Fingerprint");
        assert_fingerprint(case, &fingerprint);
        match (&expected, same_key) {
            (Some(first), true) => assert_eq!(
                &fingerprint, first,
                "case {case:?}: one key must fingerprint alike in every encoding, got \
                 {fingerprint:?} against {first:?}"
            ),
            (Some(first), false) => assert_ne!(
                &fingerprint, first,
                "case {case:?}: a different key must fingerprint differently, got \
                 {fingerprint:?}"
            ),
            (None, _) => expected = Some(fingerprint),
        }
    }
}

/// One row of the refused-material table: a body, and the awsQuery code, status and
/// `Message` the refusal carries.
struct Refused {
    case: &'static str,
    body: String,
    code: &'static str,
    status: u16,
    message: &'static str,
}

/// Rows spanning the two ways material fails, which the READING of the material tells
/// apart rather than the look of the text: a body no reader of either encoding can take a
/// public key out of is in neither encoding (prose, a bare blob, an OpenSSH line whose blob
/// or key type is not one, a PEM document that holds no public key), while a body that IS
/// such a key but one the upload will not take is refused for the key — here a key under
/// the published minimum length, in each encoding and at each of two lengths below the
/// bound.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            case: "text in no public-key encoding at all",
            body: "this is not a public key".to_string(),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            case: "a base64 blob with no key type in front of it",
            body: "AAAAB3NzaC1yc2EAAAADAQAB".to_string(),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            case: "an ssh-rsa line whose blob cannot be read",
            body: "ssh-rsa not-a-base64-blob".to_string(),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            case: "an OpenSSH line whose key type is not ssh-rsa",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 23), "")
                .replacen("ssh-rsa", "ssh-dss", 1),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            case: "a PEM structure that is no public key",
            body: "-----BEGIN CERTIFICATE-----\nAAAAB3Nz\n-----END CERTIFICATE-----\n"
                .to_string(),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            // A readable PEM document whose LABEL says it holds something other than a
            // public key: the framing is PEM, and no public key comes out of it.
            case: "a PEM document under a label that is no public key",
            body: spki_pem(&modulus(PUBLISHED_MINIMUM_BITS, 24))
                .replace("PUBLIC KEY", "CERTIFICATE"),
            code: "UnrecognizedPublicKeyEncoding",
            status: 400,
            message: UNRECOGNIZED_ENCODING_MESSAGE,
        },
        Refused {
            case: "an ssh-rsa key half the published minimum",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS - 1024, 21), ""),
            code: "InvalidPublicKey",
            status: 400,
            message: INVALID_PUBLIC_KEY_MESSAGE,
        },
        Refused {
            case: "an ssh-rsa key one byte under the published minimum",
            body: openssh_line(&modulus(PUBLISHED_MINIMUM_BITS - 8, 25), ""),
            code: "InvalidPublicKey",
            status: 400,
            message: INVALID_PUBLIC_KEY_MESSAGE,
        },
        Refused {
            case: "a PEM key under the published minimum",
            body: spki_pem(&modulus(PUBLISHED_MINIMUM_BITS - 1024, 22)),
            code: "InvalidPublicKey",
            status: 400,
            message: INVALID_PUBLIC_KEY_MESSAGE,
        },
        Refused {
            case: "a PKCS#1 PEM key one byte under the published minimum",
            body: pkcs1_pem(&modulus(PUBLISHED_MINIMUM_BITS - 8, 26)),
            code: "InvalidPublicKey",
            status: 400,
            message: INVALID_PUBLIC_KEY_MESSAGE,
        },
    ]
}

/// RULE 1's refusals over every row: the status, the code and the sentence say WHICH way
/// the material failed, and nothing is stored for the user.
#[tokio::test]
async fn material_outside_the_published_forms_is_refused_and_stores_nothing() {
    let router = router();
    let user_name = "refusing";
    create_user(&router, "refused material", user_name).await;

    for case in refused() {
        let (status, body) = upload(&router, user_name, &case.body).await;
        assert_eq!(
            status, case.status,
            "case {:?}: expected HTTP {}, got {status}: {body}",
            case.case, case.status
        );
        assert_eq!(
            error_code(&body),
            case.code,
            "case {:?}: expected the modelled awsQuery code {}, got {body}",
            case.case,
            case.code
        );
        assert_eq!(
            element(&body, "Message"),
            case.message,
            "case {:?}: expected the sentence {:?}, got {body}",
            case.case,
            case.message
        );

        let (status, listed) = list(&router, &[("UserName", user_name)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: the list must still be served, got {status}: {listed}",
            case.case
        );
        assert!(
            elements(&listed, "SSHPublicKeyId").is_empty(),
            "case {:?}: a refused upload must store nothing, got {listed}",
            case.case
        );
        assert_eq!(
            element(&listed, "IsTruncated"),
            "false",
            "case {:?}: an empty list is not truncated, got {listed}",
            case.case
        );
    }
}

/// RULE 1: one key is associated with the named user ONCE — a repeat upload of the same key
/// is refused whichever encoding it arrives in — while the same key is a fresh association
/// for another user.
#[tokio::test]
async fn a_key_the_user_already_holds_is_refused_once_per_user() {
    let router = router();
    let material = modulus(PUBLISHED_MINIMUM_BITS, 31);
    let first = openssh_line(&material, "");
    create_user(&router, "duplicates", "holder").await;
    create_user(&router, "duplicates", "other").await;

    let (status, body) = upload(&router, "holder", &first).await;
    assert_eq!(
        status, 200,
        "the first upload must be served, got {status}: {body}"
    );

    // The same key, in each encoding it can arrive in: sameness is of the key, not of the
    // text that carried it.
    for (case, body) in [
        ("the same line again", first.clone()),
        (
            "the same key with a comment on it",
            openssh_line(&material, "another name for one key"),
        ),
        ("the same key as a PEM", spki_pem(&material)),
    ] {
        let (status, refused) = upload(&router, "holder", &body).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "DuplicateSSHPublicKey".to_string()),
            "case {case:?}: expected the repeat to be refused, got {status}: {refused}"
        );
    }

    let (status, listed) = list(&router, &[("UserName", "holder")]).await;
    assert_eq!(status, 200, "the list must be served, got {status}: {listed}");
    assert_eq!(
        elements(&listed, "SSHPublicKeyId").len(),
        1,
        "the refused repeats must store nothing, got {listed}"
    );

    // Another user: the key is not held by that user, so the association is made.
    let (status, body) = upload(&router, "other", &first).await;
    assert_eq!(
        status, 200,
        "the same key must associate with another user, got {status}: {body}"
    );

    // And once THAT user holds it, a repeat for THEM is the duplicate too — the rule is
    // per user, so it does not depend on which user was asked first.
    for (case, body) in [
        ("the second holder's own repeat", first.clone()),
        ("the second holder's repeat as a PEM", spki_pem(&material)),
    ] {
        let (status, refused) = upload(&router, "other", &body).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "DuplicateSSHPublicKey".to_string()),
            "case {case:?}: expected the repeat to be refused, got {status}: {refused}"
        );
    }
    for holder in ["holder", "other"] {
        let (status, listed) = list(&router, &[("UserName", holder)]).await;
        assert_eq!(
            status, 200,
            "case {holder:?}: the list must be served, got {status}: {listed}"
        );
        assert_eq!(
            elements(&listed, "SSHPublicKeyId").len(),
            1,
            "case {holder:?}: one key must be held exactly once, got {listed}"
        );
    }
}

/// RULE 1: what one user may hold is bounded, so an upload past the quota is refused and
/// stores nothing.
#[tokio::test]
async fn the_quota_bounds_what_one_user_holds() {
    let router = router();
    create_user(&router, "quota", "quota-bound").await;

    for at in 0..PUBLISHED_KEYS_PER_USER {
        let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 41 + at as u8), "");
        let (status, uploaded) = upload(&router, "quota-bound", &body).await;
        assert_eq!(
            status, 200,
            "key {at} of the quota must be served, got {status}: {uploaded}"
        );
    }

    let over = openssh_line(
        &modulus(PUBLISHED_MINIMUM_BITS, 41 + PUBLISHED_KEYS_PER_USER as u8),
        "",
    );
    let (status, refused) = upload(&router, "quota-bound", &over).await;
    assert_eq!(
        (status, error_code(&refused)),
        (409, "LimitExceeded".to_string()),
        "expected the key past the quota to be refused, got {status}: {refused}"
    );

    let (status, listed) = list(&router, &[("UserName", "quota-bound")]).await;
    assert_eq!(status, 200, "the list must be served, got {status}: {listed}");
    assert_eq!(
        elements(&listed, "SSHPublicKeyId").len(),
        PUBLISHED_KEYS_PER_USER,
        "expected the quota's worth of keys to be held, got {listed}"
    );
}

/// RULE 2: a name the account holds no user under names no user whose keys could be
/// reported or written, which is the one client error both operations model.
#[tokio::test]
async fn a_name_the_account_holds_no_user_under_is_refused() {
    let router = router();
    create_user(&router, "absent", "present").await;

    for absent in ["absent", "Present2", "no_such_user"] {
        let (status, listed) = list(&router, &[("UserName", absent)]).await;
        assert_eq!(
            (status, error_code(&listed)),
            (404, "NoSuchEntity".to_string()),
            "case {absent:?}: expected the list to be refused, got {status}: {listed}"
        );
        let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 51), "");
        let (status, uploaded) = upload(&router, absent, &body).await;
        assert_eq!(
            (status, error_code(&uploaded)),
            (404, "NoSuchEntity".to_string()),
            "case {absent:?}: expected the upload to be refused, got {status}: {uploaded}"
        );
    }

    // A user that exists and holds nothing: "if none exists, the operation returns an empty
    // list", which is not a refusal.
    let (status, listed) = list(&router, &[("UserName", "present")]).await;
    assert_eq!(
        status, 200,
        "a held user with no keys must be served, got {status}: {listed}"
    );
    assert!(
        elements(&listed, "SSHPublicKeyId").is_empty(),
        "expected an empty list for a user holding no keys, got {listed}"
    );
}

/// RULE 2: the list pages over the user's keys under `MaxItems`, and the marker it hands
/// out resumes strictly after the last key of the page it closed — while a token this
/// service cannot have issued is refused rather than read as the start of the results.
#[tokio::test]
async fn the_list_pages_over_the_keys_one_user_holds() {
    let router = router();
    create_user(&router, "paged", "paged").await;
    let held = 5;
    for at in 0..held {
        let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 61 + at as u8), "");
        let (status, uploaded) = upload(&router, "paged", &body).await;
        assert_eq!(
            status, 200,
            "key {at} must be stored for the paging rows, got {status}: {uploaded}"
        );
    }

    for page_size in [1usize, 2, 3, held] {
        let max_items = page_size.to_string();
        let mut seen: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut params: Vec<(&str, &str)> =
                vec![("UserName", "paged"), ("MaxItems", &max_items)];
            if let Some(token) = marker.as_deref() {
                params.push(("Marker", token));
            }
            let (status, body) = list(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {page_size}: expected the page to be served, got {status}: {body}"
            );
            let page = elements(&body, "SSHPublicKeyId");
            assert!(
                page.len() <= page_size,
                "case {page_size}: a page must not exceed MaxItems, got {page:?}"
            );
            seen.extend(page);
            if element(&body, "IsTruncated") == "true" {
                marker = Some(element(&body, "Marker"));
            } else {
                break;
            }
        }
        assert_eq!(
            seen.len(),
            held,
            "case {page_size}: paging must report every held key exactly once, got {seen:?}"
        );
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            held,
            "case {page_size}: paging must not repeat a key, got {seen:?}"
        );
    }

    for token in ["not-a-marker-iam-ever-issued", "0", "users:::"] {
        let (status, body) = list(&router, &[("UserName", "paged"), ("Marker", token)]).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "ValidationError".to_string()),
            "case {token:?}: expected a token this service cannot have issued to be \
             refused, got {status}: {body}"
        );
    }
}

/// RULE: IAM authorizes its own API, so both operations are decided against the signing
/// principal's identity-based policies before anything is read or written.
#[tokio::test]
async fn both_operations_are_decided_against_the_callers_policies() {
    let router = router();
    let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 71), "");

    // (the action granted, the call made, whether it is then served)
    let rows: &[(&str, &str, bool)] = &[
        ("iam:ListSSHPublicKeys", "ListSSHPublicKeys", true),
        ("iam:UploadSSHPublicKey", "UploadSSHPublicKey", true),
        ("iam:GetUser", "ListSSHPublicKeys", false),
        ("iam:GetUser", "UploadSSHPublicKey", false),
    ];

    for (row, (granted, operation, served)) in rows.iter().enumerate() {
        let user_name = format!("gated{row}");
        let access_key_id = principal(
            &router,
            operation,
            &user_name,
            &[("grant".to_string(), statement("Allow", granted, "*"))],
        )
        .await;
        let mut params: Vec<(&str, &str)> = vec![("UserName", &user_name)];
        if *operation == "UploadSSHPublicKey" {
            params.push(("SSHPublicKeyBody", &body));
        }
        let (status, answered) =
            call_signed_by(&router, Some(&access_key_id), operation, &params).await;
        if *served {
            assert_eq!(
                status, 200,
                "case {operation} under {granted}: expected the grant to serve the call, \
                 got {status}: {answered}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered)),
                (403, "AccessDenied".to_string()),
                "case {operation} under {granted}: expected the call to be refused, got \
                 {status}: {answered}"
            );
        }
    }
}

/// Assert `fingerprint` is the MD5 digest form `publicKeyFingerprintType` admits
/// (`^[:\w]+$`): sixteen lowercase hex pairs, separated by colons.
pub(crate) fn assert_fingerprint(case: &str, fingerprint: &str) {
    let pairs: Vec<&str> = fingerprint.split(':').collect();
    assert_eq!(
        pairs.len(),
        16,
        "case {case:?}: expected the sixteen pairs of an MD5 digest, got {fingerprint:?}"
    );
    assert!(
        pairs.iter().all(|pair| pair.len() == 2
            && pair
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "case {case:?}: expected lowercase hex pairs, got {fingerprint:?}"
    );
}

/// One `UploadSSHPublicKey` call.
pub(crate) async fn upload(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_body: &str,
) -> (u16, String) {
    call(
        router,
        "UploadSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyBody", ssh_public_key_body),
        ],
    )
    .await
}

/// One `ListSSHPublicKeys` call.
async fn list(router: &MultiServiceRouter, params: &[(&str, &str)]) -> (u16, String) {
    call(router, "ListSSHPublicKeys", params).await
}

/// One `DeleteSSHPublicKey` call.
async fn delete(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_id: &str,
) -> (u16, String) {
    call(
        router,
        "DeleteSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyId", ssh_public_key_id),
        ],
    )
    .await
}

/// One `UpdateSSHPublicKey` call.
async fn update_status(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_id: &str,
    status: &str,
) -> (u16, String) {
    call(
        router,
        "UpdateSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyId", ssh_public_key_id),
            ("Status", status),
        ],
    )
    .await
}

/// RULE 4: an update moves the named key's status to the value the request carries, and
/// NOTHING else about the key — so every later read of it reports the new status beside the
/// key the upload stored, and a key the user does not hold is refused.
///
/// The rows differ along what the rule generalises over: which value of `statusType` the
/// update names (every value the enum declares, including the status the key already
/// carries), and which reader the new status is read back through.
#[tokio::test]
async fn an_update_moves_the_named_keys_status_and_nothing_else() {
    let router = router();
    let user_name = "statusholder";
    create_user(&router, "the status rows", user_name).await;
    let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 81), "");
    let (status, uploaded) = upload(&router, user_name, &body).await;
    assert_eq!(
        status, 200,
        "the fixture upload must be served, got {status}: {uploaded}"
    );
    let ssh_public_key_id = element(&uploaded, "SSHPublicKeyId");
    let fingerprint = element(&uploaded, "Fingerprint");
    let upload_date = element(&uploaded, "UploadDate");
    assert_eq!(
        element(&uploaded, "Status"),
        "Active",
        "an uploaded key is Active, got {uploaded}"
    );

    // Every value `statusType` declares (`smithy_query(query="shape", shape="statusType")`),
    // the one the key already carries among them, and then back again: the move is the
    // documented "active or inactive" direction in either direction.
    for named in ["Inactive", "Active", "Expired", "Expired", "Active"] {
        let (status, answered) = update_status(&router, user_name, &ssh_public_key_id, named).await;
        assert_eq!(
            status, 200,
            "case {named:?}: expected the update to be served, got {status}: {answered}"
        );

        let (status, read) = get(&router, user_name, &ssh_public_key_id, "SSH").await;
        assert_eq!(
            status, 200,
            "case {named:?}: the read must be served, got {status}: {read}"
        );
        assert_eq!(
            element(&read, "Status"),
            named,
            "case {named:?}: expected the read to report the status just set, got {read}"
        );
        // Nothing else about the key moved with it.
        assert_eq!(
            (
                element(&read, "Fingerprint"),
                element(&read, "SSHPublicKeyBody"),
                element(&read, "UploadDate")
            ),
            (fingerprint.clone(), body.clone(), upload_date.clone()),
            "case {named:?}: an update must move the status alone, got {read}"
        );
        // And the listing, which reads the same record, agrees with the read.
        let (status, listed) = list(&router, &[("UserName", user_name)]).await;
        assert_eq!(
            status, 200,
            "case {named:?}: the list must be served, got {status}: {listed}"
        );
        assert_eq!(
            element(&listed, "Status"),
            named,
            "case {named:?}: expected the listing to report the same status, got {listed}"
        );
    }

    // A key the user does not hold is the operation's modelled absence, and an id a delete
    // took away is one of those.
    let (status, answered) = delete(&router, user_name, &ssh_public_key_id).await;
    assert_eq!(
        status, 200,
        "the fixture delete must be served, got {status}: {answered}"
    );
    for (case, named_user, id) in [
        ("an id a delete took away", user_name, ssh_public_key_id.clone()),
        (
            "an id the account never minted",
            user_name,
            "APKANEVERMINTEDEXAMPLE".to_string(),
        ),
        (
            "a user the account does not hold",
            "nosuchstatususer",
            ssh_public_key_id.clone(),
        ),
    ] {
        let (status, refused) = update_status(&router, named_user, &id, "Inactive").await;
        assert_eq!(
            (status, error_code(&refused)),
            (404, "NoSuchEntity".to_string()),
            "case {case:?}: expected the modelled absence refusal, got {status}: {refused}"
        );
    }
}

/// The whole `Message` AWS refuses material in neither of the encodings a public key is
/// written in with — `UnrecognizedPublicKeyEncodingException`'s own sentence, spelled out
/// here rather than read from the service so a change of the wording fails this suite
/// instead of following it. It interpolates nothing: the sentence names neither the material
/// nor the encodings the upload takes.
const UNRECOGNIZED_ENCODING_MESSAGE: &str = "Invalid public key uploaded.";

/// The whole `Message` AWS refuses a body that carries a public key it will not take with —
/// `InvalidPublicKeyException`'s own sentence, spelled out here for the same reason, and
/// interpolating nothing either: it names neither the body nor what is wrong with the key.
const INVALID_PUBLIC_KEY_MESSAGE: &str = "Invalid body for public key";

/// RULE 1's refusal SENTENCE for an unrecognized encoding: every body in neither encoding is
/// refused with that one fixed sentence, which carries no part of the request.
///
/// The rows differ along what the rule generalises over: the FORM of the body (prose, a bare
/// base64 blob, a JSON document, an XML document, a key fingerprint, PEM-like framing that is
/// not PEM) and its SIZE (one character, and a body thousands of characters long) — the
/// sentence is a property of the refusal class, so no row's own text appears in it.
#[tokio::test]
async fn an_unrecognized_encoding_is_refused_with_one_sentence() {
    let router = router();
    let user_name = "encodingrefused";
    create_user(&router, "the unrecognized-encoding rows", user_name).await;

    for (case, body) in [
        ("prose in no public-key encoding", "this is not a public key".to_string()),
        (
            "a bare base64 blob with no key type in front of it",
            "AAAAB3NzaC1yc2EAAAADAQAB".to_string(),
        ),
        ("a single character", "x".to_string()),
        (
            "a JSON document",
            r#"{"publicKey":"AAAAB3NzaC1yc2EAAAADAQAB"}"#.to_string(),
        ),
        (
            "an XML document",
            "<publicKey>AAAAB3NzaC1yc2EAAAADAQAB</publicKey>".to_string(),
        ),
        (
            "a colon-separated key fingerprint",
            "ee:4e:1a:8f:17:aa:00:fa:9e:e4:7d:d2:69:a4:13:c1".to_string(),
        ),
        (
            "PEM-like framing that is not PEM",
            "BEGIN PUBLIC KEY AAAAB3NzaC1yc2EAAAADAQAB END PUBLIC KEY".to_string(),
        ),
        // `publicKeyMaterialType` is len 1..16384, so a body well inside that bound is
        // still read as material and still refused by its encoding alone.
        ("a body four thousand characters long", "z".repeat(4_000)),
    ] {
        let (status, refused) = upload(&router, user_name, &body).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "UnrecognizedPublicKeyEncoding".to_string()),
            "case {case:?}: expected the modelled encoding refusal, got {status}: {refused}"
        );
        let message = element(&refused, "Message");
        assert_eq!(
            message, UNRECOGNIZED_ENCODING_MESSAGE,
            "case {case:?}: expected {UNRECOGNIZED_ENCODING_MESSAGE:?}, got {message:?}"
        );
    }
}

/// RULE 3: a delete takes the key away from the user that held it — every later reader of
/// that user's keys stops reporting it, the user's OTHER keys are untouched, and the
/// material becomes uploadable again because the user no longer holds it.
///
/// The rows differ along what the rule generalises over: which encoding carried the deleted
/// key in, how long the key is, which position among the user's keys it occupies, and the
/// case the request spells the user's name in.
#[tokio::test]
async fn a_deleted_key_leaves_the_users_collection() {
    let router = router();
    let user_name = "keyholder";
    create_user(&router, "the deletion rows", user_name).await;

    // Three keys of one user, each carried in by a different encoding and of a different
    // length, so which one is deleted is a property of the id and not of the material.
    let rows: Vec<(&str, Vec<u8>, String)> = vec![
        (
            "an ssh-rsa line carrying the shortest key AWS takes",
            modulus(PUBLISHED_MINIMUM_BITS, 61),
            String::new(),
        ),
        (
            "a SubjectPublicKeyInfo PEM carrying a 3072-bit key",
            modulus(3072, 62),
            String::new(),
        ),
        (
            "a PKCS#1 PEM carrying a 4096-bit key",
            modulus(4096, 63),
            String::new(),
        ),
    ];
    let bodies: Vec<String> = vec![
        openssh_line(&rows[0].1, "a comment names no key"),
        spki_pem(&rows[1].1),
        pkcs1_pem(&rows[2].1),
    ];

    let mut held: Vec<(&str, String, String)> = Vec::new();
    for (index, (case, _, _)) in rows.iter().enumerate() {
        let (status, uploaded) = upload(&router, user_name, &bodies[index]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture upload must be served, got {status}: {uploaded}"
        );
        held.push((
            case,
            element(&uploaded, "SSHPublicKeyId"),
            bodies[index].clone(),
        ));
    }

    // The MIDDLE key first, and under the user's name in another case: IAM names "are not
    // distinguished by case", so the delete reaches the same user.
    let (case, deleted_id, deleted_body) = held.remove(1);
    let (status, answered) = delete(&router, &user_name.to_uppercase(), &deleted_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be served, got {status}: {answered}"
    );
    assert!(
        answered.contains("DeleteSSHPublicKeyResponse"),
        "case {case:?}: expected the modelled response envelope, got {answered}"
    );

    // Every reader of that user's keys has stopped reporting it, and the keys it did not
    // name are still there.
    let (status, listed) = list(&router, &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the list must be served, got {status}: {listed}"
    );
    let listed_ids = elements(&listed, "SSHPublicKeyId");
    assert!(
        !listed_ids.contains(&deleted_id),
        "case {case:?}: the deleted key must not be listed, got {listed}"
    );
    assert_eq!(
        listed_ids.len(),
        held.len(),
        "case {case:?}: the user's other keys must stay listed, got {listed}"
    );
    let (status, read) = get(&router, user_name, &deleted_id, "SSH").await;
    assert_eq!(
        (status, error_code(&read)),
        (404, "NoSuchEntity".to_string()),
        "case {case:?}: expected the read of a deleted key to be refused, got {status}: {read}"
    );
    for (other, id, _) in &held {
        let (status, read) = get(&router, user_name, id, "SSH").await;
        assert_eq!(
            status, 200,
            "case {other:?}: an untouched key must still be read, got {status}: {read}"
        );
    }

    // The user no longer holds that key, so the SAME material is a fresh association
    // rather than a duplicate — which is what makes a rotation loop converge.
    let (status, reuploaded) = upload(&router, user_name, &deleted_body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the material must be uploadable again, got {status}: {reuploaded}"
    );

    // And deleting every key the user holds leaves the list empty rather than draining
    // partway: "if none exists, the operation returns an empty list".
    for id in elements(&list(&router, &[("UserName", user_name)]).await.1, "SSHPublicKeyId") {
        let (status, answered) = delete(&router, user_name, &id).await;
        assert_eq!(
            status, 200,
            "case {id:?}: expected the delete to be served, got {status}: {answered}"
        );
    }
    let (status, listed) = list(&router, &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "the list must be served, got {status}: {listed}"
    );
    assert!(
        elements(&listed, "SSHPublicKeyId").is_empty(),
        "a user whose every key was deleted holds none, got {listed}"
    );
}

/// RULE 3's refusal: a delete that names no key THAT user holds removes nothing and is
/// refused with the one modelled error, whether the id was never minted, belongs to another
/// user or was already deleted — and a user the account holds none of is refused the same
/// way.
#[tokio::test]
async fn a_delete_of_a_key_the_user_does_not_hold_is_refused() {
    let router = router();
    create_user(&router, "the refused deletions", "deleter").await;
    create_user(&router, "the refused deletions", "bystander").await;

    let (status, mine) = upload(
        &router,
        "deleter",
        &openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 71), ""),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture upload must be served, got {status}: {mine}"
    );
    let my_id = element(&mine, "SSHPublicKeyId");

    let (status, theirs) = upload(
        &router,
        "bystander",
        &openssh_line(&modulus(3072, 72), ""),
    )
    .await;
    assert_eq!(
        status, 200,
        "the bystander's upload must be served, got {status}: {theirs}"
    );
    let their_id = element(&theirs, "SSHPublicKeyId");

    // One key of the caller's own, deleted once, so the third row below is an id this
    // account DID mint and this user no longer holds.
    let (status, answered) = delete(&router, "deleter", &my_id).await;
    assert_eq!(
        status, 200,
        "the first delete must be served, got {status}: {answered}"
    );

    // `publicKeyIdType` is len 20..128, pattern `^[\w]+$`, so every id below is one the
    // member's own constraints admit and the refusal is about what the account holds.
    for (case, user_name, ssh_public_key_id) in [
        (
            "an id the account never minted",
            "deleter",
            "APKANEVERMINTEDEXAMPLE".to_string(),
        ),
        ("an id another user holds", "deleter", their_id.clone()),
        ("an id already deleted", "deleter", my_id.clone()),
        (
            "an id under a user the account does not hold",
            "nosuchuser",
            their_id.clone(),
        ),
    ] {
        let (status, refused) = delete(&router, user_name, &ssh_public_key_id).await;
        assert_eq!(
            (status, error_code(&refused)),
            (404, "NoSuchEntity".to_string()),
            "case {case:?}: expected the modelled absence refusal, got {status}: {refused}"
        );
        assert!(
            !element(&refused, "Message").is_empty(),
            "case {case:?}: the refusal must name what was not found, got {refused}"
        );
    }

    // Nothing of the bystander's was removed by any of those calls.
    let (status, listed) = list(&router, &[("UserName", "bystander")]).await;
    assert_eq!(status, 200, "the list must be served, got {status}: {listed}");
    assert_eq!(
        elements(&listed, "SSHPublicKeyId"),
        vec![their_id],
        "a refused delete must remove nothing, got {listed}"
    );
}

/// Create the user `user_name`, which every row needs to exist before it uploads.
pub(crate) async fn create_user(router: &MultiServiceRouter, case: &str, user_name: &str) {
    let (status, body) = call(router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateUser must succeed, got {status}: {body}"
    );
}
