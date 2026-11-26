// SPDX-License-Identifier: CC0-1.0

//! What a TAG KEY may be, over the `awsJson1_1` wire, on every member that takes one.
//!
//! The rule under test: a tag key is admitted exactly by the `@pattern` the modelled
//! `TagKey` shape carries (`smithy_query(query="shape",shape="TagKey")`:
//! `^(?!aws:)[\p{L}\p{Z}\p{N}_.:\/=+\-@%]*$`), whose two clauses are:
//!
//! 1. the CHARACTERS a key may be written with — Unicode letters, separators and digits
//!    plus `_ . : / = + - @ %` — are what its character class lists, and
//! 2. the reserved `aws:` prefix its look-ahead excludes is a prefix no key may begin with.
//!
//! A key written with a character the class leaves out is refused, on EVERY member that
//! takes a key, with HTTP 400 `ValidationException` naming the member it was submitted at
//! and the constraint: its own dotted path for a named member, the LIST member and the
//! whole constraint set its elements carry for an element of a list of keys.
//!
//! The reserved namespace is answered by WHAT the request asks to do with the key, because
//! the two things it can ask differ in AWS:
//!
//! * a request addressing a stream's STORED tag set — `TagDeliveryStream`'s `Tags[].Key`,
//!   `UntagDeliveryStream`'s `TagKeys[]` and `ListTagsForDeliveryStream`'s
//!   `ExclusiveStartTagKey` — is refused as that same request-shape constraint, under
//!   `ValidationException`; while
//! * a request CARRYING a tag set for a stream it creates (`CreateDeliveryStream`'s
//!   `Tags[].Key`) is refused by the service's own tag policy: the modelled
//!   `InvalidArgumentException` (`smithy_query(query="operation",
//!   operation="CreateDeliveryStream")`) and one fixed sentence stating the reserved
//!   namespace, with nothing of the request in it.
//!
//! A refused request writes nothing — no tag is added or removed and no stream is created;
//! a key inside the pattern is stored and read back unchanged.
//!
//! Each table below spans a SET of keys rather than one recorded request.
//!
//! The harness (`Emulator`, `s3_configuration`, `error_code`, `error_message`) is the
//! lifecycle suite's, the tag helpers are the listing suite's and the not-found sentence is
//! the not-found suite's, so every request here goes through the same real router.

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::{
    error_code, error_message, s3_configuration, Emulator,
};
use crate::delivery_stream_tags::{create_tagged, list_tags, listed, owned};

/// `TagKey`'s `@pattern` as a constraint clause reports it: exactly as the model spells
/// it, anchors and look-ahead included — an expression whose look-around is followed by a
/// RESTRICTED character class cannot lose its anchoring without admitting values the
/// constraint refuses (see the `refusal_messages` suite for the patterns whose anchoring IS
/// normalised away, including the look-ahead one whose end anchor stands behind a run of
/// any character).
const REPORTED_TAG_KEY_PATTERN: &str = r"^(?!aws:)[\p{L}\p{Z}\p{N}_.:\/=+\-@%]*$";

/// The requirement clause `TagKey`'s pattern states, worded by the suite that owns this
/// service's `@pattern` clause (`crate::refusal_messages::must_match`), so both suites
/// expect one sentence for one constraint.
fn must_match_tag_key_pattern() -> String {
    crate::refusal_messages::must_match(REPORTED_TAG_KEY_PATTERN)
}

/// The message a refusal of the tag key at the NAMED member `member` states: the one
/// constraint the key violated, at the member's own dotted path.
///
/// `pub(crate)` so a sibling suite asserting the same refusal words it the same way
/// rather than carrying a second copy of the sentence.
pub(crate) fn pattern_refusal(member: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member}' failed to satisfy constraint: {}",
        must_match_tag_key_pattern()
    )
}

/// The message a refusal of a tag key submitted as an ELEMENT of `member` states: the
/// list member, and the whole constraint set its elements carry (`TagKey` is
/// `@length(1..128)` as well as patterned), because an element has no member name of its
/// own to report one constraint at.
///
/// `pub(crate)` beside [`pattern_refusal`], for the same reason.
pub(crate) fn element_refusal(member: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
         Member must satisfy constraint: [Member must have length less than or equal to \
         128, Member must have length greater than or equal to 1, {}]",
        must_match_tag_key_pattern()
    )
}

/// Every key the modelled pattern does NOT admit, whichever of its clauses the key breaks:
/// the characters it is written with, and the prefix it begins with.
fn keys_outside_the_pattern() -> Vec<(&'static str, String)> {
    let mut rows = keys_outside_the_character_class();
    rows.extend(keys_carrying_the_reserved_prefix());
    rows
}

/// Keys the modelled pattern does NOT admit because of the CHARACTERS they are written
/// with — its character-class clause.
///
/// One row per printable ASCII punctuation mark the character class leaves out, so none of
/// them is admitted by accident. The class is `[\p{L}\p{Z}\p{N}_.:\/=+\-@%]`, so these are
/// the marks left over.
fn keys_outside_the_character_class() -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = Vec::new();
    for mark in [
        '!', '"', '#', '$', '&', '\'', '(', ')', '*', ',', ';', '<', '>', '?', '[', '\\',
        ']', '^', '`', '{', '|', '}', '~',
    ] {
        rows.push(("a character outside the modelled class", format!("ke{mark}y")));
    }
    rows
}

/// Keys the modelled pattern does not admit because of the PREFIX they begin with: the one
/// its look-ahead excludes — alone, and in front of anything.
fn keys_carrying_the_reserved_prefix() -> Vec<(&'static str, String)> {
    vec![
        ("the reserved prefix alone", "aws:".to_string()),
        (
            "the reserved prefix in front of a key",
            "aws:cloudformation:stack-name".to_string(),
        ),
        (
            "the reserved prefix in front of an otherwise legal key",
            "aws:owner".to_string(),
        ),
        (
            "the reserved prefix in front of a Unicode key",
            "aws:ключ".to_string(),
        ),
    ]
}

/// Keys the modelled pattern DOES admit: one per character the class lists, a Unicode
/// letter, a separator, a digit, and keys that contain the reserved prefix WITHOUT
/// beginning with it (the look-ahead excludes a prefix, not an occurrence).
fn accepted_keys() -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = vec![
        ("a plain key", "owner".to_string()),
        ("one character", "k".to_string()),
        ("a Unicode letter key", "ключ".to_string()),
        ("an ideographic key", "键".to_string()),
        ("a separator inside the key", "cost centre".to_string()),
        ("digits only", "2024".to_string()),
        ("the reserved prefix without its colon", "awsome".to_string()),
        ("the reserved prefix not at the start", "my-aws:key".to_string()),
        ("the longest key the length bound admits", "k".repeat(128)),
    ];
    // One row per character the class lists beyond letters, separators and digits.
    for mark in ['_', '.', ':', '/', '=', '+', '-', '@', '%'] {
        rows.push(("a character the modelled class lists", format!("ke{mark}y")));
    }
    rows
}

/// The sentence a create carrying a key in the reserved namespace is refused with: fixed
/// text stating the namespace and nothing of the request — not the offending keys, not the
/// stream the create named, and no validation-error count, member path or pattern.
const RESERVED_TAG_KEYS_MESSAGE: &str =
    "Some tag keys in the request started with 'aws:'. System tags cannot be modified.";

/// The error code the service's TAG POLICY refuses a reserved key with: the modelled client
/// argument error (`smithy_query(query="operation", operation="CreateDeliveryStream")`
/// lists `InvalidArgumentException`, `InvalidKMSResourceException`, `LimitExceededException`
/// and `ResourceInUseException` — and no `ValidationException`, so an SDK could not map one
/// of those to a modelled type).
const TAG_POLICY_CODE: &str = "InvalidArgumentException";

/// The code every request-shape refusal of a tag key answers under.
const REQUEST_SHAPE_CODE: &str = "ValidationException";

/// What one request carrying a tag key is refused with: the wire error code and the whole
/// message a client reads.
struct Refusal {
    code: &'static str,
    message: String,
}

/// The refusal a request-shape constraint on the NAMED member `member` states.
fn shape_refusal(member: &str) -> Refusal {
    Refusal {
        code: REQUEST_SHAPE_CODE,
        message: pattern_refusal(member),
    }
}

/// The refusal a request-shape constraint on an ELEMENT of the list member `member` states.
fn shape_element_refusal(member: &str) -> Refusal {
    Refusal {
        code: REQUEST_SHAPE_CODE,
        message: element_refusal(member),
    }
}

/// What a `CreateDeliveryStream` carrying `key` is refused with: the tag policy for a key in
/// the reserved namespace, the request's shape for a key outside the character class.
fn create_refusal(key: &str) -> Refusal {
    if key.starts_with("aws:") {
        return Refusal {
            code: TAG_POLICY_CODE,
            message: RESERVED_TAG_KEYS_MESSAGE.to_string(),
        };
    }
    shape_refusal("tags.1.member.key")
}

/// The four requests that carry a tag key, each as `(expected refusal, operation, body)`
/// for the key `key` on a stream called `stream`.
fn requests_carrying(stream: &str, key: &str) -> Vec<(Refusal, &'static str, Value)> {
    vec![
        (
            shape_refusal("tags.1.member.key"),
            "TagDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "Tags": [{ "Key": key, "Value": "v" }],
            }),
        ),
        (
            shape_element_refusal("tagKeys"),
            "UntagDeliveryStream",
            json!({ "DeliveryStreamName": stream, "TagKeys": [key] }),
        ),
        (
            create_refusal(key),
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": format!("{stream}-created"),
                "S3DestinationConfiguration": s3_configuration(),
                "Tags": [{ "Key": key, "Value": "v" }],
            }),
        ),
        (
            shape_refusal("exclusiveStartTagKey"),
            "ListTagsForDeliveryStream",
            json!({ "DeliveryStreamName": stream, "ExclusiveStartTagKey": key }),
        ),
    ]
}

#[tokio::test]
async fn a_key_the_modelled_pattern_rejects_is_refused_on_every_member_that_takes_one() {
    let emulator = Emulator::new();
    create_tagged(&emulator, "keyed-stream", &owned(&[("env", "prod")])).await;

    for (reason, key) in keys_outside_the_pattern() {
        for (expected, operation, request) in requests_carrying("keyed-stream", &key) {
            let case = format!("{operation} with {reason} ({key:?})");
            let (status, refused) = emulator.call(operation, request).await;
            assert_eq!(status, 400, "case {case:?}: got {refused:?}");
            let code = error_code(&refused);
            assert_eq!(
                code, expected.code,
                "case {case:?}: expected {:?}, got {code:?} in {refused:?}",
                expected.code
            );
            let message = error_message(&refused);
            assert_eq!(
                message, expected.message,
                "case {case:?}: expected {:?}, got {message:?}",
                expected.message
            );
            // The refused key is the caller's own value, and this service states no
            // rejected value in a constraint refusal at all: there is no `Value '…'`
            // subject for one to reach the body through.
            assert!(
                !message.contains("Value '"),
                "case {case:?}: expected no rejected key in the message, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_key_the_modelled_pattern_admits_round_trips_through_every_member() {
    for (index, (reason, key)) in accepted_keys().into_iter().enumerate() {
        let case = format!("{reason} ({key:?})");
        let emulator = Emulator::new();
        let stream = format!("accepted-stream-{index}");
        // Created WITH the key: `CreateDeliveryStream`'s `Tags` takes the same shape.
        create_tagged(&emulator, &stream, &[(key.clone(), "created".to_string())]).await;
        let listed_now = list_tags(&emulator, &stream, None, None).await;
        assert_eq!(
            listed(&listed_now.1),
            vec![(key.clone(), "created".to_string())],
            "case {case:?}: a created tag reads back, got {:?}",
            listed_now.1
        );

        // Replaced through `TagDeliveryStream`.
        let (status, response) = emulator
            .call(
                "TagDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "Tags": [{ "Key": key, "Value": "tagged" }],
                }),
            )
            .await;
        assert_eq!(status, 200, "case {case:?}: tag got {response:?}");
        let (_, after_tag) = list_tags(&emulator, &stream, None, None).await;
        assert_eq!(
            listed(&after_tag),
            vec![(key.clone(), "tagged".to_string())],
            "case {case:?}: the tagged value reads back, got {after_tag:?}"
        );

        // Accepted as a paging start key, which reports what follows it.
        let (status, paged) = list_tags(&emulator, &stream, Some(&key), None).await;
        assert_eq!(status, 200, "case {case:?}: start key got {paged:?}");
        assert!(
            listed(&paged).is_empty(),
            "case {case:?}: nothing follows the stream's only key, got {paged:?}"
        );

        // Removed through `UntagDeliveryStream`.
        let (status, response) = emulator
            .call(
                "UntagDeliveryStream",
                json!({ "DeliveryStreamName": stream, "TagKeys": [key] }),
            )
            .await;
        assert_eq!(status, 200, "case {case:?}: untag got {response:?}");
        let (_, after_untag) = list_tags(&emulator, &stream, None, None).await;
        assert!(
            listed(&after_untag).is_empty(),
            "case {case:?}: the key is gone, got {after_untag:?}"
        );
    }
}

#[tokio::test]
async fn a_key_beginning_with_the_reserved_prefix_is_refused_as_the_request_asked_for_it() {
    // The namespace the look-ahead excludes is refused BY WHAT the request asks: addressing
    // a stored tag set with such a key is the request-shape constraint the expression
    // states, while creating a stream that would CARRY it is the service's own tag policy —
    // the modelled argument error and the one sentence that states the namespace.
    let emulator = Emulator::new();
    create_tagged(&emulator, "reserved-prefix-stream", &owned(&[("env", "prod")])).await;

    for (reason, key) in keys_carrying_the_reserved_prefix() {
        for (expected, operation, request) in requests_carrying("reserved-prefix-stream", &key) {
            let case = format!("{operation} with {reason} ({key:?})");
            let (status, refused) = emulator.call(operation, request).await;
            assert_eq!(status, 400, "case {case:?}: got {refused:?}");
            let code = error_code(&refused);
            assert_eq!(
                code, expected.code,
                "case {case:?}: expected {:?}, got {code:?} in {refused:?}",
                expected.code
            );
            let message = error_message(&refused);
            assert_eq!(
                message, expected.message,
                "case {case:?}: expected {:?}, got {message:?}",
                expected.message
            );
            // The refused key is the caller's own value and no part of either sentence is
            // built from it — what follows the prefix reaches the body nowhere.
            let beyond_the_prefix = key.trim_start_matches("aws:");
            assert!(
                beyond_the_prefix.is_empty() || !message.contains(beyond_the_prefix),
                "case {case:?}: expected no rejected key in the message, got {message:?}"
            );
        }
    }

    // Nothing any of them asked for happened: the stream keeps the tags it had, and the
    // create that carried such a key made no stream.
    let (_, after) = list_tags(&emulator, "reserved-prefix-stream", None, None).await;
    assert_eq!(
        listed(&after),
        owned(&[("env", "prod")]),
        "a refused request wrote nothing, got {after:?}"
    );
    let (status, absent) = list_tags(&emulator, "reserved-prefix-stream-created", None, None).await;
    assert_eq!(status, 400, "the refused create stored nothing: got {absent:?}");
    assert_eq!(
        error_code(&absent),
        "ResourceNotFoundException",
        "a refused create persists no stream, got {absent:?}"
    );
}

#[tokio::test]
async fn a_create_carrying_a_reserved_key_is_refused_by_the_tag_policy_and_creates_nothing() {
    // The create's own axis, over a SET of tag sets differing in WHERE the reserved key
    // sits, how many of them there are, what follows the prefix and what ordinary keys
    // stand beside it: one refusal — the modelled argument error and one fixed sentence —
    // and no stream, whichever row it is.
    //
    // (case, the keys the create carries)
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("the bare prefix as the only key", vec!["aws:"]),
        ("the only key", vec!["aws:reserved"]),
        ("a service's own key", vec!["aws:cloudformation:stack-name"]),
        ("a Unicode key in the namespace", vec!["aws:ключ"]),
        ("the first of two", vec!["aws:reserved", "env"]),
        ("the last of three", vec!["env", "team", "aws:reserved"]),
        ("two of three", vec!["aws:one", "env", "aws:two"]),
        ("every key", vec!["aws:one", "aws:two"]),
    ];

    let emulator = Emulator::new();
    let account = engine::emulator_account_id();
    let not_found =
        crate::stream_not_found_message::NotFoundWording::ControlPlaneRead.sentence(account);

    for (index, (case, keys)) in cases.into_iter().enumerate() {
        let stream = format!("policy-refused-{index}");
        let (status, refused) = emulator
            .call(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "S3DestinationConfiguration": s3_configuration(),
                    "Tags": keys
                        .iter()
                        .map(|key| json!({ "Key": key, "Value": "v" }))
                        .collect::<Vec<Value>>(),
                }),
            )
            .await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        let code = error_code(&refused);
        assert_eq!(
            code, TAG_POLICY_CODE,
            "case {case:?}: expected {TAG_POLICY_CODE:?}, got {code:?} in {refused:?}"
        );
        // One sentence however many such keys the request carried, and nothing of the
        // request in it: no key, no stream name, and none of the validation-error envelope
        // a request-shape refusal carries.
        let message = error_message(&refused);
        assert_eq!(
            message, RESERVED_TAG_KEYS_MESSAGE,
            "case {case:?}: expected {RESERVED_TAG_KEYS_MESSAGE:?}, got {message:?}"
        );
        assert!(
            !message.contains(&stream) && !message.contains("validation error"),
            "case {case:?}: expected nothing of the request in the sentence, got {message:?}"
        );

        // No stream was created: the read states the account the lookup was scoped to.
        let (status, listed_now) = emulator
            .call(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        assert_eq!(status, 400, "case {case:?}: the describe got {listed_now:?}");
        let code = error_code(&listed_now);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected \"ResourceNotFoundException\", got {code:?}"
        );
        let message = error_message(&listed_now);
        assert_eq!(
            message, not_found,
            "case {case:?}: expected {not_found:?}, got {message:?}"
        );
    }

    // The positive control: the same create with ordinary keys — one of them CONTAINING
    // the reserved prefix without opening with it — is accepted, settles at the status the
    // clock owes it and holds exactly those tags, so the namespace is all the policy
    // refuses.
    create_tagged(
        &emulator,
        "policy-admitted",
        &owned(&[("owner", "team"), ("my-aws:key", "y")]),
    )
    .await;
    emulator.await_active("policy-admitted").await;
    let (status, listed_now) = list_tags(&emulator, "policy-admitted", None, None).await;
    assert_eq!(status, 200, "the admitted create: got {listed_now:?}");
    assert_eq!(
        listed(&listed_now),
        owned(&[("my-aws:key", "y"), ("owner", "team")]),
        "the admitted create's tags read back, got {listed_now:?}"
    );
}

#[tokio::test]
async fn the_length_bound_is_reported_as_the_separate_constraint_it_is() {
    let emulator = Emulator::new();
    create_tagged(&emulator, "bounded-key-stream", &owned(&[("env", "prod")])).await;

    // `TagKey` is `@length(1..128)` as well as patterned
    // (`smithy_query(query="shape",shape="TagKey")`), so a key past that bound is refused
    // under the LENGTH constraint, not the pattern one — and a key AT the bound is
    // accepted (`a_key_the_modelled_pattern_admits_round_trips_through_every_member`).
    let (status, refused) = emulator
        .call(
            "TagDeliveryStream",
            json!({
                "DeliveryStreamName": "bounded-key-stream",
                "Tags": [{ "Key": "k".repeat(129), "Value": "v" }],
            }),
        )
        .await;
    assert_eq!(status, 400, "a 129-character key: got {refused:?}");
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "a 129-character key: got {refused:?}"
    );
    let expected = "1 validation error detected: Value at 'tags.1.member.key' failed to \
                    satisfy constraint: Member must have length less than or equal to 128";
    let message = error_message(&refused);
    assert_eq!(
        message, expected,
        "a 129-character key: expected {expected:?}, got {message:?}"
    );

    // The same key as an ELEMENT of `TagKeys` has no member name of its own, so it is
    // reported against the list with the element's whole constraint set — whichever of
    // those constraints it violated.
    let (status, refused) = emulator
        .call(
            "UntagDeliveryStream",
            json!({
                "DeliveryStreamName": "bounded-key-stream",
                "TagKeys": ["k".repeat(129)],
            }),
        )
        .await;
    assert_eq!(status, 400, "a 129-character key in TagKeys: {refused:?}");
    let expected = element_refusal("tagKeys");
    let message = error_message(&refused);
    assert_eq!(
        message, expected,
        "a 129-character key in TagKeys: expected {expected:?}, got {message:?}"
    );
}

#[tokio::test]
async fn a_list_of_tag_keys_is_reported_once_however_many_of_its_elements_are_refused() {
    let emulator = Emulator::new();
    create_tagged(&emulator, "listed-key-stream", &owned(&[("env", "prod")])).await;

    // (case, the keys the untag names)
    //
    // Whichever clause of the modelled expression an element breaks, and however many
    // elements break one, the list is reported once — against the list member, with the
    // whole constraint set its elements carry.
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("the only key", vec!["bad*one"]),
        ("the second of three", vec!["env", "bad*two", "three"]),
        ("the last of three", vec!["env", "two", "bad*three"]),
        ("two of three", vec!["bad*one", "env", "bad*three"]),
        ("every key", vec!["bad*one", "bad*two"]),
        ("a reserved-prefix key alone", vec!["aws:one"]),
        ("a reserved-prefix key beside a carried one", vec!["env", "aws:two"]),
        (
            "a reserved-prefix key beside one outside the class",
            vec!["aws:one", "bad*two"],
        ),
    ];

    for (case, keys) in cases {
        let (status, refused) = emulator
            .call(
                "UntagDeliveryStream",
                json!({ "DeliveryStreamName": "listed-key-stream", "TagKeys": keys }),
            )
            .await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        let expected = element_refusal("tagKeys");
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // Nothing was removed by any of them.
    let (_, after) = list_tags(&emulator, "listed-key-stream", None, None).await;
    assert_eq!(
        listed(&after),
        owned(&[("env", "prod")]),
        "a refused untag removes nothing, got {after:?}"
    );
}

#[tokio::test]
async fn a_refused_key_is_reported_at_the_position_it_was_submitted_at() {
    let emulator = Emulator::new();
    create_tagged(&emulator, "positioned-stream", &owned(&[("env", "prod")])).await;

    // (case, the keys the request carries, the paths the refusal names)
    let cases: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("the only key", vec!["bad*one"], vec!["tags.1.member.key"]),
        (
            "the second of three",
            vec!["first", "bad*second", "third"],
            vec!["tags.2.member.key"],
        ),
        (
            "the last of three",
            vec!["first", "second", "bad*third"],
            vec!["tags.3.member.key"],
        ),
        (
            "two of three",
            vec!["bad*first", "second", "bad*third"],
            vec!["tags.1.member.key", "tags.3.member.key"],
        ),
        (
            "a reserved-prefix key, second of two",
            vec!["first", "aws:second"],
            vec!["tags.2.member.key"],
        ),
        (
            "one key per clause of the expression",
            vec!["bad*first", "aws:second"],
            vec!["tags.1.member.key", "tags.2.member.key"],
        ),
    ];

    for (case, keys, paths) in cases {
        let (status, refused) = emulator
            .call(
                "TagDeliveryStream",
                json!({
                    "DeliveryStreamName": "positioned-stream",
                    "Tags": keys
                        .iter()
                        .map(|key| json!({ "Key": key, "Value": "v" }))
                        .collect::<Vec<Value>>(),
                }),
            )
            .await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        let message = error_message(&refused);
        // Every violating key is reported, in the order the request carried them — the
        // way the request boundary accumulates violations rather than stopping at one.
        let clauses: Vec<String> = paths
            .iter()
            .map(|path| {
                format!(
                    "Value at '{path}' failed to satisfy constraint: {}",
                    must_match_tag_key_pattern()
                )
            })
            .collect();
        let expected = match clauses.len() {
            1 => format!("1 validation error detected: {}", clauses[0]),
            count => format!(
                "{count} validation errors detected: {}",
                clauses.join("; ")
            ),
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_request_refused_for_its_tag_key_writes_nothing() {
    let emulator = Emulator::new();
    let held = owned(&[("env", "prod"), ("app", "api")]);
    create_tagged(&emulator, "untouched-stream", &held).await;

    // A tag naming one legal and one illegal key adds neither.
    let (status, refused) = emulator
        .call(
            "TagDeliveryStream",
            json!({
                "DeliveryStreamName": "untouched-stream",
                "Tags": [
                    { "Key": "zone", "Value": "c" },
                    { "Key": "aws:owner", "Value": "x" },
                ],
            }),
        )
        .await;
    assert_eq!(status, 400, "a mixed tag request: got {refused:?}");
    let (_, after_tag) = list_tags(&emulator, "untouched-stream", None, None).await;
    assert_eq!(
        listed(&after_tag),
        owned(&[("app", "api"), ("env", "prod")]),
        "a refused tag adds nothing, got {after_tag:?}"
    );

    // An untag naming one carried and one illegal key removes neither.
    let (status, refused) = emulator
        .call(
            "UntagDeliveryStream",
            json!({
                "DeliveryStreamName": "untouched-stream",
                "TagKeys": ["env", "aws:owner"],
            }),
        )
        .await;
    assert_eq!(status, 400, "a mixed untag request: got {refused:?}");
    let (_, after_untag) = list_tags(&emulator, "untouched-stream", None, None).await;
    assert_eq!(
        listed(&after_untag),
        owned(&[("app", "api"), ("env", "prod")]),
        "a refused untag removes nothing, got {after_untag:?}"
    );

    // A create carrying an illegal tag key creates no stream at all, whichever of the two
    // faults the key carries: a key in the reserved namespace is the tag policy, a key
    // outside the character class is the request's shape.
    for (case, key) in [
        ("a reserved key", "aws:owner"),
        ("a key outside the character class", "bad*owner"),
    ] {
        let (status, refused) = emulator
            .call(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": "never-created-stream",
                    "S3DestinationConfiguration": s3_configuration(),
                    "Tags": [{ "Key": key, "Value": "x" }],
                }),
            )
            .await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        let expected = create_refusal(key);
        let code = error_code(&refused);
        assert_eq!(
            code, expected.code,
            "case {case:?}: expected {:?}, got {code:?} in {refused:?}",
            expected.code
        );
        let message = error_message(&refused);
        assert_eq!(
            message, expected.message,
            "case {case:?}: expected {:?}, got {message:?}",
            expected.message
        );
    }
    let (status, absent) = list_tags(&emulator, "never-created-stream", None, None).await;
    assert_eq!(status, 400, "the stream was never created: got {absent:?}");
    assert_eq!(
        error_code(&absent),
        "ResourceNotFoundException",
        "a refused create persists no stream, got {absent:?}"
    );
}
