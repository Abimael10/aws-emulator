// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the two refusals a request can earn before any catalog is
//! read, over a table of inputs rather than one recorded request:
//!
//! 1. A value outside the modelled `@pattern` is a boundary `ValidationException`
//!    (HTTP 400) whose clause names the member path, the value the request carried and
//!    the expression that value has to satisfy — the pattern WITHOUT the `^`/`$` anchors
//!    the model writes to make the match total. The anchors are the ONLY thing this
//!    service changes about the clause: the rejected value is echoed for a `@pattern`
//!    exactly as it is for a violated `@range` bound, and is withheld only where the
//!    model marks the value `@sensitive`.
//! 2. `GetActionRecommendations` carries no `@required` on `campaignArn`, so a request
//!    that names no campaign reaches the handler; it is `InvalidInputException`
//!    (HTTP 400) reporting the campaign ARN the request gave — `null` for an absent
//!    member.

use serde_json::json;

use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    MAX_NUM_RESULTS, MAX_NUM_RESULTS_WITH_METADATA,
};

use crate::support::{Emulator, ACTION_RECOMMENDATIONS, PERSONALIZE_RANKING, RECOMMENDATIONS};

/// The modelled `@pattern` on `Arn`, which `campaignArn`, `recommenderArn` and
/// `filterArn` all target:
/// `smithy_query(query="operation",operation="GetRecommendations")` reports
/// `pattern: ^arn:([a-z\d-]+):personalize:.*:.*:.+$`.
const ARN_PATTERN: &str = r"^arn:([a-z\d-]+):personalize:.*:.*:.+$";

/// That same expression without its anchors — what the clause states.
const ARN_PATTERN_UNANCHORED: &str = r"arn:([a-z\d-]+):personalize:.*:.*:.+";

/// The modelled `@pattern` on `Name`, which `Promotion$name` targets:
/// `smithy_query(query="shape",shape="Name,Promotion")` reports
/// `pattern: ^[a-zA-Z0-9][a-zA-Z0-9\-_]*$` with `length 1..63`.
const NAME_PATTERN: &str = r"^[a-zA-Z0-9][a-zA-Z0-9\-_]*$";

/// That same expression without its anchors — what the clause states.
const NAME_PATTERN_UNANCHORED: &str = r"[a-zA-Z0-9][a-zA-Z0-9\-_]*";

/// The modelled `@length` ceiling on the map values of `Context` and `FilterValues`
/// (`smithy_query(query="shape",shape="Context,FilterValues")` reports
/// `AttributeValue` / `FilterAttributeValue` as `length 0..1000`, `sensitive: true`).
const MAX_SENSITIVE_VALUE: usize = 1000;

/// The clause this service states for `value` at `member` missing a modelled `@pattern`
/// whose anchor-free rendering is `expression`: the member, the value the request carried
/// and the expression it has to satisfy.
fn pattern_fragment(member: &str, value: &str, expression: &str) -> String {
    format!(
        "Value '{value}' at '{member}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {expression}"
    )
}

/// [`pattern_fragment`] inside the envelope of a request whose only violation it is.
fn pattern_clause(member: &str, value: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: {}",
        pattern_fragment(member, value, expression)
    )
}

/// A well-formed filter ARN, for a row whose subject is some OTHER member.
const FILTER: &str = "arn:aws:personalize:us-east-1:000000000000:filter/any";

/// A well-formed campaign ARN, for a row whose subject is some OTHER member.
const CAMPAIGN: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/any";

#[tokio::test]
async fn a_value_outside_the_arn_pattern_names_the_member_the_value_and_the_unanchored_pattern() {
    assert!(
        ARN_PATTERN == format!("^{ARN_PATTERN_UNANCHORED}$"),
        "the unanchored expression must be the modelled pattern minus its anchors, got \
         {ARN_PATTERN_UNANCHORED:?} against {ARN_PATTERN:?}"
    );
    let emulator = Emulator::new();

    // The rows vary the VALUE that misses the pattern and the MEMBER that carries it,
    // across every operation that binds an ARN member.
    let malformed = [
        ("not an ARN at all", "not-an-arn"),
        ("an ARN of another service", "arn:aws:s3:::bucket/key"),
        (
            "an uppercase partition",
            "arn:AWS:personalize:us-east-1:000000000000:campaign/x",
        ),
        (
            "no resource part",
            "arn:aws:personalize:us-east-1:000000000000:",
        ),
        ("an ARN prefix only", "arn:aws:personalize"),
    ];

    for (form, value) in malformed {
        let rows: [(&str, &str, serde_json::Value); 5] = [
            (
                "campaignArn",
                RECOMMENDATIONS,
                json!({ "campaignArn": value, "userId": "user-1" }),
            ),
            (
                "recommenderArn",
                RECOMMENDATIONS,
                json!({ "recommenderArn": value, "userId": "user-1" }),
            ),
            (
                "campaignArn",
                PERSONALIZE_RANKING,
                json!({ "campaignArn": value, "userId": "user-1", "inputList": ["item-1"] }),
            ),
            (
                "campaignArn",
                ACTION_RECOMMENDATIONS,
                json!({ "campaignArn": value, "userId": "user-1" }),
            ),
            (
                "filterArn",
                ACTION_RECOMMENDATIONS,
                json!({
                    "campaignArn": "arn:aws:personalize:us-east-1:000000000000:campaign/actions",
                    "userId": "user-1",
                    "filterArn": value,
                }),
            ),
        ];

        for (member, route, body) in rows {
            let refusal = emulator.refusal(route, body).await;
            assert_eq!(
                (
                    refusal.status,
                    refusal.code.as_str(),
                    refusal.errortype.as_str()
                ),
                (400, "ValidationException", "ValidationException"),
                "case {route} {member} {form}: expected 400 ValidationException, got {refusal:?}"
            );
            assert_eq!(
                refusal.message,
                pattern_clause(member, value, ARN_PATTERN_UNANCHORED),
                "case {route} {member} {form}: expected the clause to name the member, the value \
                 the request carried and the unanchored pattern, got {:?}",
                refusal.message
            );
        }
    }
}

#[tokio::test]
async fn a_value_outside_a_non_arn_pattern_is_stated_the_same_way_an_arn_is() {
    assert!(
        NAME_PATTERN == format!("^{NAME_PATTERN_UNANCHORED}$"),
        "the unanchored expression must be the modelled pattern minus its anchors, got \
         {NAME_PATTERN_UNANCHORED:?} against {NAME_PATTERN:?}"
    );
    let emulator = Emulator::new();

    // `Promotion$name` is the one member of this service whose `@pattern` the boundary
    // applies and whose shape is not a resource ARN, so it is where the subject half of the
    // clause is read off something other than an ARN. The rows vary the CHARACTER the value
    // breaks the expression with and WHERE it sits, and the last one is at the member's
    // modelled length ceiling, so neither the first-character class nor the repetition is
    // the only thing exercised.
    let malformed = [
        ("a leading hyphen", "-bad-name".to_string()),
        ("a leading underscore", "_promo".to_string()),
        ("an inner space", "bad name".to_string()),
        ("an inner dot", "bad.name".to_string()),
        ("a trailing exclamation mark", "promo!".to_string()),
        ("a non-ASCII letter", "prómo".to_string()),
        // A value carrying the characters that delimit the envelope it is quoted in: the
        // clause is written by the protocol's own error serializer, so a caller-influenced
        // value cannot break the body a client parses.
        ("a double quote and a backslash", "promo\"\\x".to_string()),
        ("a leading hyphen at the 63-character ceiling", {
            let mut value = String::from("-");
            value.push_str(&"a".repeat(62));
            value
        }),
    ];

    for (form, value) in malformed {
        let refusal = emulator
            .refusal(
                RECOMMENDATIONS,
                json!({
                    "campaignArn": CAMPAIGN,
                    "userId": "user-1",
                    "numResults": 3,
                    "promotions": [{
                        "name": value,
                        "percentPromotedItems": 50,
                        "filterArn": FILTER,
                    }],
                }),
            )
            .await;
        assert_eq!(
            (
                refusal.status,
                refusal.code.as_str(),
                refusal.errortype.as_str()
            ),
            (400, "ValidationException", "ValidationException"),
            "case promotion name with {form}: expected 400 ValidationException, got {refusal:?}"
        );
        assert_eq!(
            refusal.message,
            pattern_clause(
                "promotions.1.member.name",
                &value,
                NAME_PATTERN_UNANCHORED
            ),
            "case promotion name with {form}: expected the clause to name the member, the value \
             the request carried and the unanchored pattern, got {:?}",
            refusal.message
        );
    }
}

#[tokio::test]
async fn a_clause_about_a_sensitive_value_names_the_member_and_no_value() {
    let emulator = Emulator::new();

    // The exception to the echo, and the only one the model states: `Context`'s and
    // `FilterValues`'s map VALUES are `@sensitive`, so a clause about one names the member
    // path and withholds what the caller sent. Each row sends one character past that
    // shape's `@length` ceiling, the one constraint those shapes carry.
    let past_the_ceiling = "v".repeat(MAX_SENSITIVE_VALUE + 1);
    let rows: [(&str, &str, serde_json::Value); 3] = [
        (
            "context.GENRE",
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "context": { "GENRE": past_the_ceiling },
            }),
        ),
        (
            "filterValues.GENRE",
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "filterArn": FILTER,
                "filterValues": { "GENRE": past_the_ceiling },
            }),
        ),
        (
            "context.GENRE",
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "inputList": ["item-1"],
                "context": { "GENRE": past_the_ceiling },
            }),
        ),
    ];

    for (member, route, body) in rows {
        let refusal = emulator.refusal(route, body).await;
        assert_eq!(
            (refusal.status, refusal.code.as_str()),
            (400, "ValidationException"),
            "case {route} {member}: expected 400 ValidationException, got {refusal:?}"
        );
        assert_eq!(
            refusal.message,
            format!(
                "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
                 Member must have length less than or equal to {MAX_SENSITIVE_VALUE}"
            ),
            "case {route} {member}: expected the clause to name the member and withhold the \
             sensitive value, got {:?}",
            refusal.message
        );
        assert!(
            !refusal.message.contains(&past_the_ceiling),
            "case {route} {member}: a sensitive value must never be quoted back, got {:?}",
            refusal.message
        );
    }
}

#[tokio::test]
async fn a_violation_of_another_class_keeps_the_frameworks_clause() {
    let emulator = Emulator::new();

    // `numResults` is modelled `range 0..`, so a negative value is a `@range` violation
    // — a class this service does not reword, so its clause still echoes the value.
    let refusal = emulator
        .refusal(
            RECOMMENDATIONS,
            json!({
                "campaignArn": "arn:aws:personalize:us-east-1:000000000000:campaign/any",
                "userId": "user-1",
                "numResults": -1,
            }),
        )
        .await;
    assert_eq!(
        (refusal.status, refusal.code.as_str()),
        (400, "ValidationException"),
        "case negative numResults: expected 400 ValidationException, got {refusal:?}"
    );
    assert_eq!(
        refusal.message,
        "1 validation error detected: Value '-1' at 'numResults' failed to satisfy constraint: \
         Member must have value greater than or equal to 0",
        "case negative numResults: expected the framework's own clause, got {:?}",
        refusal.message
    );
}

/// One clause of a request that breaks `numResults`'s `@range` minimum, as the framework
/// states it.
const RANGE_CLAUSE: &str = "Value '-1' at 'numResults' failed to satisfy constraint: Member \
                            must have value greater than or equal to 0";

#[tokio::test]
async fn a_request_that_violates_two_classes_at_once_states_both_in_one_envelope() {
    let emulator = Emulator::new();

    // Two violations in ONE request: the envelope counts both and joins them with `; `, and
    // each clause carries the subject its own member earns — the value the request sent,
    // whichever class it failed and whatever shape the member targets, and no value at all
    // where the model marks the value `@sensitive`. So one envelope never states two
    // members of the same sensitivity two different ways.
    let sensitive = "v".repeat(MAX_SENSITIVE_VALUE + 1);
    let rows: [(&str, serde_json::Value, String, String); 3] = [
        (
            "an ARN pattern and a range bound",
            json!({ "campaignArn": "not-an-arn", "userId": "user-1", "numResults": -1 }),
            pattern_fragment("campaignArn", "not-an-arn", ARN_PATTERN_UNANCHORED),
            RANGE_CLAUSE.to_string(),
        ),
        (
            "a non-ARN pattern and a range bound",
            json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "numResults": -1,
                "promotions": [{ "name": "-bad-name", "percentPromotedItems": 50 }],
            }),
            pattern_fragment(
                "promotions.1.member.name",
                "-bad-name",
                NAME_PATTERN_UNANCHORED,
            ),
            RANGE_CLAUSE.to_string(),
        ),
        (
            "an ARN pattern and a sensitive length bound",
            json!({
                "campaignArn": "not-an-arn",
                "userId": "user-1",
                "context": { "GENRE": sensitive },
            }),
            pattern_fragment("campaignArn", "not-an-arn", ARN_PATTERN_UNANCHORED),
            format!(
                "Value at 'context.GENRE' failed to satisfy constraint: Member must have length \
                 less than or equal to {MAX_SENSITIVE_VALUE}"
            ),
        ),
    ];

    for (case, body, first, second) in rows {
        let refusal = emulator.refusal(RECOMMENDATIONS, body).await;
        assert_eq!(
            (refusal.status, refusal.code.as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException, got {refusal:?}"
        );
        // The walk's clause ORDER is the framework's, so either order satisfies the rule:
        // what this pins is the count, the joiner and both clauses.
        let either = [
            format!("2 validation errors detected: {first}; {second}"),
            format!("2 validation errors detected: {second}; {first}"),
        ];
        assert!(
            either.contains(&refusal.message),
            "case {case}: expected one of {either:?}, got {:?}",
            refusal.message
        );
    }
}

#[tokio::test]
async fn a_request_that_names_no_campaign_reports_the_campaign_arn_it_gave() {
    let emulator = Emulator::new();

    // `GetActionRecommendations$campaignArn` carries no `@required`, so an absent member
    // reaches the handler and is refused there, reporting what the request gave.
    let rows = [
        (
            "campaignArn absent",
            json!({ "userId": "user-1", "numResults": 3 }),
        ),
        (
            "campaignArn absent, nothing else given either",
            json!({}),
        ),
        (
            "campaignArn absent, every other member given",
            json!({
                "userId": "user-1",
                "numResults": 2,
                "filterArn": "arn:aws:personalize:us-east-1:000000000000:filter/any",
            }),
        ),
    ];

    for (case, body) in rows {
        let refusal = emulator.refusal(ACTION_RECOMMENDATIONS, body).await;
        assert_eq!(
            (
                refusal.status,
                refusal.code.as_str(),
                refusal.errortype.as_str()
            ),
            (400, "InvalidInputException", "InvalidInputException"),
            "case {case}: expected 400 InvalidInputException, got {refusal:?}"
        );
        assert_eq!(
            refusal.message, "The given campaign ARN is invalid: null",
            "case {case}: expected the refusal to report the ARN the request gave, got {:?}",
            refusal.message
        );
    }
}

/// A well-formed campaign ARN no registry holds, so a request that gets past the
/// request-only checks is refused for the campaign and nothing else.
const UNREGISTERED_CAMPAIGN: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/never-created";

#[tokio::test]
async fn an_input_list_past_its_documented_maximum_is_refused_before_the_campaign_resolves() {
    // "If you are including metadata in recommendations, the maximum is 50. Otherwise,
    // the maximum is 500." (`GetPersonalizedRankingRequest$inputList` doc.) The bound is
    // a fact about the REQUEST, so it is answered without resolving anything: at the
    // bound the request gets as far as the (unresolvable) campaign, one past it the
    // length is what is reported.
    let emulator = Emulator::new();

    let rows: [(&str, bool, usize); 4] = [
        ("without metadata, at the bound", false, MAX_NUM_RESULTS),
        ("without metadata, one past it", false, MAX_NUM_RESULTS + 1),
        (
            "with metadata, at the bound",
            true,
            MAX_NUM_RESULTS_WITH_METADATA,
        ),
        (
            "with metadata, one past it",
            true,
            MAX_NUM_RESULTS_WITH_METADATA + 1,
        ),
    ];

    for (case, with_metadata, length) in rows {
        let input_list: Vec<String> = (0..length).map(|index| format!("item-{index}")).collect();
        let max = if with_metadata {
            MAX_NUM_RESULTS_WITH_METADATA
        } else {
            MAX_NUM_RESULTS
        };
        let mut body = json!({
            "campaignArn": UNREGISTERED_CAMPAIGN,
            "userId": "user-1",
            "inputList": input_list,
        });
        if with_metadata {
            body["metadataColumns"] = json!({ "ITEMS": ["GENRE"] });
        }

        let refusal = emulator.refusal(PERSONALIZE_RANKING, body).await;
        assert_eq!(
            (refusal.status, refusal.code.as_str()),
            (400, "InvalidInputException"),
            "case {case}: expected 400 InvalidInputException, got {refusal:?}"
        );
        let expected = if length > max {
            format!("Length of 'inputList' cannot be greater than {max}")
        } else {
            format!("{UNREGISTERED_CAMPAIGN} does not exist or is not active yet.")
        };
        assert_eq!(
            refusal.message, expected,
            "case {case}: expected {expected:?}, got {:?}",
            refusal.message
        );
    }
}
