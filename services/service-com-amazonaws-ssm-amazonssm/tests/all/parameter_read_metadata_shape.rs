// SPDX-License-Identifier: CC0-1.0

//! The SHAPE of the metadata a Parameter Store read reports, end to end over the awsJson1_1
//! wire: the resolution an instant carries, and what a read says about an attribute the
//! parameter carries no value for.
//!
//! Two rules are under test, each held across every read of the plane
//! (`GetParameter`, `GetParameters`, `GetParametersByPath`, `GetParameterHistory`,
//! `DescribeParameters`), because both are properties of the SERIALIZATION rather than of one
//! operation:
//!
//! 1. **An instant is reported at millisecond resolution.** `Parameter$LastModifiedDate` and
//!    `ParameterMetadata$LastModifiedDate` target `DateTime` (`@timestamp`,
//!    `smithy_query(query="shape",shape="ParameterMetadata")`) and this protocol puts a
//!    timestamp on the wire as epoch seconds with a fraction; the finest fraction AWS states
//!    one with is the millisecond, so a reported instant carries at most three fractional
//!    digits whatever precision the host clock that stamped it reads. A client that decodes,
//!    compares or round-trips the number reads a value of the shape the service emits.
//! 2. **An optional text member a parameter carries no value for is omitted.**
//!    `ParameterMetadata$AllowedPattern` and `$Description` are optional strings, and a
//!    parameter with no effective pattern or no description has no value for them — the empty
//!    string is a value, so reporting it would tell a client a pattern that matches nothing is
//!    configured and hand code that compiles the member an empty expression.
//!
//! The rows differ along what the rules generalise over: which read reports the member, how
//! many versions and parameters it reports at once, and — for the second rule — whether the
//! put stated no value for the member, stated it empty, or stated one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call, router};

/// How many fractional digits a reported instant may carry: the millisecond is the finest
/// fraction AWS states one with, so three.
const MAX_FRACTIONAL_DIGITS: usize = 3;

/// The hierarchy every parameter this suite writes lives under, so one path read reports
/// them all.
const PATH: &str = "/read-shape";

/// One member-omission row: the case, the parameter's name, the `AllowedPattern` and
/// `Description` its put states (`None` to state the member not at all), and what a read must
/// then report for each — which is the value when there is one and NOTHING when there is not.
struct Row {
    case: &'static str,
    name: &'static str,
    allowed_pattern: Option<&'static str>,
    description: Option<&'static str>,
}

impl Row {
    /// What a read must report for `AllowedPattern`: the stated pattern, unless the put
    /// stated no value for it — a member stated empty states no value.
    fn reported_pattern(&self) -> Option<&'static str> {
        self.allowed_pattern.filter(|pattern| !pattern.is_empty())
    }

    /// What a read must report for `Description`, by the same rule.
    fn reported_description(&self) -> Option<&'static str> {
        self.description.filter(|description| !description.is_empty())
    }
}

/// The class the second rule covers: a put that states neither member, one that states each
/// of them empty, one that states them, and one that states a pattern but no description.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a put stating neither member",
            name: "/read-shape/bare",
            allowed_pattern: None,
            description: None,
        },
        Row {
            case: "a put stating both members empty",
            name: "/read-shape/empty",
            allowed_pattern: Some(""),
            description: Some(""),
        },
        Row {
            case: "a put stating an empty pattern and a description",
            name: "/read-shape/empty-pattern",
            allowed_pattern: Some(""),
            description: Some("described, with no pattern"),
        },
        Row {
            case: "a put stating both members",
            name: "/read-shape/stated",
            allowed_pattern: Some("^[a-z]+$"),
            description: Some("described, with a pattern"),
        },
    ]
}

/// The `PutParameter` body of a row, carrying only the members the row states.
fn put_body(row: &Row) -> Value {
    let mut body = json!({ "Name": row.name, "Type": "String", "Value": "value" });
    if let Some(pattern) = row.allowed_pattern {
        body["AllowedPattern"] = json!(pattern);
    }
    if let Some(description) = row.description {
        body["Description"] = json!(description);
    }
    body
}

/// Write every row's parameter, and a second version of one of them so a history read has
/// more than one entry to report.
async fn seeded() -> MultiServiceRouter {
    let router = router();
    for row in rows() {
        let (status, written) = call(&router, "PutParameter", &put_body(&row)).await;
        assert_eq!(
            status, 200,
            "case {:?}: PutParameter must succeed, got {status} {written}",
            row.case
        );
    }
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": "/read-shape/bare", "Value": "second", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the second version must be written, got {status} {written}"
    );
    router
}

/// Every `LastModifiedDate` in `body`, in the order they appear, as the decimal text the
/// number carries.
fn instants(body: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_instants(body, &mut found);
    found
}

/// [`instants`]'s walk of the response tree.
fn collect_instants(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                if name == "LastModifiedDate" {
                    found.push(member.to_string());
                } else {
                    collect_instants(member, found);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                collect_instants(entry, found);
            }
        }
        _ => {}
    }
}

/// The reads of the plane, as `(operation, request)` pairs: each reports at least one
/// parameter this suite wrote.
fn reads() -> Vec<(&'static str, Value)> {
    vec![
        ("GetParameter", json!({ "Name": "/read-shape/bare" })),
        (
            "GetParameters",
            json!({ "Names": ["/read-shape/bare", "/read-shape/stated"] }),
        ),
        ("GetParametersByPath", json!({ "Path": PATH })),
        (
            "GetParameterHistory",
            json!({ "Name": "/read-shape/bare" }),
        ),
        (
            "DescribeParameters",
            json!({ "ParameterFilters": [{ "Key": "Path", "Values": [PATH] }] }),
        ),
    ]
}

#[tokio::test]
async fn every_read_reports_its_instants_at_millisecond_resolution() {
    let router = seeded().await;
    for (operation, request) in reads() {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {operation}: the read must succeed, got {status} {body}"
        );
        let reported = instants(&body);
        assert!(
            !reported.is_empty(),
            "case {operation}: the read must report an instant, got {body}"
        );
        for instant in &reported {
            let fraction = instant.split_once('.').map_or("", |(_, digits)| digits);
            assert!(
                fraction.len() <= MAX_FRACTIONAL_DIGITS,
                "case {operation}: expected at most {MAX_FRACTIONAL_DIGITS} fractional \
                 digits, got {instant}"
            );
            assert!(
                !instant.contains('e') && !instant.contains('E'),
                "case {operation}: expected a plain epoch-seconds number, got {instant}"
            );
            assert!(
                instant.parse::<f64>().is_ok_and(|seconds| seconds > 0.0),
                "case {operation}: expected an instant after the epoch, got {instant}"
            );
        }
        // The same read again reports the same instants: a read reports what a write
        // stamped, so nothing of it is recomputed per call.
        let (status, again) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {operation}: the second read must succeed, got {status} {again}"
        );
        assert_eq!(
            instants(&again),
            reported,
            "case {operation}: expected the same instants on a second read, got {again}"
        );
    }
}

#[tokio::test]
async fn a_metadata_read_omits_an_attribute_the_parameter_carries_no_value_for() {
    let router = seeded().await;
    for row in rows() {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            &json!({ "ParameterFilters": [{ "Key": "Name", "Values": [row.name] }] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeParameters must succeed, got {status} {body}",
            row.case
        );
        let reported = body["Parameters"]
            .as_array()
            .and_then(|parameters| parameters.first().cloned())
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the describe must report {}, got {body}",
                    row.case, row.name
                )
            });
        assert_member(row.case, "DescribeParameters", &reported, "AllowedPattern", row.reported_pattern());
        assert_member(row.case, "DescribeParameters", &reported, "Description", row.reported_description());

        // The history reports the attributes of the version it names, by the same rule.
        let (status, body) = call(
            &router,
            "GetParameterHistory",
            &json!({ "Name": row.name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: GetParameterHistory must succeed, got {status} {body}",
            row.case
        );
        let entries = body["Parameters"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the history reports entries, got {body}", row.case));
        for entry in &entries {
            assert_member(row.case, "GetParameterHistory", entry, "AllowedPattern", row.reported_pattern());
            assert_member(row.case, "GetParameterHistory", entry, "Description", row.reported_description());
        }
    }
}

/// Assert that `reported` carries `member` exactly when `expected` is a value, and carries
/// the value it then is: a member with no value is ABSENT rather than empty.
fn assert_member(
    case: &str,
    operation: &str,
    reported: &Value,
    member: &str,
    expected: Option<&str>,
) {
    let actual = reported.get(member);
    match expected {
        None => assert!(
            actual.is_none(),
            "case {case:?}: {operation} must report no {member}, got {reported}"
        ),
        Some(expected) => assert_eq!(
            actual.and_then(Value::as_str),
            Some(expected),
            "case {case:?}: {operation} must report {member} {expected:?}, got {reported}"
        ),
    }
}
