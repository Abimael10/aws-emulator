// SPDX-License-Identifier: CC0-1.0

//! The WHOLE lifecycle callback of a Lambda-backed custom resource, read where a client sees
//! it: the request the provider is sent, the answer it sends back, and what the stack then
//! reports.
//!
//! RULE under test: **a custom resource whose `ServiceToken` names an AWS Lambda function is
//! sent one lifecycle request per stack operation that acts on it — a `Create` on the
//! deployment that first declares it and a `Delete` on the one that removes it — each carrying
//! a `ResponseURL` the sender PRE-SIGNED and, for a resource that already exists, the
//! `PhysicalResourceId` CloudFormation knows it by; the resource is held until the provider
//! PUTs its answer to that URL, and a `SUCCESS` answer is what the resource IS: the
//! `PhysicalResourceId` it reports becomes what `Ref` resolves to and what the stack records
//! for the resource, the `Data` it reports becomes what `Fn::GetAtt <LogicalId>.<Key>` resolves
//! against, and the operation then reaches its own terminal status — `CREATE_COMPLETE` for the
//! create, and for the deletion a stack CloudFormation no longer holds, so a later
//! `DescribeStacks` on that name refuses with `ValidationError` and `Stack with id <name> does
//! not exist`.**
//!
//! Why it matters: a Lambda-backed custom resource is the documented extension point every CDK
//! and SAM provider and every bespoke provisioning hook is built on. A stack that never settles
//! on the provider's answer wedges a deployment pipeline; a `Ref` that is not the id the
//! provider minted, or an `Fn::GetAtt` that does not resolve to the `Data` it published, hands
//! everything downstream the wrong resource; and a teardown that never re-issues the `Delete`
//! never tells the provider to release what it made, so what it provisioned outside the stack
//! leaks.
//!
//! Three crates have to agree for the rule to hold, which is why the suite lives here:
//! CloudFormation sends each request and holds the resource until the answer lands, AWS Lambda
//! runs the provider under its execution role and carries both the SDK it records with and the
//! HTTP client it answers with, and Amazon SQS owns the OUT-OF-STACK queue the provider records
//! each request it received into — the only place the `Delete` request of a stack that is being
//! torn down can still be read from after the teardown is over.
//!
//! The rows differ along what the rule generalises over: how the resource's type is spelled
//! (the generic `AWS::CloudFormation::CustomResource` and an author's own `Custom::<Name>`), how
//! many `Data` entries the provider publishes (two, one, and none at all), what those values
//! carry (a plain word, and a sentence with punctuation and markup in it), and the prefix the
//! responder mints its own physical id under. Each row is its own test, so no row waits on
//! another's transitions.
//!
//! The harness is the crate's own: the emulator, the awsQuery caller, the dispatcher, the
//! settling poll and the failure reader come from `cloudformation_completed_stack_output_values`
//! and the XML readers from `query_wire`, so nothing here is a second copy of one of them. What
//! this suite spells for itself is what no sibling has: a provider that records the request it
//! was sent into a queue OUTSIDE the stack and mints its own physical id per invocation.
//!
//! Nothing here sleeps a span on a service's behalf: every wait is a poll for the CONDITION
//! under a deadline far longer than the spans this emulator gives these transitions, so a loaded
//! host cannot decide a row.

use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, emulator, reported_failures, settled_status,
    CREATE_COMPLETE, POLL, REGION, SETTLE_DEADLINE,
};
use crate::query_wire::{element, outputs_of, physical_ids_of};

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Recorded";

/// The logical ids of the provider and the role it runs as.
const PROVIDER: &str = "Provider";
const PROVIDER_ROLE: &str = "ProviderRole";

/// The output every row declares over `Ref` on the custom resource — the id the provider minted.
const PHYSICAL_ID_OUTPUT: &str = "CustomResourcePhysicalId";

/// The refusal a read of a stack CloudFormation no longer holds answers with.
const VALIDATION_ERROR: &str = "ValidationError";

/// How many hex digits the responder appends to its own prefix when it mints a physical id.
const MINTED_DIGITS: usize = 12;

/// The provider, written the way AWS documents one: it records the lifecycle request it was
/// sent into the queue its properties name — with the AWS SDK its runtime supplies, under its
/// execution role — mints its own `PhysicalResourceId` on the request that creates the resource
/// and reuses the one CloudFormation sent on every later request, and PUTs its answer to the
/// request's `ResponseURL` with the HTTP client the managed runtime carries beside that SDK.
const PROVIDER_SOURCE: &str = r#"
import json
import random
import urllib.request

import boto3

QUEUES = boto3.client("sqs")

HEX = "0123456789abcdef"
DIGITS = 12


def _minted(prefix):
    generator = random.Random()
    return prefix + "".join([generator.choice(HEX) for _ in range(DIGITS)])


def handler(event, context):
    properties = event["ResourceProperties"]
    sent = event.get("PhysicalResourceId")
    physical = sent or _minted(properties["IdPrefix"])
    QUEUES.send_message(
        QueueUrl=properties["Recorder"],
        MessageBody=json.dumps(
            {
                "RequestType": event["RequestType"],
                "ResponseURL": event["ResponseURL"],
                "SentPhysicalResourceId": sent,
                "PhysicalResourceId": physical,
                "LogicalResourceId": event["LogicalResourceId"],
                "ResourceType": event["ResourceType"],
                "StackId": event["StackId"],
            }
        ),
    )
    answer = {
        "Status": "SUCCESS",
        "PhysicalResourceId": physical,
        "StackId": event["StackId"],
        "RequestId": event["RequestId"],
        "LogicalResourceId": event["LogicalResourceId"],
        "Data": json.loads(properties["Published"]),
    }
    request = urllib.request.Request(
        event["ResponseURL"],
        data=json.dumps(answer).encode("utf-8"),
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// One row: how the custom resource's type is spelled, the prefix its provider mints under, and
/// the `Data` that provider publishes.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The out-of-stack queue the provider records each request it was sent into.
    recorder: &'static str,
    resource_type: &'static str,
    id_prefix: &'static str,
    /// The `Data` members the provider's answer carries, each of which the template reads back
    /// through an `Fn::GetAtt` of its own.
    published: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "the generic type, publishing two Data entries one of which carries markup",
        stack: "custom-resource-lifecycle-generic",
        recorder: "custom-resource-lifecycle-generic-recorder",
        resource_type: "AWS::CloudFormation::CustomResource",
        id_prefix: "lifecycle-generic-cr-",
        published: &[
            ("Echo", "the value this deployment asked to have echoed"),
            ("Note", "a sentence with punctuation, an & and <markup> in it"),
        ],
    },
    Row {
        case: "an author's own Custom::<Name> type, publishing one Data entry",
        stack: "custom-resource-lifecycle-authored",
        recorder: "custom-resource-lifecycle-authored-recorder",
        resource_type: "Custom::LifecycleRecorder",
        id_prefix: "lifecycle-authored-cr-",
        published: &[("Echo", "authored")],
    },
    Row {
        case: "a provider publishing no Data at all",
        stack: "custom-resource-lifecycle-bare",
        recorder: "custom-resource-lifecycle-bare-recorder",
        resource_type: "Custom::BareRecorder",
        id_prefix: "lifecycle-bare-cr-",
        published: &[],
    },
];

/// The output key a row reads the `Data` member `key` back through.
fn echoed_output(key: &str) -> String {
    format!("Echoed{key}")
}

/// Call one Amazon SQS operation over its `awsJson1_0` wire, which must be answered.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let signed = authorization("sqs");
    let answered = dispatch(
        router,
        "POST",
        &format!("sqs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &target),
            ("authorization", &signed),
        ],
        Bytes::from(request.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// The recorder queue, created OUTSIDE the stack so it outlives the teardown that re-issues the
/// `Delete` request: its URL, for the provider to record into, and its ARN, for the one grant
/// the provider's execution role carries.
async fn given_recorder(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue({name:?}) must answer a URL, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueAttributes({name:?}) must answer the queue's ARN, got {attributes}")
        })
        .to_string();
    (url, arn)
}

/// The template one row deploys: the execution role granted exactly the one write the provider
/// performs, the provider function itself, and the custom resource backed by it — with `Ref` on
/// that resource and an `Fn::GetAtt` for each `Data` member the provider publishes as outputs.
fn template(row: &Row, recorder_url: &str, recorder_arn: &str) -> String {
    let published: Map<String, Value> = row
        .published
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
        .collect();
    let mut outputs = Map::new();
    outputs.insert(
        PHYSICAL_ID_OUTPUT.to_string(),
        json!({ "Value": { "Ref": CUSTOM } }),
    );
    for (key, _) in row.published {
        outputs.insert(
            echoed_output(key),
            json!({ "Value": { "Fn::GetAtt": [CUSTOM, key] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            PROVIDER_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "record-the-lifecycle-request",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Action": "sqs:SendMessage",
                                "Effect": "Allow",
                                "Resource": recorder_arn,
                            }],
                        },
                    }],
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": { "ZipFile": PROVIDER_SOURCE },
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": [PROVIDER_ROLE, "Arn"] },
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            CUSTOM: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": { "Fn::GetAtt": [PROVIDER, "Arn"] },
                    "Recorder": recorder_url,
                    "IdPrefix": row.id_prefix,
                    "Published": Value::Object(published).to_string(),
                },
            },
        },
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Every request the provider has recorded into `url` so far, drained.
async fn recorded(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answered = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answered["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let body = message["Body"].as_str().unwrap_or_default();
                    serde_json::from_str(body).unwrap_or(Value::Null)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The record of the `request_type` request, once the provider has written one — the CONDITION
/// this waits on, under a deadline far longer than the spans this emulator gives these
/// transitions.
async fn awaited_record(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    request_type: &str,
) -> Value {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        for record in recorded(router, case, url).await {
            if record["RequestType"] == json!(request_type) {
                return record;
            }
            seen.push(record);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the provider to have recorded a {request_type:?} request, \
             got {seen:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `DescribeStacks` for `stack` until CloudFormation holds no stack under that name, and
/// answer the refusal it gave.
async fn refused_describe(router: &MultiServiceRouter, case: &str, stack: &str) -> (u16, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        if answered.status != 200 {
            return (answered.status, answered.text());
        }
        let body = answered.text();
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected CloudFormation to hold no stack {stack:?} after its \
             deletion, it still reports {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Whether `url` is PRE-SIGNED: its query carries a signature and the credential that signature
/// was made with, in either of the two forms a query-authenticated AWS URL states them in —
/// SigV4's `X-Amz-Signature`/`X-Amz-Credential` or SigV2's `Signature`/`AWSAccessKeyId`.
fn presigned(url: &str) -> bool {
    let Some(query) = url.split('?').nth(1) else {
        return false;
    };
    let names: Vec<&str> = query
        .split('&')
        .filter_map(|pair| pair.split('=').next())
        .collect();
    let carries = |name: &str| names.contains(&name);
    (carries("X-Amz-Signature") && carries("X-Amz-Credential"))
        || (carries("Signature") && carries("AWSAccessKeyId"))
}

/// Whether `id` is an id the RESPONDER minted: the prefix its own declaration gave it, and the
/// hex digits it appended to that prefix itself.
fn minted_under(id: &str, prefix: &str) -> bool {
    let Some(suffix) = id.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == MINTED_DIGITS && suffix.chars().all(|digit| digit.is_ascii_hexdigit())
}

/// Drive one row's whole lifecycle: the create, every claim the completed stack makes about the
/// provider's answer, and the deletion that re-issues the request carrying the id the create
/// learned.
async fn lifecycle_of(row: &Row) {
    let case = row.case;
    let router = emulator();
    let (recorder_url, recorder_arn) = given_recorder(&router, case, row.recorder).await;
    let body = template(row, &recorder_url, &recorder_arn);

    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
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

    // 1. The create SETTLES, and settles where the provider's SUCCESS answer puts it.
    let (settled, described) = settled_status(&router, row.stack).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, row.stack).await;
        panic!(
            "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got {settled:?}; \
             the failures it reported: {failures:?}; last answer {described}"
        );
    }

    // 2. The provider's answer IS the resource: the id it minted is what `Ref` resolves to and
    // what the stack records for the resource, and the `Data` it published is what each
    // `Fn::GetAtt` resolves against.
    let outputs = outputs_of(&described);
    let physical = outputs
        .get(PHYSICAL_ID_OUTPUT)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {PHYSICAL_ID_OUTPUT} output, got {outputs:?}"));
    assert!(
        minted_under(&physical, row.id_prefix),
        "case {case:?}: expected Ref to resolve to an id the responder minted under {:?} with \
         {MINTED_DIGITS} hex digits, got {physical:?}",
        row.id_prefix
    );
    for (key, value) in row.published {
        let read = outputs.get(&echoed_output(key)).map(String::as_str);
        assert_eq!(
            read,
            Some(*value),
            "case {case:?}: expected Fn::GetAtt {CUSTOM}.{key} to resolve to the Data the \
             provider published, got {read:?} in {outputs:?}"
        );
    }
    let resources =
        cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
    assert_eq!(
        resources.status,
        200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {} with {}",
        resources.status,
        resources.text()
    );
    let held = physical_ids_of(&resources.text());
    assert_eq!(
        held.get(CUSTOM),
        Some(&physical),
        "case {case:?}: expected the stack to record {physical:?} for {CUSTOM}, got {held:?}"
    );

    // 3. The request the provider was SENT, read out of the queue outside the stack: the create
    // names no physical resource yet, carries this row's own type, and its `ResponseURL` is
    // pre-signed.
    let create = awaited_record(&router, case, &recorder_url, "Create").await;
    assert_eq!(
        create["PhysicalResourceId"], json!(physical),
        "case {case:?}: expected the id the stack adopted to be the one the invocation minted, \
         got {create}"
    );
    assert_eq!(
        create["SentPhysicalResourceId"], Value::Null,
        "case {case:?}: expected the Create request to name no physical resource, got {create}"
    );
    assert_eq!(
        create["ResourceType"], json!(row.resource_type),
        "case {case:?}: expected the request to carry the declared type, got {create}"
    );
    assert_eq!(
        create["LogicalResourceId"], json!(CUSTOM),
        "case {case:?}: expected the request to name the logical resource, got {create}"
    );
    let create_url = create["ResponseURL"].as_str().unwrap_or_default().to_string();
    assert!(
        presigned(&create_url),
        "case {case:?}: expected the Create request's ResponseURL to be pre-signed, got \
         {create_url:?}"
    );

    // 4. The deletion re-issues the request to the SAME provider, carrying the id the create
    // learned, and leaves CloudFormation holding no stack under that name.
    let deleted = cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
    assert_eq!(
        deleted.status,
        200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {} with {}",
        deleted.status,
        deleted.text()
    );
    let (status, refusal) = refused_describe(&router, case, row.stack).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 reading the deleted stack, got {status} with {refusal}"
    );
    assert_eq!(
        element(&refusal, "Code"),
        VALIDATION_ERROR,
        "case {case:?}: expected the refusal to be {VALIDATION_ERROR}, got {refusal}"
    );
    assert_eq!(
        element(&refusal, "Message"),
        format!("Stack with id {} does not exist", row.stack),
        "case {case:?}: expected the refusal to state the stack does not exist, got {refusal}"
    );

    let delete = awaited_record(&router, case, &recorder_url, "Delete").await;
    assert_eq!(
        delete["SentPhysicalResourceId"], json!(physical),
        "case {case:?}: expected the Delete request to carry the id the create learned \
         ({physical:?}), got {delete}"
    );
    let delete_url = delete["ResponseURL"].as_str().unwrap_or_default().to_string();
    assert!(
        presigned(&delete_url),
        "case {case:?}: expected the Delete request's ResponseURL to be pre-signed, got \
         {delete_url:?}"
    );
    assert_ne!(
        delete_url, create_url,
        "case {case:?}: expected each request to be answered at its own URL, both were \
         {create_url:?}"
    );
}

#[tokio::test]
async fn the_generic_custom_resource_types_lifecycle_reaches_its_provider() {
    lifecycle_of(&ROWS[0]).await;
}

#[tokio::test]
async fn an_authored_custom_resource_types_lifecycle_reaches_its_provider() {
    lifecycle_of(&ROWS[1]).await;
}

#[tokio::test]
async fn a_provider_publishing_no_data_still_mints_the_resources_id() {
    lifecycle_of(&ROWS[2]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-signing reader, over the query forms a URL handed to a caller arrives in: both
    /// documented query-authentication schemes, each missing half of itself, a query carrying
    /// neither, and a URL with no query at all.
    #[test]
    fn the_presigning_reader_answers_for_each_query_form() {
        // (case, the URL, whether it is pre-signed)
        let rows: [(&str, &str, bool); 6] = [
            (
                "a SigV4 query-authenticated URL",
                "https://host/_aws/cloudformation/s|r|q?X-Amz-Algorithm=AWS4-HMAC-SHA256\
                 &X-Amz-Credential=AKID%2F20260101%2Fus-east-1%2Fcloudformation%2Faws4_request\
                 &X-Amz-Date=20260101T000000Z&X-Amz-Expires=3600&X-Amz-SignedHeaders=host\
                 &X-Amz-Signature=abc",
                true,
            ),
            (
                "a SigV2 query-authenticated URL",
                "https://host/path?AWSAccessKeyId=AKID&Expires=1&Signature=abc",
                true,
            ),
            (
                "a SigV4 query carrying no signature",
                "https://host/path?X-Amz-Credential=AKID%2F20260101",
                false,
            ),
            (
                "a SigV2 query carrying no access key",
                "https://host/path?Signature=abc",
                false,
            ),
            ("a query carrying neither", "https://host/path?a=b", false),
            ("no query at all", "https://host/path", false),
        ];

        for (case, url, expected) in rows {
            let read = presigned(url);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }

    /// The minted-id reader, over the id forms an answer can report: the prefix with exactly the
    /// digits the responder appends, one digit short, one too many, a suffix that is not hex, and
    /// an id under another prefix entirely.
    #[test]
    fn the_minted_id_reader_holds_an_id_to_its_prefix_and_digits() {
        // (case, the id, the prefix, whether the responder minted it)
        let rows: [(&str, &str, &str, bool); 5] = [
            ("the prefix and twelve hex digits", "p-0123456789ab", "p-", true),
            ("one digit short", "p-0123456789a", "p-", false),
            ("one digit too many", "p-0123456789abc", "p-", false),
            ("a suffix that is not hex", "p-0123456789az", "p-", false),
            ("an id under another prefix", "q-0123456789ab", "p-", false),
        ];

        for (case, id, prefix, expected) in rows {
            let read = minted_under(id, prefix);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }
}
