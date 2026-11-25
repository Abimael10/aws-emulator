// SPDX-License-Identifier: CC0-1.0

//! How a Lightsail response reports an SSH public key — the `publicKeyBase64`
//! of `CreateKeyPair` and of `DownloadDefaultKeyPair` — driven over the wire.
//!
//! Every case here goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: Lightsail_20161128.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router, so the modelled-constraint walk, the
//! handlers and the awsJson error envelope are all exercised rather than
//! bypassed.
//!
//! The rule under test: a member documented as "a base64-encoded public key of
//! the `ssh-rsa` type" carries base64 text alone — one base64 token that decodes
//! to the key's own `ssh-rsa` encoding, with no algorithm label, no space and no
//! surrounding whitespace — so a caller that decodes the member gets the RFC
//! 4253 public key blob. The key it carries is the one the account holds: that
//! key's DER public key is what the stored record's `fingerprint` is the MD5
//! digest of.
//!
//! The rows differ along what the rule generalises over: WHICH call reports the
//! key (a create of a pair the request names, a download of the account's
//! default pair), the Region the call is made in — including the one a request
//! carrying no credential scope is served in — and the name the reported pair is
//! held under.

use serde_json::{json, Value};
use service_com_amazonaws_lightsail_lightsail_20161128::state::{
    DEFAULT_REGION, DEFAULT_SSH_KEY_NAME,
};

use crate::instance_lifecycle::Emulator;
// The published fingerprint rule and the DER its digest is taken over are the
// create suite's own, so every suite checks ONE rule rather than a rendering of
// it per suite.
use crate::key_pair_create::{is_md5_colon_hex, public_key_der};

/// The algorithm name the modelled output fixes for the public half: "A
/// base64-encoded public key of the `ssh-rsa` type"
/// (`CreateKeyPairResult$publicKeyBase64`, and the same wording on
/// `DownloadDefaultKeyPairResult$publicKeyBase64`):
/// `smithy_query(query="shape",shape="CreateKeyPairResult")`.
///
/// `pub(crate)` so a sibling suite names the algorithm from one place.
pub(crate) const SSH_RSA: &str = "ssh-rsa";

/// Which call reports the public key of a row, and the name the pair it reports
/// is held under.
enum Reported {
    /// A create of the key pair the request names.
    Created,
    /// A download of the account's default key pair, which the first download
    /// mints.
    DefaultDownloaded,
}

/// One call that reports an SSH public key.
struct Row {
    case: &'static str,
    reported: Reported,
    /// The name the reported pair is held under — the name a create asks for,
    /// and [`DEFAULT_SSH_KEY_NAME`] for the account's default pair.
    key_pair_name: &'static str,
    /// The Region of the call's credential scope, or `None` for a request that
    /// carries none, which is served in [`DEFAULT_REGION`].
    region: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a create with a hyphenated name, no credential scope",
        reported: Reported::Created,
        key_pair_name: "key-pair-encoding",
        region: None,
    },
    Row {
        case: "a create with underscores and digits, us-west-2",
        reported: Reported::Created,
        key_pair_name: "kp_2026_encoding",
        region: Some("us-west-2"),
    },
    Row {
        case: "a create with the shortest admitted name, eu-central-1",
        reported: Reported::Created,
        key_pair_name: "K7",
        region: Some("eu-central-1"),
    },
    Row {
        case: "a download of the default pair, no credential scope",
        reported: Reported::DefaultDownloaded,
        key_pair_name: DEFAULT_SSH_KEY_NAME,
        region: None,
    },
    Row {
        case: "a download of the default pair, ap-northeast-1",
        reported: Reported::DefaultDownloaded,
        key_pair_name: DEFAULT_SSH_KEY_NAME,
        region: Some("ap-northeast-1"),
    },
];

impl Row {
    /// The Region this row's call is served in — its credential scope's, or the
    /// Region a request carrying none is served in.
    fn region(&self) -> &str {
        self.region.unwrap_or(DEFAULT_REGION)
    }

    /// Make this row's call, answering the response body.
    async fn report(&self, emulator: &Emulator) -> Value {
        let (operation, body) = match self.reported {
            Reported::Created => (
                "CreateKeyPair",
                json!({"keyPairName": self.key_pair_name}),
            ),
            Reported::DefaultDownloaded => ("DownloadDefaultKeyPair", json!({})),
        };
        let (status, parsed) = emulator.call_in(self.region, operation, body).await;
        assert_eq!(
            status, 200,
            "case {}: expected {operation} to be served, got {parsed:?}",
            self.case
        );
        parsed
    }
}

/// Assert `value` is a reported SSH public key in the form the rule requires —
/// base64 text alone, decoding to the key's own `ssh-rsa` encoding, with no
/// algorithm label, no space and no surrounding whitespace — and answer the key
/// it carries.
///
/// `pub(crate)` so the create and download suites check the reported half the
/// same way rather than each stating the rendering itself.
pub(crate) fn assert_reported_ssh_public_key(case: &str, value: &str) -> ::ssh_key::PublicKey {
    assert!(
        !value.is_empty(),
        "case {case}: expected an {SSH_RSA} public key, got an empty value"
    );
    // A single base64 token: the algorithm label and the space that would
    // separate it belong to an `authorized_keys` LINE a caller composes, not to
    // this member.
    assert_eq!(
        value.split_whitespace().count(),
        1,
        "case {case}: expected a single base64 token, got {value:?}"
    );
    assert!(
        !value.starts_with(SSH_RSA),
        "case {case}: expected no algorithm label on the value, got {value:?}"
    );
    // No surrounding or trailing whitespace either: the value is the encoded key,
    // not a file's contents.
    assert_eq!(
        value.trim(),
        value,
        "case {case}: expected no surrounding whitespace, got {value:?}"
    );
    assert_base64_ssh_public_key(case, value)
}

/// Assert `body` is the base64 body of an `ssh-rsa` public key — base64 text
/// alone, decoding to a blob that names that algorithm inside itself — and
/// answer the key it describes.
///
/// `pub(crate)` so a suite that holds a key's BODY (the import suite, which
/// submits one) checks it the same way.
pub(crate) fn assert_base64_ssh_public_key(case: &str, body: &str) -> ::ssh_key::PublicKey {
    use ::base64::Engine;

    let value = body;
    assert!(
        !value.is_empty(),
        "case {case}: expected a base64 public key, got an empty value"
    );
    // Base64 text alone: the alphabet of RFC 4648 section 4 and its padding, so
    // any whitespace in the body is a divergence.
    let offending: Vec<char> = value
        .chars()
        .filter(|c| !(c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '='))
        .collect();
    assert!(
        offending.is_empty(),
        "case {case}: expected base64 text alone, got {offending:?} in {value:?}"
    );
    let decoded = ::base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap_or_else(|error| {
            panic!("case {case}: expected a base64 value, got {value:?}: {error}")
        });
    // An SSH public key's own encoding opens with its algorithm name as a
    // length-prefixed string (RFC 4253 section 6.6), so the blob says what kind
    // of key it is without the text prefix an OpenSSH line carries.
    let mut named = u32::try_from(SSH_RSA.len())
        .expect("the algorithm name is short")
        .to_be_bytes()
        .to_vec();
    named.extend_from_slice(SSH_RSA.as_bytes());
    assert!(
        decoded.starts_with(&named),
        "case {case}: expected the decoded blob to name the {SSH_RSA} algorithm, got {:?}",
        &decoded[..named.len().min(decoded.len())]
    );
    // Read back as a key by the crate that implements the SSH key formats, so
    // this asserts a real key of the documented type rather than a string
    // shaped like one: the reported value IS the body of that key's OpenSSH
    // line, so the line is the algorithm name and this value.
    let key = ::ssh_key::PublicKey::from_openssh(&format!("{SSH_RSA} {value}")).unwrap_or_else(
        |error| panic!("case {case}: expected an {SSH_RSA} public key, got {value:?}: {error}"),
    );
    assert_eq!(
        key.algorithm().as_str(),
        SSH_RSA,
        "case {case}: expected the {SSH_RSA} algorithm, got {value:?}"
    );
    key
}

/// The account's record of the key pair `name`, read back through `GetKeyPair`.
///
/// `pub(crate)` so a sibling suite that checks the reported public half against
/// the record it belongs to reads that record the same way.
pub(crate) async fn held_key_pair(
    emulator: &Emulator,
    case: &str,
    region: Option<&str>,
    name: &str,
) -> Value {
    let (status, body) = emulator
        .call_in(region, "GetKeyPair", json!({"keyPairName": name}))
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected the held key pair {name} to be read back, got {body:?}"
    );
    body["keyPair"].clone()
}

/// RULE: every call that reports an SSH public key reports it as base64 text
/// alone — the base64 of the key's own `ssh-rsa` encoding, with no algorithm
/// label — and the key that value carries is the one the account holds, whose
/// DER public key the stored record's `fingerprint` is the MD5 digest of.
#[tokio::test]
async fn a_reported_public_key_is_an_ssh_rsa_public_key() {
    use ::md5::Digest;

    for row in ROWS {
        let case = row.case;
        // One emulator per row: a row is the FIRST call of its kind on a fresh
        // account, which is what mints the pair it reports.
        let emulator = Emulator::new();
        let body = row.report(&emulator).await;

        let reported = body["publicKeyBase64"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected publicKeyBase64, got {body:?}"));
        let key = assert_reported_ssh_public_key(case, reported);

        // The value is usable as it stands: this service's own reader of a
        // submitted public key takes it, so a pair this account minted can be
        // imported again exactly as it was reported.
        let (import_status, imported) = emulator
            .call_in(
                row.region,
                "ImportKeyPair",
                json!({
                    "keyPairName": format!("{}-reimported", row.key_pair_name),
                    "publicKeyBase64": reported,
                }),
            )
            .await;
        assert_eq!(
            import_status, 200,
            "case {case}: expected the reported value to be importable as it stands, got \
             {imported:?}"
        );

        // The key the value carries is the pair the account now holds: the
        // record's published fingerprint is the MD5 digest of this key's DER
        // public key.
        let held = held_key_pair(&emulator, case, row.region, row.key_pair_name).await;
        let fingerprint = held["fingerprint"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a fingerprint, got {held:?}"));
        assert!(
            is_md5_colon_hex(fingerprint),
            "case {case}: expected sixteen colon-separated hex octets, got {fingerprint:?}"
        );
        let digest = ::md5::Md5::digest(public_key_der(case, key.key_data()));
        let expected: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            fingerprint,
            expected.join(":"),
            "case {case}: expected the reported public key to be the held pair's, whose \
             fingerprint is the MD5 digest of its DER public key"
        );

        // The pair is held in the Region the call was made in, so the record
        // the fingerprint came from is this call's own.
        assert_eq!(
            held["location"]["regionName"].as_str(),
            Some(row.region()),
            "case {case}: expected the pair to be held in the call's Region, got {:?}",
            held["location"]
        );
    }
}

/// RULE: the rendering is a property of the member and not of the call, so the
/// same account's two reporting calls — a create and the download of the
/// default pair — both report a base64-encoded `ssh-rsa` public key, and two
/// distinct keys.
#[tokio::test]
async fn both_reporting_calls_of_one_account_report_an_ssh_rsa_public_key() {
    let emulator = Emulator::new();
    let case = "one account, a create and a default download";

    let created = emulator
        .call_in(
            Some("us-east-1"),
            "CreateKeyPair",
            json!({"keyPairName": "kp-both-calls"}),
        )
        .await
        .1;
    let created_key = created["publicKeyBase64"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected publicKeyBase64, got {created:?}"));
    assert_reported_ssh_public_key(case, created_key);

    let downloaded = emulator
        .call_in(Some("us-east-1"), "DownloadDefaultKeyPair", json!({}))
        .await
        .1;
    let downloaded_key = downloaded["publicKeyBase64"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected publicKeyBase64, got {downloaded:?}"));
    assert_reported_ssh_public_key(case, downloaded_key);

    assert_ne!(
        created_key, downloaded_key,
        "case {case}: expected two distinct pairs, got one public key twice: \
         {created_key:?}"
    );

    // A later download reports the SAME held material, so the encoding is
    // stable across reads rather than re-derived per call.
    let again = emulator
        .call_in(Some("us-east-1"), "DownloadDefaultKeyPair", json!({}))
        .await
        .1;
    assert_eq!(
        again["publicKeyBase64"].as_str(),
        Some(downloaded_key),
        "case {case}: expected the held default pair's public half unchanged, got {again:?}"
    );
}
