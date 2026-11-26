// SPDX-License-Identifier: CC0-1.0

//! The tag-KEY rule of the tagging surface, asserted on every resource ARN form that
//! surface serves, end to end over HTTP.
//!
//! RULE: a tag key is judged by the key's OWN rule — the prefix AWS reserves (`aws:`,
//! compared without regard to case) takes one fixed sentence of its own, and a key
//! outside `TagKey`'s alphabet or length bounds takes the grammar's sentence
//! (`smithy_query(query="shape", shape="TagKey")`: `@length` 1..128, `@pattern`
//! `^(?!aws:)[ a-zA-Z+-=._:/]+$`) — so the SAME answer comes back whichever resource the
//! request addresses, because the key rule is a property of the key and not of the
//! resource. The surface addresses a resource by any ARN this service ISSUES for one
//! that carries tags, which is an API of either family and a channel namespace of an
//! `Api`; a namespace's ARN is longer than `ResourceArn`'s own `@length` window, so these
//! rows also pin that the key rule is REACHED for an ARN form the member's modelled
//! expression cannot state, rather than the addressing refusal being answered in its
//! place.
//!
//! The tables span what the rule generalises over:
//!
//! 1. the resource ARN forms — a `GraphqlApi`'s ARN, an `Api`'s, and the ARNs of
//!    namespaces whose names span `Namespace`'s own expression (one character, a
//!    hyphenated name, the longest name it admits) — crossed with
//!    the key classes the rule distinguishes: the reserved prefix in four spellings of
//!    its case, two characters outside the admitted alphabet, and a key past the
//!    modelled maximum length;
//! 2. the keys the rule ADMITS that sit closest to the reserved prefix (`aws` without
//!    its colon, a longer word beginning with those letters, in either case), plus a key
//!    on the modelled maximum length and one spanning the admitted alphabet — each
//!    written and removed on every form, so the refusals above are about the key rather
//!    than about the letters `aws` or about the route.
//!
//! No expected ARN is written by hand: every row reads the ARN the service answered for
//! the resource it created. Every assertion goes through the router the production
//! emulator mounts, so these exercise routing, the `@httpLabel` ARN binding, the
//! repeated `@httpQuery` key binding, restJson1 (de)serialization, the handlers and the
//! shared engine together. The harness is the error-contract suite's `call`/`router`
//! with the channel-namespace suite's create helpers and the tag suites' request
//! helpers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_namespace_tags::namespace_arn;
use crate::channel_namespaces::NAMESPACE_MAX_LEN;
use crate::error_contract::{call, router};
use crate::resource_tags::TAG_KEY_MAX_LEN;
use crate::tag_resource::{
    list_tags, tag_map, tag_resource, RESERVED_TAG_KEY_REFUSAL, TAG_KEY_MIN_LEN, TAG_WRITE_STATUS,
};
use crate::untag_resource::untag_resource;

/// How the surface answers a key the rule refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The reserved prefix, which the service states as one sentence of its own.
    Reserved,
    /// `TagKey`'s grammar, whose sentence names the rejected key.
    Grammar,
    /// A key the modelled length bounds rule out. On `TagResource` the keys are map
    /// keys, which the request boundary does not walk, so the handler states the
    /// grammar's sentence; on `UntagResource` they are a modelled list, so the boundary
    /// refuses the value in its own words. The status is the one answer both give.
    ByLength,
}

/// The sentence `TagKey`'s GRAMMAR is refused with: the rejected key, then the bounds
/// and the alphabet the key broke, so a caller can fix the request from the message
/// alone. Both published bounds are named, from the member's own `@length`.
fn grammar_refusal(key: &str) -> String {
    format!(
        "Invalid tag key {key}: a key must be between {TAG_KEY_MIN_LEN} and {TAG_KEY_MAX_LEN} \
         characters long, cannot begin with aws:, and can only contain letters, numbers, spaces \
         and the characters + - = . _ : /"
    )
}

/// The tags every resource below is created with, which no refused call may change.
fn baseline() -> Value {
    json!({ "env": "prod" })
}

/// One resource of every ARN form the tagging surface serves, each created with
/// [`baseline`], as (case, the ARN the service minted).
async fn served_arns(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let graphql = call(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": "key-rule-graphql",
            "authenticationType": "API_KEY",
            "tags": baseline(),
        })),
    )
    .await;
    assert_eq!(
        graphql.status, 200,
        "case create the GraphQL API: expected 200, got {graphql:?}"
    );
    let mut forms = vec![(
        "a GraphQL API's own ARN".to_string(),
        graphql.body["graphqlApi"]["arn"]
            .as_str()
            .expect("the created GraphQL API carries an ARN")
            .to_string(),
    )];

    let event = call(
        router,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "key-rule-events",
            "tags": baseline(),
            "eventConfig": {
                "authProviders": [{ "authType": "API_KEY" }],
                "connectionAuthModes": [{ "authType": "API_KEY" }],
                "defaultPublishAuthModes": [{ "authType": "API_KEY" }],
                "defaultSubscribeAuthModes": [{ "authType": "API_KEY" }],
            },
        })),
    )
    .await;
    assert_eq!(
        event.status, 200,
        "case create the Event API: expected 200, got {event:?}"
    );
    forms.push((
        "an Event API's own ARN".to_string(),
        event.body["api"]["apiArn"]
            .as_str()
            .expect("the created Event API carries an ARN")
            .to_string(),
    ));
    let api_id = event.body["api"]["apiId"]
        .as_str()
        .expect("the created Event API carries an id")
        .to_string();

    for (case, name) in [
        ("a namespace named as AppSync's default", "default".to_string()),
        ("a namespace whose name is one character", "n".to_string()),
        ("a namespace whose name carries a hyphen", "a-1".to_string()),
        (
            "a namespace whose name is the longest admitted",
            "n".repeat(NAMESPACE_MAX_LEN),
        ),
    ] {
        // The namespace suite's own helper, so one reading of what a create answers
        // serves both suites.
        let arn = namespace_arn(router, &api_id, &name, baseline()).await;
        forms.push((case.to_string(), arn));
    }
    forms
}

/// The key classes the rule refuses, and how it words each.
fn refused_keys() -> Vec<(String, String, Refusal)> {
    let mut rows: Vec<(String, String, Refusal)> = [
        // The reserved prefix, in every spelling of its case: the service states the
        // comparison as case-insensitive, so each is the same refusal.
        ("the reserved prefix alone", "aws:", Refusal::Reserved),
        ("a key under the reserved prefix", "aws:cost-center", Refusal::Reserved),
        (
            "the reserved prefix in upper case",
            "AWS:cost-center",
            Refusal::Reserved,
        ),
        (
            "the reserved prefix in mixed case",
            "Aws:billing",
            Refusal::Reserved,
        ),
        // Characters outside the alphabet `TagKey`'s pattern admits.
        ("a character outside the alphabet", "bad*key", Refusal::Grammar),
        ("a non-ASCII letter", "café", Refusal::Grammar),
    ]
    .into_iter()
    .map(|(case, key, refusal)| (case.to_string(), key.to_string(), refusal))
    .collect();
    rows.push((
        format!("a key one character over the maximum of {TAG_KEY_MAX_LEN}"),
        "k".repeat(TAG_KEY_MAX_LEN + 1),
        Refusal::ByLength,
    ));
    rows
}

/// The keys the rule ADMITS, including the ones nearest the reserved prefix.
fn admitted_keys() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = [
        // The reserved prefix is `aws:`: these only begin with those letters.
        ("the reserved letters without the colon", "aws"),
        ("a word beginning with the reserved letters", "awsome"),
        (
            "a word beginning with the reserved letters in upper case",
            "AWSOME",
        ),
        // The alphabet `TagKey`'s pattern admits: `[ a-zA-Z+-=._:/]`, whose `+-=` range
        // also admits the digits.
        (
            "a key spanning the admitted character classes",
            "Cost.center/sub:team key-01+2=3",
        ),
    ]
    .into_iter()
    .map(|(case, key)| (case.to_string(), key.to_string()))
    .collect();
    rows.push((
        format!("a key on the maximum length of {TAG_KEY_MAX_LEN}"),
        "k".repeat(TAG_KEY_MAX_LEN),
    ));
    rows
}

/// Rule 1: a key the rule refuses takes the same answer on every served ARN form, on the
/// write and on the removal, and changes nothing the resource holds.
#[tokio::test]
async fn a_refused_tag_key_answers_the_same_on_every_served_resource_arn_form() {
    let router = router();
    let forms = served_arns(&router).await;

    for (form, arn) in &forms {
        for (key_case, key, refusal) in refused_keys() {
            let case = format!("{form} with {key_case}");

            let written =
                tag_resource(&router, arn, tag_map(&[(key.clone(), "v".to_string())])).await;
            assert_eq!(
                written.status, 400,
                "case {case:?} ({arn}) on TagResource: expected status 400, got {written:?}"
            );
            assert_eq!(
                written.error_type, "BadRequestException",
                "case {case:?} on TagResource: expected BadRequestException, got {written:?}"
            );

            let removed = untag_resource(&router, arn, std::slice::from_ref(&key)).await;
            assert_eq!(
                removed.status, 400,
                "case {case:?} ({arn}) on UntagResource: expected status 400, got {removed:?}"
            );

            match refusal {
                Refusal::Reserved => {
                    for (operation, answer) in [("TagResource", &written), ("UntagResource", &removed)] {
                        assert_eq!(
                            (answer.error_type.as_str(), answer.message.as_str()),
                            ("BadRequestException", RESERVED_TAG_KEY_REFUSAL),
                            "case {case:?} on {operation}: expected the reserved-prefix \
                             sentence {RESERVED_TAG_KEY_REFUSAL:?}, got {answer:?}"
                        );
                    }
                }
                Refusal::Grammar => {
                    let expected = grammar_refusal(&key);
                    for (operation, answer) in [("TagResource", &written), ("UntagResource", &removed)] {
                        assert_eq!(
                            (answer.error_type.as_str(), answer.message.as_str()),
                            ("BadRequestException", expected.as_str()),
                            "case {case:?} on {operation}: expected the grammar's sentence \
                             {expected:?}, got {answer:?}"
                        );
                    }
                }
                Refusal::ByLength => {
                    // The write states the grammar's sentence; the removal's keys are a
                    // modelled list, so the boundary words that one and only the status
                    // is shared.
                    let expected = grammar_refusal(&key);
                    assert_eq!(
                        written.message, expected,
                        "case {case:?} on TagResource: expected the grammar's sentence \
                         {expected:?}, got {written:?}"
                    );
                }
            }
        }

        // Nothing any refusal above touched the stored map, on this form or any other.
        for (other_form, other_arn) in &forms {
            let listed = list_tags(&router, other_arn).await;
            assert_eq!(
                (listed.status, &listed.body["tags"]),
                (200, &baseline()),
                "case {form:?} then {other_form:?} ({other_arn}): expected the refused calls to \
                 leave the created tags, got {listed:?}"
            );
        }
    }
}

/// Rule 2: a key the rule admits — the ones nearest the reserved prefix included — is
/// written and removed on every served ARN form.
#[tokio::test]
async fn an_admitted_tag_key_round_trips_on_every_served_resource_arn_form() {
    let router = router();
    let forms = served_arns(&router).await;

    for (form, arn) in &forms {
        for (key_case, key) in admitted_keys() {
            let case = format!("{form} with {key_case}");

            let written =
                tag_resource(&router, arn, tag_map(&[(key.clone(), "set".to_string())])).await;
            assert_eq!(
                written.status, TAG_WRITE_STATUS,
                "case {case:?} ({arn}) on TagResource: expected {TAG_WRITE_STATUS}, got \
                 {written:?}"
            );

            let mut expected = baseline();
            expected[key.as_str()] = json!("set");
            let listed = list_tags(&router, arn).await;
            assert_eq!(
                (listed.status, &listed.body["tags"]),
                (200, &expected),
                "case {case:?} ({arn}): expected the written key in the stored map, got \
                 {listed:?}"
            );

            // The write reached the resource its ARN names and no other, so a namespace's
            // map is its own rather than the API's it hangs off.
            for (other_form, other_arn) in &forms {
                if other_arn == arn {
                    continue;
                }
                let other = list_tags(&router, other_arn).await;
                assert_eq!(
                    (other.status, &other.body["tags"]),
                    (200, &baseline()),
                    "case {case:?} then {other_form:?} ({other_arn}): expected the write to \
                     leave this resource's tags alone, got {other:?}"
                );
            }

            let removed = untag_resource(&router, arn, std::slice::from_ref(&key)).await;
            assert_eq!(
                removed.status, TAG_WRITE_STATUS,
                "case {case:?} ({arn}) on UntagResource: expected {TAG_WRITE_STATUS}, got \
                 {removed:?}"
            );
            let relisted = list_tags(&router, arn).await;
            assert_eq!(
                (relisted.status, &relisted.body["tags"]),
                (200, &baseline()),
                "case {case:?} ({arn}): expected the removal to leave the created tags, got \
                 {relisted:?}"
            );
        }
    }
}
