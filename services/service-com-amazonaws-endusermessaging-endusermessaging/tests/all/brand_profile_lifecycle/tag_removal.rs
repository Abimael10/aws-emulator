// SPDX-License-Identifier: CC0-1.0

//! `UntagResource`: the tag REMOVAL, over the wire.
//!
//! A child suite of `brand_profile_lifecycle`, which owns the tag READ and the ARN
//! resolution every tagging call shares, beside `tag_writes`, which owns the write;
//! this file owns what a removal does to the stored tags. Declared by its parent,
//! because this crate's one test target declares the suites directly under `tests/all/`.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a removal takes off the resource exactly the keys its `tagKeys` names and leaves
//!   every key it does not name — and that key's value — alone, for every taggable
//!   resource type of this service, and `ListTagsForResource` reports what is left;
//! * a key is matched exactly and case-sensitively (`TagKey`'s own documentation), so a
//!   key differing only in case is another tag and stays;
//! * the removal is by key SET, so a key the resource does not carry removes nothing, a
//!   key named twice in one request removes the same one tag, and asking again for a
//!   removal already applied leaves the same state — which is what this operation's
//!   `@idempotent` trait says a caller may rely on;
//! * a removal reaches only the resource its `resourceArn` names, and an ARN that
//!   reaches no resource of the request is refused by the shared resolution — exactly
//!   as the read and the write are — with nothing removed.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::json;

use super::tag_writes::{listed_tags, pair, tagged_configuration, tagged_profile};
use super::{sorted, Emulator, Reply, ACCOUNT_ID, SIGNING_NAME};

/// `value` as one query-parameter value of a request URI.
///
/// A tag key may carry any character (`TagKey`'s modelled `^.+$`), so a space, a colon,
/// a `/` or a literal `+` has to survive the round trip as itself: everything outside
/// the alphanumerics is percent-encoded, which is what an SDK client sends for a
/// `@httpQuery` value.
fn query_value(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// `UntagResource`. `resourceArn` is a non-greedy `@httpLabel`, so the ARN travels
/// percent-encoded as one path segment; `tagKeys` is an `@httpQuery` LIST, so each key
/// travels as its own repetition of that parameter.
async fn untag(em: &Emulator, resource_arn: &str, keys: &[String]) -> Reply {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", query_value(key)))
        .collect();
    untag_raw(em, resource_arn, &query.join("&")).await
}

/// `UntagResource` with the query string exactly as given, for a `tagKeys` no key vector
/// can express (none at all, or one the model refuses).
async fn untag_raw(em: &Emulator, resource_arn: &str, query: &str) -> Reply {
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };
    em.call(
        "DELETE",
        &format!(
            "/v1/tags/{}{suffix}",
            crate::brand_profile_default_attributes::path_label(resource_arn)
        ),
        None,
    )
    .await
}

/// One removal row: the tags the resource is created with, the keys one removal names,
/// and the set the resource carries afterwards.
struct Removal {
    case: &'static str,
    stored: Vec<(String, String)>,
    removed: Vec<String>,
    expected: Vec<(String, String)>,
}

fn key(name: &str) -> String {
    name.to_string()
}

/// The rows span what a removal has to decide: one key of several, every key at once,
/// several of many in one request, a key the resource does not carry, a key named twice,
/// a key differing from a stored one only in case — which `TagKey` ("case-sensitive")
/// makes another tag — a removal naming nothing the resource holds, a key whose stored
/// value is the empty string `TagValue`'s `@length(min: 0)` allows, a key at
/// `TagKey`'s maximum modelled length of 128, and a key spelled with characters a query
/// string has to encode.
fn removals() -> Vec<Removal> {
    vec![
        Removal {
            case: "one key of several",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            removed: vec![key("env")],
            expected: vec![pair("team", "messaging")],
        },
        Removal {
            case: "every key the resource carries",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            removed: vec![key("env"), key("team")],
            expected: Vec::new(),
        },
        Removal {
            case: "two keys of four, in an order the stored list does not use",
            stored: vec![
                pair("env", "prod"),
                pair("team", "messaging"),
                pair("cost-center", "42"),
                pair("owner", "ada"),
            ],
            removed: vec![key("owner"), key("env")],
            expected: vec![pair("team", "messaging"), pair("cost-center", "42")],
        },
        Removal {
            case: "a key the resource does not carry",
            stored: vec![pair("env", "prod")],
            removed: vec![key("team")],
            expected: vec![pair("env", "prod")],
        },
        Removal {
            case: "a key it carries beside one it does not",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            removed: vec![key("team"), key("cost-center")],
            expected: vec![pair("env", "prod")],
        },
        Removal {
            case: "a key named twice in one request",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            removed: vec![key("env"), key("env")],
            expected: vec![pair("team", "messaging")],
        },
        Removal {
            case: "a key differing from a stored key only in case",
            stored: vec![pair("env", "prod"), pair("Env", "staging")],
            removed: vec![key("Env")],
            expected: vec![pair("env", "prod")],
        },
        Removal {
            case: "the same key in the other casing",
            stored: vec![pair("env", "prod"), pair("Env", "staging")],
            removed: vec![key("env")],
            expected: vec![pair("Env", "staging")],
        },
        Removal {
            case: "a key of a resource carrying nothing",
            stored: Vec::new(),
            removed: vec![key("env")],
            expected: Vec::new(),
        },
        Removal {
            case: "a key whose stored value is the empty string",
            stored: vec![pair("env", ""), pair("team", "messaging")],
            removed: vec![key("env")],
            expected: vec![pair("team", "messaging")],
        },
        Removal {
            case: "a key at the maximum modelled length",
            stored: vec![pair("env", "prod"), ("k".repeat(128), "v".repeat(256))],
            removed: vec!["k".repeat(128)],
            expected: vec![pair("env", "prod")],
        },
        Removal {
            case: "a key spelled with characters a query string encodes",
            stored: vec![pair("my key:1+2/3", "x"), pair("env", "prod")],
            removed: vec![key("my key:1+2/3")],
            expected: vec![pair("env", "prod")],
        },
        Removal {
            case: "a key that is a prefix of another stored key",
            stored: vec![pair("env", "prod"), pair("environment", "production")],
            removed: vec![key("env")],
            expected: vec![pair("environment", "production")],
        },
    ]
}

#[tokio::test]
async fn a_removal_takes_only_the_keys_it_names() {
    for removal in removals() {
        let em = Emulator::new();
        // A second, differently tagged profile stands beside each row, so a removal that
        // reached the store rather than this resource would fail.
        let neighbour = tagged_profile(&em, "other-profile", &[pair("env", "other")]).await;
        let arn = tagged_profile(&em, "tagged-profile", &removal.stored).await;

        let removed = untag(&em, &arn, &removal.removed).await;
        assert_eq!(
            removed.status,
            200,
            "case {}: UntagResource must succeed, got {} / {:?}",
            removal.case,
            removed.status,
            removed.body
        );

        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&removal.expected),
            "case {}: unexpected tags after the removal",
            removal.case
        );
        assert_eq!(
            listed_tags(&em, &neighbour).await,
            vec![pair("env", "other")],
            "case {}: a removal must leave another resource's tags alone",
            removal.case
        );

        // The same request again is the same state: the removal is by key set, so a key
        // already gone is not an error and nothing else moves.
        let again = untag(&em, &arn, &removal.removed).await;
        assert_eq!(
            again.status,
            200,
            "case {}: a repeated removal must succeed, got {} / {:?}",
            removal.case,
            again.status,
            again.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&removal.expected),
            "case {}: a repeated removal must leave the same tags",
            removal.case
        );
    }
}

#[tokio::test]
async fn a_removal_reaches_every_taggable_resource_type() {
    // The rule is about the resource an ARN names, not about one resource type, so the
    // notify code configuration answers it exactly as a brand profile does.
    for removal in removals() {
        let em = Emulator::new();
        let arn = tagged_configuration(&em, "tagged-configuration", &removal.stored).await;

        let removed = untag(&em, &arn, &removal.removed).await;
        assert_eq!(
            removed.status,
            200,
            "case {}: UntagResource must succeed, got {} / {:?}",
            removal.case,
            removed.status,
            removed.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&removal.expected),
            "case {}: unexpected tags after the removal",
            removal.case
        );
    }
}

#[tokio::test]
async fn a_removal_leaves_the_values_of_the_keys_it_keeps_alone() {
    // A write after a removal still sees the keys that stayed: the removal takes pairs
    // out of the one stored list, so what is left is what a later merge merges into.
    let em = Emulator::new();
    let stored = vec![
        pair("env", "prod"),
        pair("team", "messaging"),
        pair("owner", "ada"),
    ];
    let arn = tagged_profile(&em, "removal-then-write", &stored).await;

    assert_eq!(
        untag(&em, &arn, &[key("team")]).await.status,
        200,
        "the removal must succeed"
    );
    let written = em
        .call(
            "POST",
            &format!(
                "/v1/tags/{}",
                crate::brand_profile_default_attributes::path_label(&arn)
            ),
            Some(json!({ "tags": [
                { "key": "env", "value": "staging" },
                { "key": "team", "value": "messaging-again" },
            ] })),
        )
        .await;
    assert_eq!(
        written.status, 200,
        "a write after a removal must succeed, got {} / {:?}",
        written.status, written.body
    );
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&[
            pair("env", "staging"),
            pair("owner", "ada"),
            pair("team", "messaging-again"),
        ]),
        "the write must replace the key that stayed and re-add the one that was removed"
    );
}

/// The ARNs that reach no resource of the request, with the refusal each answers:
/// a value naming another vendor, and a well-formed ARN of this service naming a
/// resource this store does not hold.
#[tokio::test]
async fn an_arn_that_reaches_no_resource_is_refused_and_removes_nothing() {
    let em = Emulator::new();
    let stored = vec![pair("env", "prod"), pair("team", "messaging")];
    let arn = tagged_profile(&em, "untouched-profile", &stored).await;

    // (case, resourceArn, expected status, expected error code)
    let unreachable: Vec<(&str, String, u16, &str)> = vec![
        (
            "an ARN of another vendor",
            format!("arn:aws:s3:us-east-1:{ACCOUNT_ID}:bucket/some-bucket"),
            400,
            "ValidationException",
        ),
        (
            "an ARN of another vendor with no Region or account",
            "arn:aws:s3:::another-bucket".to_string(),
            400,
            "ValidationException",
        ),
        (
            "an ARN of another partition",
            format!("arn:aws-cn:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000"),
            400,
            "ValidationException",
        ),
        (
            "an ARN of another Region",
            format!("arn:aws:{SIGNING_NAME}:eu-west-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000"),
            400,
            "ValidationException",
        ),
        (
            "an unknown brand profile id",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000"),
            404,
            "ResourceNotFoundException",
        ),
        (
            "an unknown configuration id",
            format!(
                "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:notify-code-configuration/ncc-0000000000"
            ),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a resource type this service holds nothing for",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:phone-number/pn-1234"),
            404,
            "ResourceNotFoundException",
        ),
        (
            "an ARN of another account",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:000000000000:brand-profile/bp-00000000000000000"),
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (case, resource_arn, status, code) in unreachable {
        let refused = untag(&em, &resource_arn, &[key("env")]).await;
        assert_eq!(
            refused.status, status,
            "case {case} ({resource_arn}): expected {status}, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some(code),
            "case {case} ({resource_arn}): expected {code}, got {:?}",
            refused.error
        );
        // The refusal is the shared resolution's, so the removal answers an unreachable
        // ARN exactly as the read does.
        let read = em.list_tags(&resource_arn).await;
        assert_eq!(
            (read.status, read.error.as_deref()),
            (refused.status, refused.error.as_deref()),
            "case {case} ({resource_arn}): the read and the removal must refuse it alike, \
             got {:?} and {:?}",
            read.body,
            refused.body
        );
        assert_eq!(
            read.string("message"),
            refused.string("message"),
            "case {case} ({resource_arn}): the read and the removal must word it alike, \
             got {:?} and {:?}",
            read.body,
            refused.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&stored),
            "case {case}: a refused removal must leave the stored tags alone"
        );
    }

    // A resource whose record is gone is unreachable too, and its ARN is the one the
    // store minted.
    let doomed = tagged_profile(&em, "deleted-profile", &stored).await;
    assert_eq!(
        em.delete(&doomed).await.status,
        200,
        "deleting the profile must succeed"
    );
    let refused = untag(&em, &doomed, &[key("env")]).await;
    assert_eq!(
        refused.status, 404,
        "the ARN of a deleted resource is ResourceNotFoundException (404), got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ResourceNotFoundException"),
        "unexpected error code, got {:?}",
        refused.error
    );
}

#[tokio::test]
async fn a_request_naming_no_usable_tag_key_is_refused_and_removes_nothing() {
    // `tagKeys` is `@required` with `@length(min: 1)` over members of `@length(1..128)`,
    // so a request carrying no key at all, or a key outside that window, is refused at
    // the boundary — before anything is removed.
    let em = Emulator::new();
    let stored = vec![pair("env", "prod")];
    let arn = tagged_profile(&em, "key-gate-profile", &stored).await;

    let too_long = query_value(&"k".repeat(129));
    // A request carrying NO `tagKeys` at all is not among the rows: that member is this
    // route's required query parameter, so such a request selects no operation of this
    // service — a routing question, which this suite does not own.
    let refusals: Vec<(&str, String)> = vec![
        ("an empty tag key", "tagKeys=".to_string()),
        (
            "a key past the maximum modelled length",
            format!("tagKeys={too_long}"),
        ),
        (
            "a stored key beside an empty one",
            "tagKeys=env&tagKeys=".to_string(),
        ),
    ];

    for (case, query) in refusals {
        let refused = untag_raw(&em, &arn, &query).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&stored),
            "case {case}: a refused request must leave the stored tags alone"
        );
    }

    // One key at each end of the modelled window is served: the boundary refuses what
    // lies outside it, not what lies on it.
    for length in [1, 128] {
        let present = format!("k{}", "e".repeat(length - 1));
        let on_the_bound = untag(&em, &arn, std::slice::from_ref(&present)).await;
        assert_eq!(
            on_the_bound.status, 200,
            "case {length}: a key of the modelled length must be served, got {} / {:?}",
            on_the_bound.status, on_the_bound.body
        );
    }
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&stored),
        "removing keys the resource does not carry must leave its tags alone"
    );
}
