// SPDX-License-Identifier: CC0-1.0

//! What this service RECORDS of the calls it serves: the management events CloudTrail's
//! `LookupEvents` reports for CloudWatch Logs.
//!
//! RULE: a service records every control-plane call it serves as a management event of its
//! own event source, whatever that call answered — a call it served, a call its handler
//! refused and a call the IAM gate in front of the operation refused are all events of the
//! account, each carrying the operation's own `readOnly`, the resources the call addressed,
//! the error code and message the caller was answered with, and the `requestID` that very
//! response named. A call the permission check stopped reached no operation, so it is
//! recorded with no `requestParameters` and no `responseElements`, and under the
//! AUTHORIZATION layer's own error code rather than the error class the caller was answered
//! with. A call on the CONTENTS of a log group is no control-plane call, so event history
//! holds none.
//!
//! The rows span what the rule generalises over: which operation was called (a create, a
//! listing that names no resource, a read of one group, a write on one stream, a delete), the
//! three outcomes a call can have, whether the caller is the account's own credential or a
//! role session IAM allows nothing of this service, and — on the other side of the rule — the
//! log-data calls that are recorded nowhere.
//!
//! The principals are made with IAM's and STS's OWN operations over the same engine, every
//! call goes over the wire as a client's would, and the records are read back out of the
//! SHARED log in that engine: the one store CloudTrail's `LookupEvents` reports.

use serde_json::{json, Value};
use service_common::management_events::{ManagementEvent, ManagementEventLog};

use crate::delivery_identity_authorization::{assumed_role, statement, Emulator, ACCOUNT_KEY};
use crate::harness::REGION;

/// The `eventSource` CloudTrail stamps on a management event of this service, spelled here
/// independently of the crate under test.
const EVENT_SOURCE: &str = "logs.amazonaws.com";

/// The `errorCode` a record of an IAM denial carries: the authorization layer's own code, not
/// the `AccessDeniedException` the caller is answered with. Spelled here independently of the
/// crate under test.
const RECORDED_ACCESS_DENIED: &str = "AccessDenied";

/// The `Resource$resourceType` values CloudTrail names this service's two resource kinds by.
const LOG_GROUP_TYPE: &str = "AWS::Logs::LogGroup";
const LOG_STREAM_TYPE: &str = "AWS::Logs::LogStream";

/// The log group every row addresses, and the stream under it.
const GROUP: &str = "/audited/calls";
const STREAM: &str = "2026/01/01/[$LATEST]audited";

/// A log group name this account holds no group for, for the row whose authorized call the
/// HANDLER refuses.
const ABSENT: &str = "/audited/never-created";

/// The log group ARN this account names `name` by, spelled here independently of the crate
/// under test — the form a caller may address a group by instead of its name.
fn group_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{name}",
        engine::emulator_account_id()
    )
}

/// Every record the shared log holds, newest last.
fn recorded(aws: &Emulator) -> Vec<ManagementEvent> {
    let log = aws.engine().state::<ManagementEventLog>();
    let guard = log.lock();
    guard.iter().cloned().collect()
}

/// The ONE record of `records` that names request `request_id` — the join a client holding a
/// response makes into event history — as the document CloudTrail reports paired with the
/// `(type, name)` pairs its `Resources` name.
fn joined_by_request_id(
    case: &str,
    records: &[ManagementEvent],
    request_id: &str,
) -> (Value, Vec<(String, String)>) {
    let found: Vec<&ManagementEvent> = records
        .iter()
        .filter(|record| record.request_id == request_id)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record naming request {request_id:?}, got {found:?}"
    );
    let named = found[0]
        .call
        .resources
        .iter()
        .map(|resource| {
            (
                resource.resource_type.clone(),
                resource.resource_name.clone(),
            )
        })
        .collect();
    (found[0].to_cloudtrail_event_json(), named)
}

/// One call under test and what its record must carry.
struct Audited {
    case: &'static str,
    /// `true` to sign the call with the restricted role session instead of the account's own
    /// credential.
    as_session: bool,
    operation: &'static str,
    body: Value,
    /// The HTTP status the caller must see.
    status: u16,
    /// The event's `readOnly`: the operation's own property, whatever the call answered.
    read_only: bool,
    /// The `errorCode` the record must carry, and `None` for a call that was served.
    error_code: Option<&'static str>,
    /// Whether the call was dispatched: a dispatched call's record carries the parameters it
    /// was dispatched with, and one stopped at the permission check carries none.
    dispatched: bool,
    /// For a dispatched call, `(pointer, value)` pairs its `requestParameters` must hold.
    parameters: Vec<(&'static str, Value)>,
    /// The `(type, name)` pairs the record must name among the resources the call addressed,
    /// in order.
    resources: Vec<(&'static str, String)>,
}

/// The rows: the three outcomes of a call, over five operations of this service.
fn rows() -> Vec<Audited> {
    vec![
        Audited {
            case: "a create the policies allow",
            as_session: false,
            operation: "CreateLogGroup",
            body: json!({"logGroupName": GROUP}),
            status: 200,
            read_only: false,
            error_code: None,
            dispatched: true,
            parameters: vec![("/logGroupName", json!(GROUP))],
            resources: vec![(LOG_GROUP_TYPE, GROUP.to_string())],
        },
        Audited {
            case: "a write on one stream of that group",
            as_session: false,
            operation: "CreateLogStream",
            body: json!({"logGroupName": GROUP, "logStreamName": STREAM}),
            status: 200,
            read_only: false,
            error_code: None,
            dispatched: true,
            parameters: vec![
                ("/logGroupName", json!(GROUP)),
                ("/logStreamName", json!(STREAM)),
            ],
            resources: vec![
                (LOG_GROUP_TYPE, GROUP.to_string()),
                (LOG_STREAM_TYPE, STREAM.to_string()),
            ],
        },
        Audited {
            case: "a read of one group",
            as_session: false,
            operation: "DescribeLogStreams",
            body: json!({"logGroupName": GROUP}),
            status: 200,
            read_only: true,
            error_code: None,
            dispatched: true,
            parameters: vec![("/logGroupName", json!(GROUP))],
            resources: vec![(LOG_GROUP_TYPE, GROUP.to_string())],
        },
        Audited {
            case: "a read of one group addressed by its ARN",
            as_session: false,
            operation: "DescribeLogStreams",
            body: json!({"logGroupIdentifier": group_arn(GROUP)}),
            status: 200,
            read_only: true,
            error_code: None,
            dispatched: true,
            parameters: vec![("/logGroupIdentifier", json!(group_arn(GROUP)))],
            // The same resource as the row above: an identifier is folded to the group's own
            // user-created name whichever form the caller wrote.
            resources: vec![(LOG_GROUP_TYPE, GROUP.to_string())],
        },
        Audited {
            case: "a listing that names no resource",
            as_session: false,
            operation: "DescribeLogGroups",
            body: json!({"logGroupNamePrefix": "/audited/"}),
            status: 200,
            read_only: true,
            error_code: None,
            dispatched: true,
            parameters: vec![("/logGroupNamePrefix", json!("/audited/"))],
            resources: Vec::new(),
        },
        Audited {
            case: "a delete the handler refused",
            as_session: false,
            operation: "DeleteLogGroup",
            body: json!({"logGroupName": ABSENT}),
            status: 400,
            read_only: false,
            error_code: Some("ResourceNotFoundException"),
            dispatched: true,
            parameters: vec![("/logGroupName", json!(ABSENT))],
            resources: vec![(LOG_GROUP_TYPE, ABSENT.to_string())],
        },
        Audited {
            case: "a read the permission check stopped",
            as_session: true,
            operation: "DescribeLogGroups",
            body: json!({"logGroupNamePrefix": "/audited/"}),
            status: 400,
            read_only: true,
            error_code: Some(RECORDED_ACCESS_DENIED),
            dispatched: false,
            parameters: Vec::new(),
            resources: Vec::new(),
        },
        Audited {
            case: "a create the permission check stopped",
            as_session: true,
            operation: "CreateLogGroup",
            body: json!({"logGroupName": "/audited/refused"}),
            status: 400,
            read_only: false,
            error_code: Some(RECORDED_ACCESS_DENIED),
            dispatched: false,
            parameters: Vec::new(),
            resources: Vec::new(),
        },
    ]
}

/// RULE: every control-plane call this service serves is a management event of
/// `logs.amazonaws.com` in the account's event history — the call it served, the call its
/// handler refused and the call the IAM gate refused alike — carrying the operation's own
/// `readOnly`, the resources the request addressed, the code and message the caller was
/// answered with, and the id that response named the request by.
#[tokio::test]
async fn every_control_plane_call_is_recorded_as_a_management_event() {
    let aws = Emulator::new();
    // The caller the gate refuses: a role session whose only grant is another service's
    // action, made with IAM's and STS's own operations over this same engine.
    let restricted = assumed_role(
        &aws,
        "logs-audited-nothing",
        &[statement("Allow", "sts:GetCallerIdentity", "*")],
    )
    .await;

    for row in rows() {
        let key = if row.as_session {
            restricted.access_key_id.as_str()
        } else {
            ACCOUNT_KEY
        };
        let (status, headers, parsed) = aws.reported_as(key, row.operation, &row.body).await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected HTTP {}, got {status} {parsed}",
            row.case, row.status
        );
        let request_id = headers
            .get("x-amzn-RequestId")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: every response names the request it answers, got {headers:?}",
                    row.case
                )
            })
            .to_string();
        let records = recorded(&aws);
        let (record, named) = joined_by_request_id(row.case, &records, &request_id);
        assert_eq!(
            (
                record.get("eventName"),
                record.get("eventSource"),
                record.get("awsRegion"),
                record.get("readOnly"),
                record.get("recipientAccountId"),
            ),
            (
                Some(&json!(row.operation)),
                Some(&json!(EVENT_SOURCE)),
                Some(&json!(REGION)),
                Some(&json!(row.read_only)),
                Some(&json!(engine::emulator_account_id())),
            ),
            "case {:?}: expected this call's own record, got {record}",
            row.case
        );
        assert_eq!(
            record.get("errorCode"),
            row.error_code.map(|code| json!(code)).as_ref(),
            "case {:?}: expected errorCode {:?}, got {record}",
            row.case,
            row.error_code
        );
        // The message a record carries is the one the CALLER was answered with, byte for
        // byte — the id the response named its request by included.
        let answered = parsed
            .get("message")
            .or_else(|| parsed.get("Message"))
            .and_then(Value::as_str);
        assert_eq!(
            record.get("errorMessage").and_then(Value::as_str),
            answered,
            "case {:?}: expected the caller's own message {answered:?}, got {record}",
            row.case
        );
        if row.dispatched {
            for (pointer, expected) in &row.parameters {
                assert_eq!(
                    record.pointer(&format!("/requestParameters{pointer}")),
                    Some(expected),
                    "case {:?}: expected requestParameters{pointer} to be {expected}, got \
                     {record}",
                    row.case
                );
            }
        } else {
            assert_eq!(
                (
                    record.get("requestParameters"),
                    record.get("responseElements"),
                ),
                (Some(&Value::Null), Some(&Value::Null)),
                "case {:?}: a call stopped at the permission check records neither the \
                 parameters it would have been dispatched with nor any response, got {record}",
                row.case
            );
            // The refused caller's own session is still named: what the rule withholds is the
            // refused request's input, not the event.
            assert_eq!(
                (
                    record.pointer("/userIdentity/type"),
                    record.pointer("/userIdentity/arn"),
                    record.pointer("/userIdentity/accessKeyId"),
                ),
                (
                    Some(&json!("AssumedRole")),
                    Some(&json!(restricted.arn)),
                    Some(&json!(restricted.access_key_id)),
                ),
                "case {:?}: expected the session that made the refused call, got {record}",
                row.case
            );
        }
        let expected: Vec<(String, String)> = row
            .resources
            .iter()
            .map(|(kind, name)| ((*kind).to_string(), name.clone()))
            .collect();
        assert_eq!(
            named, expected,
            "case {:?}: expected the record to name {expected:?}, got {named:?} in {record}",
            row.case
        );
    }
}

/// RULE: a reporting call records no `responseElements` and a mutating one records what it
/// answered — CloudTrail reports a read with none.
#[tokio::test]
async fn a_read_records_no_response_elements_and_a_write_records_what_it_answered() {
    let aws = Emulator::new();
    aws.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    // (case, the operation, whether its record carries response elements)
    let rows = [
        ("a read", "DescribeLogGroups", json!({}), false),
        (
            "a write that answers values",
            "PutRetentionPolicy",
            json!({"logGroupName": GROUP, "retentionInDays": 7}),
            true,
        ),
    ];
    for (case, operation, body, answers) in rows {
        let (status, headers, parsed) = aws.reported_as(ACCOUNT_KEY, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must be served, got {status} {parsed}"
        );
        let request_id = headers
            .get("x-amzn-RequestId")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let record = joined_by_request_id(case, &recorded(&aws), &request_id).0;
        let recorded_response = record.get("responseElements");
        assert_eq!(
            recorded_response != Some(&Value::Null),
            answers,
            "case {case:?}: expected response elements present={answers}, got {record}"
        );
    }
}

/// RULE: a call on the CONTENTS of a log group is no control-plane call, so event history
/// holds no record of it — neither of the call itself nor of a refusal of it.
///
/// The rows span the write of log events and the read of them back, each made once with the
/// account's own credential and once with a session IAM allows nothing of this service.
#[tokio::test]
async fn a_call_on_a_log_groups_contents_is_recorded_nowhere() {
    let aws = Emulator::new();
    aws.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    let restricted = assumed_role(&aws, "logs-audited-no-data", &[]).await;
    let before = recorded(&aws).len();
    let timestamp = crate::harness::now_ms() - 60 * 1_000;
    let data_calls = [
        (
            "a write of log events",
            "PutLogEvents",
            json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": [{"timestamp": timestamp, "message": "audited"}],
            }),
        ),
        (
            "a read of log events",
            "GetLogEvents",
            json!({"logGroupName": GROUP, "logStreamName": STREAM}),
        ),
        (
            "a search over log events",
            "FilterLogEvents",
            json!({"logGroupName": GROUP}),
        ),
    ];
    for (case, operation, body) in &data_calls {
        for (who, key) in [
            ("the account's own credential", ACCOUNT_KEY),
            ("a session allowed nothing", restricted.access_key_id.as_str()),
        ] {
            let (_status, _parsed) = aws.call_as(key, operation, body).await;
            let after = recorded(&aws).len();
            assert_eq!(
                after, before,
                "case {case:?} as {who:?}: expected no management event for a call on a log \
                 group's contents, got {} new records",
                after - before
            );
        }
    }
}
