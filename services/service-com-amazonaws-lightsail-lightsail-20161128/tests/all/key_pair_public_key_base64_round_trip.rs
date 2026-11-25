// SPDX-License-Identifier: CC0-1.0

//! What a client can DO with the public half a Lightsail response reports —
//! `publicKeyBase64` of `CreateKeyPair` and of `DownloadDefaultKeyPair` —
//! driven over the wire.
//!
//! Every case here goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: Lightsail_20161128.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router, so the modelled-constraint walk and
//! the handlers are exercised rather than bypassed.
//!
//! The rule under test: a member documented as "a base64-encoded public key of
//! the `ssh-rsa` type" carries base64 and nothing else, so base64-decoding it
//! yields the key's own encoding — the RFC 4253 `ssh-rsa` public key blob, which
//! names its algorithm inside itself — and re-encoding that blob gives the very
//! value that was reported. The algorithm label and the space of an
//! `authorized_keys` line are the LINE's, so a caller composes the line from the
//! value rather than finding one in it.
//!
//! The invariants each row is checked against:
//!
//! * decode/encode round trip: `base64(decode(value)) == value`, which holds only
//!   for a value that is base64 alone, canonically encoded and free of
//!   whitespace;
//! * the decoded blob is an `ssh-rsa` public key, read back by the crate that
//!   implements the SSH key formats;
//! * emit/accept parity: `ImportKeyPair` holds the value exactly as it was
//!   reported, under a second name, and publishes for it the same `fingerprint`
//!   the account publishes for the pair the value came from — so the two sides of
//!   a round trip agree about the form AND about the key.
//!
//! The rows differ along what the rule generalises over: which call reports the
//! key, the Region the call is made in — including the one a request carrying no
//! credential scope is served in — and the name the reported pair is held under.

use serde_json::{json, Value};
use service_com_amazonaws_lightsail_lightsail_20161128::state::{
    DEFAULT_REGION, DEFAULT_SSH_KEY_NAME,
};

use crate::instance_lifecycle::Emulator;
// The rendering rule itself (base64 alone, decoding to an `ssh-rsa` key), the
// algorithm name it fixes and the read-back of the record a reported key belongs
// to are the encoding suite's, so this suite states the round trip rather than a
// second copy of any of them.
use crate::key_pair_public_key_encoding::{
    assert_reported_ssh_public_key, held_key_pair, SSH_RSA,
};

/// Which call reports the public key of a row.
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
        key_pair_name: "kp-round-trip",
        region: None,
    },
    Row {
        case: "a create with underscores and digits, us-west-2",
        reported: Reported::Created,
        key_pair_name: "kp_2026_round_trip",
        region: Some("us-west-2"),
    },
    Row {
        case: "a create with the shortest admitted name, eu-central-1",
        reported: Reported::Created,
        key_pair_name: "R7",
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

    /// Make this row's call, answering the reported `publicKeyBase64`.
    async fn report(&self, emulator: &Emulator) -> String {
        let (operation, body) = match self.reported {
            Reported::Created => ("CreateKeyPair", json!({"keyPairName": self.key_pair_name})),
            Reported::DefaultDownloaded => ("DownloadDefaultKeyPair", json!({})),
        };
        let (status, parsed) = emulator.call_in(self.region, operation, body).await;
        assert_eq!(
            status, 200,
            "case {}: expected {operation} to be served, got {parsed:?}",
            self.case
        );
        parsed["publicKeyBase64"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {}: expected publicKeyBase64, got {parsed:?}",
                    self.case
                )
            })
            .to_string()
    }
}

/// The `fingerprint` the account publishes for the key pair `record` is.
fn fingerprint_of(case: &str, record: &Value) -> String {
    record["fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a fingerprint, got {record:?}"))
        .to_string()
}

/// RULE: a reported `publicKeyBase64` is base64 alone, so decoding it yields the
/// key's `ssh-rsa` encoding and re-encoding that yields the reported value back.
#[tokio::test]
async fn a_reported_public_key_decodes_and_re_encodes_to_itself() {
    use ::base64::Engine;

    for row in ROWS {
        let case = row.case;
        // One emulator per row: a row is the FIRST call of its kind on a fresh
        // account, which is what mints the pair it reports.
        let emulator = Emulator::new();
        let reported = row.report(&emulator).await;

        // The rendering, checked where that rule lives, which also answers the
        // key the value carries.
        let key = assert_reported_ssh_public_key(case, &reported);

        // Base64 in, base64 out: a value carrying a label, a space or any
        // whitespace does not survive this.
        let decoded = ::base64::engine::general_purpose::STANDARD
            .decode(&reported)
            .unwrap_or_else(|error| {
                panic!("case {case}: expected a base64 value, got {reported:?}: {error}")
            });
        let re_encoded = ::base64::engine::general_purpose::STANDARD.encode(&decoded);
        assert_eq!(
            re_encoded, reported,
            "case {case}: expected the value to be the canonical base64 of the key blob, got \
             {re_encoded:?}"
        );

        // The blob is the key's own encoding: the crate that implements the SSH
        // key formats writes the same base64 for the key the value carries, so
        // the value is that encoding and not a re-wrapping of it.
        let line = format!("{SSH_RSA} {reported}");
        let composed = ::ssh_key::PublicKey::from_openssh(&line).unwrap_or_else(|error| {
            panic!("case {case}: expected {line:?} to be an OpenSSH public key: {error}")
        });
        assert_eq!(
            composed.key_data(),
            key.key_data(),
            "case {case}: expected the composed line to carry the reported key"
        );
        let written = composed.to_openssh().unwrap_or_else(|error| {
            panic!("case {case}: expected the key to encode: {error}")
        });
        assert_eq!(
            written.split_whitespace().nth(1),
            Some(reported.as_str()),
            "case {case}: expected the reported value to be the key's own base64 encoding, got \
             {written:?}"
        );
    }
}

/// RULE: what a call EMITS for the public half is a value `ImportKeyPair` holds
/// as it stands, and the key it carries is the pair's own — so the account that
/// imports the reported value publishes for it the same fingerprint the account
/// publishes for the pair it was reported from.
#[tokio::test]
async fn a_reported_public_key_imports_as_it_stands() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::new();
        let reported = row.report(&emulator).await;
        let source = held_key_pair(&emulator, case, row.region, row.key_pair_name).await;
        let source_fingerprint = fingerprint_of(case, &source);

        let imported_name = format!("{}-reimported", row.key_pair_name);
        let (status, body) = emulator
            .call_in(
                row.region,
                "ImportKeyPair",
                json!({"keyPairName": imported_name, "publicKeyBase64": reported}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the reported value to be importable as it stands, got {body:?}"
        );

        let imported = held_key_pair(&emulator, case, row.region, &imported_name).await;
        assert_eq!(
            fingerprint_of(case, &imported),
            source_fingerprint,
            "case {case}: expected the imported value to carry the reported pair's key, whose \
             published fingerprint is {source_fingerprint:?}"
        );

        // The imported pair is held in the Region the call was made in, so the
        // record compared above is this call's own.
        assert_eq!(
            imported["location"]["regionName"].as_str(),
            Some(row.region()),
            "case {case}: expected the imported pair in the call's Region, got {:?}",
            imported["location"]
        );
    }
}
