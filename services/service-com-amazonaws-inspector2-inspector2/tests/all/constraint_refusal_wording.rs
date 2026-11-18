// SPDX-License-Identifier: CC0-1.0

//! How this service words the `ValidationException` it answers a request its front door
//! refused with, over every constraint class and every depth a member sits at.
//!
//! RULE, in three parts, each asserted over a table of requests rather than over one:
//!
//! 1. a clause names the offending MEMBER PATH and the requirement it broke, and nothing of the
//!    value the request carried — not the submitted bytes, and not the `null` an absent member
//!    is reported with — so the refusal of a bad value is the same sentence whatever was
//!    submitted;
//! 2. a constraint the model declares on the ELEMENTS of a collection member is stated at that
//!    member — no element position, no `member` step — with every requirement the element shape
//!    declares nested inside `Member must satisfy constraint: [ … ]`;
//! 3. an enumerated type's value set is listed in the order this service lists that type, which
//!    is neither the model's declaration order nor sorted, over the values the service
//!    publishes for it.
//!
//! Every request goes through the real `MultiServiceRouter` — routing, restJson1
//! deserialization, modelled-constraint validation, the handler, and restJson1 serialization —
//! so what is asserted here is the wire behaviour a client sees.

use serde_json::{json, Value};

use crate::filters_and_enablement::{post_in, router};

/// `AggregationType`'s value set as this service lists it.
/// Source: `smithy_query(query="shape", shape="AggregationType")` for the membership the
/// model declares; the service publishes it without the code-repository value.
const AGGREGATION_TYPE_SET: &str = "AWS_EC2_INSTANCE, PACKAGE, REPOSITORY, ACCOUNT, \
                                    IMAGE_LAYER, AWS_LAMBDA_FUNCTION, FINDING_TYPE, \
                                    AWS_ECR_CONTAINER, TITLE, LAMBDA_LAYER, \
                                    SERVERLESS_FUNCTION, CONTAINER_IMAGE, VM_INSTANCE, AMI";

/// `GroupKey`'s value set as this service lists it.
/// Source: `smithy_query(query="shape", shape="GroupKey")` for the membership the model
/// declares; the service publishes it with the scan-type grouping as well.
const GROUP_KEY_SET: &str = "PROVIDER_REGION, PROVIDER_ORG_ID, RESOURCE_TYPE, ACCOUNT_ID, \
                             SCAN_STATUS_REASON, PROVIDER, ECR_REPOSITORY_NAME, \
                             SCAN_STATUS_CODE, PROVIDER_ACCOUNT_ID, SCAN_TYPE";

/// `SortField`'s value set as this service lists it, carried by the `sortCriteria` of the
/// findings surface.
/// Source: `smithy_query(query="shape", shape="SortField")` for the membership the model
/// declares; the service publishes it with the two in-use detail fields as well.
const SORT_FIELD_SET: &str = "LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, \
                             INSPECTOR_SCORE, ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, \
                             ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, FINDING_TYPE, \
                             ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, \
                             LAST_IN_USE_AT, COMPONENT_TYPE, IN_USE_COUNT, SEVERITY, \
                             EPSS_SCORE, VULNERABILITY_SOURCE, VULNERABILITY_ID";

/// `SortOrder`'s value set as this service lists it, the other enumerated member of the same
/// `sortCriteria`.
/// Source: `smithy_query(query="shape", shape="SortOrder")`.
const SORT_ORDER_SET: &str = "DESC, ASC";

/// `StringComparison`'s value set as this service lists it, carried by every `StringFilter`
/// of the findings surface.
/// Source: `smithy_query(query="shape", shape="StringComparison")`.
const STRING_COMPARISON_SET: &str = "NOT_EQUALS, PREFIX, EQUALS";

/// `CoverageStringComparison`'s value set as this service lists it — the coverage filters
/// admit no prefix match, so the set is one value shorter than the findings one.
/// Source: `smithy_query(query="shape", shape="CoverageStringComparison")`.
const COVERAGE_STRING_COMPARISON_SET: &str = "NOT_EQUALS, EQUALS";

/// `ReportId`'s expression, which carries no whole-value anchors to drop.
/// Source: `smithy_query(query="shape", shape="ReportId")`.
const REPORT_ID_PATTERN: &str =
    r"\b[a-f0-9]{8}\b-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-\b[a-f0-9]{12}\b";

/// `FindingArn`'s expression as a requirement quotes it: the model's `@pattern` with its
/// leading `^` and trailing `$` removed.
/// Source: `smithy_query(query="shape", shape="FindingArn")`.
const FINDING_ARN_PATTERN: &str = r"arn:(aws[a-zA-Z-]*)?:inspector2:[a-z]{2}(-gov)?-[a-z]+-\d{1}:\d{12}:finding/[a-f0-9]{32}";

/// A finding ARN `FindingArn` admits, so a row refuses only the element it means to.
const FINDING_ARN: &str = "arn:aws:inspector2:us-east-1:000000000000:finding/\
                           0123456789abcdef0123456789abcdef";

/// The whole message AWS answers one refused member with: the framework's one-error
/// envelope over the clause that names `member` and `requirement` and no value.
fn one_error(member: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement a value outside an enumerated type's set broke.
fn enum_value_set(values: &str) -> String {
    format!("Member must satisfy enum value set: [{values}]")
}

/// The requirement a value outside its member's `@pattern` broke.
fn regular_expression(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The requirement every ELEMENT of a collection member has to satisfy, nested as a clause
/// stated at the member itself states it.
fn element_constraints(requirements: &[String]) -> String {
    format!("Member must satisfy constraint: [{}]", requirements.join(", "))
}

/// Every requirement a `FindingArn` element declares, in the order a clause states them: the
/// `@length` maximum, its minimum, then the expression.
/// Source: `smithy_query(query="shape", shape="FindingArn")` (`@length` 1..100).
fn finding_arn_constraints() -> String {
    element_constraints(&[
        "Member must have length less than or equal to 100".to_string(),
        "Member must have length greater than or equal to 1".to_string(),
        regular_expression(FINDING_ARN_PATTERN),
    ])
}

/// A `filterCriteria` whose `member` list carries one filter with `comparison`.
fn filter_on(member: &str, comparison: &str) -> Value {
    json!({
        "filterCriteria": {
            member: [{ "comparison": comparison, "value": "i-0123456789abcdef0" }]
        }
    })
}

/// One row: the case, the request, and the message the refusal must carry.
struct Row {
    case: &'static str,
    uri: &'static str,
    body: Value,
    message: String,
}

/// The rows span what the rule generalises over: the OPERATION, the constraint CLASS (an
/// enumerated value set, a `@pattern`, and a set of constraints a collection's elements
/// declare), the enumerated TYPE, the DEPTH of the member path (a top-level member, a member
/// inside a list of structures, a collection's elements), the FORM of the offending value
/// (wrong case, unmodelled token, empty, over a length bound) and, for a collection, WHICH
/// element breaks it and HOW MANY of the element's constraints it breaks.
fn rows() -> Vec<Row> {
    let aggregation = |case, aggregation_type: Value| Row {
        case,
        uri: "/findings/aggregation/list",
        body: json!({ "aggregationType": aggregation_type }),
        message: one_error("aggregationType", &enum_value_set(AGGREGATION_TYPE_SET)),
    };
    let findings_filter = |case, member: &'static str, comparison: &'static str| Row {
        case,
        uri: "/findings/list",
        body: filter_on(member, comparison),
        message: one_error(
            &format!("filterCriteria.{member}.1.member.comparison"),
            &enum_value_set(STRING_COMPARISON_SET),
        ),
    };
    let coverage_filter = |case, member: &'static str, comparison: &'static str| Row {
        case,
        uri: "/coverage/list",
        body: filter_on(member, comparison),
        message: one_error(
            &format!("filterCriteria.{member}.1.member.comparison"),
            &enum_value_set(COVERAGE_STRING_COMPARISON_SET),
        ),
    };
    let report_id = |case, uri, report_id: &'static str| Row {
        case,
        uri,
        body: json!({ "reportId": report_id }),
        message: one_error("reportId", &regular_expression(REPORT_ID_PATTERN)),
    };
    let finding_arns = |case, arns: Value| Row {
        case,
        uri: "/findings/details/batch/get",
        body: json!({ "findingArns": arns }),
        message: one_error("findingArns", &finding_arn_constraints()),
    };
    // The two enumerated members of the findings `sortCriteria`, each refused at its own
    // nested path with its own type's whole published set. Both members are required, so a
    // row fills the other one with a value its enum admits.
    let sort_field = |case, field: &'static str| Row {
        case,
        uri: "/findings/list",
        body: json!({ "sortCriteria": { "field": field, "sortOrder": "DESC" } }),
        message: one_error("sortCriteria.field", &enum_value_set(SORT_FIELD_SET)),
    };
    let sort_order = |case, order: &'static str| Row {
        case,
        uri: "/findings/list",
        body: json!({ "sortCriteria": { "field": "SEVERITY", "sortOrder": order } }),
        message: one_error("sortCriteria.sortOrder", &enum_value_set(SORT_ORDER_SET)),
    };

    vec![
        // An enumerated type at a top-level member, over three forms of bad value.
        aggregation("an aggregation type in the wrong case", json!("Ami")),
        aggregation(
            "an aggregation type no release declares",
            json!("NOT_A_REAL_AGGREGATION_TYPE"),
        ),
        aggregation("an empty aggregation type", json!("")),
        aggregation(
            "the one aggregation type the service publishes no more",
            json!("CODE_REPOSITORY_BUT_NOT_QUITE"),
        ),
        // The other top-level enumerated member of the coverage surface.
        Row {
            case: "a coverage group key in the wrong case",
            uri: "/coverage/statistics/list",
            body: json!({ "groupBy": "resourceType" }),
            message: one_error("groupBy", &enum_value_set(GROUP_KEY_SET)),
        },
        Row {
            case: "a coverage group key no release declares",
            uri: "/coverage/statistics/list",
            body: json!({ "groupBy": "NOT_A_REAL_GROUP_KEY" }),
            message: one_error("groupBy", &enum_value_set(GROUP_KEY_SET)),
        },
        // The same enumerated type inside a list of structures, on two different members —
        // one the findings fixtures exercise and one they do not.
        findings_filter(
            "a findings filter comparison in the wrong case",
            "resourceId",
            "equals",
        ),
        findings_filter(
            "a findings filter comparison on another member of the same criteria",
            "severity",
            "NOT_A_REAL_COMPARISON",
        ),
        findings_filter(
            "a findings filter comparison that is empty",
            "findingArn",
            "",
        ),
        // The same member ABSENT, which carried no value to withhold in the first place.
        Row {
            case: "a findings filter element that carries no comparison at all",
            uri: "/findings/list",
            body: json!({
                "filterCriteria": { "resourceId": [{ "value": "i-0123456789abcdef0" }] }
            }),
            message: one_error(
                "filterCriteria.resourceId.1.member.comparison",
                "Member must not be null",
            ),
        },
        Row {
            case: "a coverage filter element that carries no value at all",
            uri: "/coverage/list",
            body: json!({
                "filterCriteria": { "resourceId": [{ "comparison": "EQUALS" }] }
            }),
            message: one_error(
                "filterCriteria.resourceId.1.member.value",
                "Member must not be null",
            ),
        },
        // The coverage criteria's own comparison type, which admits fewer values.
        coverage_filter(
            "a coverage filter comparison the findings surface admits",
            "resourceId",
            "PREFIX",
        ),
        coverage_filter(
            "a coverage filter comparison on another member of the same criteria",
            "scanStatusCode",
            "prefix",
        ),
        // Two enumerated members of ONE nested structure, over four forms of bad value: the
        // sort criteria of the findings surface.
        sort_field("a sort field in the wrong case", "severity"),
        sort_field("a sort field no release declares", "NOT_A_REAL_SORT_FIELD"),
        sort_field("an empty sort field", ""),
        sort_field(
            "a sort field that is a neighbouring type's modelled token",
            "PACKAGE",
        ),
        sort_order("a sort order in the wrong case", "desc"),
        sort_order("a sort order no release declares", "DESCENDING"),
        // A `@pattern` at a top-level member, on two operations carrying the same shape.
        report_id(
            "a report id that is not a UUID at all",
            "/reporting/status/get",
            "not-a-report",
        ),
        report_id(
            "the same shape under another operation's member",
            "/reporting/cancel",
            "0123",
        ),
        // A collection whose ELEMENTS are constrained: the clause names the collection and
        // nests the element shape's whole set, whichever element broke it and however many
        // of its constraints that element broke.
        finding_arns(
            "one finding ARN that is not a finding ARN",
            json!(["not-an-arn"]),
        ),
        finding_arns(
            "a second element that is not a finding ARN",
            json!([FINDING_ARN, "not-an-arn"]),
        ),
        finding_arns(
            "an element breaking the expression and the length bound at once",
            json!([FINDING_ARN, "x".repeat(101)]),
        ),
        finding_arns(
            "two offending elements, which are one validation error about one member",
            json!(["not-an-arn", "also-not-an-arn"]),
        ),
        finding_arns("an element that is empty", json!([""])),
    ]
}

/// Every row is refused 400 `ValidationException` with the clause the rule states — the
/// member path, the requirement, and no value the request carried.
#[tokio::test]
async fn a_constraint_refusal_states_the_requirement_and_no_submitted_value() {
    let router = router();
    for row in rows() {
        let (status, error_type, body) = post_in(&router, "us-east-1", row.uri, &row.body).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status} / {body}",
            row.case
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {:?}: expected ValidationException, got {error_type:?} / {body}",
            row.case
        );
        let message = body["message"].as_str().unwrap_or_else(|| {
            panic!("case {:?}: the refusal carries no message: {body}", row.case)
        });
        assert_eq!(
            message, row.message,
            "case {:?}: unexpected refusal wording, got {message:?}",
            row.case
        );
    }
}

/// The refusal does not vary with the value: every unmodelled `comparison` is refused with
/// one sentence, so a client matching it matches for every bad input rather than for one.
#[tokio::test]
async fn the_refusal_is_the_same_sentence_for_every_rejected_value() {
    let router = router();
    // Rows differ along the FORM of the rejected value: case, length, emptiness, a modelled
    // token of a neighbouring type, punctuation and a non-ASCII value.
    let values: [&str; 7] = [
        "",
        "equals",
        "Equals",
        "EQUALS ",
        "EQUALS=EQUALS",
        "НЕ_РАВНО",
        "AN_UNMODELLED_COMPARISON_THAT_IS_RATHER_LONGER_THAN_THE_OTHERS",
    ];

    let expected = one_error(
        "filterCriteria.resourceId.1.member.comparison",
        &enum_value_set(STRING_COMPARISON_SET),
    );
    for value in values {
        let (status, _, body) =
            post_in(&router, "us-east-1", "/findings/list", &filter_on("resourceId", value)).await;
        assert_eq!(
            status, 400,
            "case {value:?}: expected 400, got {status} / {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {value:?}: every rejected value must be refused with one sentence, got {body}"
        );
        assert!(
            !body["message"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("'{value}'")),
            "case {value:?}: the refusal must not quote the submitted value, got {body}"
        );
    }
}

/// A value every modelled constraint admits reaches the HANDLER: the rows above fail for the
/// constraint they name and not because the request was malformed in some other way.
///
/// Each row states the status its operation answers once the boundary has let the request
/// through — 200 for the one of these operations that is implemented, and the 501 an operation
/// still carrying the scaffold header answers (`grep -rl '^//! SCAFFOLDED, NOT IMPLEMENTED:'
/// src/ops/`) — so the row cannot be satisfied by a refusal of any kind.
#[tokio::test]
async fn a_value_inside_every_constraint_reaches_the_handler() {
    let router = router();
    // One admitted request per operation the rows refuse, each carrying the member the
    // matching row broke, filled with a value its constraints admit.
    let served: [(&str, &str, Value, u16); 7] = [
        (
            "an aggregation type the enum declares",
            "/findings/aggregation/list",
            json!({ "aggregationType": "ACCOUNT" }),
            200,
        ),
        (
            "a group key the enum declares",
            "/coverage/statistics/list",
            json!({ "groupBy": "RESOURCE_TYPE" }),
            501,
        ),
        (
            "a findings filter comparison the enum declares",
            "/findings/list",
            filter_on("resourceId", "PREFIX"),
            501,
        ),
        (
            "a sort criteria both of its enums declare",
            "/findings/list",
            json!({ "sortCriteria": { "field": "EPSS_SCORE", "sortOrder": "ASC" } }),
            501,
        ),
        (
            "a coverage filter comparison the enum declares",
            "/coverage/list",
            filter_on("resourceId", "NOT_EQUALS"),
            501,
        ),
        (
            "a report id the expression admits",
            "/reporting/status/get",
            json!({ "reportId": "0123abcd-4567-89ab-cdef-0123456789ab" }),
            501,
        ),
        (
            "a finding ARN the expression admits",
            "/findings/details/batch/get",
            json!({ "findingArns": [FINDING_ARN] }),
            501,
        ),
    ];

    for (case, uri, body, expected) in served {
        let (status, error_type, answer) = post_in(&router, "us-east-1", uri, &body).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected} once the boundary admitted the request, got \
             {status} / {error_type:?} / {answer}"
        );
        assert_ne!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: a request inside every constraint must not be refused, got {answer}"
        );
        assert!(
            !answer["message"]
                .as_str()
                .unwrap_or_default()
                .contains("validation error detected"),
            "case {case:?}: a request inside every constraint must not be refused, got {answer}"
        );
    }
}
