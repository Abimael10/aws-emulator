// SPDX-License-Identifier: CC0-1.0

//! How `DescribeKeyPairs` READS the two selectors a request narrows it by, driven over the
//! wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=DescribeKeyPairs`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that EVERY SELECTOR A REQUEST SUPPLIES IS READ FOR ITS FORM BEFORE
//! ANYTHING IS LOOKED UP, so the three answers never get confused:
//!
//! * a value given as `KeyPairIds` that is not a key pair id names no key pair of any
//!   account, so it is refused as a complaint about the REQUEST — `InvalidParameterValue`,
//!   `Invalid value '<value>' for keyPairId.` — whatever the account holds;
//! * a key pair NAME outside the bounds EC2 publishes for one ("up to 255 ASCII characters",
//!   `CreateKeyPairRequest$KeyName`) is refused the same way, naming the bound it broke;
//! * only a WELL-FORMED selector the account holds no key pair under is
//!   `InvalidKeyPair.NotFound`, worded for the member the request gave it — the id form
//!   `The keyPairId '<id>' does not exist`, which spells the member as the wire carries it,
//!   or the name form `The key pair '<name>' does not exist`.
//!
//! That ordering is what lets a client tell a typo'd or ill-formed id from a valid id whose
//! key pair was deleted or belongs to another account: the two carry different codes, and SDK
//! error mapping branches on the code.
//!
//! Beside it, the two facts the same lookup settles: key pair NAMES are compared byte for
//! byte, so names differing only in letter case are different key pairs; and a deleted key
//! pair stops resolving under either member, so a client polling until it is gone terminates.
//!
//! Every table row is a distinct input along what the rule generalises over — the id's
//! prefix, its digit count, its digit alphabet and its letter case, the name's length and
//! character set, how many selectors the request carries and whether the account holds any
//! key pair at all — so the one value a recording happens to carry is one row among many
//! rather than the specification. `DescribeKeyPairs`' filters and its full projection are
//! [`super::key_pairs`]'s subject; the import round trip is [`super::import_key_pair`]'s.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for. A key pair carries no Region member, so the
/// Region only selects the ec2Query envelope.
const REGION: &str = "eu-west-1";

/// The emulator the tests drive. The store is reached only through the wire here, so the
/// engine handle is not needed.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `CreateKeyPair` of `key_type` that must succeed, answering the id it minted.
async fn create(
    case: &str,
    router: &MultiServiceRouter,
    key_name: &str,
    key_type: &str,
) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "CreateKeyPair",
        &[param("KeyName", key_name), param("KeyType", key_type)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateKeyPair, got {status} with body {body}"
    );
    element(&body, "keyPairId")
        .unwrap_or_else(|| panic!("case {case:?}: no <keyPairId> in {body}"))
}

/// One `DescribeKeyPairs` carrying `params`, as the status and body it answered.
async fn describe(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "DescribeKeyPairs", params).await
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
    let (status, body) = describe(router, params).await;
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

/// The key pairs one `DescribeKeyPairs` body reports, as `(keyName, keyPairId, keyType)`
/// triples in the order they were reported.
fn reported(case: &str, body: &str) -> Vec<(String, String, String)> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "keySet")
        .into_iter()
        .map(|item| {
            let text = |name: &str| child_text(item, name).unwrap_or_default();
            (text("keyName"), text("keyPairId"), text("keyType"))
        })
        .collect()
}

/// The key pairs a `DescribeKeyPairs` narrowed by `params` reports, the call having to
/// succeed.
async fn reported_by(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> Vec<(String, String, String)> {
    let (status, body) = describe(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeKeyPairs, got {status} with body {body}"
    );
    reported(case, &body)
}

/// Every key pair id the account holds, as an unnarrowed `DescribeKeyPairs` reports them.
async fn held_ids(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = describe(router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from an unnarrowed DescribeKeyPairs, got {status} \
         with body {body}"
    );
    elements(&body, "keyPairId")
}

/// A well-formed key pair id the account holds no key pair under, in the long form this
/// service mints — 17 lowercase hexadecimal digits.
const ABSENT_LONG_ID: &str = "key-0fedcba987654321f";

/// A well-formed key pair id in the SHORT form EC2 also issued ids in — 8 lowercase
/// hexadecimal digits — which the account holds no key pair under either.
const ABSENT_SHORT_ID: &str = "key-1a2b3c4d";

/// The values a request may give as a `KeyPairIds` entry that are NOT key pair ids, each a
/// different way the form fails: the digit count, the digit alphabet, the letter case, the
/// prefix, and a value that is a key pair NAME rather than an id.
const MALFORMED_IDS: [(&str, &str); 9] = [
    ("too few digits", "key-123"),
    ("one digit short of the long form", "key-0fedcba987654321"),
    ("one digit past the long form", "key-0fedcba987654321fa"),
    ("a digit outside hexadecimal", "key-0fedcba98765432zz"),
    ("digits in upper case", "key-0FEDCBA987654321F"),
    ("the prefix in upper case", "KEY-0fedcba987654321f"),
    ("another resource kind's prefix", "vpc-0fedcba987654321f"),
    ("no digits at all", "key-"),
    ("a key pair name", "deploy-key"),
];

/// RULE: a value a request gives as a key pair id is read for its FORM before any lookup — a
/// value that is not a key pair id is `InvalidParameterValue` naming the value the request
/// gave, and `InvalidKeyPair.NotFound` is reserved for a well-formed id the account holds no
/// key pair under.
#[tokio::test]
async fn describe_key_pairs_tells_a_value_that_is_no_key_pair_id_from_one_that_is_absent() {
    let router = emulator();

    // Every malformed value is refused the same way against an EMPTY account, where no
    // lookup could have found anything either: the refusal is about the request.
    for (case, malformed) in MALFORMED_IDS {
        assert_refused(
            case,
            &router,
            &indexed("KeyPairId", &[malformed]),
            "InvalidParameterValue",
            &format!("Invalid value '{malformed}' for keyPairId."),
        )
        .await;
    }

    // A well-formed id the account holds no key pair under is the OTHER answer, in both id
    // forms EC2 has issued.
    for absent in [ABSENT_LONG_ID, ABSENT_SHORT_ID] {
        assert_refused(
            "a well-formed id no key pair carries",
            &router,
            &indexed("KeyPairId", &[absent]),
            "InvalidKeyPair.NotFound",
            &format!("The keyPairId '{absent}' does not exist"),
        )
        .await;
    }

    // The same answers once the account DOES hold key pairs, and whichever position the
    // malformed value takes in the list: the form of every supplied id is read before the
    // one that resolves is looked up, so a request mixing a held id with a malformed one is
    // refused on the malformed one rather than answered with the held key pair.
    let held_id = create("the held key pair", &router, "deploy-key", "ed25519").await;
    let second_id = create("the second key pair", &router, "ops-key", "rsa").await;
    for (case, malformed) in MALFORMED_IDS {
        for (position, params) in [
            ("first", vec![malformed, held_id.as_str()]),
            ("last", vec![second_id.as_str(), malformed]),
            (
                "between two held ids",
                vec![held_id.as_str(), malformed, second_id.as_str()],
            ),
        ] {
            let case = format!("{case} {position}");
            assert_refused(
                &case,
                &router,
                &indexed("KeyPairId", &params),
                "InvalidParameterValue",
                &format!("Invalid value '{malformed}' for keyPairId."),
            )
            .await;
        }
    }

    // A request naming only ids that resolve is answered, so the refusals above are the
    // reading of the values and not a refusal of the member.
    let reported = reported_by(
        "both held ids",
        &router,
        &indexed("KeyPairId", &[held_id.as_str(), second_id.as_str()]),
    )
    .await;
    assert_eq!(
        reported.len(),
        2,
        "a describe naming both held ids must report both key pairs, got {reported:?}"
    );

    // Nothing any refusal touched: a read reports, so the account still holds exactly the
    // two key pairs it was given.
    let held = held_ids("after every refusal", &router).await;
    assert_eq!(
        held,
        vec![held_id.clone(), second_id.clone()],
        "the account must still hold exactly the key pairs it was given, got {held:?}"
    );
}

/// RULE: a key pair NAME a request supplies is read against the bounds EC2 publishes for one
/// before any lookup — a name outside them is `InvalidParameterValue` naming the bound it
/// broke, a name inside them that no key pair carries is `InvalidKeyPair.NotFound` in the
/// NAME form of that sentence, and a name inside them that a key pair carries is reported.
#[tokio::test]
async fn describe_key_pairs_reads_a_key_name_against_the_published_bounds() {
    let router = emulator();

    // "Up to 255 ASCII characters" (`CreateKeyPairRequest$KeyName`): AT the bound the name is
    // a name, so it is looked up — and the key pair carrying it is reported.
    let at_bound = "n".repeat(ec2::state::MAX_KEY_PAIR_NAME_CHARS);
    let at_bound_id = create("a name at the published maximum", &router, &at_bound, "rsa").await;
    let reported = reported_by(
        "a name at the published maximum",
        &router,
        &indexed("KeyName", &[at_bound.as_str()]),
    )
    .await;
    assert_eq!(
        reported,
        vec![(at_bound.clone(), at_bound_id.clone(), "rsa".to_string())],
        "the key pair named at the published maximum must read back, got {reported:?}"
    );

    // One character PAST the bound, and outside ASCII: complaints about the request.
    let past_bound = "n".repeat(ec2::state::MAX_KEY_PAIR_NAME_CHARS + 1);
    for (case, key_name, message) in [
        (
            "a name one character past the published maximum",
            past_bound.clone(),
            format!(
                "Value ({past_bound}) for parameter keyName is invalid. A key pair name \
                 accepts at most {} characters",
                ec2::state::MAX_KEY_PAIR_NAME_CHARS
            ),
        ),
        (
            "a name outside ASCII",
            "clé-de-déploiement".to_string(),
            "Value (clé-de-déploiement) for parameter keyName is invalid. A key pair name \
             accepts ASCII characters only"
                .to_string(),
        ),
    ] {
        assert_refused(
            case,
            &router,
            &indexed("KeyName", &[key_name.as_str()]),
            "InvalidParameterValue",
            &message,
        )
        .await;
    }

    // A name inside the bounds that no key pair carries is the lookup's answer, in the NAME
    // form of the sentence — never the id form.
    let absent_at_bound = "z".repeat(ec2::state::MAX_KEY_PAIR_NAME_CHARS);
    for (case, absent) in [
        ("a name of one character", "k"),
        ("a name of punctuation and digits", "key-2026.06_v2/west"),
        ("a name that looks like a key pair id", ABSENT_LONG_ID),
        (
            "an absent name at the published maximum",
            absent_at_bound.as_str(),
        ),
    ] {
        assert_refused(
            case,
            &router,
            &indexed("KeyName", &[absent]),
            "InvalidKeyPair.NotFound",
            &format!("The key pair '{absent}' does not exist"),
        )
        .await;
    }
}

/// RULE: a key pair name is compared byte for byte, so two names differing only in letter
/// case are two distinct key pairs, each read back under the exact name its caller supplied;
/// and a deleted key pair stops resolving under either member, leaving the others untouched.
#[tokio::test]
async fn key_pair_names_are_case_sensitive_and_a_deleted_key_pair_stops_resolving() {
    let router = emulator();

    // Four names differing only in letter case — and one that differs in more than case, so
    // the report is not merely "every key pair".
    let cased: Vec<(&str, &str)> = vec![
        ("all lower case", "deploy-key"),
        ("leading capital", "Deploy-Key"),
        ("all upper case", "DEPLOY-KEY"),
        ("one capital in the middle", "deploy-Key"),
        ("a different name", "release-key"),
    ];
    let mut minted: Vec<(&str, &str, String)> = Vec::new();
    for &(case, key_name) in &cased {
        let key_type = if key_name.starts_with('d') {
            "ed25519"
        } else {
            "rsa"
        };
        let key_pair_id = create(case, &router, key_name, key_type).await;
        minted.push((case, key_name, key_pair_id));
    }

    // Every one of them is its own key pair, and a name-scoped read answers that key pair
    // and no other.
    for (case, key_name, key_pair_id) in &minted {
        let (case, key_name) = (*case, *key_name);
        let reported = reported_by(case, &router, &indexed("KeyName", &[key_name])).await;
        let names: Vec<&str> = reported
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![key_name],
            "case {case:?}: a name-scoped read must report exactly the key pair named \
             {key_name:?}, got {reported:?}"
        );
        assert_eq!(
            reported[0].1,
            *key_pair_id,
            "case {case:?}: the key pair named {key_name:?} must be the one that name minted, \
             got {reported:?}"
        );
    }

    // Deleting one of them removes that key pair and nothing else: the name no longer
    // resolves, its id no longer resolves, and every case-distinct sibling still does.
    let (case, deleted_name, deleted_id) = minted.remove(1);
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "DeleteKeyPair",
        &[param("KeyName", deleted_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteKeyPair, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "keyPairId").as_deref(),
        Some(deleted_id.as_str()),
        "case {case:?}: the delete must report the key pair it removed, got body {body}"
    );

    for (selector, params, message) in [
        (
            "KeyName",
            indexed("KeyName", &[deleted_name]),
            format!("The key pair '{deleted_name}' does not exist"),
        ),
        (
            "KeyPairId",
            indexed("KeyPairId", &[deleted_id.as_str()]),
            format!("The keyPairId '{deleted_id}' does not exist"),
        ),
    ] {
        assert_refused(
            &format!("{case} read back by {selector}"),
            &router,
            &params,
            "InvalidKeyPair.NotFound",
            &message,
        )
        .await;
    }

    for (case, key_name, key_pair_id) in &minted {
        let (case, key_name) = (*case, *key_name);
        let reported = reported_by(case, &router, &indexed("KeyName", &[key_name])).await;
        assert_eq!(
            reported.iter().map(|(_, id, _)| id.clone()).collect::<Vec<_>>(),
            vec![key_pair_id.clone()],
            "case {case:?}: the key pair named {key_name:?} must survive the delete of a \
             case-distinct name, got {reported:?}"
        );
    }
    let held = held_ids("after the delete", &router).await;
    assert!(
        !held.contains(&deleted_id),
        "the deleted key pair {deleted_id:?} must be gone from the account, got {held:?}"
    );
    assert_eq!(
        held.len(),
        minted.len(),
        "the account must hold exactly the key pairs no delete named, got {held:?}"
    );
}
