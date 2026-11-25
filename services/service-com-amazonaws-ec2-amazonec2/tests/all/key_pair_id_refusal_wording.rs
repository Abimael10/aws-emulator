// SPDX-License-Identifier: CC0-1.0

//! How EC2 WORDS a refusal about the key pair id a request supplied, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=DescribeKeyPairs`, answered with XML (README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test is that WHERE A REFUSAL NAMES THE REQUEST MEMBER AN ID CAME FROM, IT
//! SPELLS THAT MEMBER AS THE WIRE CARRIES IT: the `KeyPairIds` entries of a
//! `DescribeKeyPairs` reach the service as the `keyPairId` parameter
//! (`DescribeKeyPairsRequest$KeyPairIds` carries `xmlName` `KeyPairId`), so both refusals
//! that parameter can earn name it that way and never in English words —
//!
//! * a value that is no key pair id at all: `InvalidParameterValue`,
//!   `Invalid value '<value>' for keyPairId.`;
//! * a well-formed id the account holds no key pair under: `InvalidKeyPair.NotFound`,
//!   `The keyPairId '<id>' does not exist`.
//!
//! A client that reads the parameter's name out of the message therefore reads a name the
//! request really carries, and the two sentences agree about how that parameter is spelled.
//! The sentence belongs to the MEMBER, not to the code: the same `InvalidKeyPair.NotFound`
//! answers a request that named a key pair by NAME with the name sentence, and a tagging call
//! — which is handed a resource id without being told its kind, and carries no `keyPairId`
//! parameter at all — words that code its own way.
//!
//! Every table row is a distinct input along what the rule generalises over: the id's digit
//! count (both forms EC2 has issued ids in), its digit alphabet and magnitude, whether the
//! account holds any key pair at all, how many ids the request names and where the
//! unresolvable one sits among them, and whether the id was never minted, sits one past the
//! highest the account holds, or belonged to a key pair that was deleted. The selector
//! READING itself — which values are refused as malformed at all — is
//! [`super::describe_key_pairs_selector_reading`]'s subject; this suite is about the words.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed, tagging};
use super::wire::{param, Param};

/// The Region every call here is signed for. A key pair carries no Region member, so the
/// Region only selects the ec2Query envelope.
const REGION: &str = "us-east-2";

/// The emulator the tests drive, reached only through the wire.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `CreateKeyPair` that must succeed, answering the id it minted.
async fn create(case: &str, router: &MultiServiceRouter, key_name: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "CreateKeyPair",
        &[param("KeyName", key_name), param("KeyType", "ed25519")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateKeyPair, got {status} with body {body}"
    );
    element(&body, "keyPairId")
        .unwrap_or_else(|| panic!("case {case:?}: no <keyPairId> in {body}"))
}

/// Assert that a `DescribeKeyPairs` carrying `params` is refused HTTP 400 with `code` and
/// `message`, naming the case.
async fn assert_refused(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
    code: &str,
    message: &str,
) {
    let (status, body) =
        super::ec2_query::call(router, REGION, "DescribeKeyPairs", params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from DescribeKeyPairs, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        code,
        "case {case:?}: expected the code {code:?}, got body {body}"
    );
    assert_eq!(
        error_message(&body),
        message,
        "case {case:?}: expected the message {message:?}, got body {body}"
    );
}

/// The sentence EC2 answers for a key pair the `keyPairId` parameter named that the account
/// holds none of — the fixed text with the id the REQUEST sent in it.
fn absent_id_sentence(key_pair_id: &str) -> String {
    format!("The keyPairId '{key_pair_id}' does not exist")
}

/// Key pair ids in the two forms EC2 has issued ids in that no key pair of a fresh account
/// carries, each row a different point of the form: the 8-digit short form, the 17-digit long
/// form at both ends of its digit alphabet, the all-zero long form every client reaches for
/// as a placeholder, and a long form of mixed digits.
///
/// None of them is an id this store mints for the key pairs these tests create (the mint
/// counts from one, so those read `key-0000000000000000…`), which is what makes every row an
/// id the account holds no key pair under.
const ABSENT_IDS: [(&str, &str); 6] = [
    ("the short form", "key-1a2b3c4d"),
    ("the short form of the highest digits", "key-ffffffff"),
    ("a long id of low digits", "key-000000000000000b3"),
    ("a long id of the highest digits", "key-0ffffffffffffffff"),
    ("the all-zero long id", "key-00000000000000000"),
    ("a long id of mixed digits", "key-0a1b2c3d4e5f60718"),
];

/// RULE: the refusal that reports the key pair the `keyPairId` parameter named as absent is
/// `InvalidKeyPair.NotFound` worded `The keyPairId '<id>' does not exist`, spelling the
/// parameter as the wire carries it, with the id taken from the request — whatever the
/// account holds, however many ids the request names and wherever the unresolvable one sits.
#[tokio::test]
async fn an_absent_key_pair_id_is_reported_against_the_parameter_the_wire_carries() {
    let router = emulator();

    // Against an EMPTY account first: no lookup could have found anything, and the sentence
    // is already the one that names the parameter.
    for (case, absent) in ABSENT_IDS {
        assert_refused(
            case,
            &router,
            &indexed("KeyPairId", &[absent]),
            "InvalidKeyPair.NotFound",
            &absent_id_sentence(absent),
        )
        .await;
    }

    // The same sentence once the account DOES hold key pairs, and whichever position the
    // unresolvable id takes among ids that do resolve.
    let held_id = create("the held key pair", &router, "deploy-key").await;
    let second_id = create("the second key pair", &router, "ops-key").await;
    for (case, absent) in ABSENT_IDS {
        for (position, ids) in [
            ("alone", vec![absent]),
            ("first", vec![absent, held_id.as_str()]),
            ("last", vec![second_id.as_str(), absent]),
            (
                "between two held ids",
                vec![held_id.as_str(), absent, second_id.as_str()],
            ),
        ] {
            assert_refused(
                &format!("{case} {position}"),
                &router,
                &indexed("KeyPairId", &ids),
                "InvalidKeyPair.NotFound",
                &absent_id_sentence(absent),
            )
            .await;
        }
    }

    // An id DERIVED from the account's own state: one past the highest id it holds, which is
    // the id the next create would be nearest to and so the row no hand-written constant
    // covers.
    let one_past = next_long_id(&second_id);
    assert_refused(
        "one past the highest id the account holds",
        &router,
        &indexed("KeyPairId", &[one_past.as_str()]),
        "InvalidKeyPair.NotFound",
        &absent_id_sentence(&one_past),
    )
    .await;

    // Round trip: the ids that DO resolve are still reported, so the sentence above is the
    // answer to an absent id rather than a refusal of the member.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "DescribeKeyPairs",
        &indexed("KeyPairId", &[held_id.as_str(), second_id.as_str()]),
    )
    .await;
    assert_eq!(
        status, 200,
        "a describe naming both held ids must succeed, got {status} with body {body}"
    );
    let reported = super::ec2_query::elements(&body, "keyPairId");
    assert_eq!(
        reported,
        vec![held_id.clone(), second_id.clone()],
        "a describe naming both held ids must report both, got {reported:?}"
    );

    // And a key pair that is DELETED earns exactly the same sentence under its own id, so the
    // wording does not depend on whether an id was ever minted.
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "DeleteKeyPair",
        &[param("KeyPairId", held_id.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteKeyPair must succeed, got {status} with body {body}"
    );
    assert_refused(
        "the id of a deleted key pair",
        &router,
        &indexed("KeyPairId", &[held_id.as_str()]),
        "InvalidKeyPair.NotFound",
        &absent_id_sentence(&held_id),
    )
    .await;
}

/// RULE: both refusals the `keyPairId` parameter can earn spell it the same way — a value
/// that is no key pair id is `InvalidParameterValue` `Invalid value '<value>' for
/// keyPairId.`, and a well-formed id nothing is stored under is `InvalidKeyPair.NotFound`
/// `The keyPairId '<id>' does not exist` — while the NAME selector carries its own sentence.
#[tokio::test]
async fn the_two_refusals_of_the_key_pair_id_parameter_agree_about_its_name() {
    let router = emulator();
    let held_id = create("the held key pair", &router, "deploy-key").await;

    // A value outside the two id forms, each row a different way the form fails: the refusal
    // is about the request, and it names the parameter the value arrived in.
    for (case, malformed) in [
        ("too few digits", "key-123"),
        ("one digit past the long form", "key-0a1b2c3d4e5f607189"),
        ("a digit outside hexadecimal", "key-0a1b2c3d4e5f6071g"),
        ("digits in upper case", "key-0A1B2C3D4E5F60718"),
        ("another resource kind's prefix", "vol-0a1b2c3d4e5f60718"),
    ] {
        assert_refused(
            case,
            &router,
            &indexed("KeyPairId", &[malformed]),
            "InvalidParameterValue",
            &format!("Invalid value '{malformed}' for keyPairId."),
        )
        .await;
    }

    // The NAME member is a different member, so the same code carries the name sentence —
    // even for a name that is SHAPED like a key pair id, which is a name a key pair may
    // carry.
    for (case, absent_name) in [
        ("a plain name", "no-such-key"),
        ("a name shaped like a key pair id", "key-0a1b2c3d4e5f60718"),
    ] {
        assert_refused(
            case,
            &router,
            &indexed("KeyName", &[absent_name]),
            "InvalidKeyPair.NotFound",
            &format!("The key pair '{absent_name}' does not exist"),
        )
        .await;
    }

    // Nothing any refusal touched: a read reports, so the account still holds the key pair it
    // was given.
    let (status, body) =
        super::ec2_query::call(&router, REGION, "DescribeKeyPairs", &[]).await;
    assert_eq!(
        status, 200,
        "an unnarrowed describe must succeed, got {status} with body {body}"
    );
    assert_eq!(
        super::ec2_query::elements(&body, "keyPairId"),
        vec![held_id.clone()],
        "the account must still hold exactly the key pair it was given, got body {body}"
    );
}

/// RULE: the sentence belongs to the member the refusal is about, so a call that is handed a
/// key pair id as a RESOURCE rather than through the `keyPairId` parameter answers the same
/// `InvalidKeyPair.NotFound` code in its own words — naming the id it was given, and never a
/// parameter that request does not carry.
#[tokio::test]
async fn a_tagging_call_does_not_name_the_key_pair_id_parameter() {
    let router = emulator();

    for (case, absent) in ABSENT_IDS {
        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "CreateTags",
            &tagging(absent, &[("Name", "probe")]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: tagging a key pair the account does not hold must be refused, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidKeyPair.NotFound",
            "case {case:?}: expected the key pair not-found code, got body {body}"
        );
        let message = error_message(&body);
        assert!(
            message.contains(absent),
            "case {case:?}: the refusal must name the id the request gave, got {message:?}"
        );
        assert_ne!(
            message,
            absent_id_sentence(absent),
            "case {case:?}: a request carrying no keyPairId parameter must not be refused in \
             that parameter's words, got {message:?}"
        );
    }
}

/// The long-form key pair id one past `key_pair_id`, which the account therefore holds no key
/// pair under while the mint is at `key_pair_id`.
///
/// Reads the id's digits as the hexadecimal they are and prints the successor in the same
/// 17-digit long form the store mints, so the row is derived from what the account holds
/// rather than written out by hand.
fn next_long_id(key_pair_id: &str) -> String {
    let digits = key_pair_id
        .strip_prefix("key-")
        .unwrap_or_else(|| panic!("{key_pair_id:?} must be a key pair id"));
    let value = u64::from_str_radix(digits, 16)
        .unwrap_or_else(|_| panic!("{key_pair_id:?} must carry hexadecimal digits"));
    format!("key-{:017x}", value + 1)
}
