// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a LIST carrying more entries than the maximum the model declares
//! for it — the CLASS of that refusal and the clause it states, on the alarm operations.
//!
//! The rule under test: **a violation of a list's modelled `@length` maximum is a
//! constraint-validation failure, so it is answered in the request-validation class — HTTP 400,
//! body `__type` the qualified id of that class, and the legacy `x-amzn-query-error` report
//! `ValidationError;Sender` for a query-compatible client — with AWS's constraint clause: the
//! collection the request carried, quoted in request order, at the member path in lowerCamelCase,
//! and the maximum the model declares.**
//!
//! It is not a parameter-VALUE failure: an SDK maps the two to different modelled error types and
//! the `awsQuery` error mapper keys on the legacy report, so a client branching on either must
//! take the same path here as against AWS.
//!
//! The rows span what the rule generalises over — which list member carries the collection (each
//! of the three action lists every alarm writer takes, and the alarm-name lists a write, a read
//! and an action switch take), which operation it arrived on, the DEPTH it sits at (a member of
//! the request and a member of a structure inside it), the modelled maximum it broke, how many
//! entries past that maximum the request carried, and the FORM of the entries — because the class
//! and the clause are properties of the constraint, not of any one member or recorded request.
//!
//! A collection AT its maximum is tabled beside them: it is served and reads back, which is what
//! makes the refusals above about the entry past the maximum rather than about the member being
//! submitted at all.

use serde_json::{json, Value};

use crate::harness::{
    build_router, call_in_query_mode, call_with_headers, described_alarm, error_message,
    log_alarm_request, merged, metric_alarm_request, ok, AlarmKind,
};

/// The header a query-compatible client reads the legacy error code and the fault side in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The shape id the body names the request-validation class under: the class belongs to the
/// validation layer in front of the service, which is the package the id names.
const VALIDATION_SHAPE_ID: &str = "com.amazon.coral.validate#ValidationException";

/// The legacy query report of that class: its `awsQueryError` code and the fault side.
const VALIDATION_REPORT: &str = "ValidationError;Sender";

/// The maximum a list of alarm ACTIONS may carry, as the model declares it
/// (`smithy_query(query="shape",shape="ResourceList")`: `@length {min: 0, max: 5}`) — the shape
/// every action member of every alarm writer targets.
const ACTIONS_MAX: usize = 5;

/// The maximum a list of ALARM NAMES may carry
/// (`smithy_query(query="shape",shape="AlarmNames")`: `@length {min: 0, max: 100}`).
const ALARM_NAMES_MAX: usize = 100;

/// The three action lists every alarm writer takes, each targeting `ResourceList`
/// (`smithy_query(query="operation",operation="PutMetricAlarm,PutCompositeAlarm,PutLogAlarm")`),
/// paired with the member path in lowerCamelCase a refusal names each one at — the modelled
/// member name with its FIRST letter lowered and nothing else changed, which is why the member
/// spelled `OKActions` is named `oKActions`.
const ACTION_MEMBERS: [(&str, &str); 3] = [
    ("AlarmActions", "alarmActions"),
    ("OKActions", "oKActions"),
    ("InsufficientDataActions", "insufficientDataActions"),
];

/// The clause AWS states for a collection longer than its modelled maximum: the collection the
/// request carried, the member path it arrived on, and that maximum.
fn over_the_maximum(submitted: &[String], member: &str, maximum: usize) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at '{member}' failed to satisfy constraint: \
         Member must have length less than or equal to {maximum}",
        submitted.join(", ")
    )
}

/// The forms an alarm action may take, as the member documents them: an SNS topic, a Lambda
/// function, an EC2 instance action and an OpsItem
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="AlarmActions")`). A row's
/// entries cycle through them, so the quoted collection is not one repeated string.
const ACTION_FORMS: [&str; 4] = [
    "arn:aws:sns:us-east-1:000000000000:topic",
    "arn:aws:lambda:us-east-1:000000000000:function:page",
    "arn:aws:automate:us-east-1:ec2:reboot",
    "arn:aws:ssm:us-east-1:000000000000:opsitem:3",
];

/// `count` actions, cycling through the forms above and numbered by position so each entry is
/// distinct — as a caller attaching that many actions sends them. The number is appended to the
/// form's own resource, which is what a request over the maximum carries: such a request is
/// refused by the constraint walk, before any handler reads an ARN.
fn actions(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{}-{index}", ACTION_FORMS[index % ACTION_FORMS.len()]))
        .collect()
}

/// `count` actions an alarm writer ACCEPTS: distinct SNS topics, the form a notification action
/// takes, so the served rows turn on the collection's SIZE rather than on any ARN's own grammar.
fn accepted_actions(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("arn:aws:sns:us-east-1:000000000000:alarm-topic-{index}"))
        .collect()
}

/// `count` alarm names, as a caller that lists or targets that many sends them.
fn alarm_names(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("alarm-{index}")).collect()
}

/// One refused call: what it varies, the call itself, and the clause the answer owes.
struct Row {
    /// What this row varies, named in every assertion.
    case: String,
    operation: &'static str,
    request: Value,
    /// The member path the clause names, in the lowerCamelCase AWS spells it in.
    member: &'static str,
    /// The modelled maximum the collection broke.
    maximum: usize,
    /// The collection the request carried, in request order.
    submitted: Vec<String>,
}

/// Every row: one per action member of each of the three alarm writers, at one and at three
/// entries past the maximum, plus the alarm-name lists of a write, a read and an action switch.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (wire, path) in ACTION_MEMBERS {
        for (writer, base) in [
            ("PutMetricAlarm", metric_alarm_request("over-actioned")),
            (
                "PutCompositeAlarm",
                json!({"AlarmName": "over-actioned", "AlarmRule": "ALARM(\"child\")"}),
            ),
            ("PutLogAlarm", log_alarm_request("over-actioned")),
        ] {
            for past in [1, 3] {
                let submitted = actions(ACTIONS_MAX + past);
                rows.push(Row {
                    case: format!("{writer}'s {wire}, {past} past the {ACTIONS_MAX} it may take"),
                    operation: writer,
                    request: merged(base.clone(), &json!({wire: submitted.clone()})),
                    member: path,
                    maximum: ACTIONS_MAX,
                    submitted,
                });
            }
        }
    }
    let names = alarm_names(ALARM_NAMES_MAX + 1);
    rows.push(Row {
        case: "the alarm names a mute rule targets, one structure deep in the request".to_string(),
        operation: "PutAlarmMuteRule",
        request: json!({
            "Name": "over-targeted",
            "Rule": {"Schedule": {"Expression": "cron(0 2 * * *)", "Duration": "PT1H"}},
            "MuteTargets": {"AlarmNames": names.clone()},
        }),
        member: "muteTargets.alarmNames",
        maximum: ALARM_NAMES_MAX,
        submitted: names.clone(),
    });
    rows.push(Row {
        case: "the alarm names a READ is narrowed to".to_string(),
        operation: "DescribeAlarms",
        request: json!({"AlarmNames": names.clone()}),
        member: "alarmNames",
        maximum: ALARM_NAMES_MAX,
        submitted: names.clone(),
    });
    rows.push(Row {
        case: "the alarm names an action SWITCH lists".to_string(),
        operation: "EnableAlarmActions",
        request: json!({"AlarmNames": names.clone()}),
        member: "alarmNames",
        maximum: ALARM_NAMES_MAX,
        submitted: names,
    });
    rows
}

#[tokio::test]
async fn a_list_over_its_modelled_maximum_is_the_request_validation_class() {
    let router = build_router();
    for Row {
        case,
        operation,
        request,
        member,
        maximum,
        submitted,
    } in rows()
    {
        let expected = over_the_maximum(&submitted, member, maximum);

        // 1. A query-compatible client reads the class in both halves of the envelope.
        let (status, headers, body) = call_in_query_mode(&router, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let reported_type = body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported_type, VALIDATION_SHAPE_ID,
            "case {case:?}: expected the body to name {VALIDATION_SHAPE_ID}, got \
             {reported_type:?} in {body}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, VALIDATION_REPORT,
            "case {case:?}: expected {QUERY_ERROR_HEADER} to read {VALIDATION_REPORT:?}, got \
             {reported:?}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // 2. A native JSON client gets the same answer and no legacy report: the report is an
        // addition for a client that asked for it.
        let (plain_status, plain_headers, plain_body) =
            call_with_headers(&router, operation, &request).await;
        let plain_message = error_message(&plain_body);
        assert_eq!(
            (
                plain_status,
                plain_body
                    .get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                plain_message.as_str()
            ),
            (400, VALIDATION_SHAPE_ID, expected.as_str()),
            "case {case:?}: a native JSON client must get the same answer, got {plain_status} \
             with {plain_body}"
        );
        assert!(
            plain_headers.get(QUERY_ERROR_HEADER).is_none(),
            "case {case:?}: a client that did not opt in must get no report, got {:?}",
            plain_headers.get(QUERY_ERROR_HEADER)
        );
    }
}

#[tokio::test]
async fn a_list_at_its_modelled_maximum_is_served_and_reads_back() {
    let router = build_router();

    // The five actions each member may carry are accepted on a metric alarm and read back, which
    // is what makes the refusals above about the entry PAST the maximum.
    let filled = accepted_actions(ACTIONS_MAX);
    let mut request = metric_alarm_request("fully-actioned");
    for (wire, _) in ACTION_MEMBERS {
        request = merged(request, &json!({wire: filled.clone()}));
    }
    ok(&router, "PutMetricAlarm", &request).await;
    let stored = described_alarm(&router, AlarmKind::Metric, "fully-actioned")
        .await
        .expect("the alarm an accepted write created is reported");
    for (wire, _) in ACTION_MEMBERS {
        let reported = stored.get(wire).and_then(Value::as_array).cloned();
        let expected: Vec<Value> = filled.iter().map(|arn| json!(arn)).collect();
        assert_eq!(
            reported.as_deref(),
            Some(expected.as_slice()),
            "case {wire:?}: a collection at its maximum must be stored in request order, got \
             {stored}"
        );
    }

    // The hundred names a reader may name are served the same way: an empty listing, not a
    // refusal.
    let listed = ok(
        &router,
        "DescribeAlarms",
        &json!({"AlarmNames": alarm_names(ALARM_NAMES_MAX)}),
    )
    .await;
    let reported = listed
        .get("MetricAlarms")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        reported, 0,
        "case \"a read at the alarm-name maximum\": expected no alarm to match those names, got \
         {listed}"
    );
}
