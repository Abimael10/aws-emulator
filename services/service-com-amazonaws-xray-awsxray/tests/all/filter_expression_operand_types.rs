// SPDX-License-Identifier: CC0-1.0

//! How a filter expression whose operands cannot be compared is refused, on
//! every surface that takes one, end to end over the wire (the `Xray` harness in
//! [`super::xray_wire`]).
//!
//! The rule: a comparison is NUMERIC when either side makes it so — the keyword
//! it names holds numbers, or the operator orders its operands, which only
//! numbers do — and such a comparison is refused for the side at fault, naming
//! the keyword as the expression wrote it and the operator it was written with:
//! a keyword whose values are not numbers cannot be compared under that
//! operator, and a value that is not a number cannot be compared against a
//! keyword whose values are. One judgement, so every surface that takes an
//! expression — the two that STORE one (`CreateGroup`, `UpdateGroup`) and the
//! one that EVALUATES it (`GetTraceSummaries`) — refuses one expression with one
//! sentence, and a refused store writes nothing.
//!
//! The rows differ along which side is at fault, along the type of the keyword
//! (number, string, boolean, and an annotation, whose type is the submitted
//! value's), along the type of the literal (string, number, boolean) and along
//! the operator (each ordering operator, equality, and a string operator), with
//! well-typed controls that must keep being served.

use serde_json::{json, Value};

use super::xray_wire::{array, Xray};

/// The keywords the rows below name, with the type each one holds, from the
/// catalog of indexed fields the crate's one parser carries
/// (`service_com_amazonaws_xray_awsxray::state::filter_expression`, which takes
/// it from AWS's "Using filter expressions" keyword reference): `duration`,
/// `responsetime`, `http.status` and a response-time cause's `coverage` hold
/// numbers, the rest of `http.*`, `user`, `resource.arn` and `trace_id` hold
/// strings, `ok`, `error`, `fault`, `throttle` and `partial` hold booleans, and
/// `annotation.<key>` holds whatever the segment document recorded.
const NUMERIC_KEYWORDS: [&str; 4] = [
    "duration",
    "responsetime",
    "http.status",
    "rootcause.responsetime.entity.coverage",
];

/// The window every query below asks about: well in the past, so no bound of the
/// read is at stake here.
const WINDOW: (f64, f64) = (1_000.0, 2_000.0);

/// One surface an expression arrives on: what to call it in a case label, the
/// path it is called at, and the request it takes the expression in.
struct Surface {
    name: &'static str,
    path: &'static str,
    request: fn(&str) -> Value,
}

/// The three surfaces an expression arrives on: the two that store one and the
/// one that evaluates it. Each takes the expression in a member of its own, so
/// each row is asked on each of them with the SAME expression.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            name: "CreateGroup",
            path: "/CreateGroup",
            request: |expression| {
                json!({ "GroupName": "operand-probe", "FilterExpression": expression })
            },
        },
        Surface {
            name: "UpdateGroup",
            path: "/UpdateGroup",
            request: |expression| {
                json!({ "GroupName": "stored-group", "FilterExpression": expression })
            },
        },
        Surface {
            name: "GetTraceSummaries",
            path: "/TraceSummaries",
            request: |expression| {
                json!({
                    "StartTime": WINDOW.0,
                    "EndTime": WINDOW.1,
                    "FilterExpression": expression,
                })
            },
        },
    ]
}

/// A group every `UpdateGroup` row writes to, so the row is refused for its
/// expression rather than for an unknown group.
async fn seed_group(xray: &Xray) {
    let created = xray
        .call(
            "/CreateGroup",
            &json!({ "GroupName": "stored-group", "FilterExpression": "fault" }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the fixture group must be created, got {created:?}"
    );
}

/// Whether the account holds a group named `name`.
async fn holds_group(xray: &Xray, name: &str) -> bool {
    let listed = xray.get_groups(None).await;
    array(&listed.body, "Groups").iter().any(|group| {
        group.get("GroupName").and_then(Value::as_str) == Some(name)
    })
}

#[tokio::test]
async fn a_mistyped_operand_is_refused_the_same_way_on_every_surface() {
    // Each row is one expression and the one sentence every surface must answer
    // it with.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a quoted operand against a numeric keyword, ordered",
            "responsetime > \"abc\"".to_string(),
            "You can't compare a non-numeric value of type [String] against property \
             [responsetime] for the > operator."
                .to_string(),
        ),
        (
            "a quoted operand against a numeric keyword, under >=",
            "duration >= \"5\"".to_string(),
            "You can't compare a non-numeric value of type [String] against property [duration] \
             for the >= operator."
                .to_string(),
        ),
        (
            "a boolean operand against a numeric keyword, under <",
            "http.status < true".to_string(),
            "You can't compare a non-numeric value of type [Boolean] against property \
             [http.status] for the < operator."
                .to_string(),
        ),
        (
            "a quoted operand against a numeric keyword, under equality",
            "responsetime = \"abc\"".to_string(),
            "You can't compare a non-numeric value of type [String] against property \
             [responsetime] for the = operator."
                .to_string(),
        ),
        (
            "a quoted operand against an annotation, ordered",
            "annotation.latency > \"abc\"".to_string(),
            "You can't compare a non-numeric value of type [String] against property \
             [annotation.latency] for the > operator."
                .to_string(),
        ),
        (
            "a numeric operator on a string keyword",
            "http.url > 5".to_string(),
            "You can't compare a non-numeric property [http.url] for the > operator.".to_string(),
        ),
        (
            "a numeric operator on another string keyword, under <",
            "user < 3".to_string(),
            "You can't compare a non-numeric property [user] for the < operator.".to_string(),
        ),
        (
            "a numeric operator on a string keyword given a string",
            "resource.arn <= \"arn:aws:ec2:us-east-1:123456789012:instance/i-1\"".to_string(),
            "You can't compare a non-numeric property [resource.arn] for the <= operator."
                .to_string(),
        ),
        (
            "a numeric operator on a boolean keyword",
            "fault > 1".to_string(),
            "You can't compare a non-numeric property [fault] for the > operator.".to_string(),
        ),
        (
            "a numeric operator on a boolean keyword, under >=",
            "partial >= 0".to_string(),
            "You can't compare a non-numeric property [partial] for the >= operator.".to_string(),
        ),
        (
            "a mistyped operand inside a larger expression",
            "http.method = \"GET\" AND responsetime > \"abc\"".to_string(),
            "You can't compare a non-numeric value of type [String] against property \
             [responsetime] for the > operator."
                .to_string(),
        ),
    ];

    for (case, expression, expected) in rows {
        for surface in surfaces() {
            let xray = Xray::new();
            seed_group(&xray).await;
            let answered = xray.call(surface.path, &(surface.request)(&expression)).await;
            let label = format!("{case} on {}", surface.name);
            assert_eq!(
                (answered.status, answered.error_type.as_deref()),
                (400, Some("InvalidRequestException")),
                "case {label:?}: {expression} must be refused as an invalid request, got \
                 {answered:?}"
            );
            let said = answered.message().unwrap_or_default();
            assert_eq!(
                said, expected,
                "case {label:?}: expected {expected:?}, got {said:?}"
            );
            assert!(
                !holds_group(&xray, "operand-probe").await,
                "case {label:?}: a refused expression must store no group"
            );
        }
    }
}

#[tokio::test]
async fn every_numeric_keyword_refuses_a_quoted_operand_the_same_way() {
    // The rule holds for the keyword CLASS, not for the one keyword the rows
    // above happen to name: each numeric keyword of the catalog refuses a quoted
    // operand in the same sentence, with its own name in it.
    let xray = Xray::new();
    for keyword in NUMERIC_KEYWORDS {
        let expression = format!("{keyword} > \"abc\"");
        let expected = format!(
            "You can't compare a non-numeric value of type [String] against property [{keyword}] \
             for the > operator."
        );
        let answered = xray
            .filtered(WINDOW.0, WINDOW.1, &expression)
            .await;
        let said = answered.message().unwrap_or_default();
        assert_eq!(
            (answered.status, said),
            (400, expected.as_str()),
            "case {keyword:?}: expected {expected:?}, got {answered:?}"
        );
    }
}

#[tokio::test]
async fn a_well_typed_comparison_is_served_on_every_surface() {
    // The controls: every one of them compares a keyword with a value of its own
    // type, so none of them is refused — which is what makes the refusals above
    // about the types rather than about the grammar.
    let controls = [
        "responsetime > 5 AND http.url CONTAINS \"/probe\"",
        "http.status = 200",
        "duration >= 0.25",
        "http.url BEGINSWITH \"https://\"",
        "user = \"ana\"",
        "fault",
        "annotation.latency > 5",
        "annotation.stage = \"beta\"",
        "resource.arn = \"arn:aws:ec2:us-east-1:123456789012:instance/i-1\"",
    ];
    for (nth, expression) in controls.iter().enumerate() {
        for surface in surfaces() {
            let xray = Xray::new();
            seed_group(&xray).await;
            let mut request = (surface.request)(expression);
            if surface.name == "CreateGroup" {
                request["GroupName"] = json!(format!("control-{nth}"));
            }
            let answered = xray.call(surface.path, &request).await;
            let label = format!("{expression} on {}", surface.name);
            assert_eq!(
                answered.status, 200,
                "case {label:?}: a well-typed expression must be served, got {answered:?}"
            );
        }
    }
}
