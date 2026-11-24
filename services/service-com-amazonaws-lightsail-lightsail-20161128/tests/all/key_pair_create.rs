// SPDX-License-Identifier: CC0-1.0

//! Putting a Lightsail SSH key pair into an account — `CreateKeyPair`, which
//! mints one, and `ImportKeyPair` ([`importing`]), which holds the public half a
//! caller brought — driven over the wire.
//!
//! Every case here goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: Lightsail_20161128.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router, so the modelled-constraint walk, the
//! handlers and the awsJson error envelope are all exercised rather than
//! bypassed.
//!
//! The rules under test:
//!
//! * A create mints a REAL RSA key pair, in the two encodings the modelled
//!   output documents: the base64-encoded `ssh-rsa` key the sibling
//!   public-key-encoding suite owns the rule for, a PKCS#1
//!   `RSA PRIVATE KEY` PEM carrying the same modulus for the private half, and a
//!   `fingerprint` that is the MD5 digest of that pair's DER-encoded public key
//!   in the colon-hex form an AWS key pair fingerprint is published in. The
//!   PUBLIC half alone is held, under the name the request named, so a later
//!   `GetKeyPair` reports exactly the record the create stored.
//! * A key pair name addresses one key pair of the account, so a create naming
//!   one the account already holds is refused with the modelled
//!   `InvalidInputException` (HTTP 400) and changes nothing — whichever Region
//!   the colliding call is made in, and however its tags differ — while a create
//!   for a name the account does not hold is served. Because the uniqueness check
//!   and the insert happen under one exclusive hold on the store, concurrent
//!   creates of ONE name elect exactly one winner however many race, and every
//!   loser is refused that same way.
//! * An import holds the public half the request submitted under the name it
//!   names, as a key pair of the account like any other — the rules [`importing`]
//!   states, which share the fingerprint rule and the name-uniqueness rule above,
//!   which is why they are checked beside them rather than against a second copy
//!   of each.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lightsail_lightsail_20161128::state::{self, DEFAULT_REGION};

use crate::instance_lifecycle::{error_code, is_uuid_shaped, Emulator};

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN and
/// support code carries.
const ACCOUNT: &str = "000000000000";

/// One `CreateKeyPair` request, and the Region its call is made in.
///
/// The rows differ along everything the rule generalises over: the characters and
/// length of the name (letters, digits, hyphens, underscores, mixed case, the
/// shortest name the modelled `ResourceName` pattern `^\w[\w\-]*\w$` admits), how
/// many tags the request carries (none, one, two), and the Region the call is
/// made in — including the one a request carrying no credential scope is served
/// in.
struct Row {
    case: &'static str,
    key_pair_name: &'static str,
    /// The Region of the call's credential scope, or `None` for a request that
    /// carries none, which is served in [`DEFAULT_REGION`].
    region: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a hyphenated name, no tags, no credential scope",
        key_pair_name: "key-pair-alpha",
        region: None,
        tags: &[],
    },
    Row {
        case: "underscores and digits, one tag, us-west-2",
        key_pair_name: "kp_2026_build",
        region: Some("us-west-2"),
        tags: &[("env", "staging")],
    },
    Row {
        case: "a two-character name, two tags, eu-central-1",
        key_pair_name: "K7",
        region: Some("eu-central-1"),
        tags: &[("team", "ssh"), ("cost-centre", "8080")],
    },
    Row {
        case: "a long mixed-case name, one tag, ap-southeast-1",
        key_pair_name: "sim-LS-kp-with-a-much-longer-name-9",
        region: Some("ap-southeast-1"),
        tags: &[("keep", "yes")],
    },
];

impl Row {
    /// The Region this row's key pair is created in — the credential scope's,
    /// or the Region a request carrying none is served in.
    fn region(&self) -> &str {
        self.region.unwrap_or(DEFAULT_REGION)
    }

    /// The `CreateKeyPair` body this row sends, carrying `tags` only when the row
    /// has any, so a row with none exercises the member's absence.
    fn body(&self) -> Value {
        self.body_named(self.key_pair_name)
    }

    /// The same body for another `name`, for a case that collides with a held
    /// name while carrying this row's tags.
    fn body_named(&self, name: &str) -> Value {
        let mut request = json!({"keyPairName": name});
        if !self.tags.is_empty() {
            request["tags"] = Value::Array(
                self.tags
                    .iter()
                    .map(|(key, value)| json!({"key": key, "value": value}))
                    .collect(),
            );
        }
        request
    }

    /// The tags this row's request carries, as the response reports them.
    fn tag_values(&self) -> Value {
        Value::Array(
            self.tags
                .iter()
                .map(|(key, value)| json!({"key": key, "value": value}))
                .collect(),
        )
    }
}

/// How AWS words the refusal of a create whose name the account already holds,
/// with the name the request asked for substituted in: the fixed text is spelled
/// out HERE rather than taken from the handler's own helper, so the wording is
/// pinned and not merely re-stated.
fn name_in_use_message(name: &str) -> String {
    format!("Some names are already in use: {name}")
}

/// Create the key pair `body` asks for, in `region`, answering the status and the
/// parsed body.
async fn create(emulator: &Emulator, region: Option<&str>, body: Value) -> (u16, Value) {
    emulator.call_in(region, "CreateKeyPair", body).await
}

/// The `keyPair` a served `GetKeyPair` answers for `name`, or `None` when the
/// account holds no key pair of that name.
async fn read(emulator: &Emulator, name: &str) -> Option<Value> {
    let (status, body) = emulator
        .call("GetKeyPair", json!({"keyPairName": name}))
        .await;
    (status == 200).then(|| body["keyPair"].clone())
}

/// Every key pair name the store holds.
fn held_names(emulator: &Emulator) -> Vec<String> {
    state::with_state(emulator.engine(), |store| {
        store.key_pairs.keys().cloned().collect()
    })
    .expect("the shared engine holds this service's store")
}

/// The id the server-assigned members of `key_pair` are built from: the tail of
/// its ARN, which the support code must name as well.
fn minted_id(case: &str, key_pair: &Value) -> String {
    let arn = key_pair["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected an arn, got {key_pair:?}"));
    arn.rsplit('/')
        .next()
        .unwrap_or_else(|| panic!("case {case}: expected an arn with a resource id, got {arn:?}"))
        .to_string()
}

/// Assert the `keyPair` a create or a read answered reports the record the
/// account holds for `row` — every member the modelled `KeyPair` carries
/// (`smithy_query(query="shape",shape="KeyPair")`), computed from the request plus
/// the store's own allocators.
fn assert_reported(case: &str, row: &Row, key_pair: &Value) {
    let members: BTreeSet<&str> = key_pair
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: expected a keyPair object, got {key_pair:?}"))
        .keys()
        .map(String::as_str)
        .collect();
    let modelled: BTreeSet<&str> = [
        "name",
        "arn",
        "supportCode",
        "createdAt",
        "location",
        "resourceType",
        "tags",
        "fingerprint",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        members, modelled,
        "case {case}: expected exactly the modelled KeyPair members, got {key_pair:?}"
    );
    assert_eq!(
        key_pair["name"].as_str(),
        Some(row.key_pair_name),
        "case {case}: expected the requested name, got {key_pair:?}"
    );
    let id = minted_id(case, key_pair);
    assert!(
        is_uuid_shaped(&id),
        "case {case}: expected a uuid-shaped resource id in the arn, got {id:?}"
    );
    assert_eq!(
        key_pair["arn"].as_str(),
        Some(state::key_pair_arn(row.region(), &id).as_str()),
        "case {case}: expected the key pair ARN of the create's Region, got {key_pair:?}"
    );
    assert_eq!(
        key_pair["supportCode"].as_str(),
        Some(format!("{ACCOUNT}/{id}").as_str()),
        "case {case}: expected the support code of the same resource id, got {key_pair:?}"
    );
    assert!(
        key_pair["createdAt"].as_f64().unwrap_or_default() > 0.0,
        "case {case}: expected the creation instant, got {key_pair:?}"
    );
    assert_eq!(
        key_pair["location"]["regionName"].as_str(),
        Some(row.region()),
        "case {case}: expected the Region the create was made in, got {:?}",
        key_pair["location"]
    );
    assert_eq!(
        key_pair["location"]["availabilityZone"].as_str(),
        Some("all"),
        "case {case}: a key pair is not zonal, so it is reported in every zone of its \
         region, got {:?}",
        key_pair["location"]
    );
    assert_eq!(
        key_pair["resourceType"].as_str(),
        Some("KeyPair"),
        "case {case}: expected the KeyPair resource type, got {key_pair:?}"
    );
    assert_eq!(
        key_pair["tags"],
        row.tag_values(),
        "case {case}: expected the tags the request asked for, got {key_pair:?}"
    );
}

/// The PEM header and footer the modelled output fixes for the private half: "A
/// base64-encoded RSA private key" (`CreateKeyPairResult$privateKeyBase64`), the
/// PKCS#1 labels of RFC 7468.
const RSA_PRIVATE_KEY_BEGIN: &str = "-----BEGIN RSA PRIVATE KEY-----";
const RSA_PRIVATE_KEY_END: &str = "-----END RSA PRIVATE KEY-----";

/// How many bits the RSA key pair a create mints carries, so the public half the
/// response reports is a usable SSH key and not a toy one.
const MODULUS_BITS: usize = 2048;

/// Whether `value` is the form an AWS key pair fingerprint is published in:
/// sixteen lowercase-hex octets separated by colons — the MD5 digest of the
/// DER-encoded public key, which is what `KeyPair$fingerprint` ("The RSA
/// fingerprint of the key pair") carries.
///
/// `pub(crate)` so a sibling suite in this test target checks a published
/// fingerprint the same way.
pub(crate) fn is_md5_colon_hex(value: &str) -> bool {
    let octets: Vec<&str> = value.split(':').collect();
    octets.len() == 16
        && octets.iter().all(|octet| {
            octet.len() == 2
                && octet
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

/// The DER-encoded public key (a `SubjectPublicKeyInfo`, what `openssl rsa
/// -pubout -outform DER` writes) of the RSA public half `public`, rebuilt here
/// from the modulus and exponent the response reported.
///
/// Built by the DER crates rather than by the handler's own call, so the
/// fingerprint assertion below checks the published RULE — the MD5 digest of the
/// DER public key — rather than repeating whatever the implementation computed.
///
/// `pub(crate)` so a sibling suite in this test target rebuilds it the same way.
pub(crate) fn public_key_der(case: &str, public: &::ssh_key::public::KeyData) -> Vec<u8> {
    use ::der::Encode;

    let rsa = public
        .rsa()
        .unwrap_or_else(|| panic!("case {case}: expected an RSA public key, got {public:?}"));
    let modulus = rsa
        .n
        .as_positive_bytes()
        .unwrap_or_else(|| panic!("case {case}: expected a positive modulus"));
    let exponent = rsa
        .e
        .as_positive_bytes()
        .unwrap_or_else(|| panic!("case {case}: expected a positive exponent"));
    assert_eq!(
        modulus.len() * 8,
        MODULUS_BITS,
        "case {case}: expected a {MODULUS_BITS}-bit RSA modulus, got {} bits",
        modulus.len() * 8
    );
    let pkcs1 = ::pkcs1::RsaPublicKey {
        modulus: ::der::asn1::UintRef::new(modulus).expect("the modulus is a DER integer"),
        public_exponent: ::der::asn1::UintRef::new(exponent)
            .expect("the exponent is a DER integer"),
    }
    .to_der()
    .expect("the PKCS#1 public key encodes");
    ::spki::SubjectPublicKeyInfoRef {
        algorithm: ::pkcs1::ALGORITHM_ID,
        subject_public_key: ::der::asn1::BitStringRef::new(0, &pkcs1)
            .expect("the public key is a DER bit string"),
    }
    .to_der()
    .expect("the SubjectPublicKeyInfo encodes")
}

/// Assert the two halves `body` answered are one real RSA key pair of the type
/// the modelled output fixes, and that the `fingerprint` the record carries is
/// the MD5 digest of that pair's DER-encoded public key; answer the public half
/// as the response reported it.
///
/// The material is re-read with the crates that implement each published
/// encoding, so this asserts a real key of the documented type rather than a
/// string shaped like one.
fn assert_real_key_material(case: &str, body: &Value) -> String {
    use ::md5::Digest;

    let public_key = body["publicKeyBase64"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected publicKeyBase64, got {body:?}"))
        .to_string();
    let private_key = body["privateKeyBase64"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected privateKeyBase64, got {body:?}"))
        .to_string();

    // The public half: the `ssh-rsa` public key the model documents — the
    // algorithm name and the key's base64 encoding, checked where that rule
    // lives.
    let public = crate::key_pair_public_key_encoding::assert_reported_ssh_public_key(
        case,
        &public_key,
    );

    // The private half: the PKCS#1 `RSA PRIVATE KEY` PEM the model documents,
    // holding the same modulus — so the two halves are ONE pair.
    assert!(
        private_key.starts_with(RSA_PRIVATE_KEY_BEGIN)
            && private_key.trim_end().ends_with(RSA_PRIVATE_KEY_END),
        "case {case}: expected an {RSA_PRIVATE_KEY_BEGIN} PEM, got {:?}",
        private_key.lines().next()
    );
    let document = ::der::Document::from_pem(&private_key)
        .unwrap_or_else(|error| panic!("case {case}: expected a PEM private half: {error}"))
        .1;
    let parsed = <::pkcs1::RsaPrivateKey as ::der::Decode>::from_der(document.as_bytes())
        .unwrap_or_else(|error| {
            panic!("case {case}: expected a PKCS#1 RSA private key: {error}")
        });
    let reported = public
        .key_data()
        .rsa()
        .unwrap_or_else(|| panic!("case {case}: expected an RSA public key"));
    assert_eq!(
        parsed.modulus.as_bytes(),
        reported.n.as_positive_bytes().unwrap_or_default(),
        "case {case}: expected the private half to carry the public half's modulus"
    );

    // The fingerprint: the MD5 digest of the DER-encoded public key, in the
    // colon-hex form an AWS key pair fingerprint is published in.
    let fingerprint = body["keyPair"]["fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a fingerprint, got {body:?}"));
    assert!(
        is_md5_colon_hex(fingerprint),
        "case {case}: expected sixteen colon-separated hex octets, got {fingerprint:?}"
    );
    let digest = ::md5::Md5::digest(public_key_der(case, public.key_data()));
    let expected: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        fingerprint,
        expected.join(":"),
        "case {case}: expected the MD5 digest of the DER public key"
    );
    public_key
}

/// Assert the operation `body` answered is the terminal `CreateKeyPair` entry of
/// the key pair `row` describes.
fn assert_create_operation(case: &str, row: &Row, body: &Value) {
    let operation = &body["operation"];
    assert_eq!(
        operation["operationType"].as_str(),
        Some("CreateKeyPair"),
        "case {case}: expected the CreateKeyPair operation type, got {operation:?}"
    );
    assert_eq!(
        operation["resourceType"].as_str(),
        Some("KeyPair"),
        "case {case}: expected the KeyPair resource type, got {operation:?}"
    );
    assert_eq!(
        operation["resourceName"].as_str(),
        Some(row.key_pair_name),
        "case {case}: expected the operation to name the created key pair, got {operation:?}"
    );
    assert_eq!(
        operation["location"]["regionName"].as_str(),
        Some(row.region()),
        "case {case}: expected the key pair's own Region on the log entry, got {:?}",
        operation["location"]
    );
    assert_eq!(
        operation["status"].as_str(),
        Some("Succeeded"),
        "case {case}: the pair exists as the call returns, so expected a Succeeded \
         operation, got {operation:?}"
    );
    assert_eq!(
        operation["isTerminal"].as_bool(),
        Some(true),
        "case {case}: expected a terminal operation, got {operation:?}"
    );
    let id = operation["id"].as_str().unwrap_or_default().to_string();
    assert!(
        is_uuid_shaped(&id),
        "case {case}: expected a uuid-shaped operation id, got {id:?}"
    );
}

// ---------------------------------------------------------------------------
// The create table
// ---------------------------------------------------------------------------

/// RULE: `CreateKeyPair` mints a real RSA key pair of the type its modelled
/// output documents, answers both halves once, holds the public half under the
/// requested name with the MD5 fingerprint of that half's DER encoding, and logs
/// ONE terminal `CreateKeyPair` operation — so a later `GetKeyPair` reports
/// exactly what the create stored.
#[tokio::test]
async fn creating_a_key_pair_mints_real_material_and_holds_the_public_half() {
    // ONE emulator for every row: each row names a different key pair, so the
    // material and the names must not collide across rows either.
    let emulator = Emulator::new();
    let mut public_keys: BTreeSet<String> = BTreeSet::new();
    for row in ROWS {
        let case = row.case;
        let (status, body) = create(&emulator, row.region, row.body()).await;
        assert_eq!(status, 200, "case {case}: expected a served create, got {body:?}");
        assert_reported(case, row, &body["keyPair"]);
        assert_create_operation(case, row, &body);
        public_keys.insert(assert_real_key_material(case, &body));

        let read_back = read(&emulator, row.key_pair_name)
            .await
            .unwrap_or_else(|| panic!("case {case}: expected GetKeyPair to serve the new pair"));
        assert_eq!(
            read_back, body["keyPair"],
            "case {case}: expected GetKeyPair to report the created record, got {read_back:?}"
        );
    }
    assert_eq!(
        public_keys.len(),
        ROWS.len(),
        "expected one freshly minted public key per create, got {} distinct keys for {} \
         creates",
        public_keys.len(),
        ROWS.len()
    );
    let held = held_names(&emulator);
    let wanted: Vec<String> = {
        let mut names: Vec<String> = ROWS
            .iter()
            .map(|row| row.key_pair_name.to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        held, wanted,
        "expected the account to hold exactly the created key pairs, got {held:?}"
    );
}

/// RULE: a key pair name addresses ONE key pair of the account, so a create of a
/// name the account already holds is refused with the modelled
/// `InvalidInputException` and changes nothing — whichever Region the colliding
/// call is made in and whatever tags it carries — while a create of a name the
/// account does not hold is served.
#[tokio::test]
async fn creating_a_key_pair_whose_name_is_taken_is_refused_and_changes_nothing() {
    /// How a colliding second create differs from the create that won the name.
    #[derive(Clone, Copy, Debug)]
    enum Collision {
        /// Repeated exactly, in the same Region.
        Same,
        /// The same name from another Region's endpoint — a key pair is
        /// addressed by name alone, so the name is taken there too.
        OtherRegion,
        /// The same name carrying another row's tags.
        OtherTags,
    }

    const COLLISIONS: &[Collision] = &[Collision::Same, Collision::OtherRegion, Collision::OtherTags];

    for collision in COLLISIONS {
        for (index, row) in ROWS.iter().enumerate() {
            let case = format!("{} colliding {collision:?}", row.case);
            let emulator = Emulator::new();
            let (status, first) = create(&emulator, row.region, row.body()).await;
            assert_eq!(status, 200, "case {case}: expected a served create, got {first:?}");
            let held = first["keyPair"].clone();

            let (region, body) = match collision {
                Collision::Same => (row.region, row.body()),
                // A Region no row is created in, so the collision is about the
                // NAME and not about the Region agreeing.
                Collision::OtherRegion => (Some("eu-west-3"), row.body()),
                Collision::OtherTags => (
                    row.region,
                    ROWS[(index + 1) % ROWS.len()].body_named(row.key_pair_name),
                ),
            };
            let (status, refusal) = create(&emulator, region, body).await;
            assert_eq!(
                status, 400,
                "case {case}: expected the taken name to be refused, got {refusal:?}"
            );
            assert_eq!(
                error_code(&refusal),
                "InvalidInputException",
                "case {case}: expected the modelled InvalidInputException, got {refusal:?}"
            );
            assert_eq!(
                refusal["message"].as_str(),
                Some(name_in_use_message(row.key_pair_name).as_str()),
                "case {case}: expected the refusal to name the key pair already in use, got \
                 {refusal:?}"
            );

            let after = read(&emulator, row.key_pair_name)
                .await
                .unwrap_or_else(|| panic!("case {case}: the first key pair must still be held"));
            assert_eq!(
                after, held,
                "case {case}: a refused create must change nothing, got {after:?}"
            );
            assert_eq!(
                held_names(&emulator),
                vec![row.key_pair_name.to_string()],
                "case {case}: a refused create must hold no second record"
            );

            // The positive control: the refusal is about the name alone, so a
            // name the account does not hold is still served — and once the held
            // pair is deleted, its name is free again.
            let other = format!("{}-second", row.key_pair_name);
            let (status, served) = create(&emulator, region, row.body_named(&other)).await;
            assert_eq!(
                status, 200,
                "case {case}: expected a free name to be served, got {served:?}"
            );
            assert_eq!(
                served["keyPair"]["name"].as_str(),
                Some(other.as_str()),
                "case {case}: expected the free name to be created, got {served:?}"
            );
            let (status, deleted) = emulator
                .call_in(
                    row.region,
                    "DeleteKeyPair",
                    json!({"keyPairName": row.key_pair_name}),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected the held pair to delete, got {deleted:?}"
            );
            let (status, recreated) = create(&emulator, row.region, row.body()).await;
            assert_eq!(
                status, 200,
                "case {case}: a deleted name is free again, got {recreated:?}"
            );
            assert_ne!(
                recreated["keyPair"]["fingerprint"], held["fingerprint"],
                "case {case}: expected the re-created pair to carry freshly minted material"
            );
        }
    }
}

/// RULE: the uniqueness check and the insert happen under one exclusive hold on
/// the store, so however many creates race for ONE name exactly one is served and
/// every loser is refused with the modelled `InvalidInputException` — and the
/// account then holds the one key pair the winner reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_of_one_name_elect_exactly_one_winner() {
    // The counts differ along what the rule generalises over: the NUMBER of
    // callers racing for one name, from the smallest race there is upwards.
    const RACER_COUNTS: &[usize] = &[2, 3, 6];

    for racers in RACER_COUNTS {
        let emulator = Arc::new(Emulator::new());
        let name = format!("race-{racers}-way");
        let mut racing = Vec::new();
        for racer in 0..*racers {
            let emulator = Arc::clone(&emulator);
            let body = json!({
                "keyPairName": name.clone(),
                "tags": [{"key": "racer", "value": racer.to_string()}],
            });
            racing.push(tokio::spawn(async move {
                emulator.call("CreateKeyPair", body).await
            }));
        }

        let mut winners: Vec<Value> = Vec::new();
        let mut refusals: Vec<Value> = Vec::new();
        for racer in racing {
            let (status, body) = racer.await.expect("the racing call completed");
            match status {
                200 => winners.push(body),
                _ => {
                    assert_eq!(
                        status, 400,
                        "case {racers} racers: expected a 400 refusal for a loser, got \
                         {status} {body:?}"
                    );
                    refusals.push(body);
                }
            }
        }

        assert_eq!(
            winners.len(),
            1,
            "case {racers} racers: expected exactly one winner, got {} (refusals: {refusals:?})",
            winners.len()
        );
        assert_eq!(
            refusals.len(),
            racers - 1,
            "case {racers} racers: expected every loser to be refused, got {refusals:?}"
        );
        for refusal in &refusals {
            assert_eq!(
                error_code(refusal),
                "InvalidInputException",
                "case {racers} racers: expected the modelled InvalidInputException for a \
                 loser, got {refusal:?}"
            );
            assert_eq!(
                refusal["message"].as_str(),
                Some(name_in_use_message(&name).as_str()),
                "case {racers} racers: expected the loser to be told the name is in use, \
                 got {refusal:?}"
            );
        }

        let winner = &winners[0];
        assert_eq!(
            winner["keyPair"]["name"].as_str(),
            Some(name.as_str()),
            "case {racers} racers: expected the winner to carry the requested name, got \
             {winner:?}"
        );
        let read_back = read(&emulator, &name)
            .await
            .unwrap_or_else(|| panic!("case {racers} racers: the won name must be held"));
        assert_eq!(
            read_back, winner["keyPair"],
            "case {racers} racers: expected the held record to be the winner's, got \
             {read_back:?}"
        );
        assert_eq!(
            held_names(&emulator),
            vec![name.clone()],
            "case {racers} racers: expected exactly one stored key pair"
        );
    }
}

// ---------------------------------------------------------------------------
// The import table
// ---------------------------------------------------------------------------

/// Importing a key pair a caller brought — `ImportKeyPair` — driven over the same
/// wire as the creates above.
///
/// The rules under test:
///
/// * An import HOLDS the public half the request submitted, as the account's key
///   pair under the name the request names: a later `GetKeyPair` and `GetKeyPairs`
///   report a record whose `fingerprint` is the MD5 digest of that key's
///   DER-encoded public half in the colon-hex an AWS key pair fingerprint is
///   published in — the SAME value the account that minted the key publishes for
///   it, because the digest is taken over the key and not over the text it
///   arrived in — and the call logs ONE terminal `ImportKeyPair` operation naming
///   that key pair, its resource type and its location. The submitted value is
///   the key's own `ssh-rsa` encoding in base64, read in either rendering that
///   encoding is published in: the whole OpenSSH public key line a caller reads
///   out of a `.pub` file — which is also what every member of this service that
///   reports a key pair's public half carries — with or without its comment, and
///   the base64 body alone. Whitespace around the value is not key material.
/// * A value that carries no public key of the `ssh-rsa` type — text that is not
///   base64, base64 that is not an SSH public key, a truncated blob, a private
///   key, a real public key of another algorithm, or a line whose label disagrees
///   with its body — is refused with the modelled `InvalidInputException`
///   (HTTP 400) and leaves the account holding nothing.
/// * A key pair name addresses ONE key pair of the account whichever call put it
///   there, so an import under a name the account already holds is refused with
///   the same sentence a colliding create is answered, changes nothing, and
///   becomes importable again once that pair is deleted — while the same key
///   under another name is held beside it.
mod importing {
    use super::*;

    /// The algorithm name the modelled input fixes for the key an import brings:
    /// "A base64-encoded public key of the ssh-rsa type"
    /// (`ImportKeyPairRequest$publicKeyBase64`):
    /// `smithy_query(query="operation",operation="ImportKeyPair")`.
    const SSH_RSA: &str = "ssh-rsa";

    /// How the service words the refusal of a `publicKeyBase64` that carries no
    /// public key of the type the member documents: the member that is wrong and
    /// the constraint it broke, spelled out here so the wording is pinned and not
    /// merely re-stated.
    const NOT_A_PUBLIC_KEY_MESSAGE: &str =
        "publicKeyBase64 is not a base64-encoded public key of the ssh-rsa type";

    /// A real `ssh-rsa` public key to import, and the fingerprint the service
    /// publishes for the key pair it belongs to.
    ///
    /// Both come from a `CreateKeyPair` call on a THROWAWAY account, so a row
    /// imports a key this service really minted rather than a canned string — and
    /// the fingerprint is what the account that minted it publishes, which an
    /// import of the same key has to publish too.
    struct MintedKey {
        /// The key's BODY: the base64 of its own `ssh-rsa` encoding, without the
        /// algorithm label the reporting member carries in front of it.
        body: String,
        /// The `fingerprint` the minting account's own record publishes.
        fingerprint: String,
        /// The private half that account was handed, which is NOT a public key
        /// and so is one of the values an import refuses.
        private_key: String,
    }

    impl MintedKey {
        /// Mint a fresh RSA key pair on a throwaway account and answer its public
        /// half, the fingerprint that account publishes for it, and the private
        /// half it was handed.
        async fn mint(case: &str) -> Self {
            let source = Emulator::new();
            let (status, created) = source
                .call("CreateKeyPair", json!({"keyPairName": "kp-import-source"}))
                .await;
            assert_eq!(
                status, 200,
                "case {case}: the source key pair must be created, got {created:?}"
            );
            // The create reports the whole `ssh-rsa` public key; a row chooses
            // which published rendering of it to submit, so the BODY is kept
            // here and each rendering is built from it.
            let reported = created["publicKeyBase64"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case}: expected publicKeyBase64, got {created:?}")
                });
            let body = reported
                .split_whitespace()
                .nth(1)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected an {SSH_RSA} public key carrying its body, got \
                         {reported:?}"
                    )
                })
                .to_string();
            let fingerprint = created["keyPair"]["fingerprint"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: expected a fingerprint, got {created:?}"))
                .to_string();
            let private_key = created["privateKeyBase64"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case}: expected privateKeyBase64, got {created:?}")
                })
                .to_string();
            Self {
                body,
                fingerprint,
                private_key,
            }
        }
    }

    /// Which published rendering of the key's `ssh-rsa` encoding a row submits.
    #[derive(Clone, Copy, Debug)]
    enum Submitted {
        /// The base64 body alone, which is the key without the algorithm label
        /// the reporting members carry in front of it.
        Body,
        /// The body with whitespace around it, as a value read out of a file
        /// arrives.
        PaddedBody,
        /// The whole OpenSSH public key line, `<algorithm> <body>`.
        OpensshLine,
        /// The whole OpenSSH line with the comment such a line may carry,
        /// `<algorithm> <body> <comment>`.
        OpensshLineWithComment,
        /// The whole OpenSSH line as a `.pub` file holds it, with its trailing
        /// newline.
        OpensshLineWithNewline,
    }

    impl Submitted {
        /// The `publicKeyBase64` value this rendering of `body` is submitted as.
        fn value(self, body: &str) -> String {
            match self {
                Self::Body => body.to_string(),
                Self::PaddedBody => format!("  {body}\n"),
                Self::OpensshLine => format!("{SSH_RSA} {body}"),
                Self::OpensshLineWithComment => format!("{SSH_RSA} {body} caller@workstation"),
                Self::OpensshLineWithNewline => format!("{SSH_RSA} {body} caller@workstation\n"),
            }
        }
    }

    /// One `ImportKeyPair` request, and the Region its call is made in.
    ///
    /// The rows differ along everything the rule generalises over: which
    /// rendering of the key's encoding the value carries, the characters and
    /// length of the name (letters, digits, hyphens, underscores, mixed case, the
    /// shortest name the modelled `ResourceName` pattern `^\w[\w\-]*\w$` admits),
    /// and the Region the call is made in — including the one a request carrying
    /// no credential scope is served in. Every row imports a DIFFERENT key.
    struct ImportRow {
        case: &'static str,
        submitted: Submitted,
        key_pair_name: &'static str,
        /// The Region of the call's credential scope, or `None` for a request
        /// that carries none, which is served in [`DEFAULT_REGION`].
        region: Option<&'static str>,
    }

    const IMPORT_ROWS: &[ImportRow] = &[
        ImportRow {
            case: "the base64 body alone, a hyphenated name, no credential scope",
            submitted: Submitted::Body,
            key_pair_name: "imported-key-pair",
            region: None,
        },
        ImportRow {
            case: "a whole OpenSSH line, underscores and digits, us-west-2",
            submitted: Submitted::OpensshLine,
            key_pair_name: "kp_2026_imported",
            region: Some("us-west-2"),
        },
        ImportRow {
            case: "an OpenSSH line with a comment, the shortest admitted name, eu-central-1",
            submitted: Submitted::OpensshLineWithComment,
            key_pair_name: "K7",
            region: Some("eu-central-1"),
        },
        ImportRow {
            case: "the body with whitespace around it, a long mixed-case name, ap-southeast-1",
            submitted: Submitted::PaddedBody,
            key_pair_name: "sim-LS-kp-imported-9",
            region: Some("ap-southeast-1"),
        },
        ImportRow {
            case: "an OpenSSH line with its trailing newline, a short name, ap-northeast-1",
            submitted: Submitted::OpensshLineWithNewline,
            key_pair_name: "kp2026",
            region: Some("ap-northeast-1"),
        },
    ];

    impl ImportRow {
        /// The Region this row's key pair is held in — the credential scope's, or
        /// the Region a request carrying none is served in.
        fn region(&self) -> &str {
            self.region.unwrap_or(DEFAULT_REGION)
        }
    }

    /// Import `public_key` under `name` in `region`, answering the status and the
    /// parsed body.
    async fn import(
        emulator: &Emulator,
        region: Option<&str>,
        name: &str,
        public_key: &str,
    ) -> (u16, Value) {
        emulator
            .call_in(
                region,
                "ImportKeyPair",
                json!({"keyPairName": name, "publicKeyBase64": public_key}),
            )
            .await
    }

    /// The `keyPair` a served `GetKeyPair` answers for `name`, or `None` when the
    /// account holds no key pair of that name.
    async fn read_key_pair(emulator: &Emulator, region: Option<&str>, name: &str) -> Option<Value> {
        let (status, body) = emulator
            .call_in(region, "GetKeyPair", json!({"keyPairName": name}))
            .await;
        (status == 200).then(|| body["keyPair"].clone())
    }

    /// Every key pair `GetKeyPairs` reports, walking every page.
    async fn listed(emulator: &Emulator, region: Option<&str>) -> Vec<Value> {
        let mut pairs = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let request = match &token {
                Some(token) => json!({"pageToken": token}),
                None => json!({}),
            };
            let (status, body) = emulator.call_in(region, "GetKeyPairs", request).await;
            assert_eq!(status, 200, "GetKeyPairs must succeed, got {body:?}");
            pairs.extend(body["keyPairs"].as_array().cloned().unwrap_or_default());
            match body["nextPageToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        pairs
    }

    /// Assert the operation `body` answered is the terminal `ImportKeyPair` entry
    /// of `name` in `region`, and answer its id.
    fn assert_import_operation(case: &str, name: &str, region: &str, body: &Value) -> String {
        let members: BTreeSet<&str> = body
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: expected a result object, got {body:?}"))
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            members,
            ["operation"].into_iter().collect::<BTreeSet<&str>>(),
            "case {case}: the caller brought the key, so expected the modelled result to \
             carry the operation alone and no key material, got {body:?}"
        );
        let operation = &body["operation"];
        assert_eq!(
            operation["operationType"].as_str(),
            Some("ImportKeyPair"),
            "case {case}: expected the entry to name the call that wrote it, got {operation:?}"
        );
        assert_eq!(
            operation["resourceType"].as_str(),
            Some("KeyPair"),
            "case {case}: expected the KeyPair resource type, got {operation:?}"
        );
        assert_eq!(
            operation["resourceName"].as_str(),
            Some(name),
            "case {case}: expected the entry to name the imported key pair, got {operation:?}"
        );
        assert_eq!(
            operation["location"]["regionName"].as_str(),
            Some(region),
            "case {case}: expected the Region the import was made in, got {:?}",
            operation["location"]
        );
        assert_eq!(
            operation["location"]["availabilityZone"].as_str(),
            Some("all"),
            "case {case}: a key pair is not zonal, so the entry is reported in every zone of \
             its region, got {:?}",
            operation["location"]
        );
        assert_eq!(
            operation["status"].as_str(),
            Some("Succeeded"),
            "case {case}: the public key is held as the call returns, so expected a Succeeded \
             operation, got {operation:?}"
        );
        assert_eq!(
            operation["isTerminal"].as_bool(),
            Some(true),
            "case {case}: expected a terminal operation, got {operation:?}"
        );
        assert!(
            operation["createdAt"].as_f64().unwrap_or_default() > 0.0,
            "case {case}: expected a creation timestamp, got {operation:?}"
        );
        let id = operation["id"].as_str().unwrap_or_default().to_string();
        assert!(
            is_uuid_shaped(&id),
            "case {case}: expected a uuid-shaped operation id, got {id:?}"
        );
        id
    }

    /// Assert `key_pair` is the record an import of `minted` under `row`'s name
    /// left the account holding: every member the modelled `KeyPair` carries
    /// (`smithy_query(query="shape",shape="KeyPair")`), with the fingerprint of
    /// the imported key.
    fn assert_holds_imported_key(
        case: &str,
        row: &ImportRow,
        minted: &MintedKey,
        key_pair: &Value,
    ) {
        use ::md5::Digest;

        let members: BTreeSet<&str> = key_pair
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: expected a keyPair object, got {key_pair:?}"))
            .keys()
            .map(String::as_str)
            .collect();
        let modelled: BTreeSet<&str> = [
            "name",
            "arn",
            "supportCode",
            "createdAt",
            "location",
            "resourceType",
            "tags",
            "fingerprint",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            members, modelled,
            "case {case}: expected exactly the modelled KeyPair members, got {key_pair:?}"
        );
        assert_eq!(
            key_pair["name"].as_str(),
            Some(row.key_pair_name),
            "case {case}: expected the requested name, got {key_pair:?}"
        );
        let id = minted_id(case, key_pair);
        assert!(
            is_uuid_shaped(&id),
            "case {case}: expected a uuid-shaped resource id in the arn, got {id:?}"
        );
        assert_eq!(
            key_pair["arn"].as_str(),
            Some(state::key_pair_arn(row.region(), &id).as_str()),
            "case {case}: expected the key pair ARN of the import's Region, got {key_pair:?}"
        );
        assert_eq!(
            key_pair["supportCode"].as_str(),
            Some(format!("{ACCOUNT}/{id}").as_str()),
            "case {case}: expected the support code of the same resource id, got {key_pair:?}"
        );
        assert!(
            key_pair["createdAt"].as_f64().unwrap_or_default() > 0.0,
            "case {case}: expected the creation instant, got {key_pair:?}"
        );
        assert_eq!(
            key_pair["location"]["regionName"].as_str(),
            Some(row.region()),
            "case {case}: expected the Region the import was made in, got {:?}",
            key_pair["location"]
        );
        assert_eq!(
            key_pair["location"]["availabilityZone"].as_str(),
            Some("all"),
            "case {case}: a key pair is not zonal, so it is reported in every zone of its \
             region, got {:?}",
            key_pair["location"]
        );
        assert_eq!(
            key_pair["resourceType"].as_str(),
            Some("KeyPair"),
            "case {case}: expected the KeyPair resource type, got {key_pair:?}"
        );
        assert_eq!(
            key_pair["tags"],
            json!([]),
            "case {case}: nothing in an import asks for tags, so expected none, got \
             {key_pair:?}"
        );

        // The fingerprint is the published digest of the key that was submitted:
        // the MD5 of its DER public key, rebuilt here from the key the value
        // carried, in the colon-hex an AWS key pair fingerprint is published in.
        let fingerprint = key_pair["fingerprint"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a fingerprint, got {key_pair:?}"));
        assert!(
            is_md5_colon_hex(fingerprint),
            "case {case}: expected sixteen colon-separated hex octets, got {fingerprint:?}"
        );
        let key = crate::key_pair_public_key_encoding::assert_base64_ssh_public_key(
            case,
            &minted.body,
        );
        let digest = ::md5::Md5::digest(public_key_der(case, key.key_data()));
        let expected: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            fingerprint,
            expected.join(":"),
            "case {case}: expected the MD5 digest of the imported key's DER public key"
        );
        // And it is the value the account that MINTED this key publishes for it,
        // so the digest is taken over the key rather than over the text it
        // arrived in or the name it is held under.
        assert_eq!(
            fingerprint, minted.fingerprint,
            "case {case}: expected the fingerprint the minting account publishes for this key"
        );
    }

    /// RULE: an import holds the public half the request submitted as the
    /// account's key pair under the name it names — readable back through
    /// `GetKeyPair` and `GetKeyPairs` with the fingerprint of that key — and logs
    /// ONE terminal `ImportKeyPair` operation naming it, whichever published
    /// rendering of the key's encoding the value carried.
    #[tokio::test]
    async fn an_imported_public_key_is_held_as_the_accounts_key_pair() {
        let mut fingerprints: BTreeSet<String> = BTreeSet::new();

        for row in IMPORT_ROWS {
            let case = row.case;
            // One account per row: a row's import is the only thing the account
            // holds, so the listing below is about this import alone.
            let emulator = Emulator::new();
            let minted = MintedKey::mint(case).await;

            let (status, body) = import(
                &emulator,
                row.region,
                row.key_pair_name,
                &row.submitted.value(&minted.body),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: expected the import to be served, got {status} {body:?}"
            );
            let operation_id =
                assert_import_operation(case, row.key_pair_name, row.region(), &body);

            // The pair is HELD: read back through the one projection every key
            // pair read answers with.
            let held = read_key_pair(&emulator, row.region, row.key_pair_name)
                .await
                .unwrap_or_else(|| panic!("case {case}: expected the imported pair to be held"));
            assert_holds_imported_key(case, row, &minted, &held);
            fingerprints.insert(held["fingerprint"].as_str().unwrap_or_default().to_string());

            // And it is the account's only key pair, reported by the listing
            // exactly as by the single read.
            let pairs = listed(&emulator, row.region).await;
            assert_eq!(
                pairs,
                vec![held.clone()],
                "case {case}: expected the listing to report exactly the imported pair"
            );

            // The entry is in the log, not just in the response.
            let (log_status, logged) = emulator
                .call_in(
                    row.region,
                    "GetOperation",
                    json!({"operationId": operation_id}),
                )
                .await;
            assert_eq!(
                log_status, 200,
                "case {case}: expected the logged operation to be readable, got {logged:?}"
            );
            assert_eq!(
                logged["operation"], body["operation"],
                "case {case}: expected the log to hold the operation the import answered, got \
                 {logged:?}"
            );
            let (for_resource_status, for_resource) = emulator
                .call_in(
                    row.region,
                    "GetOperationsForResource",
                    json!({"resourceName": row.key_pair_name}),
                )
                .await;
            assert_eq!(
                for_resource_status, 200,
                "case {case}: expected the key pair's operations to be readable, got \
                 {for_resource:?}"
            );
            assert_eq!(
                for_resource["operations"],
                json!([body["operation"]]),
                "case {case}: expected the imported pair's log to hold exactly this entry, got \
                 {for_resource:?}"
            );
        }

        assert_eq!(
            fingerprints.len(),
            IMPORT_ROWS.len(),
            "expected each row's own key to publish its own fingerprint, got {fingerprints:?} \
             for {} rows",
            IMPORT_ROWS.len()
        );
    }

    /// A real public key of ANOTHER algorithm, as an OpenSSH public key line: an
    /// Ed25519 key, generated with the crate that implements the SSH key formats,
    /// so the refusal below is about the key's TYPE and not about a malformed
    /// value.
    fn other_algorithm_openssh_line() -> String {
        let key = ::ssh_key::PrivateKey::from(::ssh_key::private::Ed25519Keypair::random(
            &mut ::ssh_key::rand_core::OsRng,
        ));
        key.public_key()
            .to_openssh()
            .expect("the public key writes an OpenSSH line")
    }

    /// RULE: a `publicKeyBase64` that carries no public key of the `ssh-rsa` type
    /// is refused with the modelled `InvalidInputException` and leaves the account
    /// holding nothing.
    #[tokio::test]
    async fn a_value_that_carries_no_ssh_rsa_public_key_is_refused() {
        const REGION: Option<&str> = Some("us-east-1");
        const NAME: &str = "kp-import-refused";

        let emulator = Emulator::new();
        let minted = MintedKey::mint("the key the refused values are derived from").await;
        let other = other_algorithm_openssh_line();
        let other_body = other
            .split_whitespace()
            .nth(1)
            .expect("an OpenSSH line carries its body as its second field")
            .to_string();

        // The rows differ along HOW the value fails to carry a public key of the
        // documented type: it is empty, it is whitespace, it is not base64, it is
        // base64 of something that is not an SSH public key, it is a truncated
        // key blob, it is the pair's PRIVATE half, it is a real public key of
        // another algorithm — in either rendering such a key is published in —
        // or it is a line whose algorithm label disagrees with its body.
        let truncated = minted.body[..44].to_string();
        let rows: Vec<(&str, String)> = vec![
            ("an empty value", String::new()),
            ("whitespace alone", " \n\t ".to_string()),
            (
                "text that is not base64",
                "this is not base64!!".to_string(),
            ),
            (
                "base64 of something that is not an SSH public key",
                "aGVsbG8sIHdvcmxkIQ==".to_string(),
            ),
            ("a truncated ssh-rsa key blob", truncated),
            (
                "the pair's private half, which is not a public key",
                minted.private_key.clone(),
            ),
            (
                "a real public key of another algorithm, as an OpenSSH line",
                other.clone(),
            ),
            (
                "a real public key of another algorithm, body alone",
                other_body,
            ),
            (
                "an ssh-rsa key body under another algorithm's label",
                format!("ssh-ed25519 {}", minted.body),
            ),
        ];

        for (case, value) in &rows {
            let (status, refusal) = import(&emulator, REGION, NAME, value).await;
            assert_eq!(
                status, 400,
                "case {case}: expected the value to be refused, got {status} {refusal:?}"
            );
            assert_eq!(
                error_code(&refusal),
                "InvalidInputException",
                "case {case}: expected the modelled InvalidInputException, got {refusal:?}"
            );
            assert_eq!(
                refusal["message"].as_str(),
                Some(NOT_A_PUBLIC_KEY_MESSAGE),
                "case {case}: expected the refusal to name the member and its constraint, got \
                 {refusal:?}"
            );

            // Nothing was held: the name the refused import asked for addresses
            // no key pair, and the account holds none at all.
            let (read_status, read_body) = emulator
                .call_in(REGION, "GetKeyPair", json!({"keyPairName": NAME}))
                .await;
            assert_eq!(
                read_status, 400,
                "case {case}: expected the name to address no key pair, got {read_status} \
                 {read_body:?}"
            );
            assert_eq!(
                error_code(&read_body),
                "NotFoundException",
                "case {case}: expected the modelled NotFoundException, got {read_body:?}"
            );
            assert_eq!(
                read_body["message"].as_str(),
                Some(
                    crate::instance_lifecycle::does_not_exist_message("KeyPair", NAME).as_str()
                ),
                "case {case}: expected the refusal to name the resource type and the name, got \
                 {read_body:?}"
            );
            assert!(
                listed(&emulator, REGION).await.is_empty(),
                "case {case}: a refused import must hold no key pair"
            );
        }

        // The positive control: the SAME account imports the real key those
        // values were derived from, so the refusals are about the values alone.
        let (status, served) = import(&emulator, REGION, NAME, &minted.body).await;
        assert_eq!(
            status, 200,
            "expected the real key to be imported after the refusals, got {status} {served:?}"
        );
        let held = read_key_pair(&emulator, REGION, NAME)
            .await
            .expect("the imported pair must be held");
        assert_eq!(
            held["fingerprint"].as_str(),
            Some(minted.fingerprint.as_str()),
            "expected the imported key's own fingerprint, got {held:?}"
        );
    }

    /// RULE: a key pair name addresses ONE key pair of the account whichever call
    /// put it there, so an import under a held name is refused and changes
    /// nothing, the same key under another name is held beside it, and the name is
    /// importable again once the pair it named is deleted.
    #[tokio::test]
    async fn importing_a_name_the_account_holds_is_refused_and_changes_nothing() {
        /// Which call holds the name an import then collides with, and where that
        /// colliding import is made from.
        #[derive(Clone, Copy, Debug)]
        enum Collision {
            /// The name was taken by an earlier import, from the same Region.
            Imported,
            /// The name was taken by an earlier import, and the colliding import
            /// is made from another Region's endpoint — a key pair is addressed
            /// by name alone, so the name is taken there too.
            ImportedThenCalledFromAnotherRegion,
            /// The name was taken by a `CreateKeyPair` call, which minted the
            /// pair itself.
            Created,
        }

        const COLLISIONS: &[Collision] = &[
            Collision::Imported,
            Collision::ImportedThenCalledFromAnotherRegion,
            Collision::Created,
        ];
        const HELD_REGION: Option<&str> = Some("us-east-1");
        const NAME: &str = "kp-import-collision";

        for collision in COLLISIONS {
            let case = format!("{collision:?}");
            let emulator = Emulator::new();
            let first = MintedKey::mint(&case).await;
            let second = MintedKey::mint(&case).await;

            // Take the name, by the call this case says took it.
            match collision {
                Collision::Created => {
                    let (status, created) = emulator
                        .call_in(HELD_REGION, "CreateKeyPair", json!({"keyPairName": NAME}))
                        .await;
                    assert_eq!(
                        status, 200,
                        "case {case}: the first key pair must be created, got {created:?}"
                    );
                }
                Collision::Imported | Collision::ImportedThenCalledFromAnotherRegion => {
                    let (status, imported) = import(&emulator, HELD_REGION, NAME, &first.body).await;
                    assert_eq!(
                        status, 200,
                        "case {case}: the first import must be served, got {imported:?}"
                    );
                }
            }
            let held = read_key_pair(&emulator, HELD_REGION, NAME)
                .await
                .unwrap_or_else(|| panic!("case {case}: expected the name to be held"));

            let from: Option<&str> = match collision {
                Collision::ImportedThenCalledFromAnotherRegion => Some("eu-west-3"),
                _ => HELD_REGION,
            };
            let (status, refusal) = import(&emulator, from, NAME, &second.body).await;
            assert_eq!(
                status, 400,
                "case {case}: expected the taken name to be refused, got {status} {refusal:?}"
            );
            assert_eq!(
                error_code(&refusal),
                "InvalidInputException",
                "case {case}: expected the modelled InvalidInputException, got {refusal:?}"
            );
            assert_eq!(
                refusal["message"].as_str(),
                Some(name_in_use_message(NAME).as_str()),
                "case {case}: expected the refusal to name the key pair already in use, got \
                 {refusal:?}"
            );
            let after = read_key_pair(&emulator, HELD_REGION, NAME)
                .await
                .unwrap_or_else(|| panic!("case {case}: the first key pair must still be held"));
            assert_eq!(
                after, held,
                "case {case}: a refused import must change nothing, got {after:?}"
            );

            // The same key under ANOTHER name is held beside it: a key pair is
            // addressed by its name, so the refusal is about the name alone.
            let twin = format!("{NAME}-twin");
            let (twin_status, twin_body) =
                import(&emulator, HELD_REGION, &twin, &second.body).await;
            assert_eq!(
                twin_status, 200,
                "case {case}: expected a free name to be served, got {twin_status} {twin_body:?}"
            );
            let twin_held = read_key_pair(&emulator, HELD_REGION, &twin)
                .await
                .unwrap_or_else(|| panic!("case {case}: expected the second pair to be held"));
            assert_eq!(
                twin_held["fingerprint"].as_str(),
                Some(second.fingerprint.as_str()),
                "case {case}: expected the imported key's own fingerprint, got {twin_held:?}"
            );

            // And once the pair the name addressed is deleted, the name is
            // importable again — now carrying the second key.
            let (delete_status, deleted) = emulator
                .call_in(HELD_REGION, "DeleteKeyPair", json!({"keyPairName": NAME}))
                .await;
            assert_eq!(
                delete_status, 200,
                "case {case}: the delete must be served, got {deleted:?}"
            );
            let (again_status, again) = import(&emulator, HELD_REGION, NAME, &second.body).await;
            assert_eq!(
                again_status, 200,
                "case {case}: expected the freed name to be importable, got {again_status} \
                 {again:?}"
            );
            let reimported = read_key_pair(&emulator, HELD_REGION, NAME)
                .await
                .unwrap_or_else(|| panic!("case {case}: expected the re-imported pair to be held"));
            assert_eq!(
                reimported["fingerprint"].as_str(),
                Some(second.fingerprint.as_str()),
                "case {case}: expected the newly imported key's fingerprint, got {reimported:?}"
            );
        }
    }
}
