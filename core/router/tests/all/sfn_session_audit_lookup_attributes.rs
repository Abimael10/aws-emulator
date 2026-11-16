// SPDX-License-Identifier: CC0-1.0

//! Auditing a Step Functions role session through every lookup attribute CloudTrail
//! supports — across four services on one engine.
//!
//! RULE: a service records EVERY control-plane call it serves in the account's event
//! history, reporting and mutating operations alike and refused calls as well as served
//! ones, so `cloudtrail:LookupEvents` reports each call under the serving service's
//! `eventSource` with the `Management` category, `readOnly` taken from the operation's own
//! documented semantics, the Region and account it was served for, the parameters the
//! request carried, the id the CALLER was answered under as `requestID`, the caller's
//! session identity, and — for a refused call — the fault shape's own name as `errorCode`
//! with the message the caller saw and no `responseElements`; and every lookup attribute
//! the model declares selects exactly the records whose reported field equals the value
//! asked for.
//!
//! The table below spans what that rule generalises over rather than the two operations one
//! recording happened to cover: nine Step Functions operations across activities,
//! executions, history and tags; both kinds (reporting and mutating); both outcomes (served
//! and refused) over two fault shapes; and each record re-found through every attribute key
//! that names a field it carries, not only through `EventName`.
//!
//! Only a cross-service suite can pin this: the roles are written by `iam:CreateRole` and
//! `iam:PutRolePolicy`, the caller's session is minted by `sts:AssumeRole`, the calls are
//! served by Step Functions, and every record is read back through
//! `cloudtrail:LookupEvents` — four services over ONE engine, as `core/router/src/lib.rs`
//! mounts them. The per-call shape of one session's start and its refused read is
//! `sfn_management_events.rs`, whose harness this suite drives rather than restating.

use serde_json::{json, Value};

use crate::sfn_management_events::{
    assert_session_attribution, assert_sfn_record, at, shape_name, Answered, Audited,
    BOOT_STACK_SIZE, REGION, SESSION_NAME, SETUP_KEY_ID,
};

/// The state machine the session runs, the execution it starts, and the activity it
/// creates, describes and tags.
const MACHINE: &str = "sfn-lookup-session";
const EXECUTION: &str = "sfn-lookup-run";
const ACTIVITY: &str = "sfn-lookup-activity";

/// A one-`Pass`-state STANDARD state machine.
const DEFINITION: &str =
    r#"{"StartAt":"Only","States":{"Only":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#;

/// The trust policy of the state machine's own EXECUTION role.
const EXECUTION_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"states.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// What the caller's role may do: every Step Functions action the session makes below, so
/// no call of the table is refused by the identity gate in front of the operations (a
/// request refused there reached no handler and is recorded as nothing).
const CALLER_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Action":["states:CreateActivity","states:DescribeActivity","states:ListActivities",
"states:StartExecution","states:DescribeExecution","states:GetExecutionHistory",
"states:StopExecution","states:TagResource","states:ListTagsForResource"],
"Resource":"*"}]}"#;

/// The lookup attribute keys this suite re-finds each record through.
///
/// The catalog is CloudTrail's own model and not a transcription:
/// `smithy_query(query="shape",shape="LookupAttributeKey",model_path="services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/smithy-model/com-amazonaws-cloudtrail-cloudtrail-20131101.json")`
/// reports `EventId`, `EventName`, `ReadOnly`, `Username`, `ResourceType`, `ResourceName`,
/// `EventSource` and `AccessKeyId`. The two RESOURCE keys are left out: they select a record
/// by a resource it REFERS to, and a management record of a served call names the call, its
/// parameters and its outcome rather than a resource list, so they are the subject of the
/// data-event suites instead.
const ATTRIBUTE_KEYS: [&str; 6] = [
    "EventId",
    "EventName",
    "ReadOnly",
    "Username",
    "EventSource",
    "AccessKeyId",
];

/// One call of the table, as the record of it must read.
struct Audit {
    /// What this row is, named in every assertion it drives.
    case: &'static str,
    /// The operation the call named, which is the record's `eventName`.
    event_name: &'static str,
    /// Whether the operation only REPORTS state, which is the record's `readOnly`.
    read_only: bool,
    /// The id the CALLER was answered under, which the record names the request by.
    request_id: String,
    /// `(pointer, value)` pairs the record must carry — the parameters the request sent and,
    /// for a served mutation, what it answered.
    members: Vec<(&'static str, String)>,
    /// The `(errorCode, errorMessage)` a refused call's record carries, or `None` for a
    /// served one.
    error: Option<(String, String)>,
}

/// The refusal a call was answered with, as its caller read it: the fault SHAPE's name and
/// the message, which is what the record of that call must repeat.
fn refusal(case: &str, answered: &Answered, expected_code: &str) -> (String, String) {
    assert_eq!(
        answered.status, 400,
        "case {case:?}: expected the modelled client refusal, got {} with {}",
        answered.status, answered.body
    );
    let code = shape_name(&answered.body);
    assert_eq!(
        code, expected_code,
        "case {case:?}: expected the fault shape {expected_code:?}, got {}",
        answered.body
    );
    let message = answered.body["message"]
        .as_str()
        .or_else(|| answered.body["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        !message.is_empty(),
        "case {case:?}: the refusal states a message, got {}",
        answered.body
    );
    (code, message)
}

/// The ONE record of the call `request_id` answered: the records this session made for the
/// operation (the sibling suite's own reader), kept to the request the caller was answered
/// under — which is what distinguishes two calls of the same operation.
fn record_of(
    emulator: &Audited,
    case: &str,
    event_name: &str,
    session_arn: &str,
    request_id: &str,
) -> (Value, Value) {
    let found: Vec<(Value, Value)> = emulator
        .session_records(event_name, session_arn)
        .into_iter()
        .filter(|(_, document)| at(document, "/requestID").as_deref() == Some(request_id))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: the session's {event_name} answered under {request_id:?} is exactly \
         one record, got {found:?}"
    );
    found.into_iter().next().expect("one record")
}

/// The value `key` selects `document`/`envelope` by, read off the SAME field the record
/// reports under that name.
fn reported_for(key: &str, envelope: &Value, document: &Value) -> Option<String> {
    match key {
        "EventId" => at(document, "/eventID"),
        "EventName" => at(document, "/eventName"),
        // `Event$ReadOnly` is modelled as a string, so the attribute's value is the
        // record's boolean rendered as one.
        "ReadOnly" => document.get("readOnly").map(ToString::to_string),
        "Username" => at(envelope, "/Username"),
        "EventSource" => at(document, "/eventSource"),
        "AccessKeyId" => at(document, "/userIdentity/accessKeyId"),
        other => panic!("no attribute key {other:?} is looked up here"),
    }
}

#[test]
fn every_call_of_a_session_is_auditable_through_each_lookup_attribute() {
    std::thread::Builder::new()
        .name("sfn-session-audit-lookup".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Audited::new();
            let account = engine::emulator_account_id().to_string();

            let execution_role = emulator.role("sfn-lookup-execution", EXECUTION_TRUST_POLICY);
            let caller_trust = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
                 \"Action\":\"sts:AssumeRole\"}}]}}"
            );
            let caller_role = emulator.role("sfn-lookup-caller", &caller_trust);
            emulator.put_role_policy("sfn-lookup-caller", "sfn-calls", CALLER_POLICY);
            let (session_key, session_arn) = emulator.assume(&caller_role, SESSION_NAME);

            // The setup's own machine, created with the long-term credential so the
            // session's calls are the only ones the table grades.
            let created = emulator.sfn_ok(
                SETUP_KEY_ID,
                "CreateStateMachine",
                &json!({
                    "name": MACHINE,
                    "definition": DEFINITION,
                    "roleArn": execution_role,
                    "type": "STANDARD",
                }),
            );
            let machine = created.body["stateMachineArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("setup: the create reports its ARN, got {}", created.body)
                })
                .to_string();

            // Two ARNs in this account's own namespace that name nothing: what the refused
            // calls below address.
            let missing_execution =
                format!("arn:aws:states:{REGION}:{account}:execution:{MACHINE}:never-started");
            let missing_activity =
                format!("arn:aws:states:{REGION}:{account}:activity:never-created");

            // The session's calls: a mutation and a read over activities, a start and two
            // reads over executions, a tag write and a tag read, then three refusals.
            let make_activity =
                emulator.sfn_ok(&session_key, "CreateActivity", &json!({ "name": ACTIVITY }));
            let activity = make_activity.body["activityArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "the create reports its activity, got {}",
                        make_activity.body
                    )
                })
                .to_string();
            let read_activity = emulator.sfn_ok(
                &session_key,
                "DescribeActivity",
                &json!({ "activityArn": activity }),
            );
            let list_activities = emulator.sfn_ok(&session_key, "ListActivities", &json!({}));
            let started = emulator.sfn_ok(
                &session_key,
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": EXECUTION }),
            );
            let execution = started.body["executionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("the start reports its execution, got {}", started.body)
                })
                .to_string();
            let read_execution = emulator.sfn_ok(
                &session_key,
                "DescribeExecution",
                &json!({ "executionArn": execution }),
            );
            let history = emulator.sfn_ok(
                &session_key,
                "GetExecutionHistory",
                &json!({ "executionArn": execution }),
            );
            let tagged = emulator.sfn_ok(
                &session_key,
                "TagResource",
                &json!({
                    "resourceArn": activity,
                    "tags": [{ "key": "owner", "value": "audit" }],
                }),
            );
            let read_tags = emulator.sfn_ok(
                &session_key,
                "ListTagsForResource",
                &json!({ "resourceArn": activity }),
            );
            let refused_read = emulator.sfn(
                &session_key,
                "DescribeExecution",
                &json!({ "executionArn": missing_execution }),
            );
            let refused_stop = emulator.sfn(
                &session_key,
                "StopExecution",
                &json!({ "executionArn": missing_execution }),
            );
            let refused_activity = emulator.sfn(
                &session_key,
                "DescribeActivity",
                &json!({ "activityArn": missing_activity }),
            );

            // The `readOnly` per row is the operation's own documented semantics, which is
            // what the serving service states per operation
            // (`service::management_events::audited_table`): the `Describe*`/`List*`/`Get*`
            // readers report state, and everything that creates, starts, tags or stops
            // changes it.
            let table = vec![
                Audit {
                    case: "the activity create",
                    event_name: "CreateActivity",
                    read_only: false,
                    request_id: make_activity.request_id.clone(),
                    members: vec![
                        ("/requestParameters/name", ACTIVITY.to_string()),
                        ("/responseElements/activityArn", activity.clone()),
                    ],
                    error: None,
                },
                Audit {
                    case: "the activity read",
                    event_name: "DescribeActivity",
                    read_only: true,
                    request_id: read_activity.request_id.clone(),
                    members: vec![("/requestParameters/activityArn", activity.clone())],
                    error: None,
                },
                Audit {
                    case: "the activity listing",
                    event_name: "ListActivities",
                    read_only: true,
                    request_id: list_activities.request_id.clone(),
                    members: Vec::new(),
                    error: None,
                },
                Audit {
                    case: "the start",
                    event_name: "StartExecution",
                    read_only: false,
                    request_id: started.request_id.clone(),
                    members: vec![
                        ("/requestParameters/stateMachineArn", machine.clone()),
                        ("/requestParameters/name", EXECUTION.to_string()),
                        ("/responseElements/executionArn", execution.clone()),
                    ],
                    error: None,
                },
                Audit {
                    case: "the execution read",
                    event_name: "DescribeExecution",
                    read_only: true,
                    request_id: read_execution.request_id.clone(),
                    members: vec![("/requestParameters/executionArn", execution.clone())],
                    error: None,
                },
                Audit {
                    case: "the history read",
                    event_name: "GetExecutionHistory",
                    read_only: true,
                    request_id: history.request_id.clone(),
                    members: vec![("/requestParameters/executionArn", execution.clone())],
                    error: None,
                },
                Audit {
                    case: "the tag write",
                    event_name: "TagResource",
                    read_only: false,
                    request_id: tagged.request_id.clone(),
                    members: vec![("/requestParameters/resourceArn", activity.clone())],
                    error: None,
                },
                Audit {
                    case: "the tag read",
                    event_name: "ListTagsForResource",
                    read_only: true,
                    request_id: read_tags.request_id.clone(),
                    members: vec![("/requestParameters/resourceArn", activity.clone())],
                    error: None,
                },
                Audit {
                    case: "the refused execution read",
                    event_name: "DescribeExecution",
                    read_only: true,
                    request_id: refused_read.request_id.clone(),
                    members: vec![(
                        "/requestParameters/executionArn",
                        missing_execution.clone(),
                    )],
                    error: Some(refusal(
                        "the refused execution read",
                        &refused_read,
                        "ExecutionDoesNotExist",
                    )),
                },
                Audit {
                    case: "the refused stop",
                    event_name: "StopExecution",
                    read_only: false,
                    request_id: refused_stop.request_id.clone(),
                    members: vec![(
                        "/requestParameters/executionArn",
                        missing_execution.clone(),
                    )],
                    error: Some(refusal(
                        "the refused stop",
                        &refused_stop,
                        "ExecutionDoesNotExist",
                    )),
                },
                Audit {
                    case: "the refused activity read",
                    event_name: "DescribeActivity",
                    read_only: true,
                    request_id: refused_activity.request_id.clone(),
                    members: vec![(
                        "/requestParameters/activityArn",
                        missing_activity.clone(),
                    )],
                    error: Some(refusal(
                        "the refused activity read",
                        &refused_activity,
                        "ActivityDoesNotExist",
                    )),
                },
            ];

            for row in &table {
                let case = row.case;
                let (envelope, document) = record_of(
                    &emulator,
                    case,
                    row.event_name,
                    &session_arn,
                    &row.request_id,
                );

                // What every record of a served call of this service carries, and the
                // session it is attributed to: the sibling suite's own readings of both, so
                // the two suites cannot disagree about what a record of this service says.
                assert_sfn_record(
                    case,
                    &envelope,
                    &document,
                    row.event_name,
                    row.read_only,
                    &account,
                );
                assert_session_attribution(
                    case,
                    &envelope,
                    &document,
                    &session_key,
                    &session_arn,
                    &caller_role,
                    &execution_role,
                );
                for (pointer, expected) in &row.members {
                    assert_eq!(
                        at(&document, pointer).as_deref(),
                        Some(expected.as_str()),
                        "case {case:?}: expected {pointer} {expected:?}, got {document}"
                    );
                }

                // The outcome: a refused call records the fault shape's name and the message
                // its caller saw and answers nothing; a served one records no error at all.
                let expected_code = row.error.as_ref().map(|(code, _)| code.as_str());
                let expected_message = row.error.as_ref().map(|(_, message)| message.as_str());
                assert_eq!(
                    at(&document, "/errorCode").as_deref(),
                    expected_code,
                    "case {case:?}: expected errorCode {expected_code:?}, got {document}"
                );
                assert_eq!(
                    at(&document, "/errorMessage").as_deref(),
                    expected_message,
                    "case {case:?}: the record repeats the message the caller saw \
                     ({expected_message:?}), got {document}"
                );
                if row.error.is_some() {
                    // A refused call answered nothing, so its record states none: the member
                    // is either absent or null, which is the same fact on the wire.
                    assert!(
                        document
                            .get("responseElements")
                            .is_none_or(Value::is_null),
                        "case {case:?}: a refused call answers nothing to record, got \
                         {document}"
                    );
                }

                // Each attribute of the model's own catalog selects this record, and selects
                // ONLY records whose reported field equals the value asked for.
                let event_id = at(&document, "/eventID")
                    .unwrap_or_else(|| panic!("case {case:?}: the record has an id"));
                for key in ATTRIBUTE_KEYS {
                    let value = reported_for(key, &envelope, &document).unwrap_or_else(|| {
                        panic!("case {case:?}: the record reports a value for {key}")
                    });
                    let found = emulator.lookup(key, &value);
                    assert!(
                        found.iter().any(|(_, candidate)| {
                            at(candidate, "/eventID").as_deref() == Some(event_id.as_str())
                        }),
                        "case {case:?}: a lookup by {key}={value:?} reports this record \
                         ({event_id}), got {found:?}"
                    );
                    for (candidate_envelope, candidate) in &found {
                        assert_eq!(
                            reported_for(key, candidate_envelope, candidate).as_deref(),
                            Some(value.as_str()),
                            "case {case:?}: a lookup by {key}={value:?} reports only records \
                             whose {key} is that value, got {candidate}"
                        );
                    }
                }
            }
        })
        .expect("spawn the Step Functions lookup-attribute audit thread")
        .join()
        .expect("every Step Functions call of a session is auditable by every attribute");
}
