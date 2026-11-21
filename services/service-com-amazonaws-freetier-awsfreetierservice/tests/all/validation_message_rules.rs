// SPDX-License-Identifier: CC0-1.0

//! What this service's refusals SAY, driven over the wire through the same
//! multi-service router the emulator boots.
//!
//! A refusal's `message` is part of its contract: a client branches on it, logs it and
//! re-prompts from it, so a sentence this emulator wrote itself is a defect even when the
//! status and the error code are right. Two layers produce one, and each has its own rule:
//!
//! 1. a condition a HANDLER checks (the root-operator arity of a `filter`, the number of
//!    match options, a `nextToken` the service did not issue) is stated as ONE fixed
//!    sentence for the whole condition: nothing of the request appears in it — not the
//!    value submitted, not the member or operator the caller chose, not the dotted path
//!    the value arrived at — so every input that breaks the rule reads the same. It holds
//!    for EVERY condition such a layer refuses rather than for the ones one report cited.
//!    WHICH sentence, and whether it is wrapped
//!    in the envelope this service's filter-shape layer states a condition in
//!    (`crate::state::stated_validation`), is a property of the operation that states it;
//!    an unissued `nextToken` is the condition two operations reach and state differently;
//! 2. a constraint the MODEL declares is refused in front of the handler and stated in the
//!    AWS validation envelope — one clause per failing member, with the value as
//!    submitted, the member's dotted path and the requirement — and a constraint broken by
//!    a LIST's elements is stated ONCE at the list member, with the whole list as the value
//!    and every constraint the element shape declares nested.
//!
//! Every assertion below is about those rules over a TABLE of inputs rather than one
//! recorded call: the rows differ along the arity, the operator set, the nesting depth, the
//! token, the match-option count, the member path, the constraint class and the number of
//! failing elements. Each refusal table carries its positive control, so a passing row
//! cannot mean "every request is refused".

use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_freetier_awsfreetierservice::ops::types::{
    ACTIVITY_STATUS_VALUES, DIMENSION_VALUES, MATCH_OPTION_VALUES,
};
use service_com_amazonaws_freetier_awsfreetierservice::state::{
    stated_validation, ComAmazonawsFreetierAwsfreetierserviceState, ACCOUNT_PLAN_TYPE_VALUE_SET,
    ACTIVITIES_INVALID_NEXT_TOKEN, INVALID_NEXT_TOKEN, PAYMENT_INSTRUMENT_VET_FAILURE,
    PLAN_STATUS_ACTIVE, PLAN_STATUS_EXPIRED, PLAN_TYPE_FREE, PLAN_TYPE_PAID,
};

use crate::free_tier::{call, error_code, harness, ok, text};

/// The modelled `Value` constraints, from
/// `smithy_query(query="shape", shape="Values,Value")`: the list's member targets `Value`,
/// a string of 1..20 characters matching `^[ a-zA-Z0-9\-\:\.\_\/\,\$\(\)]*$`.
const MAX_VALUE_LENGTH: usize = 20;

/// The requirements a clause lists for one element of `Values`: every constraint that
/// member's element shape declares — the length maximum, the length minimum and the
/// pattern, which is stated without the anchors the model writes the whole-value match
/// with.
const VALUES_ELEMENT_CONSTRAINTS: &str = "Member must satisfy constraint: [Member must have \
     length less than or equal to 20, Member must have length greater than or equal to 1, \
     Member must satisfy regular expression pattern: \
     [ a-zA-Z0-9\\-\\:\\.\\_\\/\\,\\$\\(\\)]*]";

/// The `message` of an error body.
fn message(payload: &Value) -> String {
    payload["message"].as_str().unwrap_or_default().to_string()
}

/// Assert that one request is refused `400 ValidationException` and report its message.
///
/// Shared with the suites that assert ONE of these rules over a wider table (see
/// `tests/all/match_option_cardinality.rs`), so every suite reads a refusal's message the
/// same way.
pub(crate) async fn refusal_message(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> String {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400 from {operation}, got {status} with {payload}"
    );
    assert_eq!(
        error_code(&payload),
        "ValidationException",
        "case {case}: expected a ValidationException from {operation}, got {payload}"
    );
    message(&payload)
}

/// One `Dimensions` comparison as a filter expression.
fn comparison(key: &str, values: &[&str], match_options: &[&str]) -> Value {
    json!({"Dimensions": {"Key": key, "Values": values, "MatchOptions": match_options}})
}

/// A well-formed single-root filter, for a positive control and as the other operand of a
/// filter that sets two roots.
fn one_root() -> Value {
    comparison("REGION", &["us-east-1"], &["EQUALS"])
}

/// RULE 1, the root-operator arity: an `Expression` that does not set exactly one of its
/// root operators is refused with ONE sentence, whichever operators it set and however
/// many — the rule is the same rule in every case, so the reason a client reads does not
/// vary with the request.
#[tokio::test]
async fn a_filter_that_does_not_set_one_root_operator_states_one_sentence() {
    let router = harness().0;
    let expected = "[Message = Only one root operator is allowed for each expression: And, Or, \
                    Not, Dimensions]";

    let branch = one_root();
    let other = comparison("REGION", &["us-west-2"], &["EQUALS"]);
    // What each root operator carries, so a row can set any subset of them.
    let roots: [(&str, Value); 4] = [
        ("Or", json!([other.clone()])),
        ("And", json!([branch.clone()])),
        ("Not", branch.clone()),
        (
            "Dimensions",
            json!({"Key": "SERVICE", "Values": ["AWSLambda"], "MatchOptions": ["EQUALS"]}),
        ),
    ];

    // Every subset of the four roots EXCEPT the four that set exactly one: the empty
    // expression and every pair, triple and the whole set.
    let mut rows: Vec<(String, Value)> = Vec::new();
    for mask in 0..16_u8 {
        let set: Vec<&(&str, Value)> = roots
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, root)| root)
            .collect();
        if set.len() == 1 {
            continue;
        }
        let mut filter = serde_json::Map::new();
        for (name, operand) in &set {
            filter.insert((*name).to_string(), operand.clone());
        }
        let names: Vec<&str> = set.iter().map(|(name, _)| *name).collect();
        rows.push((
            format!("a filter setting {names:?}"),
            Value::Object(filter),
        ));
    }
    assert_eq!(
        rows.len(),
        12,
        "case the arity table: expected every subset but the four single ones, got {}",
        rows.len()
    );

    for (case, filter) in rows {
        // The same expression at every position of the tree: the rule governs each
        // level, so a nested one is refused with the same sentence.
        let places: [(&str, Value); 4] = [
            ("at the root", filter.clone()),
            ("inside And", json!({"And": [filter.clone()]})),
            ("inside Or", json!({"Or": [branch.clone(), filter.clone()]})),
            ("inside Not", json!({"Not": {"Or": [filter.clone()]}})),
        ];
        for (place, nested) in places {
            let case = format!("{case} {place}");
            let actual = refusal_message(
                &router,
                &case,
                "GetFreeTierUsage",
                &json!({"maxResults": 10, "filter": nested}),
            )
            .await;
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    // The positive control: each root operator ON ITS OWN is served.
    for (name, operand) in &roots {
        let filter = json!({ name.to_string(): operand.clone() });
        let answered = ok(
            &router,
            "GetFreeTierUsage",
            &json!({"maxResults": 10, "filter": filter}),
        )
        .await;
        assert!(
            answered["freeTierUsages"].is_array(),
            "case the single root operator {name}: expected a served page, got {answered}"
        );
    }
}

/// RULE 1, the match-option count: `MatchOptions` carries exactly one option
/// ("You can specify only one of these values in the array"), and every other count is
/// refused with ONE sentence that states neither the count submitted nor the options.
#[tokio::test]
async fn a_match_options_count_other_than_one_states_one_sentence() {
    let router = harness().0;
    let expected = "[Message = Exactly one match option must be provided]";

    // The rows differ along the count (none, two, three, all of them) and along WHICH
    // options are listed, including one option repeated.
    let rows: Vec<(String, Vec<&str>)> = vec![
        ("no option at all".to_string(), vec![]),
        ("two options".to_string(), vec!["EQUALS", "STARTS_WITH"]),
        (
            "two options, the other way round".to_string(),
            vec!["STARTS_WITH", "EQUALS"],
        ),
        (
            "one option repeated".to_string(),
            vec!["CONTAINS", "CONTAINS"],
        ),
        (
            "three options".to_string(),
            vec!["EQUALS", "CONTAINS", "ENDS_WITH"],
        ),
        (
            "every option the model declares".to_string(),
            MATCH_OPTION_VALUES.to_vec(),
        ),
    ];

    for (case, options) in rows {
        let filter = comparison("REGION", &["us-east-1"], &options);
        let actual = refusal_message(
            &router,
            &case,
            "GetFreeTierUsage",
            &json!({"maxResults": 10, "filter": filter}),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case} ({options:?}): expected {expected:?}, got {actual:?}"
        );
    }

    // The positive control: exactly one option is served, for every option a text
    // dimension compares by (`GREATER_THAN_OR_EQUAL` compares magnitudes, so it is
    // driven on the one numeric dimension instead).
    for option in MATCH_OPTION_VALUES {
        let (key, value) = if *option == "GREATER_THAN_OR_EQUAL" {
            ("USAGE_PERCENTAGE", "0")
        } else {
            ("REGION", "us-east-1")
        };
        let filter = comparison(key, &[value], &[option]);
        let answered = ok(
            &router,
            "GetFreeTierUsage",
            &json!({"maxResults": 10, "filter": filter}),
        )
        .await;
        assert!(
            answered["freeTierUsages"].is_array(),
            "case the single match option {option}: expected a served page, got {answered}"
        );
    }
}

/// RULE 1, an unissued `nextToken`: each paginated operation refuses a token it did not
/// issue with ONE sentence of its own, which names neither the operation asked nor the
/// token sent — so every way of failing to be a token of that listing reads the same,
/// including a token the OTHER listing issued.
#[tokio::test]
async fn a_next_token_the_service_did_not_issue_states_one_sentence() {
    let router = harness().0;

    // A token each paginated operation really issued, so a row can send one operation's
    // token to the other.
    let usage_token = ok(&router, "GetFreeTierUsage", &json!({"maxResults": 1})).await
        ["nextToken"]
        .as_str()
        .expect("a page that leaves rows unreported issues a token")
        .to_string();
    let activities_token = ok(&router, "ListAccountActivities", &json!({"maxResults": 1})).await
        ["nextToken"]
        .as_str()
        .expect("a page that leaves rows unreported issues a token")
        .to_string();

    // The rows differ along what the token IS: prose, something that is not the encoding
    // at all, the encoding of another listing, and a prefix of a real token.
    let rows: Vec<(String, String)> = vec![
        (
            "a token that was never one".to_string(),
            "not-a-real-pagination-token".to_string(),
        ),
        ("a single character".to_string(), "x".to_string()),
        (
            "hex that decodes to something else".to_string(),
            ::hex::encode("ftv0:activities:0"),
        ),
        (
            "a position with no listing named".to_string(),
            ::hex::encode("12"),
        ),
        (
            "a prefix of a token this listing issued".to_string(),
            usage_token[..usage_token.len() - 2].to_string(),
        ),
    ];

    // Both paginated operations, each with the sentence IT states and with the token it
    // did NOT issue among the rows.
    let operations = [
        (
            "GetFreeTierUsage",
            activities_token.clone(),
            stated_validation(INVALID_NEXT_TOKEN),
        ),
        (
            "ListAccountActivities",
            usage_token.clone(),
            ACTIVITIES_INVALID_NEXT_TOKEN.to_string(),
        ),
    ];
    for (operation, foreign, expected) in &operations {
        for (case, token) in &rows {
            let case = format!("{operation} refusing {case}");
            let actual =
                refusal_message(&router, &case, operation, &json!({"nextToken": token})).await;
            assert_eq!(
                &actual, expected,
                "case {case} ({token:?}): expected {expected:?}, got {actual:?}"
            );
        }
        // The token of the OTHER listing, which is a token — just not this one's.
        let case = format!("{operation} refusing the other listing's token");
        let actual =
            refusal_message(&router, &case, operation, &json!({"nextToken": foreign})).await;
        assert_eq!(
            &actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
    }

    // The positive control: the token each operation issued resumes its own listing.
    let resumed = ok(
        &router,
        "GetFreeTierUsage",
        &json!({"maxResults": 1, "nextToken": usage_token}),
    )
    .await;
    assert!(
        resumed["freeTierUsages"].is_array(),
        "case a token this listing issued: expected the next page, got {resumed}"
    );
    let resumed = ok(
        &router,
        "ListAccountActivities",
        &json!({"maxResults": 1, "nextToken": activities_token}),
    )
    .await;
    assert!(
        resumed["activities"].is_array(),
        "case a token this listing issued: expected the next page, got {resumed}"
    );
}

/// RULE 2, a constraint broken by a LIST's elements: the clause is stated once at the LIST
/// member — never at an indexed element — with the value that member holds (the whole list
/// the caller sent, bracketed) and with EVERY constraint the element shape declares nested,
/// whichever one the request broke.
#[tokio::test]
async fn a_list_elements_constraint_failure_is_stated_at_the_list_member() {
    let router = harness().0;

    let overlong = "x".repeat(MAX_VALUE_LENGTH + 1);
    // The rows differ along the constraint class broken (the element pattern, its length
    // maximum, its length minimum), the number and position of the failing elements, and
    // the member PATH the list is reached at — the same `Values` list nested under a
    // logical operator is reported at its own path.
    let rows: Vec<(String, Value, Vec<String>, &str)> = vec![
        (
            "one element outside the element pattern".to_string(),
            comparison("SERVICE", &["Amazon!EC2"], &["CONTAINS"]),
            vec!["Amazon!EC2".to_string()],
            "filter.dimensions.values",
        ),
        (
            "a bad element after a good one".to_string(),
            comparison("SERVICE", &["AWSLambda", "Amazon!EC2"], &["EQUALS"]),
            vec!["AWSLambda".to_string(), "Amazon!EC2".to_string()],
            "filter.dimensions.values",
        ),
        (
            "an element one character past the length maximum".to_string(),
            comparison("SERVICE", &[&overlong], &["EQUALS"]),
            vec![overlong.clone()],
            "filter.dimensions.values",
        ),
        (
            "an element below the length minimum".to_string(),
            comparison("SERVICE", &[""], &["EQUALS"]),
            vec![String::new()],
            "filter.dimensions.values",
        ),
        (
            "two bad elements, each breaking a different constraint".to_string(),
            comparison("SERVICE", &["Amazon!EC2", &overlong], &["EQUALS"]),
            vec!["Amazon!EC2".to_string(), overlong.clone()],
            "filter.dimensions.values",
        ),
        (
            "the same list nested under And".to_string(),
            json!({"And": [comparison("SERVICE", &["Amazon!EC2"], &["CONTAINS"])]}),
            vec!["Amazon!EC2".to_string()],
            "filter.and.1.member.dimensions.values",
        ),
        (
            "the same list nested under Not inside Or".to_string(),
            json!({"Or": [{"Not": comparison("SERVICE", &["Amazon!EC2"], &["CONTAINS"])}]}),
            vec!["Amazon!EC2".to_string()],
            "filter.or.1.member.not.dimensions.values",
        ),
    ];

    for (case, filter, submitted, path) in rows {
        let expected = format!(
            "1 validation error detected: Value '[{}]' at '{path}' failed to satisfy \
             constraint: {VALUES_ELEMENT_CONSTRAINTS}",
            submitted.join(", ")
        );
        let actual = refusal_message(
            &router,
            &case,
            "GetFreeTierUsage",
            &json!({"maxResults": 10, "filter": filter}),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
    }

    // The positive control, at the published bound and one inside it: a value of exactly
    // the maximum length, and one of the minimum, are served.
    for value in [
        "x".repeat(MAX_VALUE_LENGTH),
        "x".to_string(),
        "Amazon-EC2".to_string(),
    ] {
        let filter = comparison("SERVICE", &[&value], &["CONTAINS"]);
        let answered = ok(
            &router,
            "GetFreeTierUsage",
            &json!({"maxResults": 10, "filter": filter}),
        )
        .await;
        assert!(
            answered["freeTierUsages"].is_array(),
            "case the well-formed value {value:?}: expected a served page, got {answered}"
        );
    }
}

/// The same rule on ANOTHER list member of another operation, whose element shape declares
/// a different constraint class: the clause is stated at `filterActivityStatuses` with the
/// whole list as the value and the element's value set nested.
///
/// The set is compared as a SET: which values it holds is the model's, while the order they
/// are listed in is the order that model declares them, and this suite reads the model
/// through a sorted object.
#[tokio::test]
async fn a_list_of_enumerated_elements_is_stated_at_the_list_member_too() {
    let router = harness().0;

    // The rows differ along the number of elements and the position of the bad one.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("one value outside the set", vec!["ABANDONED"]),
        (
            "a bad value after a good one",
            vec!["COMPLETED", "ABANDONED"],
        ),
        (
            "a bad value between good ones",
            vec!["COMPLETED", "ABANDONED", "EXPIRING"],
        ),
        ("two bad values", vec!["ABANDONED", "FORGOTTEN"]),
    ];

    for (case, statuses) in rows {
        let actual = refusal_message(
            &router,
            case,
            "ListAccountActivities",
            &json!({"filterActivityStatuses": statuses}),
        )
        .await;
        let opening = format!(
            "1 validation error detected: Value '[{}]' at 'filterActivityStatuses' failed to \
             satisfy constraint: Member must satisfy constraint: [Member must satisfy enum \
             value set: [",
            statuses.join(", ")
        );
        let listed = actual
            .strip_prefix(&opening)
            .and_then(|rest| rest.strip_suffix("]]"))
            .unwrap_or_default();
        assert!(
            !listed.is_empty(),
            "case {case}: expected a clause opening {opening:?}, got {actual:?}"
        );
        let mut listed: Vec<&str> = listed.split(", ").collect();
        listed.sort_unstable();
        let mut declared: Vec<&str> = ACTIVITY_STATUS_VALUES.to_vec();
        declared.sort_unstable();
        assert_eq!(
            listed, declared,
            "case {case}: expected the value set the model declares, {declared:?}, got \
             {listed:?} in {actual:?}"
        );
    }

    // The positive control: every value the set declares is served.
    for status in ACTIVITY_STATUS_VALUES {
        let answered = ok(
            &router,
            "ListAccountActivities",
            &json!({"filterActivityStatuses": [status]}),
        )
        .await;
        assert!(
            answered["activities"].is_array(),
            "case the declared status {status}: expected a served page, got {answered}"
        );
    }
}

/// RULE 2 on a member that is NOT a list element: the clause states the value as
/// submitted, the member's dotted path and the one requirement it failed — a `@pattern`
/// without the anchors the model writes the whole-value match with, and a COLLECTION's own
/// `@length` with no value at all, because a list has no scalar rendering to quote.
#[tokio::test]
async fn a_members_own_constraint_is_stated_with_its_value_path_and_requirement() {
    let router = harness().0;

    let overlong_id = "0".repeat(33);
    // (case, operation, body, the message stated)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a @pattern on a scalar member",
            "GetAccountActivity",
            json!({"activityId": "0000000000000000000000000000000-"}),
            "1 validation error detected: Value '0000000000000000000000000000000-' at \
             'activityId' failed to satisfy constraint: Member must satisfy regular \
             expression pattern: [a-zA-Z0-9]+"
                .to_string(),
        ),
        (
            "a @length maximum on a scalar member",
            "GetAccountActivity",
            json!({"activityId": overlong_id}),
            format!(
                "1 validation error detected: Value '{overlong_id}' at 'activityId' failed to \
                 satisfy constraint: Member must have length less than or equal to 32"
            ),
        ),
        (
            "a @length minimum on a scalar member",
            "GetAccountActivity",
            json!({"activityId": "short"}),
            "1 validation error detected: Value 'short' at 'activityId' failed to satisfy \
             constraint: Member must have length greater than or equal to 32"
                .to_string(),
        ),
        (
            "a @range minimum",
            "GetFreeTierUsage",
            json!({"maxResults": 0}),
            "1 validation error detected: Value '0' at 'maxResults' failed to satisfy \
             constraint: Member must have value greater than or equal to 1"
                .to_string(),
        ),
        (
            "a @range maximum",
            "ListAccountActivities",
            json!({"maxResults": 5000}),
            "1 validation error detected: Value '5000' at 'maxResults' failed to satisfy \
             constraint: Member must have value less than or equal to 1000"
                .to_string(),
        ),
        (
            "an absent @required member",
            "UpgradeAccountPlan",
            json!({}),
            "1 validation error detected: Value null at 'accountPlanType' failed to satisfy \
             constraint: Member must not be null"
                .to_string(),
        ),
        (
            "a value outside an enumerated member",
            "UpgradeAccountPlan",
            json!({"accountPlanType": "GOLD"}),
            format!(
                "1 validation error detected: Value 'GOLD' at 'accountPlanType' failed to \
                 satisfy constraint: Member must satisfy enum value set: [{}]",
                ACCOUNT_PLAN_TYPE_VALUE_SET.join(", ")
            ),
        ),
        (
            "a LIST's own @length, stated with no value",
            "GetFreeTierUsage",
            json!({"filter": {"Dimensions":
                {"Key": "SERVICE", "Values": [], "MatchOptions": ["EQUALS"]}}}),
            "1 validation error detected: Value at 'filter.dimensions.values' failed to \
             satisfy constraint: Member must have length greater than or equal to 1"
                .to_string(),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let actual = refusal_message(&router, case, operation, &body).await;
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The one `Dimension` whose value on a usage record is a number, so it is the one
/// compared by magnitude (`smithy_query(query="shape",shape="Dimension")` declares the
/// seven dimensions; `UsageRecord::dimension_value` answers which kind each takes).
const NUMERIC_DIMENSION: &str = "USAGE_PERCENTAGE";

/// The `MatchOption` that compares magnitudes rather than text
/// (`smithy_query(query="shape",shape="MatchOption")`).
const MAGNITUDE_OPTION: &str = "GREATER_THAN_OR_EQUAL";

/// The option a value of either kind is compared for equality by.
const EQUALITY_OPTION: &str = "EQUALS";

/// How the request spells the filter and the members of one comparison, as the model
/// declares them: a sentence this service's own validation layer states names none of
/// them, because where in the request a value arrived is no part of the condition the
/// sentence reports (the clause that DOES name a member path is the boundary walk's,
/// asserted by RULE 2 above).
const MEMBER_NAMES: [&str; 8] = [
    "filter",
    "Dimensions",
    "MatchOptions",
    "Values",
    "Key",
    "Or",
    "And",
    "Not",
];

/// The places one offending expression can sit in a `filter`: every condition below is
/// checked at every level of the tree, so each row is driven at each of them.
fn positions(offending: &Value) -> Vec<(&'static str, Value)> {
    vec![
        ("at the root", offending.clone()),
        ("inside And", json!({"And": [offending.clone()]})),
        (
            "inside Or, after a well-formed branch",
            json!({"Or": [one_root(), offending.clone()]}),
        ),
        ("inside Not", json!({"Not": offending.clone()})),
        (
            "two levels down, inside Or inside And",
            json!({"And": [{"Or": [offending.clone()]}]}),
        ),
    ]
}

/// One condition `GetFreeTierUsage` refuses for ITSELF, with the sentence a client reads
/// and the offending requests that reach it.
struct SelfWordedRefusal {
    /// What the condition is.
    case: &'static str,
    /// The whole `message`, the envelope included.
    expected: &'static str,
    /// One offending request each: its case, the request body, and the strings THIS
    /// request contributes that must not appear in what the client reads.
    rows: Vec<(String, Value, Vec<String>)>,
}

/// One `Dimensions` comparison as a request body.
fn usage_request(filter: Value) -> Value {
    json!({"maxResults": 10, "filter": filter})
}

/// Every condition this operation's own checks refuse on a well-modelled `filter`, each
/// over a table of offending requests.
///
/// Reachability: every value below satisfies the model, so the framework's constraint
/// walk passes the request to the handler — `Value` is 1..20 characters matching
/// `^[ a-zA-Z0-9\-\:\.\_\/\,\$\(\)]*$`, `Dimension` and `MatchOption` are the enums the
/// model declares, and `Expressions` declares no length of its own
/// (`smithy_query(query="shape",shape="DimensionValues,Values,Expression")`). The two
/// conditions the model DOES declare a constraint for — an empty `Values` list and a
/// value outside either enum — are refused in front of the handler and are asserted by
/// RULE 2 above instead.
fn self_worded_refusals() -> Vec<SelfWordedRefusal> {
    let text_dimensions: Vec<&str> = DIMENSION_VALUES
        .iter()
        .copied()
        .filter(|key| *key != NUMERIC_DIMENSION)
        .collect();
    let text_options: Vec<&str> = MATCH_OPTION_VALUES
        .iter()
        .copied()
        .filter(|option| *option != EQUALITY_OPTION && *option != MAGNITUDE_OPTION)
        .collect();
    assert!(
        text_dimensions.len() > 1 && !text_options.is_empty(),
        "case the model's own sets: expected several text dimensions and at least one \
         text-only option, got {text_dimensions:?} and {text_options:?}"
    );

    // The magnitude option asked of a dimension whose values are text. The sentence
    // names that option because every request breaking this condition submitted exactly
    // it — that is what the condition IS — while the dimension and the value the rows
    // vary are the request's, so neither appears.
    let mut magnitude_on_text = Vec::new();
    for key in &text_dimensions {
        for value in ["us-east-1", "AWSLambda"] {
            let offending = comparison(key, &[value], &[MAGNITUDE_OPTION]);
            for (place, filter) in positions(&offending) {
                magnitude_on_text.push((
                    format!("{key} against {value:?} {place}"),
                    usage_request(filter),
                    vec![(*key).to_string(), value.to_string()],
                ));
            }
        }
    }

    // A dimension whose values are numbers compared under an option that compares text.
    // The sentence names the two options the rule ADMITS; which of the text options was
    // asked is the input.
    let mut text_option_on_numbers = Vec::new();
    for option in &text_options {
        for values in [vec!["0"], vec!["50"], vec!["0", "100"]] {
            let offending = comparison(NUMERIC_DIMENSION, &values, &[option]);
            for (place, filter) in positions(&offending) {
                text_option_on_numbers.push((
                    format!("{NUMERIC_DIMENSION} {option} {values:?} {place}"),
                    usage_request(filter),
                    std::iter::once((*option).to_string())
                        .chain(values.iter().map(|value| (*value).to_string()))
                        .chain(std::iter::once(NUMERIC_DIMENSION.to_string()))
                        .collect(),
                ));
            }
        }
    }

    // A value that names no magnitude, on the dimension that compares magnitudes: the
    // rows differ along which value cannot be read, how it is spelled, how many values
    // sit beside it and which option compares them.
    let mut unreadable_magnitude = Vec::new();
    for values in [
        vec!["half"],
        vec!["1,5"],
        vec!["0x32"],
        vec![" "],
        vec!["NaN"],
        vec!["1e400"],
        vec!["fifty", "50"],
        vec!["0", "25", "nearly all"],
    ] {
        for option in [EQUALITY_OPTION, MAGNITUDE_OPTION] {
            let offending = comparison(NUMERIC_DIMENSION, &values, &[option]);
            for (place, filter) in positions(&offending) {
                unreadable_magnitude.push((
                    format!("{NUMERIC_DIMENSION} {option} {values:?} {place}"),
                    usage_request(filter),
                    values
                        .iter()
                        .map(|value| (*value).to_string())
                        .chain(std::iter::once(NUMERIC_DIMENSION.to_string()))
                        .collect(),
                ));
            }
        }
    }

    // A logical operator that lists no expression at all, which states no condition:
    // the rows differ along which operator holds the empty list and where it sits.
    let mut empty_operator = Vec::new();
    for (case, filter) in [
        ("Or at the root", json!({"Or": []})),
        ("And at the root", json!({"And": []})),
        ("Or inside And", json!({"And": [{"Or": []}]})),
        ("And inside Not", json!({"Not": {"And": []}})),
        (
            "And after a well-formed branch of Or",
            json!({"Or": [one_root(), {"And": []}]}),
        ),
        (
            "Or after a well-formed branch of And",
            json!({"And": [one_root(), {"Or": []}]}),
        ),
    ] {
        empty_operator.push((case.to_string(), usage_request(filter), Vec::new()));
    }

    vec![
        SelfWordedRefusal {
            case: "the magnitude option on a dimension whose values are text",
            expected: "[Message = GREATER_THAN_OR_EQUAL applies only to a dimension whose \
                       values are numbers]",
            rows: magnitude_on_text,
        },
        SelfWordedRefusal {
            case: "a text option on the dimension whose values are numbers",
            expected: "[Message = A dimension whose values are numbers must be compared with \
                       EQUALS or GREATER_THAN_OR_EQUAL]",
            rows: text_option_on_numbers,
        },
        SelfWordedRefusal {
            case: "a value naming no number where the dimension's values are numbers",
            expected: "[Message = Every value compared against a dimension whose values are \
                       numbers must be a number]",
            rows: unreadable_magnitude,
        },
        SelfWordedRefusal {
            case: "a logical operator listing no expression",
            expected: "[Message = A logical operator must list at least one expression]",
            rows: empty_operator,
        },
    ]
}

/// RULE 1 over EVERY condition this operation refuses for itself, not only the two whose
/// sentences were already pinned: each is stated as ONE fixed sentence in this service's
/// own validation envelope, identical for every request that breaks it, and carrying
/// nothing of that request — not the dimension, option or value submitted, and not the
/// member path the value arrived at.
#[tokio::test]
async fn every_condition_this_operation_refuses_itself_states_one_fixed_sentence() {
    let router = harness().0;

    for condition in self_worded_refusals() {
        let mut stated: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        assert!(
            condition.rows.len() > 3,
            "case {}: the table must span the condition, got {} rows",
            condition.case,
            condition.rows.len()
        );
        for (row, body, borrowed) in &condition.rows {
            let case = format!("{} \u{2014} {row}", condition.case);
            let actual = refusal_message(&router, &case, "GetFreeTierUsage", body).await;
            assert_eq!(
                actual, condition.expected,
                "case {case}: expected {:?}, got {actual:?}",
                condition.expected
            );
            for token in borrowed
                .iter()
                .map(String::as_str)
                .chain(MEMBER_NAMES.iter().copied())
            {
                // A value that is whitespace alone contributes no token to look for.
                if token.trim().is_empty() {
                    continue;
                }
                assert!(
                    !actual.contains(token),
                    "case {case}: expected a sentence carrying nothing of the request, but it \
                     names {token:?}: {actual:?}"
                );
            }
            stated.insert(actual);
        }
        let stated: Vec<String> = stated.into_iter().collect();
        assert_eq!(
            stated.len(),
            1,
            "case {}: expected one sentence for the whole condition, got {stated:?}",
            condition.case
        );
    }

    // The positive controls: the well-formed neighbour of each condition is served, so a
    // passing row above cannot mean "every filter is refused".
    for (case, filter) in [
        (
            "a dimension whose values are text, compared for equality",
            comparison("SERVICE", &["AWSLambda"], &[EQUALITY_OPTION]),
        ),
        (
            "a dimension whose values are text, compared by a text option",
            comparison("REGION", &["us-east"], &["STARTS_WITH"]),
        ),
        (
            "the numeric dimension compared for equality against a number",
            comparison(NUMERIC_DIMENSION, &["0"], &[EQUALITY_OPTION]),
        ),
        (
            "the numeric dimension compared by magnitude against a number",
            comparison(NUMERIC_DIMENSION, &["50"], &[MAGNITUDE_OPTION]),
        ),
        (
            "a logical operator listing one expression",
            json!({"And": [one_root()]}),
        ),
    ] {
        let answered = ok(&router, "GetFreeTierUsage", &usage_request(filter)).await;
        assert!(
            answered["freeTierUsages"].is_array(),
            "case {case}: expected a served page, got {answered}"
        );
    }
}

/// A filter nesting `levels` logical operators above one well-formed comparison. `shape`
/// says what each level is: `Not` over the expression, `And`/`Or` over a list holding it,
/// and anything else alternates the two, because the rule is about the DEPTH and not
/// about which operator sits at which level.
fn nested(levels: usize, shape: &str) -> Value {
    let mut filter = one_root();
    for level in 0..levels {
        filter = match (shape, level % 2 == 0) {
            ("Not", _) => json!({"Not": filter}),
            ("And", _) => json!({"And": [filter]}),
            ("Or", _) => json!({"Or": [filter]}),
            (_, true) => json!({"And": [filter]}),
            (_, false) => json!({"Not": filter}),
        };
    }
    filter
}

/// RULE 1 at the nesting bound this service enforces on a `filter`: an expression that
/// nests past it is refused with ONE sentence, whatever operators nested it and however
/// far past the bound it went — and the bound that sentence states is the bound the
/// service really enforces, which is the one number in it and the reason it states a
/// number at all (a bound a caller can read nowhere else, exactly as a clause for a
/// bound this service enforces on top of the model's states its ceiling).
#[tokio::test]
async fn an_expression_nesting_past_the_depth_bound_states_one_sentence() {
    let router = harness().0;
    let shapes = ["Not", "And", "Or", "alternating"];

    // Where the bound sits, FOUND by asking rather than written down, so the sentence is
    // checked against the depth the service really evaluates: the shallowest nesting it
    // refuses.
    let mut threshold = None;
    for levels in 1..=32 {
        let (status, _) = call(
            &router,
            "GetFreeTierUsage",
            &usage_request(nested(levels, "Not")),
        )
        .await;
        if status != 200 {
            threshold = Some(levels);
            break;
        }
    }
    let threshold = threshold.expect(
        "case the nesting bound: an expression nesting 32 levels must be refused, or this \
         service evaluates expressions without a bound",
    );
    assert!(
        threshold > 1,
        "case the nesting bound: a filter nesting one operator must be served, got a refusal \
         at {threshold} levels"
    );
    let expected = format!(
        "[Message = An expression must not nest more than {} levels]",
        threshold - 1
    );

    for shape in shapes {
        // Just past the bound, further past it, and far past it.
        for levels in [threshold, threshold + 1, threshold + 3, threshold + 6] {
            let case = format!("{levels} levels of {shape}");
            let actual = refusal_message(
                &router,
                &case,
                "GetFreeTierUsage",
                &usage_request(nested(levels, shape)),
            )
            .await;
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );
            // The depth SUBMITTED is the input; only the bound is part of the rule.
            let submitted = levels.to_string();
            assert!(
                !actual.contains(&submitted),
                "case {case}: expected a sentence carrying nothing of the request, but it \
                 names the depth submitted ({submitted}): {actual:?}"
            );
            for name in MEMBER_NAMES {
                assert!(
                    !actual.contains(name),
                    "case {case}: expected a sentence naming no member of the request, but it \
                     names {name:?}: {actual:?}"
                );
            }
        }

        // The positive control, at the bound the sentence states: an expression nesting
        // exactly that many levels is served, for every operator the rows nest with.
        let case = format!("{} levels of {shape}", threshold - 1);
        let answered = ok(
            &router,
            "GetFreeTierUsage",
            &usage_request(nested(threshold - 1, shape)),
        )
        .await;
        assert!(
            answered["freeTierUsages"].is_array(),
            "case {case}: expected a served page at the bound the refusal states, got {answered}"
        );
    }
}

/// RULE 1 on the condition another operation of this service refuses for itself and
/// decides from STATE rather than from the request: moving an account back onto the
/// one-off free plan whose window it no longer holds. One fixed sentence, the same for
/// every account state the condition holds for — so the plan the account holds and the
/// status that plan is in, which the rule does not distinguish between, are no part of
/// what a client branches on.
#[tokio::test]
async fn the_closed_free_window_refusal_states_one_sentence() {
    let expected = "[Message = The FREE plan is a one-off window this account no longer holds]";
    let day: i64 = 86_400;

    // The stored states the window is closed in: an account that left the free plan for
    // the paid one, and one whose window the clock has carried past its own instant.
    let rows: [(&str, &str, Option<i64>); 5] = [
        ("an account on the paid plan", PLAN_TYPE_PAID, None),
        (
            "an account on the paid plan carrying an instant of its own",
            PLAN_TYPE_PAID,
            Some(day),
        ),
        (
            "a free plan one second past its instant",
            PLAN_TYPE_FREE,
            Some(-1),
        ),
        (
            "a free plan expiring at this very instant",
            PLAN_TYPE_FREE,
            Some(0),
        ),
        (
            "a free plan years past its instant",
            PLAN_TYPE_FREE,
            Some(-3_650 * day),
        ),
    ];

    let mut stated: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (case, stored_type, offset) in rows {
        let (router, engine) = harness();
        {
            let state = engine.state::<ComAmazonawsFreetierAwsfreetierserviceState>();
            let mut store = state.lock();
            let now = DateTime::from(std::time::SystemTime::now()).secs();
            let plan = store.plan_mut(engine::emulator_account_id());
            plan.plan_type = stored_type.to_string();
            plan.plan_status = PLAN_STATUS_ACTIVE.to_string();
            plan.expiration_date = offset.map(|offset| DateTime::from_secs(now + offset));
        }

        let actual = refusal_message(
            &router,
            case,
            "UpgradeAccountPlan",
            &json!({"accountPlanType": PLAN_TYPE_FREE}),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
        // Neither the account's stored plan nor its status nor the member the plan was
        // asked at belongs in the sentence. That the sentence does not echo the plan
        // asked FOR is what the rows above show: every stored state reads alike.
        for token in [PLAN_TYPE_PAID, "accountPlanType", PLAN_STATUS_ACTIVE, "EXPIRED"] {
            assert!(
                !actual.contains(token),
                "case {case}: expected a sentence carrying nothing of the request or of the \
                 stored plan, but it names {token:?}: {actual:?}"
            );
        }
        stated.insert(actual);
    }

    let stated: Vec<String> = stated.into_iter().collect();
    assert_eq!(
        stated,
        vec![expected.to_string()],
        "case the whole table of closed windows: expected the one sentence {expected:?}, got \
         {stated:?}"
    );

    // The positive control: an account whose window IS open is moved onto the free plan.
    let (router, engine) = harness();
    {
        let state = engine.state::<ComAmazonawsFreetierAwsfreetierserviceState>();
        let mut store = state.lock();
        let now = DateTime::from(std::time::SystemTime::now()).secs();
        let plan = store.plan_mut(engine::emulator_account_id());
        plan.plan_type = PLAN_TYPE_FREE.to_string();
        plan.plan_status = PLAN_STATUS_ACTIVE.to_string();
        plan.expiration_date = Some(DateTime::from_secs(now + day));
    }
    let answered = ok(
        &router,
        "UpgradeAccountPlan",
        &json!({"accountPlanType": PLAN_TYPE_FREE}),
    )
    .await;
    assert_eq!(
        answered["accountPlanType"].as_str().unwrap_or_default(),
        PLAN_TYPE_FREE,
        "case an account whose window is open: expected the plan asked for, got {answered}"
    );
}

/// How the account's one-off free window came to be closed — the two ways a CLIENT can
/// close it, so a row reaches the refusal through this service's own operations rather
/// than through a store a test wrote.
enum WindowClosedBy {
    /// The account left the free plan for the paid one, after asking for the free plan
    /// `free_calls_first` times while its window was still open.
    LeavingForThePaidPlan { free_calls_first: usize },
    /// The clock carried the free window `seconds` past the instant the plan itself
    /// names.
    TheWindowElapsing { seconds: i64 },
}

/// Mount the service and close the account's one-off free window the way `how` says,
/// driving every plan write through `UpgradeAccountPlan` itself. Reports the router and
/// the plan the store then holds, as `GetAccountPlanState` reports it.
async fn window_closed(case: &str, how: &WindowClosedBy) -> (MultiServiceRouter, Value) {
    let (router, engine) = harness();
    let free = json!({"accountPlanType": PLAN_TYPE_FREE});

    match *how {
        WindowClosedBy::LeavingForThePaidPlan { free_calls_first } => {
            // The paid plan's own precondition is not what these rows are about (its
            // wording is pinned in `paid_plan_vetting.rs`), so the account carries a
            // vetted payment instrument and the move onto that plan is served.
            engine
                .state::<ComAmazonawsFreetierAwsfreetierserviceState>()
                .lock()
                .vet_payment_instrument(engine::emulator_account_id());
            for attempt in 1..=free_calls_first {
                let inside = ok(&router, "UpgradeAccountPlan", &free).await;
                assert_eq!(
                    text(&inside, "accountPlanType"),
                    PLAN_TYPE_FREE,
                    "case {case}: call {attempt} inside the open window must be served the \
                     free plan, got {inside}"
                );
            }
            let paid = ok(
                &router,
                "UpgradeAccountPlan",
                &json!({"accountPlanType": PLAN_TYPE_PAID}),
            )
            .await;
            assert_eq!(
                text(&paid, "accountPlanType"),
                PLAN_TYPE_PAID,
                "case {case}: the account must reach the paid plan over the wire, got {paid}"
            );
        }
        WindowClosedBy::TheWindowElapsing { seconds } => {
            let established = ok(&router, "UpgradeAccountPlan", &free).await;
            assert_eq!(
                text(&established, "accountPlanType"),
                PLAN_TYPE_FREE,
                "case {case}: the window has to be open before it can elapse, got \
                 {established}"
            );
            // What the clock does to a window that elapses: the instant the plan names
            // is now in the past.
            let state = engine.state::<ComAmazonawsFreetierAwsfreetierserviceState>();
            let mut store = state.lock();
            let now = DateTime::from(std::time::SystemTime::now()).secs();
            store.plan_mut(engine::emulator_account_id()).expiration_date =
                Some(DateTime::from_secs(now - seconds));
        }
    }

    let plan = ok(&router, "GetAccountPlanState", &json!({})).await;
    (router, plan)
}

/// RULE 1 on the same handler-checked condition, reached the way a CLIENT reaches it:
/// the one-off free window closed by this service's own operations and by the clock
/// rather than by a store a test wrote. However the window came to be closed — the
/// account left the free plan for the paid one, or the window's own instant passed — and
/// however often the refused call is repeated, the refusal states ONE sentence for the
/// whole condition, so the plan the store holds and the status it is in, which the rule
/// does not distinguish between, are no part of what a client branches on.
///
/// The rows differ along what the rule generalises over: which of the two ways closed
/// the window, how many served calls preceded it, and how far past its instant the
/// window is.
#[tokio::test]
async fn the_closed_free_window_reads_alike_however_the_window_was_closed() {
    const DAY: i64 = 86_400;
    // The sentence the handler states for this whole condition, in the envelope this
    // service states a handler-checked condition in (`state::stated_validation`).
    const FREE_WINDOW_CLOSED: &str = "The FREE plan is a one-off window this account no \
                                      longer holds";
    let expected = stated_validation(FREE_WINDOW_CLOSED);
    let request = json!({"accountPlanType": PLAN_TYPE_FREE});

    let rows: [(&str, WindowClosedBy); 5] = [
        (
            "an account that moved straight onto the paid plan",
            WindowClosedBy::LeavingForThePaidPlan {
                free_calls_first: 0,
            },
        ),
        (
            "an account that asked for its open free window three times, then left it",
            WindowClosedBy::LeavingForThePaidPlan {
                free_calls_first: 3,
            },
        ),
        (
            "a free window one second past its instant",
            WindowClosedBy::TheWindowElapsing { seconds: 1 },
        ),
        (
            "a free window a day past its instant",
            WindowClosedBy::TheWindowElapsing { seconds: DAY },
        ),
        (
            "a free window a decade past its instant",
            WindowClosedBy::TheWindowElapsing {
                seconds: 3_650 * DAY,
            },
        ),
    ];

    let mut stated: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut stored: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (case, how) in &rows {
        let (router, plan) = window_closed(case, how).await;
        stored.insert(format!(
            "{}/{}",
            text(&plan, "accountPlanType"),
            text(&plan, "accountPlanStatus")
        ));

        // Repeated, because the condition is the same condition every time it is
        // reached: a refusal that reworded itself on a retry is the same defect as one
        // that reworded itself per stored state.
        for attempt in 1..=3 {
            let actual = refusal_message(&router, case, "UpgradeAccountPlan", &request).await;
            assert_eq!(
                actual, expected,
                "case {case}, attempt {attempt}: expected {expected:?}, got {actual:?}"
            );
            // Neither the plan the store holds, nor the status it is in, nor the member
            // the plan was asked at, nor the account asking belongs in the sentence.
            for token in [
                PLAN_TYPE_PAID,
                PLAN_STATUS_ACTIVE,
                PLAN_STATUS_EXPIRED,
                "accountPlanType",
                engine::emulator_account_id(),
            ] {
                assert!(
                    !actual.contains(token),
                    "case {case}, attempt {attempt}: expected a sentence carrying nothing of \
                     the request or of the stored plan, but it names {token:?}: {actual:?}"
                );
            }
            stated.insert(actual);
        }
    }

    let stated: Vec<String> = stated.into_iter().collect();
    assert_eq!(
        stated,
        vec![expected.clone()],
        "case the whole table of closed windows: expected the one sentence {expected:?}, got \
         {stated:?}"
    );
    // That the rows really did reach the condition both ways: one half of it is an
    // account on the paid plan, the other a free plan the clock has expired, and the one
    // sentence above was stated for both.
    let reached: std::collections::BTreeSet<String> = [
        format!("{PLAN_TYPE_PAID}/{PLAN_STATUS_ACTIVE}"),
        format!("{PLAN_TYPE_FREE}/{PLAN_STATUS_EXPIRED}"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        stored, reached,
        "case the whole table of closed windows: expected both ways of closing the window to \
         be reached over the wire ({reached:?}), got {stored:?}"
    );

    // The positive control, and the contrast: on an account whose window is still open
    // the free plan is served, and the handler's OTHER self-worded refusal — the paid
    // plan's unvetted payment instrument — answers its own fixed text, outside the
    // envelope above. Two conditions, two fixed sentences, neither varying with the
    // store.
    let (router, _engine) = harness();
    let served = ok(&router, "UpgradeAccountPlan", &request).await;
    assert_eq!(
        text(&served, "accountPlanType"),
        PLAN_TYPE_FREE,
        "case an account whose window is open: expected the plan asked for, got {served}"
    );
    let vetting = refusal_message(
        &router,
        "an account with no vetted payment instrument",
        "UpgradeAccountPlan",
        &json!({"accountPlanType": PLAN_TYPE_PAID}),
    )
    .await;
    assert_eq!(
        vetting, PAYMENT_INSTRUMENT_VET_FAILURE,
        "case an account with no vetted payment instrument: expected \
         {PAYMENT_INSTRUMENT_VET_FAILURE:?}, got {vetting:?}"
    );
    assert_ne!(
        vetting, expected,
        "case the two self-worded refusals of this operation: two conditions must not read \
         alike, got {vetting:?} for both"
    );
}
