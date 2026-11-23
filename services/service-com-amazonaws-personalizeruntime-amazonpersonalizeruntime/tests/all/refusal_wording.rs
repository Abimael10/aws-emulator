// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the two refusals a request can earn before any catalog is
//! read, over a table of inputs rather than one recorded request:
//!
//! 1. A value outside the modelled `@pattern` is a boundary `ValidationException`
//!    (HTTP 400) whose clause names the member path and the expression that value has to
//!    satisfy — the pattern WITHOUT the `^`/`$` anchors the model writes to make the match
//!    total — and echoes NO value. Amazon Personalize states every clause that way,
//!    whatever constraint the value broke and whether or not the model marks that member
//!    `@sensitive`.
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

/// The clause this service states for a value at `member` missing a modelled `@pattern`
/// whose anchor-free rendering is `expression`: the member and the expression it has to
/// satisfy, and no value.
fn pattern_fragment(member: &str, expression: &str) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {expression}"
    )
}

/// [`pattern_fragment`] inside the envelope of a request whose only violation it is.
fn pattern_clause(member: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: {}",
        pattern_fragment(member, expression)
    )
}

/// A well-formed filter ARN, for a row whose subject is some OTHER member.
const FILTER: &str = "arn:aws:personalize:us-east-1:000000000000:filter/any";

/// A well-formed campaign ARN, for a row whose subject is some OTHER member.
const CAMPAIGN: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/any";

#[tokio::test]
async fn a_value_outside_the_arn_pattern_names_the_member_and_the_unanchored_pattern() {
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
                pattern_clause(member, ARN_PATTERN_UNANCHORED),
                "case {route} {member} {form}: expected the clause to name the member and the \
                 unanchored pattern and to echo no value, got {:?}",
                refusal.message
            );
            assert!(
                !refusal.message.contains(value),
                "case {route} {member} {form}: the submitted value must not be echoed back, got \
                 {:?}",
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
            pattern_clause("promotions.1.member.name", NAME_PATTERN_UNANCHORED),
            "case promotion name with {form}: expected the clause to name the member and the \
             unanchored pattern and to echo no value, got {:?}",
            refusal.message
        );
    }
}

#[tokio::test]
async fn a_clause_about_a_sensitive_value_names_the_member_and_no_value() {
    let emulator = Emulator::new();

    // `Context`'s and `FilterValues`'s map VALUES are the shapes the model marks
    // `@sensitive`, and a clause about one reads exactly as every other clause of this
    // service does: the member path and the requirement, and no value. Each row sends one
    // character past that shape's `@length` ceiling, the one constraint those shapes carry.
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
async fn a_violation_of_another_class_states_the_member_and_no_value_too() {
    let emulator = Emulator::new();

    // `numResults` is modelled `range 0..`, so a negative value is a `@range` violation — a
    // different class from a `@pattern`, stated by the same rule: the member and the
    // requirement, and no value.
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
        format!("1 validation error detected: {RANGE_CLAUSE}"),
        "case negative numResults: expected the member and the requirement and no value, got {:?}",
        refusal.message
    );
}

/// One clause of a request that breaks `numResults`'s `@range` minimum.
const RANGE_CLAUSE: &str = "Value at 'numResults' failed to satisfy constraint: Member \
                            must have value greater than or equal to 0";

#[tokio::test]
async fn a_request_that_violates_two_classes_at_once_states_both_in_one_envelope() {
    let emulator = Emulator::new();

    // Two violations in ONE request: the envelope counts both and joins them with `; `, and
    // every clause reads the same way — the member path and the requirement, and no value —
    // whichever class it failed, whatever shape the member targets and whether or not the
    // model marks that value `@sensitive`. So one envelope never states two members two
    // different ways.
    let sensitive = "v".repeat(MAX_SENSITIVE_VALUE + 1);
    let rows: [(&str, serde_json::Value, String, String); 3] = [
        (
            "an ARN pattern and a range bound",
            json!({ "campaignArn": "not-an-arn", "userId": "user-1", "numResults": -1 }),
            pattern_fragment("campaignArn", ARN_PATTERN_UNANCHORED),
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
            pattern_fragment("promotions.1.member.name", NAME_PATTERN_UNANCHORED),
            RANGE_CLAUSE.to_string(),
        ),
        (
            "an ARN pattern and a sensitive length bound",
            json!({
                "campaignArn": "not-an-arn",
                "userId": "user-1",
                "context": { "GENRE": sensitive },
            }),
            pattern_fragment("campaignArn", ARN_PATTERN_UNANCHORED),
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

/// The modelled `@length` ceiling on `Name`, which `Promotion$name` targets
/// (`smithy_query(query="shape",shape="Name")` reports `length 1..63`).
const MAX_PROMOTION_NAME: usize = 63;

/// One boundary refusal: the request that earns it, the member path its clause names, the
/// requirement that clause states, and the bytes the request submitted there.
struct BoundaryClause {
    case: &'static str,
    route: &'static str,
    body: serde_json::Value,
    member: &'static str,
    requirement: String,
    /// What the request carried at that member, which no clause may quote back. `None` for
    /// a member the request did not carry at all.
    submitted: Option<String>,
}

#[tokio::test]
async fn every_boundary_clause_names_the_member_and_the_requirement_and_echoes_no_value() {
    // The rule, over the CLASSES of constraint a request can break on this service rather
    // than over one recorded request: whatever the class, whatever shape the member targets
    // and whether or not the model marks that value `@sensitive`, the clause names the member
    // path and the requirement and quotes no value — and a `@pattern` requirement is quoted
    // without the anchors the model writes.
    let emulator = Emulator::new();

    let sensitive = "v".repeat(MAX_SENSITIVE_VALUE + 1);
    let long_name = "a".repeat(MAX_PROMOTION_NAME + 1);
    let rows: Vec<BoundaryClause> = vec![
        BoundaryClause {
            case: "an ARN member outside its pattern",
            route: RECOMMENDATIONS,
            body: json!({ "campaignArn": "not-an-arn", "userId": "user-1" }),
            member: "campaignArn",
            requirement: format!(
                "Member must satisfy regular expression pattern: {ARN_PATTERN_UNANCHORED}"
            ),
            submitted: Some("not-an-arn".to_string()),
        },
        BoundaryClause {
            case: "another ARN member, on another operation, outside the same pattern",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "inputList": ["item-1"],
                "filterArn": "arn:aws:s3:::bucket/key",
            }),
            member: "filterArn",
            requirement: format!(
                "Member must satisfy regular expression pattern: {ARN_PATTERN_UNANCHORED}"
            ),
            submitted: Some("arn:aws:s3:::bucket/key".to_string()),
        },
        BoundaryClause {
            case: "a non-ARN member nested in a list, outside its own pattern",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "promotions": [{ "name": "_promo", "percentPromotedItems": 25 }],
            }),
            member: "promotions.1.member.name",
            requirement: format!(
                "Member must satisfy regular expression pattern: {NAME_PATTERN_UNANCHORED}"
            ),
            submitted: Some("_promo".to_string()),
        },
        BoundaryClause {
            case: "an integer member under its range minimum",
            route: ACTION_RECOMMENDATIONS,
            body: json!({ "campaignArn": CAMPAIGN, "userId": "user-1", "numResults": -5 }),
            member: "numResults",
            requirement: "Member must have value greater than or equal to 0".to_string(),
            submitted: Some("-5".to_string()),
        },
        BoundaryClause {
            case: "a string member past its length maximum",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "promotions": [{ "name": long_name.clone(), "percentPromotedItems": 25 }],
            }),
            member: "promotions.1.member.name",
            requirement: format!(
                "Member must have length less than or equal to {MAX_PROMOTION_NAME}"
            ),
            submitted: Some(long_name.clone()),
        },
        BoundaryClause {
            case: "a sensitive map value past its length maximum",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN,
                "userId": "user-1",
                "context": { "GENRE": sensitive.clone() },
            }),
            member: "context.GENRE",
            requirement: format!(
                "Member must have length less than or equal to {MAX_SENSITIVE_VALUE}"
            ),
            submitted: Some(sensitive.clone()),
        },
        BoundaryClause {
            case: "a required member the request left out",
            route: PERSONALIZE_RANKING,
            body: json!({ "campaignArn": CAMPAIGN, "userId": "user-1" }),
            member: "inputList",
            requirement: "Member must not be null".to_string(),
            submitted: None,
        },
    ];

    for row in rows {
        let submitted = row.submitted.clone();
        let refusal = emulator.refusal(row.route, row.body).await;
        assert_eq!(
            (
                refusal.status,
                refusal.code.as_str(),
                refusal.errortype.as_str()
            ),
            (400, "ValidationException", "ValidationException"),
            "case {}: expected the boundary's 400 ValidationException, got {refusal:?}",
            row.case
        );
        let expected = format!(
            "1 validation error detected: Value at '{}' failed to satisfy constraint: {}",
            row.member, row.requirement
        );
        assert_eq!(
            refusal.message, expected,
            "case {}: expected {expected:?}, got {:?}",
            row.case, refusal.message
        );
        if let Some(submitted) = submitted {
            assert!(
                !refusal.message.contains(&submitted),
                "case {}: the submitted value must never be quoted back, got {:?}",
                row.case,
                refusal.message
            );
        }
        for anchor in ["^arn", "^[a-zA-Z0-9]", ".+$", "*$"] {
            assert!(
                !refusal.message.contains(anchor),
                "case {}: a quoted pattern must carry no {anchor:?} anchor, got {:?}",
                row.case,
                refusal.message
            );
        }
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
