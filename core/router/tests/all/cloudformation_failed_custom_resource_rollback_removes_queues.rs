// SPDX-License-Identifier: CC0-1.0

//! What a Lambda-backed custom resource answering `Status` `FAILED` does to the resources the
//! same create had ALREADY made in the services that own them.
//!
//! RULE under test: **a custom resource whose provider answers its `Create` with `Status`
//! `FAILED` fails that resource with the `Reason` the provider gave and rolls the create back;
//! the rollback DELETES every resource the create had already made in the service that owns it
//! — so that service no longer holds it, not merely CloudFormation reporting it
//! `DELETE_COMPLETE` — and the stack settles in `ROLLBACK_COMPLETE`, the terminal status a
//! clean failure leaves behind, rather than `ROLLBACK_FAILED`, which is the status a rollback
//! that itself broke leaves and which no plain `UpdateStack` can leave. The same template whose
//! provider answers `SUCCESS` reaches `CREATE_COMPLETE`, keeps those resources in their owning
//! service, and publishes what the answer carried.**
//!
//! Three crates have to agree for that to hold, which is why the suite lives here:
//! CloudFormation sends the provider request, rolls the failed create back and reports the
//! stack; AWS Lambda runs the provider that answers `FAILED`; and Amazon SQS is the service
//! that OWNS the queue the create made before the provider was asked — so whether the rollback
//! really removed it is only answerable through SQS's own wire.
//!
//! The rows differ along what the rule generalises over: the `Status` the provider answers
//! (`FAILED` and the `SUCCESS` control), how many resources of the owning service the create
//! made before the custom resource was asked (one and two), how the custom resource's type is
//! spelled (the generic `AWS::CloudFormation::CustomResource` and an author's own
//! `Custom::<Name>`), and the text of the `Reason` the provider states (plain, and a diagnostic
//! sentence carrying punctuation).
//!
//! The harness is the crate's own: the emulator, the awsQuery caller, the dispatcher and the
//! settling poll come from `cloudformation_completed_stack_output_values`, the stack-event
//! reader from `cloudformation_failed_custom_resource_rollback_deletes` — the suite that pins
//! the CloudFormation-side half of this rule — and the awsJson error code from
//! `bedrock_agentcore_data_plane_parent_resources`, so nothing here is a second copy of one of
//! them. What this suite spells for itself is what no sibling has: a provider whose answer its
//! own declaration decides, and a template that makes resources of another service before that
//! provider is asked anything.
//!
//! Nothing here sleeps a span on a service's behalf: every wait is a poll for the CONDITION
//! under a deadline far longer than the spans this emulator gives these transitions, so a
//! loaded host cannot decide a row.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_agentcore_data_plane_parent_resources::error_code;
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, emulator, settled_status, Answered, CREATE_COMPLETE,
    REGION,
};
use crate::cloudformation_failed_custom_resource_rollback_deletes::resource_events;
use crate::query_wire::outputs_of;

/// The terminal status a create whose rollback was carried out reaches.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The resource statuses the rule is stated about.
const CREATE_FAILED: &str = "CREATE_FAILED";
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The logical id every template declares its custom resource under.
const CUSTOM: &str = "Probe";

/// The `Data` entry the provider echoes from its own declaration, which is what the control row
/// reads back out of the stack's outputs — a value that could only have come from a run of the
/// provider's code.
const MARKER: &str = "answered by the provider";

/// The error Amazon SQS answers a name it holds no queue under with, which is what says the
/// rollback really removed the queue rather than only reporting it gone.
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";

/// The provider: it answers the `Create` with the `Status` its own declaration asks for —
/// `FAILED` carrying the declared `Reason`, or `SUCCESS` carrying the declared marker as its
/// `Data` — and carries every other request (the `Delete` a rollback or a deletion sends) out,
/// which is what an ordinary responder does.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    request_type = event["RequestType"]
    properties = event["ResourceProperties"]
    physical = event.get("PhysicalResourceId") or "probe-physical-id"
    if request_type != "Create":
        answer = {"Status": "SUCCESS", "PhysicalResourceId": physical}
    elif properties["ResponseStatus"] == "FAILED":
        answer = {
            "Status": "FAILED",
            "Reason": properties["FailureReason"],
            "PhysicalResourceId": physical,
        }
    else:
        answer = {
            "Status": "SUCCESS",
            "PhysicalResourceId": "probe-" + event["LogicalResourceId"],
            "Data": {"Echo": properties["Echo"]},
        }
    body = json.dumps(answer).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// Call one Amazon SQS operation over its `awsJson1_0` wire, with whatever status it answered:
/// a read of a queue the rollback removed is a REFUSAL, so the status comes back rather than
/// being asserted.
async fn sqs(router: &MultiServiceRouter, operation: &str, request: &Value) -> Answered {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let target = format!("AmazonSQS.{operation}");
    let signed = authorization("sqs");
    dispatch(
        router,
        "POST",
        &host,
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &target),
            ("authorization", &signed),
        ],
        Bytes::from(request.to_string()),
    )
    .await
}

/// One row: what the provider answers the `Create` with, the queues the same create makes
/// before the provider is asked, and how the custom resource is declared.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Status` the provider answers the `Create` with.
    response_status: &'static str,
    /// The `Reason` it states with a `FAILED` answer.
    reason: &'static str,
    resource_type: &'static str,
    /// The queues the template declares, which the custom resource's own declaration refers to
    /// — so each is created, in Amazon SQS, before the provider is asked anything.
    queues: &'static [&'static str],
    /// The status the stack must settle in.
    settles_to: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a failing provider over one queue the create had already made",
        stack: "failed-custom-resource-one-queue",
        response_status: "FAILED",
        reason: "the provisioner could not create the thing",
        resource_type: "AWS::CloudFormation::CustomResource",
        queues: &["failed-custom-resource-recorder"],
        settles_to: ROLLBACK_COMPLETE,
    },
    Row {
        case: "a failing provider over two queues, refusing with a diagnostic sentence",
        stack: "failed-custom-resource-two-queues",
        response_status: "FAILED",
        reason: "Error: the recorder answered 500 (InternalFailure); see the log stream.",
        resource_type: "Custom::Recorder",
        queues: &[
            "failed-custom-resource-recorder-a",
            "failed-custom-resource-recorder-b",
        ],
        settles_to: ROLLBACK_COMPLETE,
    },
    // The control: the SAME template answered SUCCESS completes and keeps its queue, so no row
    // above passes against an emulator that tears a stack down whatever its provider answered.
    Row {
        case: "the same template whose provider answers SUCCESS",
        stack: "answered-custom-resource-one-queue",
        response_status: "SUCCESS",
        reason: "unused by a provider that answers SUCCESS",
        resource_type: "Custom::Recorder",
        queues: &["answered-custom-resource-recorder"],
        settles_to: CREATE_COMPLETE,
    },
];

/// The template one row deploys: the execution role the provider assumes, the provider
/// function, the row's queues, and the custom resource whose declaration REFERS to each of
/// them — which is what makes the queues exist before the provider is asked for anything.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Action": "sts:AssumeRole",
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                    }],
                },
            },
        }),
    );
    resources.insert(
        "Provider".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Code": {"ZipFile": PROVIDER_SOURCE},
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                "Runtime": "python3.12",
                "Timeout": 60,
            },
        }),
    );
    let mut queue_urls = Vec::new();
    for (index, name) in row.queues.iter().enumerate() {
        resources.insert(
            queue_logical_id(index),
            json!({
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": name},
            }),
        );
        queue_urls.push(json!({"Ref": queue_logical_id(index)}));
    }
    resources.insert(
        CUSTOM.to_string(),
        json!({
            "Type": row.resource_type,
            "Properties": {
                "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                "ResponseStatus": row.response_status,
                "FailureReason": row.reason,
                "Echo": MARKER,
                "Recorders": Value::Array(queue_urls),
            },
        }),
    );
    json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            "ProbeRef": {"Value": {"Ref": CUSTOM}},
            "ProbeEcho": {"Value": {"Fn::GetAtt": [CUSTOM, "Echo"]}},
        },
    })
    .to_string()
}

/// The logical id the `index`-th queue of a row is declared under.
fn queue_logical_id(index: usize) -> String {
    format!("Recorder{index}")
}

#[tokio::test]
async fn a_failed_provider_answer_rolls_back_the_queues_the_create_had_made() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row)),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {}",
            created.status,
            created.text()
        );

        // 1. The stack settles, and settles where the provider's answer puts it: a rollback
        // that was carried out reports ROLLBACK_COMPLETE, never the ROLLBACK_FAILED a broken
        // rollback leaves.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, row.settles_to,
            "case {case:?}: expected the stack to settle at {:?}, got {settled:?} with \
             {described}",
            row.settles_to
        );

        // 2. What the stack says about the custom resource and about each queue it made.
        let events = resource_events(&router, row.stack, CUSTOM).await;
        if row.settles_to == ROLLBACK_COMPLETE {
            let failed: Vec<&(String, String)> = events
                .iter()
                .filter(|(status, _)| status == CREATE_FAILED)
                .collect();
            assert!(
                failed.iter().any(|(_, reason)| reason.contains(row.reason)),
                "case {case:?}: expected a {CREATE_FAILED} event for {CUSTOM} carrying the \
                 provider's own reason {:?}, got {events:?}",
                row.reason
            );
            for index in 0..row.queues.len() {
                let logical_id = queue_logical_id(index);
                let reported = resource_events(&router, row.stack, &logical_id).await;
                // The queue was MADE before the provider was asked, and the rollback took it
                // down: both halves on the row's own events, so the SQS read below is a read
                // of a queue that had existed.
                assert!(
                    reported
                        .iter()
                        .any(|(status, _)| status == RESOURCE_CREATE_COMPLETE),
                    "case {case:?}: expected {logical_id} to have been created before the \
                     provider answered, so its events carry {RESOURCE_CREATE_COMPLETE}, got \
                     {reported:?}"
                );
                assert!(
                    reported.iter().any(|(status, _)| status == DELETE_COMPLETE),
                    "case {case:?}: expected the rollback to report {logical_id} \
                     {DELETE_COMPLETE}, got {reported:?}"
                );
            }
        } else {
            let outputs = outputs_of(&described);
            assert_eq!(
                outputs.get("ProbeRef").map(String::as_str),
                Some(format!("probe-{CUSTOM}").as_str()),
                "case {case:?}: expected Ref to resolve to the id the provider answered, got \
                 {outputs:?}"
            );
            assert_eq!(
                outputs.get("ProbeEcho").map(String::as_str),
                Some(MARKER),
                "case {case:?}: expected Fn::GetAtt to resolve to the Data the provider \
                 answered, got {outputs:?}"
            );
        }

        // 3. The claim the stack's own report cannot make: whether Amazon SQS still holds each
        // queue the create made. A rolled-back create leaves none of them there, and the
        // control's completed create leaves every one.
        for name in row.queues {
            let answered = sqs(&router, "GetQueueUrl", &json!({"QueueName": name})).await;
            let body = answered.json();
            if row.settles_to == ROLLBACK_COMPLETE {
                assert_eq!(
                    answered.status,
                    400,
                    "case {case:?}: expected Amazon SQS to hold no queue {name:?} after the \
                     rollback, got {} with {body}",
                    answered.status
                );
                assert_eq!(
                    error_code(&body),
                    QUEUE_DOES_NOT_EXIST,
                    "case {case:?}: expected the refusal for {name:?} to be \
                     {QUEUE_DOES_NOT_EXIST}, got {body}"
                );
            } else {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: expected Amazon SQS to still hold the queue {name:?} the \
                     completed create made, got {} with {body}",
                    answered.status
                );
                let url = body.get("QueueUrl").and_then(Value::as_str).unwrap_or_default();
                assert!(
                    url.ends_with(&format!("/{name}")),
                    "case {case:?}: expected the url of {name:?} to end in its own name, got \
                     {body}"
                );
            }
        }
    }
}
