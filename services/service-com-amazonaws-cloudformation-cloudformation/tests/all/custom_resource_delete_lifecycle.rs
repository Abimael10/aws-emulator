// SPDX-License-Identifier: CC0-1.0

//! The DELETE half of a Lambda-backed custom resource's lifecycle, end to end: a stack whose
//! `Custom::` resource is backed by a function the same stack deploys comes DOWN again.
//!
//! RULE under test: **on a stack operation that REMOVES a custom resource, CloudFormation
//! invokes the resource's `ServiceToken` provider with a request whose `RequestType` is
//! `Delete`, carrying the `PhysicalResourceId` that provider last answered with and the
//! properties the resource was last declared with, holds the resource — and the stack
//! operation behind it — in its `*_IN_PROGRESS` status until the provider answers at that
//! request's `ResponseURL`, and then takes the resource and the stack to their terminal
//! status.** A removal is never reported complete without the provider's answer, and the wait
//! for it never becomes a hang: the stack reaches `DELETE_COMPLETE`.
//!
//! Every observation here is one a client makes, over each service's own wire: the stack
//! operations through CloudFormation's awsQuery endpoint, the provider created in AWS Lambda by
//! the stack itself, and — crucially — the record the provider's OWN CODE writes for each
//! request it was invoked with, onto an Amazon SQS queue created OUTSIDE the stack so it
//! outlives the stack's deletion. A row whose stack reported a deletion its provider was never
//! asked about therefore fails here: nothing in this suite writes to the `ResponseURL` and
//! nothing but a run of the deployed provider puts a record on that queue.
//!
//! The rows differ along what the rule generalises over: the RUNTIME the provider is written
//! for and the SDK its code reaches the marker queue through, the TYPE the custom resource is
//! declared with (`Custom::<Name>` and the modelled `AWS::CloudFormation::CustomResource`), the
//! `PhysicalResourceId` the provider mints — which is what the `Delete` has to carry back — and
//! the property text the resource is declared with.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, settled_stack_status, xml_blocks, xml_text, xml_texts, REGION,
};
use crate::stack_lifecycle::acknowledging_iam;

/// The logical id every row declares its custom resource under, and the one it declares the
/// provider under.
const RESOURCE_ID: &str = "Thing";
const PROVIDER_ID: &str = "Provider";

/// The sentence CloudFormation words a resource's failure with when no provider answer ever
/// arrives — the fixed text, whose one variable part is the request id. No row may produce it:
/// that sentence is what a deletion nobody answered settles as, and the rule says the provider
/// IS asked and DOES answer.
const NO_RESPONSE: &str = "CloudFormation did not receive a response from your Custom Resource.";

/// The environment variable each provider reads the marker queue's NAME out of, so the
/// deployed source carries no url the test had to compute.
const MARKER_QUEUE_VARIABLE: &str = "MARKER_QUEUE";

/// Amazon SQS's `awsJson1_0` wire: the media type and the service shape its `X-Amz-Target`
/// names an operation under.
const SQS_JSON: &str = "application/x-amz-json-1.0";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// How long an observation of work the CLOCK owes — a stack coming up or down, a provider
/// invocation, a record landing on the marker queue — goes on looking before this suite calls
/// it a hang, and how long it hands the runtime back between two looks.
///
/// A wall-clock DEADLINE over a loop that waits for the CONDITION, with generous slack over the
/// spans these services choose: a row fails only when the work did not happen at all, never
/// because the host was busy. Nothing here asserts how long anything took.
const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A provider written for a managed Python runtime: it records the request it was invoked with
/// on the marker queue — addressed by NAME through Amazon SQS's own `GetQueueUrl` — and then
/// PUTs its response document to the request's own `ResponseURL` with the standard library's
/// HTTP client.
const PYTHON_PROVIDER: &str = r#"
import json
import os
import urllib.request

import boto3


def handler(event, context):
    properties = event["ResourceProperties"]
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["MARKER_QUEUE"])["QueueUrl"]
    client.send_message(
        QueueUrl=url,
        MessageBody=json.dumps(
            {
                "RequestType": event["RequestType"],
                "LogicalResourceId": event["LogicalResourceId"],
                "PhysicalResourceId": event.get("PhysicalResourceId", ""),
                "Marker": properties.get("Marker", ""),
            }
        ),
    )
    document = json.dumps(
        {
            "Status": "SUCCESS",
            "PhysicalResourceId": properties["AnswerId"],
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": {"Echo": properties.get("Marker", "")},
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=document,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// The same provider written for a managed Node runtime, reaching the marker queue through that
/// runtime's own AWS SDK and answering with its own HTTP client — the shape every provider the
/// CDK synthesises answers in.
const NODE_PROVIDER: &str = r#"
'use strict';

const https = require('https');
const url = require('url');
const { SQSClient, GetQueueUrlCommand, SendMessageCommand } = require('@aws-sdk/client-sqs');

function put(options, document) {
    return new Promise((resolve, reject) => {
        try {
            const request = https.request(options, () => resolve());
            request.on('error', reject);
            request.write(document);
            request.end();
        } catch (error) {
            reject(error);
        }
    });
}

exports.handler = async (event) => {
    const properties = event.ResourceProperties || {};
    const client = new SQSClient({});
    const queue = await client.send(new GetQueueUrlCommand({
        QueueName: process.env.MARKER_QUEUE,
    }));
    await client.send(new SendMessageCommand({
        QueueUrl: queue.QueueUrl,
        MessageBody: JSON.stringify({
            RequestType: event.RequestType,
            LogicalResourceId: event.LogicalResourceId,
            PhysicalResourceId: event.PhysicalResourceId || '',
            Marker: properties.Marker || '',
        }),
    }));
    const document = JSON.stringify({
        Status: 'SUCCESS',
        PhysicalResourceId: properties.AnswerId,
        StackId: event.StackId,
        RequestId: event.RequestId,
        LogicalResourceId: event.LogicalResourceId,
        Data: { Echo: properties.Marker || '' },
    });
    const parsed = url.parse(event.ResponseURL);
    await put({
        hostname: parsed.hostname,
        port: parsed.port,
        path: parsed.path,
        method: 'PUT',
        headers: {
            'content-type': '',
            'content-length': Buffer.byteLength(document, 'utf8'),
        },
    }, document);
    return {};
};
"#;

/// One row: how its resources are named, the provider it deploys and the runtime that provider
/// is declared for, the type the custom resource takes, the id the provider mints for it, and
/// the property text it is declared with.
struct Row {
    case: &'static str,
    slug: &'static str,
    source: &'static str,
    runtime: &'static str,
    resource_type: &'static str,
    answer_id: &'static str,
    marker: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a Python provider backing a Custom:: resource",
        slug: "custom-delete-python",
        source: PYTHON_PROVIDER,
        runtime: "python3.12",
        resource_type: "Custom::Probe",
        answer_id: "probe-python-1",
        marker: "v1",
    },
    // The same rule under the MODELLED resource type, on a Node runtime whose provider reaches
    // the marker queue through that runtime's own SDK, with an id whose text a form body and an
    // XML envelope both have to carry unharmed.
    Row {
        case: "a Node provider backing the modelled custom resource type",
        slug: "custom-delete-node",
        source: NODE_PROVIDER,
        runtime: "nodejs20.x",
        resource_type: "AWS::CloudFormation::CustomResource",
        answer_id: "probe/node 2",
        marker: "a marker, with spaces & a ✓",
    },
];

/// The names one row's resources carry, computed from its slug so no two rows share one.
fn stack_name(row: &Row) -> String {
    format!("{}-stack", row.slug)
}
/// See [`stack_name`].
fn marker_queue(row: &Row) -> String {
    format!("{}-markers", row.slug)
}
/// See [`stack_name`].
fn function_name(row: &Row) -> String {
    format!("{}-provider", row.slug)
}

/// A router carrying CloudFormation and the three services this estate lives in, all on ONE
/// engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `declared_topic_policy_decides_the_alarm_action.rs` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn provider_estate_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One Amazon SQS call through the same router, signed for `sqs` the way a client's SDK signs
/// it; it must answer 200, since every call here is one this estate depends on.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://sqs.{REGION}.amazonaws.com/"))
        .header("content-type", SQS_JSON)
        .header("x-amz-target", format!("{SQS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    let answered: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()));
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {answered}"
    );
    answered
}

/// One record the deployed provider wrote for a request it was invoked with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invoked {
    request_type: String,
    logical_resource_id: String,
    /// The `PhysicalResourceId` the request carried, empty where it carried none — which is
    /// what a `Create` carries, the resource not existing yet.
    physical_resource_id: String,
    marker: String,
}

/// The provider's records waiting on the marker queue at `url` right now, taken off it so no
/// later read counts one twice.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Invoked> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let mut records = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let body = message["Body"].as_str().unwrap_or_default();
        let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the record is not the provider's document ({error}): {body}")
        });
        records.push(Invoked {
            request_type: document["RequestType"].as_str().unwrap_or_default().to_string(),
            logical_resource_id: document["LogicalResourceId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            physical_resource_id: document["PhysicalResourceId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            marker: document["Marker"].as_str().unwrap_or_default().to_string(),
        });
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    records
}

/// Look at the marker queue until the provider has recorded `wanted` requests or the deadline
/// passes, reading it at least once — so a row asserting about these records has really waited
/// for the CONDITION rather than for a span of the clock.
async fn invocations_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Invoked> {
    let deadline = Instant::now() + BUDGET;
    let mut held = drained(router, case, url).await;
    while held.len() < wanted && Instant::now() < deadline {
        tokio::time::sleep(LOOK_AGAIN).await;
        held.extend(drained(router, case, url).await);
    }
    held
}

/// The template one row deploys: the execution role the provider assumes, with the grant its
/// own calls to the marker queue need; the provider itself, carrying the row's inline source and
/// the queue's name in its environment; and the custom resource that provider backs, whose
/// properties say what it is to answer. The outputs publish what `Ref` and `Fn::GetAtt` resolve
/// to, so the create's own answer is observable before anything is deleted.
fn template(row: &Row) -> String {
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            // The provider addresses the marker queue by name and writes one record per
            // request. The queue is NOT this stack's, so the grant names the actions rather
            // than a resource of a template that does not declare it.
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    });
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a Lambda-backed custom resource whose provider answers every request",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "record-every-request",
                        "PolicyDocument": grant,
                    }],
                },
            },
            PROVIDER_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Runtime": row.runtime,
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Code": {"ZipFile": row.source},
                    "Environment": {
                        "Variables": {MARKER_QUEUE_VARIABLE: marker_queue(row)},
                    },
                },
            },
            RESOURCE_ID: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER_ID, "Arn"]},
                    "AnswerId": row.answer_id,
                    "Marker": row.marker,
                },
            },
        },
        "Outputs": {
            "ThingRef": {"Value": {"Ref": RESOURCE_ID}},
            "ThingEcho": {"Value": {"Fn::GetAtt": [RESOURCE_ID, "Echo"]}},
        },
    })
    .to_string()
}

/// A stack's reported outputs, as the key/value map a client reads.
fn outputs_of(xml: &str) -> std::collections::BTreeMap<String, String> {
    xml_texts(xml, "OutputKey")
        .into_iter()
        .zip(xml_texts(xml, "OutputValue"))
        .collect()
}

/// The statuses the stack `id` recorded its resource `logical_id` MOVING THROUGH, oldest first,
/// beside every `ResourceStatusReason` of the whole stream — read by stack ID, which stays
/// addressable once a deletion has completed.
///
/// A resource may be recorded more than once in the SAME status while it is in flight — a
/// request published, then progress reported under it — so consecutive repeats are collapsed:
/// what the sequence says is which statuses the resource passed through and in what order.
async fn recorded_events(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    logical_id: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, body) = call(router, "DescribeStackEvents", &[member("StackName", id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {body}"
    );
    let mut newest_first: Vec<String> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .map(|event| xml_text(event, "ResourceStatus").unwrap_or_default())
        .collect();
    newest_first.dedup();
    newest_first.reverse();
    (newest_first, xml_texts(&body, "ResourceStatusReason"))
}

#[tokio::test]
async fn a_custom_resources_delete_reaches_its_provider_and_the_stack_comes_down() {
    for row in ROWS {
        let case = row.case;
        let router = provider_estate_router();
        let stack = stack_name(row);

        // The marker queue is created OUTSIDE the stack, so the records the provider writes for
        // the stack's DELETION are still readable once that stack is gone.
        let created_queue = sqs(
            &router,
            case,
            "CreateQueue",
            &json!({"QueueName": marker_queue(row)}),
        )
        .await;
        let queue_url = created_queue["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected CreateQueue to report a QueueUrl, got {created_queue}")
            })
            .to_string();

        let (status, created) =
            create_stack(&router, &stack, &template(row), &acknowledging_iam(&[])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status} with {created}"
        );
        let stack_id = xml_text(&created, "StackId").unwrap_or_else(|| {
            panic!("case {case:?}: expected CreateStack to report a StackId, got {created}")
        });

        // The CREATE settles on the provider's answer, which is what the DELETE then has to
        // carry back: the id the provider minted.
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete on its provider's answer, got \
             {settled:?}"
        );
        let (_, described) = call(&router, "DescribeStacks", &[member("StackName", &stack)]).await;
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("ThingRef").map(String::as_str),
            Some(row.answer_id),
            "case {case:?}: expected Ref to resolve to the id the provider answered, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("ThingEcho").map(String::as_str),
            Some(row.marker),
            "case {case:?}: expected Fn::GetAtt to resolve to the Data the provider answered, \
             got {outputs:?}"
        );
        let on_create = invocations_until(&router, case, &queue_url, 1).await;
        assert_eq!(
            on_create,
            vec![Invoked {
                request_type: "Create".to_string(),
                logical_resource_id: RESOURCE_ID.to_string(),
                physical_resource_id: String::new(),
                marker: row.marker.to_string(),
            }],
            "case {case:?}: expected the provider to have been invoked once, with the Create, \
             got {on_create:?}"
        );

        // ...and now the stack comes DOWN.
        let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteStack to be accepted, got {status} with {deleted}"
        );
        // The deletion is HELD while the provider is asked: a stack's terminal status is due a
        // span after the call that starts it, so the removal cannot already be reported done.
        let (_, in_flight) = call(&router, "DescribeStacks", &[member("StackName", &stack)]).await;
        assert_eq!(
            xml_text(&in_flight, "StackStatus").as_deref(),
            Some("DELETE_IN_PROGRESS"),
            "case {case:?}: expected the deletion to be in flight while the provider is asked, \
             got {in_flight}"
        );

        // The provider's OWN record of the Delete: it names the resource, carries back the id
        // the provider minted on the create, and carries the properties the resource was last
        // declared with.
        let on_delete = invocations_until(&router, case, &queue_url, 1).await;
        assert_eq!(
            on_delete,
            vec![Invoked {
                request_type: "Delete".to_string(),
                logical_resource_id: RESOURCE_ID.to_string(),
                physical_resource_id: row.answer_id.to_string(),
                marker: row.marker.to_string(),
            }],
            "case {case:?}: expected the provider to have been sent the Delete naming the \
             resource it minted, got {on_delete:?}"
        );

        // The answered deletion CONVERGES: the stack reaches its terminal status rather than
        // waiting forever, and the resource's own events record the deletion completing.
        let after = settled_stack_status(case, &router, REGION, &stack_id).await;
        assert_eq!(
            after, "DELETE_COMPLETE",
            "case {case:?}: expected the deletion to complete on the provider's answer, got \
             {after:?}"
        );
        let (statuses, reasons) = recorded_events(&router, case, &stack_id, RESOURCE_ID).await;
        assert_eq!(
            statuses,
            vec![
                "CREATE_IN_PROGRESS".to_string(),
                "CREATE_COMPLETE".to_string(),
                "DELETE_IN_PROGRESS".to_string(),
                "DELETE_COMPLETE".to_string(),
            ],
            "case {case:?}: expected the resource to pass through both lifecycles, got \
             {statuses:?}"
        );
        assert!(
            !reasons.iter().any(|reason| reason.contains(NO_RESPONSE)),
            "case {case:?}: no event may report that no provider answer arrived, got {reasons:?}"
        );
    }
}
