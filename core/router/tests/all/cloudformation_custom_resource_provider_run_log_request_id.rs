// SPDX-License-Identifier: CC0-1.0

//! What a CUSTOM RESOURCE's provider WROTE while it answered its request, and the id that
//! correlates that run with the resource's own stack event.
//!
//! RULE under test: **the invocation CloudFormation makes of a custom resource's provider is an
//! ordinary AWS Lambda invocation, so everything the handler writes to stdout or stderr is
//! delivered to the group that function logs to — `/aws/lambda/<function name>` by default
//! (`LoggingConfig$LogGroup`: "by default, Lambda functions send logs to a default log group
//! named `/aws/lambda/<function name>`") — in a stream of that group, readable back through
//! `DescribeLogStreams` and `GetLogEvents` beside the runtime's own closing frame; and the
//! `RequestId` that run read out of its event is the id CloudFormation sent that ONE request
//! under, so a provider that answers `FAILED` has its message reported on the resource's event
//! as `Received response status [FAILED] from custom resource. Message returned: <message>
//! (RequestId: <that id>)`, and each further request about the resource — the `Delete` a
//! rollback owes — carries an id of its own.**
//!
//! Why it matters: when a provider-backed deploy fails, the handler's own log is the only place
//! a client can see what the provider was asked and what it answered, and the `RequestId` is the
//! only thing that ties the two together. With no records in the group, a client cannot tell a
//! provider that was never invoked from one that was invoked and refused, and cannot match the
//! resource's failure reason against the run that produced it.
//!
//! Three crates have to agree for that to hold, which is why the suite lives here:
//! CloudFormation mints the request and sends it, AWS Lambda runs the provider and delivers what
//! it wrote, and Amazon CloudWatch Logs holds the group, its streams and its events. What the
//! neighbours pin and this one does not restate:
//! [`crate::cloudformation_lambda_function_log_destination`] drives WHERE a deployed function's
//! records land (and owns the assembled-emulator wire harness this suite reuses),
//! [`crate::cloudformation_failed_custom_resource_rollback_deletes`] what a `FAILED` answer does
//! to the stack behind it (and owns the stack-event reader), and
//! [`crate::cloudformation_completed_stack_output_values`] the wait for a stack to settle. What
//! only this suite reaches is the CORRELATION: the id the run logged against the id the
//! resource's reason names.
//!
//! The rows differ along what the rule generalises over rather than restating one handler: how
//! the handler emits its line (`print` to stdout, a write to stderr, a record through the
//! `logging` module, several lines around the marker), and what the provider then answers
//! (`SUCCESS`, and `FAILED` with a message stated as one clause and as one carrying its own
//! punctuation).
//!
//! No expected value here comes from a recording: every function name, group name and message is
//! built by the row, and the `RequestId` asserted on is whatever this run's own CloudFormation
//! minted — only its FORM is pinned. Nothing sleeps a span on a service's behalf: every wait
//! looks again for the condition it asserts, under a deadline far longer than the spans these
//! services choose, and nothing asserts on elapsed time.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_completed_stack_output_values::settled_status;
use crate::cloudformation_failed_custom_resource_rollback_deletes::resource_events;
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, logs, logs_policy, trust_policy, CREATE_COMPLETE,
    DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT, REPORT_PREFIX,
};
use crate::query_wire::{element, outputs_of};
use crate::sns_lambda_subscription_permission_boundary::iam;

/// The terminal `StackStatus` a create whose custom resource failed rolls back to.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` such a resource takes.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// How long a log record another service owes is waited for, and how often it is looked at. The
/// budget is far longer than any span these services give these transitions — generous enough
/// that a host running every suite of the workspace at once is slower without the rule being any
/// different. The wait for a STACK to settle is the shared one's ([`settled_status`]), under its
/// own equally generous budget.
const LOG_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(50);

/// The logical ids every template below declares.
const PROVIDER_ID: &str = "Provider";
const CUSTOM_ID: &str = "Provisioned";

/// The execution role the provider functions run as, created in IAM before any stack — so a
/// row's stack declares the function and the resource it backs and nothing else, and the chain
/// CloudFormation walks before the provider is reached is as short as the rule allows. What a
/// role of a stack's own is, and what happens when it is short of the logs grants, are other
/// suites' rules ([`crate::cloudformation_lambda_function_log_destination`]).
const EXECUTION_ROLE: &str = "cr-run-log-provider-exec";

/// The fixed text of the line every provider below writes: the marker, then the request type it
/// was sent and the `RequestId` it was sent under. A client searching a provider's log for the
/// request it is diagnosing searches for exactly this.
const MARKER_PREFIX: &str = "REQUEST_ID=";

/// The marker a declaration carries and a successful answer echoes back as its `Data`, so a
/// completed row proves the provider really ran rather than that nothing refused.
const MARKER: &str = "reached the provider";

/// The sentence CloudFormation reports a custom resource whose provider ANSWERED `FAILED` with:
/// its fixed text, whose only variable parts are the message that answer returned and the
/// `RequestId` of the request it answered.
///
/// Spelled out here on purpose rather than imported from the service that writes it: an
/// expectation taken from the implementation's own formatter could not fail when that formatter
/// changes, which is the one thing this suite is for.
fn answered_failure_reason(message: &str, request_id: &str) -> String {
    format!(
        "Received response status [FAILED] from custom resource. Message returned: {message} \
         (RequestId: {request_id})"
    )
}

/// The execution role every provider below runs as, answered by the ARN IAM persisted for it:
/// it trusts AWS Lambda, and it may make the three CloudWatch Logs calls an invocation's log
/// delivery makes — what the AWS-managed basic execution policy grants a function.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust_policy().to_string()),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        arn.starts_with("arn:"),
        "setup: CreateRole({EXECUTION_ROLE:?}) must answer an ARN, got {created}"
    );
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", "basic-execution"),
            ("PolicyDocument", &logs_policy().to_string()),
        ],
    )
    .await;
    arn
}

/// The provider one row deploys: it writes the marker line the way the row states, and then
/// answers the request — `FAILED` with `message` for the `Create` where the row states one, and
/// `SUCCESS` echoing the declaration's marker otherwise, so a rollback's `Delete` is always
/// carried out.
fn handler_source(emit: &str, message: Option<&str>) -> String {
    let failure = match message {
        Some(message) => serde_json::to_string(message).expect("a message is a JSON string"),
        None => "None".to_string(),
    };
    format!(
        r#"
import logging
import sys

import cfnresponse

FAILURE_MESSAGE = {failure}

logging.getLogger().setLevel(logging.INFO)


def handler(event, context):
    marker = "{MARKER_PREFIX}" + event["RequestType"] + ":" + event["RequestId"]
{emit}
    physical = event.get("PhysicalResourceId") or "provider-minted-id"
    if event["RequestType"] == "Create" and FAILURE_MESSAGE is not None:
        cfnresponse.send(
            event,
            context,
            cfnresponse.FAILED,
            {{}},
            physicalResourceId=physical,
            reason=FAILURE_MESSAGE,
        )
    else:
        cfnresponse.send(
            event,
            context,
            cfnresponse.SUCCESS,
            {{"Echo": event["ResourceProperties"]["Marker"]}},
            physicalResourceId=physical,
        )
"#
    )
}

/// The template one row deploys: the provider function carrying that row's handler, and the
/// custom resource that provider backs, publishing the answer's own `Data`.
fn template(function: &str, source: &str, role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            PROVIDER_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": source },
                },
            },
            CUSTOM_ID: {
                "Type": "Custom::Provisioned",
                "Properties": {
                    "ServiceToken": { "Fn::GetAtt": [PROVIDER_ID, "Arn"] },
                    "Marker": MARKER,
                },
            },
        },
        "Outputs": {
            "Echo": { "Value": { "Fn::GetAtt": [CUSTOM_ID, "Echo"] } },
        },
    })
    .to_string()
}

/// Deploy one row's stack, asserting only that the create was ACCEPTED: what it then does is what
/// the row is about.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) {
    let created = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// What one look at a group read: the streams `DescribeLogStreams` reported, every message
/// `GetLogEvents` answered across them, and the last answer that was NOT a 200 — so a group that
/// stays empty because a read was refused says which call was refused and with what, rather than
/// only that nothing was there.
#[derive(Debug, Default)]
struct GroupRead {
    streams: Vec<String>,
    messages: Vec<String>,
    refusal: Option<String>,
}

/// Every stream `DescribeLogStreams` reports for `group`, with every message `GetLogEvents`
/// answers across them: the whole of what a client holding the group's name can read out of it.
///
/// A group nothing has written to yet does not exist at all, which CloudWatch Logs answers as a
/// refusal rather than as an empty list — so such an answer reads here as no streams and no
/// records, which is what this answers while a run is still in flight. The refusal itself is
/// kept, because a read withheld for any OTHER reason looks the same from here.
async fn read_group(router: &MultiServiceRouter, group: &str) -> GroupRead {
    let described = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": group }),
    )
    .await;
    if described.status != 200 {
        return GroupRead {
            refusal: Some(format!(
                "DescribeLogStreams({group:?}) answered {} with {:?}",
                described.status,
                described.text()
            )),
            ..GroupRead::default()
        };
    }
    let streams: Vec<String> = described
        .json()
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream.get("logStreamName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut read = GroupRead {
        streams,
        ..GroupRead::default()
    };
    for stream in &read.streams {
        let answered = logs(
            router,
            "GetLogEvents",
            json!({
                "logGroupName": group,
                "logStreamName": stream,
                // From the head, so the whole stream is answered rather than its tail.
                "startFromHead": true,
            }),
        )
        .await;
        if answered.status != 200 {
            read.refusal = Some(format!(
                "GetLogEvents({group:?}/{stream:?}) answered {} with {:?}",
                answered.status,
                answered.text()
            ));
            continue;
        }
        if let Some(events) = answered.json().get("events").and_then(Value::as_array) {
            for event in events {
                if let Some(message) = event.get("message").and_then(Value::as_str) {
                    read.messages.push(message.to_string());
                }
            }
        }
    }
    read
}

/// Look at `group` again until some message of it carries `needle`, answering its streams and
/// every message they hold.
///
/// The delivery of a run's log is not part of the answer to the call that caused the run, so what
/// is waited for is the CONDITION — the line being readable — under [`LOG_DEADLINE`].
async fn awaited_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    needle: &str,
) -> (Vec<String>, Vec<String>) {
    let giving_up_at = Instant::now() + LOG_DEADLINE;
    loop {
        let read = read_group(router, group).await;
        if read.messages.iter().any(|message| message.contains(needle)) {
            return (read.streams, read.messages);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {group:?} to hold a record carrying {needle:?}, it reported \
             streams {:?} holding {:?}, with the last answer that was not a 200 being {:?}",
            read.streams,
            read.messages,
            read.refusal
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every distinct `RequestId` the provider logged for requests of type `request_type`, read out
/// of the marker lines of `messages`.
fn logged_request_ids(messages: &[String], request_type: &str) -> BTreeSet<String> {
    let marker = format!("{MARKER_PREFIX}{request_type}:");
    let mut found = BTreeSet::new();
    for message in messages {
        let mut rest = message.as_str();
        while let Some(at) = rest.find(&marker) {
            let tail = &rest[at + marker.len()..];
            let id: String = tail
                .chars()
                .take_while(|character| character.is_ascii_hexdigit() || *character == '-')
                .collect();
            if !id.is_empty() {
                found.insert(id);
            }
            rest = tail;
        }
    }
    found
}

/// Whether `id` has the form CloudFormation gives a request id — the 8-4-4-4-12 grouping of
/// lowercase hexadecimal every identifier this service hands out carries.
///
/// The id itself is this run's own, so only its form is pinned: a value one recording happened to
/// carry would say nothing about any other run.
fn is_a_request_id(id: &str) -> bool {
    let lowercase_hex =
        |character: char| character.is_ascii_hexdigit() && !character.is_ascii_uppercase();
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(width, part)| part.len() == *width)
        && parts
            .iter()
            .all(|part| part.chars().all(lowercase_hex))
}

/// The one `RequestId` the provider logged for `request_type`, asserted to be exactly one of the
/// minted form: a resource is sent one request per stack operation, each under its own id.
fn the_logged_request_id(
    case: &str,
    group: &str,
    messages: &[String],
    request_type: &str,
) -> String {
    let found = logged_request_ids(messages, request_type);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one distinct {request_type} RequestId in {group:?}, got \
         {found:?} from {messages:?}"
    );
    let id = found.into_iter().next().unwrap_or_default();
    assert!(
        is_a_request_id(&id),
        "case {case:?}: expected the logged {request_type} RequestId to carry the minted \
         8-4-4-4-12 form, got {id:?}"
    );
    id
}

/// One row of the output table: how the handler emits its marker line, and what else it writes
/// around it.
struct Output {
    case: &'static str,
    function: &'static str,
    /// The statements that emit the marker, as the body of the handler — indented as Python's
    /// own block.
    emit: &'static str,
    /// What the handler writes BESIDE the marker, every line of which has to be readable too.
    also: &'static [&'static str],
}

const OUTPUTS: &[Output] = &[
    Output {
        case: "a handler that prints the marker to stdout",
        function: "cr-run-log-stdout",
        emit: "    print(marker)",
        also: &[],
    },
    Output {
        case: "a handler that writes the marker to stderr",
        function: "cr-run-log-stderr",
        emit: "    sys.stderr.write(marker + \"\\n\")",
        also: &[],
    },
    Output {
        case: "a handler that logs the marker through the logging module",
        function: "cr-run-log-logging",
        emit: "    logging.getLogger().info(marker)",
        also: &[],
    },
    Output {
        case: "a handler that prints several lines around the marker",
        function: "cr-run-log-many",
        emit: "    print(\"provider begins\")\n    print(marker)\n    print(\"provider ends\")",
        also: &["provider begins", "provider ends"],
    },
];

/// One row of the failure table: the message the provider's `FAILED` answer states.
struct Failure {
    case: &'static str,
    function: &'static str,
    message: &'static str,
}

const FAILURES: &[Failure] = &[
    Failure {
        case: "a provider whose answer states a one-clause message",
        function: "cr-run-log-failed-plain",
        message: "Operation timed out",
    },
    Failure {
        case: "a provider whose answer states a message carrying its own punctuation",
        function: "cr-run-log-failed-punctuated",
        message: "the gate never reported done: giving up (at the stated budget)",
    },
];

#[tokio::test]
async fn everything_a_custom_resources_provider_writes_reaches_the_functions_log_group() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone. The execution role the providers run as is
    // created in IAM first, so no row's stack has to provision one before its provider exists.
    let router = emulator();
    let role_arn = execution_role(&router).await;

    for (index, row) in OUTPUTS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cr-run-log-{index}");
        let source = handler_source(row.emit, None);
        deploy(
            &router,
            case,
            &stack,
            &template(row.function, &source, &role_arn),
        )
        .await;

        // 1. The provider really ran and really answered: the stack settles complete, and what
        //    it publishes is the `Data` that answer carried.
        let (reported, described) = settled_status(&router, &stack).await;
        if reported != CREATE_COMPLETE {
            // What the resource itself recorded says WHY a row did not complete, which the stack's
            // own status does not.
            let events = resource_events(&router, &stack, CUSTOM_ID).await;
            panic!(
                "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?}; \
                 the resource recorded {events:?}; last answer {described}"
            );
        }
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("Echo").map(String::as_str),
            Some(MARKER),
            "case {case:?}: expected Fn::GetAtt to resolve to the answered Data, got {outputs:?}"
        );

        // 2. ... and what that run WROTE is in the group the function logs to, in a stream of it,
        //    readable through CloudWatch Logs' own readers.
        let group = format!("{DEFAULT_GROUP_PREFIX}{}", row.function);
        let marker = format!("{MARKER_PREFIX}Create:");
        let (streams, messages) = awaited_group(&router, case, &group, &marker).await;
        assert!(
            !streams.is_empty(),
            "case {case:?}: expected the run's records to be in a stream of {group:?}, got none"
        );

        // 3. Every line the handler wrote is there, however it wrote it...
        for line in row.also {
            assert!(
                messages.iter().any(|message| message.contains(line)),
                "case {case:?}: expected {group:?} to hold the line the handler wrote, {line:?}, \
                 got {messages:?}"
            );
        }

        // 4. ... beside the runtime's own closing frame, which is what says the whole run's log
        //    was delivered rather than one line of it.
        assert!(
            messages
                .iter()
                .any(|message| message.starts_with(REPORT_PREFIX)),
            "case {case:?}: expected {group:?} to hold the runtime's {REPORT_PREFIX:?} frame for \
             the run, got {messages:?}"
        );

        // 5. ... and the id the run read out of its event is one request id of the minted form:
        //    the resource was sent exactly one `Create`.
        the_logged_request_id(case, &group, &messages, "Create");
    }
}

#[tokio::test]
async fn the_request_id_a_provider_logged_is_the_one_the_failed_resources_reason_names() {
    let router = emulator();
    let role_arn = execution_role(&router).await;

    for (index, row) in FAILURES.iter().enumerate() {
        let case = row.case;
        let stack = format!("cr-run-log-failed-{index}");
        let source = handler_source("    print(marker)", Some(row.message));
        deploy(
            &router,
            case,
            &stack,
            &template(row.function, &source, &role_arn),
        )
        .await;

        // 1. The provider answered FAILED, so the create fails and is rolled back.
        let (reported, described) = settled_status(&router, &stack).await;
        if reported != ROLLBACK_COMPLETE {
            let events = resource_events(&router, &stack, CUSTOM_ID).await;
            panic!(
                "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got \
                 {reported:?}; the resource recorded {events:?}; last answer {described}"
            );
        }

        // 2. The run that answered it left its own log behind, and the id it read out of its
        //    event is the one id the `Create` was sent under.
        let group = format!("{DEFAULT_GROUP_PREFIX}{}", row.function);
        let marker = format!("{MARKER_PREFIX}Create:");
        let (streams, messages) = awaited_group(&router, case, &group, &marker).await;
        assert!(
            !streams.is_empty(),
            "case {case:?}: expected the run's records to be in a stream of {group:?}, got none"
        );
        let create_id = the_logged_request_id(case, &group, &messages, "Create");

        // 3. ... and THAT is the id the resource's own event names, in the sentence
        //    CloudFormation reports a provider's own refusal with: the response status it
        //    received, the message that response returned, and the request it answered. The
        //    reason is compared WHOLE — a sentence with anything added to either end is a
        //    different sentence, and the wording is the requirement here.
        let expected = answered_failure_reason(row.message, &create_id);
        let events = resource_events(&router, &stack, CUSTOM_ID).await;
        assert!(
            events
                .iter()
                .any(|(status, reason)| status == CREATE_FAILED && reason == &expected),
            "case {case:?}: expected a {CREATE_FAILED} event for {CUSTOM_ID} reporting \
             {expected:?}, got {events:?}"
        );

        // 4. The rollback asked the provider once more, under an id of its own: a fresh request
        //    id per request, so two requests about one resource are never read as one.
        let delete_marker = format!("{MARKER_PREFIX}Delete:");
        let (_, after) = awaited_group(&router, case, &group, &delete_marker).await;
        let delete_id = the_logged_request_id(case, &group, &after, "Delete");
        assert_ne!(
            delete_id, create_id,
            "case {case:?}: expected the rollback's Delete to carry a request id of its own, it \
             reported the Create's {create_id:?} in {after:?}"
        );
    }
}
