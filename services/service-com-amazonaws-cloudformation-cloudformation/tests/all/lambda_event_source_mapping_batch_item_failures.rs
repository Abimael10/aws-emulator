// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Lambda::EventSourceMapping` with
//! `FunctionResponseTypes: [ReportBatchItemFailures]` over a queue the same template declares,
//! end to end: the mapping the stack creates POLLS that queue, INVOKES the function with an
//! `aws:sqs` `Records` batch, and HONOURS the report the handler answers.
//!
//! RULE under test: **a mapping whose `FunctionResponseTypes` asks for its function's report of
//! which records of the batch failed deletes from its source exactly the records that report
//! leaves out, and keeps — so hands over again — exactly the records it names.**
//! `FunctionResponseTypes` is "a list of current response type enums applied to the event source
//! mapping", whose one value (`smithy_query(query="shape", shape="FunctionResponseType")`) is
//! `ReportBatchItemFailures`, and a record leaves a queue only once the invocation it was handed
//! to accounted for it. The declaration route is part of the rule: a resource of a type
//! `state::provisioning` creates is created in the service that OWNS it, so the mapping a
//! template declares is the mapping AWS Lambda holds — carrying the response types the template
//! asked for, reading the queue the template named.
//!
//! Four services have to agree for that to hold, and this suite drives all four through one
//! router over their own wires: CloudFormation provisions the queues, the role and the function
//! (archiving its inline source into the deployment package) and creates the mapping in Lambda;
//! IAM answers for the execution role the poller reads as; Amazon SQS holds the records and
//! their delivery bookkeeping; AWS Lambda polls, invokes and reads the report. Every
//! observation is one a client makes: the marker queue the handler writes each record it was
//! handed to, the source queue's own counts, and `ReceiveMessage` on the source once nothing
//! polls it.
//!
//! The rows differ along what the rule generalises over — WHICH records of the batch the report
//! names: none of them, one of three, every one of two, and two of five, with the declared
//! `BatchSize` sized to the row's own batch. Which records those are is decided by each body's
//! own text, so nothing in the deployed handler is keyed to a row's ids. Every expected value
//! is computed from what the row sent, except the ids Amazon SQS mints for its records and the
//! id AWS Lambda mints for the mapping, which are those services' to assign.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, settled_stack_status, xml_text, REGION};
use crate::stack_lifecycle::acknowledging_iam;

/// The `StackStatus` each row's stack has to settle on before anything is sent.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where `CreateEventSourceMapping` is bound (`/2015-03-31/event-source-mappings`), which is
/// also the collection ONE mapping is addressed inside.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The one value `FunctionResponseType` enumerates
/// (`smithy_query(query="shape", shape="FunctionResponseType")`), which is what a template asks
/// the partial-batch contract for.
const REPORT_BATCH_ITEM_FAILURES: &str = "ReportBatchItemFailures";

/// The `State` a mapping that polls its event source reports
/// (`EventSourceMappingConfiguration$State`).
const STATE_ENABLED: &str = "Enabled";

/// The `eventSource` every record of a queue-sourced event carries.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// Amazon SQS's `awsJson1_0` wire: the media type and the service shape its `X-Amz-Target`
/// names an operation under.
const SQS_JSON: &str = "application/x-amz-json-1.0";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// How long the source queue hides a record it handed a consumer, in seconds, and the mapped
/// function's own `Timeout`: Lambda takes a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`, and this is the span a record the
/// report named waits before it is handed over again.
const HOLD_SECONDS: i64 = 3;

/// How long an observation of work the CLOCK owes — a stack coming up, a mapping settling, an
/// invocation, a record handed over again — goes on looking before this suite calls it a hang,
/// and how long it hands the runtime back between two looks.
///
/// A wall-clock DEADLINE on a loop that waits for the CONDITION, generous slack over the spans
/// the services here choose: a row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The prefix of a body whose record the handler reports as failed — how each row chooses the
/// report its function answers without the deployed source carrying the row's own ids.
const FAILING_BODY_PREFIX: &str = "fail";

/// The placeholder [`FAILING_BODY_PREFIX`] is written into the deployed source at, so the rule
/// the handler answers its report by and the rule this suite expects of it are ONE value.
const FAILING_PREFIX_PLACEHOLDER: &str = "FAILING_PREFIX";

/// The handler every row deploys inline: it forwards each record it was handed to the marker
/// queue its environment names — addressed by NAME through Amazon SQS's own `GetQueueUrl`, so
/// the source carries nothing the deployment had to compute — and then answers the
/// `batchItemFailures` report naming the records whose body asks to fail.
const REPORTING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["MARKER_QUEUE"])["QueueUrl"]
    failures = []
    for record in event["Records"]:
        client.send_message(QueueUrl=url, MessageBody=json.dumps(record))
        if record["body"].startswith("FAILING_PREFIX"):
            failures.append({"itemIdentifier": record["messageId"]})
    return {"batchItemFailures": failures}
"#;

/// A router carrying CloudFormation and the three services this estate lives in, all on ONE
/// engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `declared_topic_policy_decides_the_alarm_action.rs` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn deployed_consumer_router() -> MultiServiceRouter {
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

/// One AWS Lambda call through the same router over its `restJson1` wire, with whatever status
/// it answered: the id the stack published is read back, and the mapping is deleted, through
/// Lambda's own API.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let http_request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed restJson1 request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    let answered: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (status, answered)
}

/// One row of the table: how the row's resources are named, and the bodies it sends as one
/// batch — every body whose text asks to fail being one the handler's report names.
struct Row {
    case: &'static str,
    slug: &'static str,
    bodies: &'static [&'static str],
}

/// The rows differ along WHICH records of the batch the report names: none of them, one of
/// three, every one of two, and two of five.
const ROWS: &[Row] = &[
    Row {
        case: "a report naming no record of the batch",
        slug: "esm-report-none",
        bodies: &["keep-one", "keep-two"],
    },
    Row {
        case: "a report naming one record of three",
        slug: "esm-report-one",
        bodies: &["keep-one", "fail-two", "keep-three"],
    },
    Row {
        case: "a report naming every record of the batch",
        slug: "esm-report-all",
        bodies: &["fail-one", "fail-two"],
    },
    Row {
        case: "a report naming two records of five",
        slug: "esm-report-two-of-five",
        bodies: &["keep-one", "fail-two", "keep-three", "fail-four", "keep-five"],
    },
];

/// Whether a body asks its record to be reported failed, which is the rule the deployed handler
/// answers its report by.
fn reported_failed(body: &str) -> bool {
    body.starts_with(FAILING_BODY_PREFIX)
}

/// The names one row's resources carry, computed from its slug so no two rows share one.
fn source_queue(row: &Row) -> String {
    format!("{}-source", row.slug)
}
/// See [`source_queue`].
fn marker_queue(row: &Row) -> String {
    format!("{}-markers", row.slug)
}
/// See [`source_queue`].
fn function_name(row: &Row) -> String {
    format!("{}-consumer", row.slug)
}
/// See [`source_queue`].
fn stack_name(row: &Row) -> String {
    format!("{}-stack", row.slug)
}

/// The template one row deploys: the source queue, the marker queue the handler reports to, the
/// execution role the poller and the handler act as, the function whose inline source is
/// archived into its deployment package, and the mapping that declares the partial-batch
/// contract over the queue the same template declares.
fn template(row: &Row) -> String {
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            // The poller's own reads of the source, the deletes a processed record earns, and
            // the writes the handler makes to the marker queue. Both queues belong to this same
            // stack, so the grant names the ACTIONS rather than resources the role would have
            // to be created after.
            "Action": [
                "sqs:ReceiveMessage",
                "sqs:DeleteMessage",
                "sqs:GetQueueAttributes",
                "sqs:GetQueueUrl",
                "sqs:SendMessage",
            ],
            "Resource": "*",
        }],
    });
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a declared SQS event source mapping reporting batch item failures",
        "Resources": {
            "Source": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": source_queue(row),
                    "VisibilityTimeout": HOLD_SECONDS,
                },
            },
            "Markers": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": marker_queue(row) },
            },
            "ConsumerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "consume-and-report",
                        "PolicyDocument": grant,
                    }],
                },
            },
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
                    // At most as long as the queue hides a record it handed over, which is what
                    // Lambda holds a queue mapping to.
                    "Timeout": HOLD_SECONDS,
                    "Environment": { "Variables": { "MARKER_QUEUE": marker_queue(row) } },
                    "Code": {
                        "ZipFile": REPORTING_HANDLER
                            .replace(FAILING_PREFIX_PLACEHOLDER, FAILING_BODY_PREFIX),
                    },
                },
            },
            "Trigger": {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "FunctionName": { "Ref": "Consumer" },
                    "EventSourceArn": { "Fn::GetAtt": ["Source", "Arn"] },
                    "BatchSize": row.bodies.len(),
                    "Enabled": true,
                    "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
                },
            },
        },
    })
    .to_string()
}

/// The physical id the stack published for one of its logical resources — for the mapping, "Ref
/// returns the event source mapping's ID", the id AWS Lambda assigned it.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource for {logical_id}, got \
         {status} with {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: no PhysicalResourceId for {logical_id} in {body}")
    })
}

/// The url of the queue called `name`, through Amazon SQS's own `GetQueueUrl` — the queue the
/// stack created, addressed the way a client addresses it.
async fn queue_url(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let read = sqs(router, case, "GetQueueUrl", &json!({ "QueueName": name })).await;
    read["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueUrl {name:?} reports no url, got {read}"))
        .to_string()
}

/// The ARN Amazon SQS itself reports for the queue at `url`.
async fn queue_arn(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    read["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue at {url} reports no QueueArn"))
        .to_string()
}

/// How many records the queue at `url` still holds, visible or in flight — which is what says
/// whether a record was deleted, whoever is holding it at this instant.
async fn records_left(router: &MultiServiceRouter, case: &str, url: &str) -> u64 {
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ]
    .iter()
    .map(|attribute| {
        read["Attributes"][attribute]
            .as_str()
            .and_then(|count| count.parse::<u64>().ok())
            .unwrap_or_default()
    })
    .sum()
}

/// Take whatever is waiting on the marker queue, deleting each message so a later look finds
/// only later markers, and answer the records the handler forwarded.
async fn take_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut records = Vec::with_capacity(messages.len());
    for message in &messages {
        let text = message["Body"].as_str().unwrap_or_default().to_string();
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        records.push(serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected a forwarded record as a JSON document, got {text:?} \
                 ({error})"
            )
        }));
    }
    records
}

/// The `ApproximateReceiveCount` a forwarded record carries, as a number — 0 when it carries
/// none, which fails the assertion that reads it.
fn receive_count(record: &Value) -> u64 {
    record["attributes"]["ApproximateReceiveCount"]
        .as_str()
        .and_then(|count| count.parse().ok())
        .unwrap_or_default()
}

/// The `messageId` a forwarded record names.
fn record_message_id(record: &Value) -> String {
    record["messageId"].as_str().unwrap_or_default().to_string()
}

/// Poll `GetEventSourceMapping` until Lambda reports the mapping `Enabled` — the state a mapping
/// that polls its source is in — and answer the body it last reported.
///
/// The wait is on that CONDITION, looking again until it holds or [`BUDGET`] passes.
async fn mapping_until_enabled(router: &MultiServiceRouter, case: &str, uuid: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetEventSourceMapping to resolve the id the stack \
             published, got {status} with {read}"
        );
        if read["State"].as_str() == Some(STATE_ENABLED) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported {STATE_ENABLED}, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take markers into `seen` until what has been seen satisfies `holds`, or [`BUDGET`] passes —
/// the one wait every observation of the handler's own reports goes through.
async fn await_markers(
    router: &MultiServiceRouter,
    case: &str,
    marker_url: &str,
    seen: &mut Vec<Value>,
    waited_for: &str,
    holds: &dyn Fn(&[Value]) -> bool,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        seen.extend(take_markers(router, case, marker_url).await);
        if holds(seen) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to report {waited_for}, it reported {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Read the source queue's own count until it stands at `wanted`, and answer what it last
/// reported — so a row that deleted too much or too little fails on the count it reached.
async fn await_records_left(
    router: &MultiServiceRouter,
    case: &str,
    source_url: &str,
    wanted: u64,
) -> u64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let left = records_left(router, case, source_url).await;
        if left == wanted || Instant::now() >= deadline {
            return left;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Receive from the source queue as an ordinary consumer until `wanted` distinct records have
/// been handed out, deleting each, and answer the ids it handed out.
///
/// With the mapping gone nothing else polls that queue, so what it hands out is what the report
/// kept. A row whose report named nothing asks for nothing and this answers at once.
async fn drain(
    router: &MultiServiceRouter,
    case: &str,
    source_url: &str,
    wanted: usize,
) -> BTreeSet<String> {
    let mut taken: BTreeSet<String> = BTreeSet::new();
    if wanted == 0 {
        return taken;
    }
    let deadline = Instant::now() + BUDGET;
    loop {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": source_url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            if let Some(id) = message["MessageId"].as_str() {
                taken.insert(id.to_string());
            }
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": source_url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
        if taken.len() >= wanted {
            return taken;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the queue to hand out {wanted} record(s), it handed out \
             {taken:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a declared `AWS::Lambda::EventSourceMapping` whose `FunctionResponseTypes` asks for
/// `ReportBatchItemFailures` polls the queue it names, invokes the function with an `aws:sqs`
/// records batch, and deletes exactly the records its function's `batchItemFailures` report
/// leaves out — keeping, and handing over again, exactly the ones it names.
#[tokio::test]
async fn a_declared_mapping_honours_the_batch_item_failures_its_function_reports() {
    let router = deployed_consumer_router();

    for row in ROWS {
        let case = row.case;
        let stack = stack_name(row);

        // 1. The stack deploys, and the mapping it declared is one AWS LAMBDA holds, under the
        //    id the stack published, carrying the response types the template asked for and
        //    reading the queue the template named.
        let (status, created) = call(
            &router,
            "CreateStack",
            &acknowledging_iam(&[
                member("StackName", &stack),
                member("TemplateBody", &template(row)),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to settle on {CREATE_COMPLETE}, got {settled:?}"
        );
        let uuid = physical_id(&router, case, &stack, "Trigger").await;
        let source_url = queue_url(&router, case, &source_queue(row)).await;
        let source_arn = queue_arn(&router, case, &source_url).await;
        let marker_url = queue_url(&router, case, &marker_queue(row)).await;

        let mapping = mapping_until_enabled(&router, case, &uuid).await;
        assert_eq!(
            mapping["FunctionResponseTypes"],
            json!([REPORT_BATCH_ITEM_FAILURES]),
            "case {case:?}: expected the declared response types to be the mapping's own, got \
             {mapping}"
        );
        assert_eq!(
            mapping["EventSourceArn"].as_str(),
            Some(source_arn.as_str()),
            "case {case:?}: expected the mapping to read the queue the template declared, got \
             {mapping}"
        );

        // 2. The row's batch, sent as ONE call, with the ids Amazon SQS minted for its records.
        let entries: Vec<Value> = row
            .bodies
            .iter()
            .enumerate()
            .map(|(index, body)| json!({ "Id": format!("e{index}"), "MessageBody": *body }))
            .collect();
        let sent = sqs(
            &router,
            case,
            "SendMessageBatch",
            &json!({ "QueueUrl": source_url, "Entries": entries }),
        )
        .await;
        let accepted = sent["Successful"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            accepted.len(),
            row.bodies.len(),
            "case {case:?}: expected every entry of the batch to be accepted, got {sent}"
        );
        // (body, the id the queue minted for it), in the order the row sent them.
        let ids: Vec<(&str, String)> = row
            .bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                let wanted = format!("e{index}");
                let entry = accepted
                    .iter()
                    .find(|entry| entry["Id"].as_str() == Some(wanted.as_str()))
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: no accepted entry for {wanted} in {sent}")
                    });
                let id = entry["MessageId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the accepted entry reports no MessageId: {entry}")
                    })
                    .to_string();
                (*body, id)
            })
            .collect();
        let named: BTreeSet<String> = ids
            .iter()
            .filter(|(body, _)| reported_failed(body))
            .map(|(_, id)| id.clone())
            .collect();
        let left_out: BTreeSet<String> = ids
            .iter()
            .filter(|(body, _)| !reported_failed(body))
            .map(|(_, id)| id.clone())
            .collect();

        // 3. The declared mapping really INVOKES: every record of the batch reaches the handler
        //    as an `aws:sqs` record of the queue the template declared, carrying the body sent.
        let mut seen: Vec<Value> = Vec::new();
        let every_record: BTreeSet<String> = ids.iter().map(|(_, id)| id.clone()).collect();
        await_markers(
            &router,
            case,
            &marker_url,
            &mut seen,
            "the whole batch it was handed",
            &|seen| {
                let delivered: BTreeSet<String> = seen.iter().map(record_message_id).collect();
                every_record.iter().all(|id| delivered.contains(id))
            },
        )
        .await;
        for (body, id) in &ids {
            let record = seen
                .iter()
                .find(|record| &record_message_id(record) == id)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no record of the message {id} in {seen:?}")
                });
            assert_eq!(
                record["body"].as_str(),
                Some(*body),
                "case {case:?}: expected the record to carry the body as sent, got {record}"
            );
            assert_eq!(
                record["eventSource"].as_str(),
                Some(SQS_EVENT_SOURCE),
                "case {case:?}: expected the record to name its event source, got {record}"
            );
            assert_eq!(
                record["eventSourceARN"].as_str(),
                Some(source_arn.as_str()),
                "case {case:?}: expected the record to name the declared queue, got {record}"
            );
        }

        // 4. The report is HONOURED: the records it left out are deleted, so the queue is left
        //    holding exactly the records it named.
        let left = await_records_left(&router, case, &source_url, named.len() as u64).await;
        assert_eq!(
            left,
            named.len() as u64,
            "case {case:?}: expected the queue to be left holding only the {} record(s) the \
             report named, it holds {left}",
            named.len()
        );

        // 5. Each record the report NAMED is handed over again, under a higher
        //    `ApproximateReceiveCount`; no record it left out ever is, which is what "deleted"
        //    means to a consumer.
        if !named.is_empty() {
            let expected = named.clone();
            await_markers(
                &router,
                case,
                &marker_url,
                &mut seen,
                "every record its report named a second time",
                &|seen| {
                    expected.iter().all(|id| {
                        seen.iter().any(|record| {
                            &record_message_id(record) == id && receive_count(record) > 1
                        })
                    })
                },
            )
            .await;
        }
        let handed_again: BTreeSet<String> = seen
            .iter()
            .filter(|record| receive_count(record) > 1)
            .map(record_message_id)
            .collect();
        for id in &left_out {
            assert!(
                !handed_again.contains(id),
                "case {case:?}: the record {id} the report left out was deleted, so it must not \
                 be handed over again, got {seen:?}"
            );
        }

        // 6. And with nothing polling the queue any more, an ordinary consumer is handed exactly
        //    the records the report named.
        let (status, deleted) =
            lambda(&router, "DELETE", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        assert_eq!(
            status, 202,
            "case {case:?}: DeleteEventSourceMapping answered {status} {deleted}"
        );
        let drained = drain(&router, case, &source_url, named.len()).await;
        assert_eq!(
            drained, named,
            "case {case:?}: expected the queue to hand out exactly the records the report named \
             {named:?}, got {drained:?}"
        );
    }
}
