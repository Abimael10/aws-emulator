// SPDX-License-Identifier: CC0-1.0

//! The failure half of the serverless pipeline a client builds for poison events, end to end
//! over the wire: EventBridge, Lambda, Amazon SQS, IAM and the token service on ONE engine, a
//! real `python3.x` handler that RAISES, and the record read back where a client reads it — off
//! the queue the function's own `DestinationConfig.OnFailure` names.
//!
//! The RULE pinned here: a rule whose target is a function ARN invokes that function with the
//! ASYNCHRONOUS invocation type, so the retry budget of the function's own event-invoke
//! configuration governs what follows a handler that raised — with the retries that
//! configuration asks for spent, Lambda delivers ONE invocation record to the destination its
//! `OnFailure` slot names ("the destination configuration for failed invocations",
//! `smithy_query(query="shape", shape="DestinationConfig",
//! model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`),
//! carrying the condition the event ended on, the invocation's own request id, the function
//! qualified by the version that ran, what EventBridge delivered, and a `responsePayload`
//! reproducing the error the HANDLER itself raised. The delivery is made as the function's own
//! execution role, so the grant that admits it is that role's, and the record reaches the
//! destination whatever resource policy the destination itself carries.
//!
//! The record's member names and the literal condition of an event Lambda gave up on are the
//! fixed part of that contract; its request id, its function ARN and the error text are the
//! variable parts, each taken from the invocation rather than from the configuration — which is
//! what the rows below vary.
//!
//! Each row reads its configuration back through `GetFunctionEventInvokeConfig` before
//! publishing anything, so the destination the record later reaches is the one the STORED
//! configuration reports: the function qualified by the version the configuration belongs to,
//! both destination slots reported, and an empty document for the slot nobody stated.
//!
//! The rows span that class rather than one case. They differ along the class and the sentence
//! the handler raises (two built-in exception classes and the package's OWN class, with
//! messages carrying different punctuation), along the resource policy the destination queue
//! carries (one naming Lambda's service principal scoped by `aws:SourceArn` to the function,
//! one naming it unscoped, and none at all), along whether the grant that admits the rule's
//! invocation is scoped by `SourceArn` to the rule, and along whether the configuration also
//! states an `OnSuccess` slot — the last of which pins that the record goes to the slot the
//! OUTCOME names and nowhere else. Every row states `MaximumRetryAttempts` 0
//! (`FunctionEventInvokeConfig$MaximumRetryAttempts`, `@range(min: 0, max: 2)`), so each event
//! has the one attempt that allowance leaves and the attempt count the record reports is a
//! known number; how many attempts a larger allowance gives, and how long Lambda waits between
//! two of them, is the Lambda crate's own rule.
//!
//! Only a suite mounting Lambda, Amazon SQS, IAM and the token service beside EventBridge can
//! pin this: EventBridge hands the event over, Lambda runs the handler and writes the record as
//! a session of the execution role, the token service vends that session, IAM holds the role's
//! grants, and the queue the record lands on is the queue a client polls.
//!
//! `lambda_target_async_destination_records` is the suite for the same handover driven over the
//! OUTCOMES a handler reaches and the FORM a target delivers; what this one adds is the failure
//! outcome under the authorization a client really wires — a destination governed by its own
//! policy, an execution role carrying the send, and a permission scoped to the rule — together
//! with the error the record reproduces being the handler's own class and sentence.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handler_execution_role_publication::{
    Emulator, EVENTS_PRINCIPAL, FUNCTIONS_URI, LOOK_AGAIN,
};

/// Where `PutFunctionEventInvokeConfig` and `GetFunctionEventInvokeConfig` are bound.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The `source` and `detail-type` every event this suite publishes carries, and all each rule
/// matches on — so a record on a destination belongs to this suite's own event.
const SOURCE: &str = "com.demo.failure-destination";
const DETAIL_TYPE: &str = "Failure Destination Probe";

/// The action an invocation of a function performs, which the grant admitting the rule's
/// handover has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The id of the one target each rule of a row carries.
const TARGET_ID: &str = "fn";

/// The statement id of the grant each row writes on its function.
const STATEMENT_ID: &str = "events-invoke";

/// The service principal a destination queue's own policy names, the action such a statement
/// grants, and the condition key that scopes it to the function whose records it accepts.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";
const SOURCE_ARN_CONDITION_KEY: &str = "aws:SourceArn";

/// The name of the inline identity policy each row writes on its execution role: the record is
/// delivered as a session of that role, so the send it makes is the role's own.
const INLINE_POLICY: &str = "write-the-record";

/// The retries every configuration below asks for: none, the floor of the member's own
/// `@range(min: 0, max: 2)`. So each event has exactly ONE delivery attempt.
const NO_RETRIES: i64 = 0;

/// The `requestContext.condition` of the record of an event Lambda gave up on after every
/// delivery attempt its configuration allowed.
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `FunctionError` a run whose handler raised reports, which is what the record's
/// `responseContext` carries beside the status of the run.
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// The status the `responseContext` of a record of an invocation that RAN reports.
const INVOKED_STATUS: i64 = 200;

/// The qualifier an unqualified asynchronous invocation resolves to, and so the qualifier the
/// record's `functionArn` names: the unpublished version, whose configuration delivered it.
const UNPUBLISHED_VERSION: &str = "$LATEST";

/// How long a row waits for work the CLOCK owes — the asynchronous delivery of the event, the
/// handler's run, the record that follows it. A ceiling on PATIENCE and never a span anything
/// here asserts on: every wait looks again until what it waits for is there or the deadline
/// passes, so a loaded host is waited out.
const RECORD_WAIT: Duration = Duration::from_secs(90);

/// The handler every row deploys. WHICH error it raises is taken from the event it was handed,
/// so the class and the sentence a record reproduces are the handler's own rather than this
/// suite's: the package defines an exception class of its own beside the built-in ones, and the
/// event names the one to raise.
const RAISING_SOURCE: &str = r#"
class PipelineFailure(Exception):
    """The deployment package's OWN exception class."""


_CLASSES = {
    "RuntimeError": RuntimeError,
    "ValueError": ValueError,
    "PipelineFailure": PipelineFailure,
}


def lambda_handler(event, context):
    detail = event["detail"]
    raise _CLASSES[detail["error_type"]](detail["message"])
"#;

/// One row: the error the handler raises, how the destination and the handover are authorized,
/// and whether the configuration also states a slot the outcome does not select.
struct Row {
    /// What this row is, as every assertion names it.
    case: &'static str,
    /// The exception class the handler raises, which is the `errorType` the record reports.
    error_type: &'static str,
    /// The sentence it raises it with, which is the `errorMessage` the record reports.
    message: &'static str,
    /// The policy the destination queue carries: a statement naming Lambda's service principal
    /// scoped by `aws:SourceArn` to the function, the same statement unscoped, or no policy.
    queue_policy: QueuePolicy,
    /// Whether the grant admitting the rule's invocation is scoped by `SourceArn` to the rule.
    scoped_permission: bool,
    /// Whether the configuration also states `OnSuccess`, so the row pins that the slot the
    /// outcome did not select receives nothing.
    also_on_success: bool,
}

/// The form of the resource policy a row's destination queue carries; see [`Row::queue_policy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuePolicy {
    /// No policy at all.
    None,
    /// A statement naming Lambda's service principal, granting the send on the queue.
    ServicePrincipal,
    /// The same statement, scoped by `aws:SourceArn` to the function whose records it accepts.
    ScopedToFunction,
}

/// The rows, spanning the class the rule holds over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a built-in error class, a destination scoped to the function and a \
                   permission scoped to the rule",
            error_type: "RuntimeError",
            message: "the order could not be priced",
            queue_policy: QueuePolicy::ScopedToFunction,
            scoped_permission: true,
            also_on_success: true,
        },
        Row {
            case: "another built-in error class, a destination carrying no policy and an \
                   unscoped permission",
            error_type: "ValueError",
            message: "the field \"amount\" is not a number: '12,50'",
            queue_policy: QueuePolicy::None,
            scoped_permission: false,
            also_on_success: false,
        },
        Row {
            case: "the deployment package's own error class and an unscoped destination policy",
            error_type: "PipelineFailure",
            message: "pass 7 of the reconciliation stopped (0 of 42 settled)",
            queue_policy: QueuePolicy::ServicePrincipal,
            scoped_permission: false,
            also_on_success: true,
        },
    ]
}

/// Write the resource policy `form` describes onto the queue at `url`, whose ARN is `arn` and
/// whose records come from the function `function_arn`.
async fn set_queue_policy(
    emulator: &Emulator,
    case: &str,
    url: &str,
    arn: &str,
    function_arn: &str,
    form: QueuePolicy,
) {
    let mut statement = json!({
        "Sid": "AllowLambdaDestinationRecords",
        "Effect": "Allow",
        "Principal": { "Service": LAMBDA_PRINCIPAL },
        "Action": SEND_MESSAGE_ACTION,
        "Resource": arn,
    });
    match form {
        QueuePolicy::None => return,
        QueuePolicy::ServicePrincipal => {}
        QueuePolicy::ScopedToFunction => {
            statement["Condition"] =
                json!({ "ArnEquals": { SOURCE_ARN_CONDITION_KEY: function_arn } });
        }
    }
    let policy = json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string();
    emulator
        .sqs(
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        )
        .await;
}

/// The ONE invocation record waiting on the queue at `url`, as the document its body holds —
/// looking again until it is there or [`RECORD_WAIT`] has passed, so nothing sleeps for a span.
async fn one_record(emulator: &Emulator, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + RECORD_WAIT;
    let mut delivered = loop {
        let held = emulator.raw_messages(case, url).await;
        if !held.is_empty() {
            break held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no invocation record ever reached the OnFailure destination"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    // One receive answers at most what it happened to pick up, so a second record written at
    // the same instant is taken off by another look before the count below is asserted.
    delivered.extend(emulator.raw_messages(case, url).await);
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected exactly one invocation record at the destination, got \
         {delivered:?}"
    );
    let body = delivered[0]["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: expected an invocation record document, got {body:?} ({error})")
    })
}

/// The ARN Lambda assigned the function `name`, read back through its own configuration.
async fn function_arn(emulator: &Emulator, case: &str, name: &str) -> String {
    let (status, _headers, configuration) = emulator
        .lambda("GET", &format!("{FUNCTIONS_URI}/{name}/configuration"), &[])
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the configuration of {name:?}, got {status} \
         ({configuration})"
    );
    configuration["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {configuration}"))
        .to_string()
}

/// State the function's asynchronous-invocation configuration — the retries `NO_RETRIES` asks
/// for and the destination slots named — and read it back through
/// `GetFunctionEventInvokeConfig`, which is where a client confirms what it configured.
async fn event_invoke_config(
    emulator: &Emulator,
    case: &str,
    name: &str,
    function_arn: &str,
    on_failure: &str,
    on_success: Option<&str>,
) {
    let mut destinations = json!({ "OnFailure": { "Destination": on_failure } });
    if let Some(destination) = on_success {
        destinations["OnSuccess"] = json!({ "Destination": destination });
    }
    let body = json!({
        "MaximumRetryAttempts": NO_RETRIES,
        "DestinationConfig": destinations,
    });
    let (status, _headers, answered) = emulator
        .lambda(
            "PUT",
            &format!("{EVENT_INVOKE_CONFIG_URI}/{name}/event-invoke-config"),
            &serde_json::to_vec(&body).expect("the configuration body serializes"),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutFunctionEventInvokeConfig, got {status} \
         ({answered})"
    );
    let (status, _headers, read) = emulator
        .lambda(
            "GET",
            &format!("{EVENT_INVOKE_CONFIG_URI}/{name}/event-invoke-config"),
            &[],
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetFunctionEventInvokeConfig, got {status} \
         ({read})"
    );
    assert_eq!(
        read["MaximumRetryAttempts"],
        json!(NO_RETRIES),
        "case {case:?}: expected the stored configuration to report {NO_RETRIES} retries, got \
         {read}"
    );
    // The configuration is the one OF A QUALIFIER, so the read names the function qualified by
    // the version the destination was configured for.
    assert_eq!(
        read["FunctionArn"],
        json!(format!("{function_arn}:{UNPUBLISHED_VERSION}")),
        "case {case:?}: expected the configuration to name \
         {function_arn}:{UNPUBLISHED_VERSION}, got {read}"
    );
    // Both slots are reported, each carrying what was stated for it and an empty document for
    // the one nobody stated — so a client reads back which outcome its destination is for.
    assert_eq!(
        read["DestinationConfig"]["OnFailure"],
        json!({ "Destination": on_failure }),
        "case {case:?}: expected the stored configuration to report the failure destination it \
         was given, got {read}"
    );
    let reported_success = on_success
        .map_or_else(|| json!({}), |destination| json!({ "Destination": destination }));
    assert_eq!(
        read["DestinationConfig"]["OnSuccess"], reported_success,
        "case {case:?}: expected the stored configuration to report {reported_success} as its \
         success destination, got {read}"
    );
}

/// Grant this service the invocation of the function `name` — scoped by `SourceArn` to
/// `rule_arn` where the row asks for it — which is what a function target's handover is decided
/// by.
async fn add_permission(
    emulator: &Emulator,
    case: &str,
    name: &str,
    rule_arn: &str,
    scoped: bool,
) {
    let mut body = json!({
        "StatementId": STATEMENT_ID,
        "Action": INVOKE_ACTION,
        "Principal": EVENTS_PRINCIPAL,
    });
    if scoped {
        body["SourceArn"] = json!(rule_arn);
    }
    let (status, _headers, answered) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/policy"),
            &serde_json::to_vec(&body).expect("the permission body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {status} ({answered})"
    );
}

/// The rule matching this suite's event, with the function as its only target; answers the
/// rule's ARN, which is the source a scoped grant names.
async fn rule(emulator: &Emulator, case: &str, name: &str) -> String {
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let created = emulator
        .events(
            case,
            "PutRule",
            &json!({ "Name": name, "EventPattern": pattern, "State": "ENABLED" }),
        )
        .await;
    created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {created}"))
        .to_string()
}

/// Wire `function_arn` as the only target of the rule `name`.
async fn put_target(emulator: &Emulator, case: &str, name: &str, function_arn: &str) {
    let wired = emulator
        .events(
            case,
            "PutTargets",
            &json!({
                "Rule": name,
                "Targets": [{ "Id": TARGET_ID, "Arn": function_arn }],
            }),
        )
        .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the function target to be accepted, got {wired}"
    );
}

/// The execution role `role_name`, trusted by Lambda and carrying the one grant a destination
/// delivery needs: the send on the queues `destinations` names. A record is written as a
/// SESSION of this role, so the grant that admits the write is this role's own. Answers its ARN.
async fn execution_role(
    emulator: &Emulator,
    case: &str,
    role_name: &str,
    destinations: &[&str],
) -> String {
    let role = emulator.execution_role(case, role_name).await;
    emulator
        .role_policy(
            case,
            role_name,
            INLINE_POLICY,
            Some(
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": SEND_MESSAGE_ACTION,
                        "Resource": destinations,
                    }],
                })
                .to_string(),
            ),
        )
        .await;
    role
}

/// Publish the one matching entry of a row, whose detail asks the handler for the error the row
/// is about; answers the detail as published, which is what the record reports it was handed.
async fn publish(emulator: &Emulator, row: &Row, marker: &str) -> Value {
    let detail = json!({
        "marker": marker,
        "error_type": row.error_type,
        "message": row.message,
    });
    let published = emulator
        .events(
            row.case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": detail.to_string(),
                }],
            }),
        )
        .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {}: expected the entry to be accepted, got {published}",
        row.case
    );
    detail
}

/// RULE: a rule's function target is invoked asynchronously, so a handler that raises with the
/// retries its event-invoke configuration allows spent has Lambda deliver one invocation record
/// to that configuration's `OnFailure` destination — reporting the condition the event ended
/// on, the invocation's own request id, the function qualified by the version that ran, what
/// was delivered, and the error the HANDLER raised — and the slot the outcome did not select
/// receives nothing.
#[tokio::test]
async fn an_eventbridge_lambda_targets_handler_failure_records_at_its_on_failure_destination() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();

        let (failure_url, failure_arn) =
            emulator.queue(case, &format!("on-failure-{index}")).await;
        let (success_url, success_arn) =
            emulator.queue(case, &format!("on-success-{index}")).await;

        // The record is written as a session of the function's own execution role, so that
        // role carries the send on both slots' queues.
        let role_name = format!("failure-destination-role-{index}");
        let role = execution_role(
            &emulator,
            case,
            &role_name,
            &[failure_arn.as_str(), success_arn.as_str()],
        )
        .await;

        let function_name = format!("raises-for-the-destination-{index}");
        emulator
            .function(case, &function_name, &role, RAISING_SOURCE)
            .await;
        let function = function_arn(&emulator, case, &function_name).await;
        set_queue_policy(
            &emulator,
            case,
            &failure_url,
            &failure_arn,
            &function,
            row.queue_policy,
        )
        .await;
        let on_success = row.also_on_success.then_some(success_arn.as_str());
        event_invoke_config(
            &emulator,
            case,
            &function_name,
            &function,
            &failure_arn,
            on_success,
        )
        .await;

        let rule_name = format!("failure-destination-rule-{index}");
        let rule_arn = rule(&emulator, case, &rule_name).await;
        add_permission(
            &emulator,
            case,
            &function_name,
            &rule_arn,
            row.scoped_permission,
        )
        .await;
        put_target(&emulator, case, &rule_name, &function).await;

        let marker = format!("marker-{index}");
        let detail = publish(&emulator, row, &marker).await;

        let record = one_record(&emulator, case, &failure_url).await;
        let context = &record["requestContext"];
        assert_eq!(
            context["condition"],
            json!(CONDITION_RETRIES_EXHAUSTED),
            "case {case:?}: expected the condition {CONDITION_RETRIES_EXHAUSTED:?}, got {record}"
        );
        assert_eq!(
            context["approximateInvokeCount"],
            json!(1),
            "case {case:?}: expected the one attempt no retry allows, got {record}"
        );
        // The record names the function QUALIFIED by the version the unqualified invocation
        // resolved to, which is the qualifier whose configuration delivered it.
        assert_eq!(
            context["functionArn"],
            json!(format!("{function}:{UNPUBLISHED_VERSION}")),
            "case {case:?}: expected the record to name {function}:{UNPUBLISHED_VERSION}, got \
             {record}"
        );
        assert!(
            context["requestId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the invocation's own request id, got {record}"
        );

        // `requestPayload` is what EventBridge DELIVERED: the envelope of the published entry.
        let payload = &record["requestPayload"];
        assert_eq!(
            payload["source"],
            json!(SOURCE),
            "case {case:?}: expected the envelope's source {SOURCE:?}, got {record}"
        );
        assert_eq!(
            payload["detail-type"],
            json!(DETAIL_TYPE),
            "case {case:?}: expected the envelope's detail-type {DETAIL_TYPE:?}, got {record}"
        );
        assert_eq!(
            payload["detail"], detail,
            "case {case:?}: expected the published detail in the request payload, got {record}"
        );

        // The response the record reports is the run's own: the status of a run that executed,
        // the function error a raising handler reports, and the handler's OWN class and
        // sentence as the payload.
        let response = &record["responseContext"];
        assert_eq!(
            response["statusCode"],
            json!(INVOKED_STATUS),
            "case {case:?}: expected the status of a run that executed, got {record}"
        );
        assert_eq!(
            response["functionError"],
            json!(FUNCTION_ERROR_UNHANDLED),
            "case {case:?}: expected the function error of a raising handler, got {record}"
        );
        assert_eq!(
            record["responsePayload"]["errorType"],
            json!(row.error_type),
            "case {case:?}: expected the handler's own error class {:?}, got {record}",
            row.error_type
        );
        assert_eq!(
            record["responsePayload"]["errorMessage"],
            json!(row.message),
            "case {case:?}: expected the handler's own sentence {:?}, got {record}",
            row.message
        );

        // The slot the outcome did not select received nothing: the record above has already
        // waited the completion out, so anything owed to it would be on it by now.
        if row.also_on_success {
            let other = emulator.raw_messages(case, &success_url).await;
            assert!(
                other.is_empty(),
                "case {case:?}: expected the OnSuccess slot to receive nothing, got {other:?}"
            );
        }
    }
}
