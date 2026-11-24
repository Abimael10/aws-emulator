// SPDX-License-Identifier: CC0-1.0

//! A whole alarm lifecycle driven over the `awsQuery` form wire, and the refusals that wire
//! answers the calls of that lifecycle with.
//!
//! The rule under test is the PROTOCOL's rather than any one operation's: **a request whose
//! body is form-encoded is an `awsQuery` call of the operation its `Action` field names, and is
//! answered in that protocol's envelope — `text/xml`, a root `<<Operation>Response>` carrying
//! the operation's `<<Operation>Result>` when its output models members and a
//! `<ResponseMetadata><RequestId>` always, with a failure answered as
//! `<ErrorResponse><Error><Type>Sender</Type><Code>…</Code></Error></ErrorResponse>` under the
//! status that error's query code is declared with** — and the state the call reads or writes
//! is the one store, so a write on this wire is reported by a read on it.
//!
//! This service's model declares `awsQuery` beside `awsJson1_0` and `rpcv2Cbor`
//! (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`),
//! so every operation of the API is reachable on it; the rows below differ along what the rule
//! generalises over: the operation called (a write, a read, a state forcing, a deletion), the
//! kind of alarm the lifecycle is driven for, the state forced on it, which selector a read
//! names the alarms it wants by, and — for the refusals — which class the call fails in and so
//! which status and code the envelope carries.

use crate::harness::{build_router, element, elements, query_call};

/// The media type an `awsQuery` client of this service sends its form body under.
const FORM: &str = "application/x-www-form-urlencoded";

/// The namespace and metric the metric alarm of a row watches.
const NAMESPACE: &str = "Custom/QueryLifecycle";
const METRIC: &str = "Faults";

/// The alarm a row drives through the lifecycle, and the child a composite one's rule
/// references.
const ALARM: &str = "query-lifecycle-alarm";
const CHILD: &str = "query-lifecycle-child";

/// Which kind of alarm a row drives the lifecycle for: the two kinds a single call creates,
/// each written by its own operation and reported under its own `DescribeAlarms` member
/// (`smithy_query(query="operation",operation="DescribeAlarms")`: `MetricAlarms`,
/// `CompositeAlarms`). The third kind this service stores, a log alarm, is created by
/// `PutLogAlarm` against a log group, which is a different setup rather than a different row of
/// this rule, so it is left to the suites that own it.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Metric,
    Composite,
}

impl Kind {
    /// The operation that writes an alarm of this kind, and the form fields it carries — a
    /// composite alarm's being the `rule` the caller states over the alarms it watches.
    fn write<'a>(self, name: &'a str, rule: &'a str) -> (&'static str, Vec<(&'a str, &'a str)>) {
        match self {
            Self::Metric => ("PutMetricAlarm", metric_alarm_fields(name)),
            Self::Composite => (
                "PutCompositeAlarm",
                vec![("AlarmName", name), ("AlarmRule", rule)],
            ),
        }
    }

    /// The modelled `AlarmType` a reader asks for this kind by: "If you omit this parameter,
    /// only metric alarms are returned" (`DescribeAlarmsInput$AlarmTypes`).
    fn alarm_type(self) -> &'static str {
        match self {
            Self::Metric => "MetricAlarm",
            Self::Composite => "CompositeAlarm",
        }
    }
}

/// The form fields a `PutMetricAlarm` call carries for a static-threshold alarm on [`METRIC`].
fn metric_alarm_fields(name: &str) -> Vec<(&str, &str)> {
    vec![
        ("AlarmName", name),
        ("Namespace", NAMESPACE),
        ("MetricName", METRIC),
        ("ComparisonOperator", "GreaterThanThreshold"),
        ("EvaluationPeriods", "1"),
        ("Period", "60"),
        ("Statistic", "Average"),
        ("Threshold", "1"),
    ]
}

/// One row of the reading table below: the case, the selectors the read names, and the alarm
/// names it must report. Named so the row type stays simple enough to read.
type ReadCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);

/// One row of the refusal table below: the case, the operation called, the form fields it
/// carries, and the status and query code the class that call fails in is declared with.
type RefusalCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], u16, &'a str);

/// One `awsQuery` call, asserted to be answered in that protocol's SUCCESS envelope: the
/// status, the media type, the operation's own response element, the request id every answer
/// names, and — when the operation's output models members (`results`) — the result element
/// those members are carried in.
async fn served(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
    results: bool,
) -> String {
    let (status, media_type, body) = query_call(router, FORM, &[], action, params).await;
    assert_eq!(
        (status, media_type.as_str()),
        (200, "text/xml"),
        "case {case:?}: expected {action} to be served in the query envelope, got {status} / \
         {media_type:?} with {body}"
    );
    assert!(
        body.contains(&format!("<{action}Response")),
        "case {case:?}: expected the answer in the <{action}Response> envelope, got {body}"
    );
    assert!(
        !element(&body, "RequestId").is_empty(),
        "case {case:?}: expected {action} to name the request it answered, got {body}"
    );
    assert!(
        !results || body.contains(&format!("<{action}Result")),
        "case {case:?}: expected the answered members in a <{action}Result> element, got {body}"
    );
    body
}

#[tokio::test]
async fn an_alarm_is_written_read_forced_and_deleted_over_the_query_wire() {
    // (case, the kind of alarm, the state the row forces on it)
    let rows: [(&str, Kind, &str); 3] = [
        ("a metric alarm forced into ALARM", Kind::Metric, "ALARM"),
        ("a metric alarm forced into OK", Kind::Metric, "OK"),
        (
            "a composite alarm forced into INSUFFICIENT_DATA",
            Kind::Composite,
            "INSUFFICIENT_DATA",
        ),
    ];

    // The rule a composite alarm states over the child every row writes first.
    let rule = format!("ALARM(\"{CHILD}\")");

    for (case, kind, forced) in rows {
        // A fresh emulator per row, so the only alarms a read reports are this row's.
        let router = build_router();
        let (child_action, child_fields) = Kind::Metric.write(CHILD, &rule);
        served(&router, case, child_action, &child_fields, false).await;

        // 1. The write: created on the query wire.
        let (action, params) = kind.write(ALARM, &rule);
        served(&router, case, action, &params, false).await;

        // 2. The read: the alarm the write stored is reported on the same wire, under the
        //    member its kind is reported in, inside the read's own result element.
        let read = [
            ("AlarmNames.member.1", ALARM),
            ("AlarmTypes.member.1", kind.alarm_type()),
        ];
        let described = served(&router, case, "DescribeAlarms", &read, true).await;
        assert_eq!(
            elements(&described, "AlarmName"),
            vec![ALARM.to_string()],
            "case {case:?}: expected the read to report the alarm just written, got {described}"
        );

        // 3. The state forcing, and 4. the read that shows it: the state a client forced over
        //    this wire is the state the store holds, not something the answer made up.
        let forcing = [
            ("AlarmName", ALARM),
            ("StateValue", forced),
            ("StateReason", "forced over the query wire"),
        ];
        served(&router, case, "SetAlarmState", &forcing, false).await;
        let described = served(&router, case, "DescribeAlarms", &read, true).await;
        assert_eq!(
            elements(&described, "StateValue"),
            vec![forced.to_string()],
            "case {case:?}: expected the read to report the forced state {forced:?}, got \
             {described}"
        );
        assert_eq!(
            elements(&described, "StateReason"),
            vec!["forced over the query wire".to_string()],
            "case {case:?}: expected the read to report the reason the forcing gave, got \
             {described}"
        );

        // 5. The deletion, and 6. the read that shows the alarm gone: a lifecycle driven end
        //    to end on one wire.
        served(
            &router,
            case,
            "DeleteAlarms",
            &[("AlarmNames.member.1", ALARM)],
            false,
        )
        .await;
        let described = served(&router, case, "DescribeAlarms", &read, true).await;
        assert!(
            elements(&described, "AlarmName").is_empty(),
            "case {case:?}: expected the deleted alarm to be reported by nothing, got \
             {described}"
        );
    }
}

#[tokio::test]
async fn a_read_on_the_query_wire_reports_what_its_selectors_match() {
    // The envelope is the protocol's, so a read reports the alarms its selectors match — and
    // only those — in the same document whether that is some alarm or none at all. ONE metric
    // alarm is stored first, so every row below has something each selector must either match
    // or exclude.
    let router = build_router();
    served(
        &router,
        "the alarm every row reads against",
        "PutMetricAlarm",
        &metric_alarm_fields(ALARM),
        false,
    )
    .await;

    // (case, the selectors the read names, the alarm names it must report)
    let rows: [ReadCase; 6] = [
        ("no selector at all", &[], &[ALARM]),
        ("the name the stored alarm has", &[("AlarmNames.member.1", ALARM)], &[ALARM]),
        (
            "a name no alarm has",
            &[("AlarmNames.member.1", "nothing-of-this-name")],
            &[],
        ),
        (
            "the prefix the stored alarm's name starts with",
            &[("AlarmNamePrefix", "query-")],
            &[ALARM],
        ),
        ("a prefix nothing matches", &[("AlarmNamePrefix", "absent-")], &[]),
        (
            "a kind no stored alarm is of",
            &[("AlarmTypes.member.1", Kind::Composite.alarm_type())],
            &[],
        ),
    ];

    for (case, params, expected) in rows {
        let body = served(&router, case, "DescribeAlarms", params, true).await;
        let reported = elements(&body, "AlarmName");
        assert_eq!(
            reported,
            expected
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the read to report {expected:?}, got {reported:?} in {body}"
        );
    }
}

#[tokio::test]
async fn a_failed_call_on_the_query_wire_is_answered_in_that_protocols_error_envelope() {
    // (case, action, the form fields, the status and query code the class is declared with)
    //
    // The two classes the calls of this lifecycle fail in, which carry a different status and
    // a different code: a value outside an enumerated member, refused in the
    // request-validation class, and a call naming a resource this account holds none of, which
    // `SetAlarmState` models as `ResourceNotFound` with query code `ResourceNotFound` and HTTP
    // 404 (`smithy_query(query="operation",operation="SetAlarmState")`). The enumerated
    // members of `PutMetricAlarm` are spanned by `crate::query_wire_alarm_round_trip`, which
    // owns the sentence they are refused with.
    let rows: [RefusalCase; 2] = [
        (
            "a state outside its enumerated type",
            "SetAlarmState",
            &[
                ("AlarmName", ALARM),
                ("StateValue", "BROKEN"),
                ("StateReason", "a state no type has"),
            ],
            400,
            "ValidationError",
        ),
        (
            "a state forced on an alarm this account holds none of",
            "SetAlarmState",
            &[
                ("AlarmName", "no-such-alarm"),
                ("StateValue", "ALARM"),
                ("StateReason", "no alarm to force"),
            ],
            404,
            "ResourceNotFound",
        ),
    ];

    for (case, action, params, status, code) in rows {
        let router = build_router();
        let (answered, media_type, body) = query_call(&router, FORM, &[], action, params).await;
        assert_eq!(
            (answered, media_type.as_str()),
            (status, "text/xml"),
            "case {case:?}: expected {action} to be refused in the query envelope under \
             {status}, got {answered} / {media_type:?} with {body}"
        );
        assert!(
            body.contains("<ErrorResponse"),
            "case {case:?}: expected the failure in an <ErrorResponse> document, got {body}"
        );
        assert_eq!(
            (
                element(&body, "Type").as_str(),
                element(&body, "Code").as_str(),
            ),
            ("Sender", code),
            "case {case:?}: expected a Sender fault named by the query code {code:?}, got {body}"
        );
        assert!(
            !element(&body, "RequestId").is_empty(),
            "case {case:?}: expected the failure to name the request it answered, got {body}"
        );
    }
}
