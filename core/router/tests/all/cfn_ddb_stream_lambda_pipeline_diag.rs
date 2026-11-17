// SPDX-License-Identifier: CC0-1.0

//! Why a declarative pipeline's deployment FAILED, read the way a client reads it: the stack's
//! own event stream.
//!
//! The RULE pinned here: **a stack whose create could not make one of its declared resources
//! records that resource's own failure as a stack event — `ResourceStatus` `CREATE_FAILED`
//! under the failing resource's `LogicalResourceId`, carrying the `ResourceStatusReason` the
//! owning service refused it with — before the stack settles in its terminal rollback status**.
//! `StackEvent$ResourceStatusReason` is "Success/failure message associated with the resource",
//! and a rollback is only ever the consequence of such a failure, so a stack that reports
//! `ROLLBACK_COMPLETE` and nothing else leaves a client with no way to tell WHICH declaration
//! the services would not take.
//!
//! The pipeline is the one `cfn_ddb_stream_lambda_pipeline.rs` deploys — a stream-enabled
//! DynamoDB table, the consumer's role, the consumer function and the event source mapping
//! between them — with ONE declaration the owning service must refuse. The rows differ along
//! WHICH declaration that is, and so along which service does the refusing: the mapping's event
//! source (AWS Lambda refuses a source it cannot resolve), the function's runtime (AWS Lambda
//! refuses a runtime it does not offer), and the table's key schema (Amazon DynamoDB refuses a
//! key naming an attribute the table does not define). Every other declaration of each row is
//! well formed, so what is observed is the one failure and not a template nothing could deploy.
//!
//! Every observation is one a client makes: `DescribeStacks` for the terminal status and
//! `DescribeStackEvents` for the per-resource reason.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, elements, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `StackStatus` a create reports while it runs, and the `ResourceStatus` a declaration the
/// owning service refused reports — both from the model's own enums.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids the template declares its resources under.
const TABLE: &str = "SourceTable";
const ROLE: &str = "ConsumerRole";
const FUNCTION: &str = "Consumer";
const MAPPING: &str = "StreamTrigger";

/// How long the wait for the create to finish may take before this suite calls it a hang, and
/// how long between two looks. Generous: a row fails only when the create never finished.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The five services this template spans, over ONE engine as `core/router/src/lib.rs` mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`], which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One row: which declaration the owning service must refuse.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the declaration that cannot be created.
    failing: &'static str,
    /// What that declaration asks for, merged over the well-formed one.
    broken: Value,
}

/// The template one row deploys: the pipeline, with `row`'s one declaration replaced.
fn template(row: &Row) -> String {
    let mut resources = json!({
        TABLE: {
            "Type": "AWS::DynamoDB::Table",
            "Properties": {
                "TableName": "diagnosable-source",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
            },
        },
        ROLE: {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": "diagnosable-consumer-role",
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }],
                },
            },
        },
        FUNCTION: {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": "diagnosable-consumer",
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                "Code": {"ZipFile": "def handler(event, context):\n    return event\n"},
            },
        },
        MAPPING: {
            "Type": "AWS::Lambda::EventSourceMapping",
            "Properties": {
                "EventSourceArn": {"Fn::GetAtt": [TABLE, "StreamArn"]},
                "FunctionName": {"Ref": FUNCTION},
                "StartingPosition": "TRIM_HORIZON",
                "BatchSize": 1,
            },
        },
    });
    resources[row.failing]["Properties"] = row.broken.clone();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
    })
    .to_string()
}

/// Poll `DescribeStacks` until the create has finished, in whatever status it finished in: the
/// transitions on the way are owed on the stack's own clock, so this waits for the CONDITION.
async fn past_the_create(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < DEADLINE {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer HTTP 200, got {status} with {body}"
        );
        if opt_element(&body, "StackStatus").is_some_and(|reported| reported != CREATE_IN_PROGRESS)
        {
            return body;
        }
        last = body;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the stack never left {CREATE_IN_PROGRESS} within {DEADLINE:?}: {last}")
}

/// RULE: the stack event stream names the declaration whose create failed and why, so the
/// rollback a client reads has a cause they can act on.
///
/// One test per row below, because each row deploys a whole stack of its own.
async fn the_failure_is_recorded(row: &Row) {
    {
        let case = row.case;
        let router = emulator();
        let body = template(row);
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer HTTP 200, got {status} with {answered}"
        );

        let described = past_the_create(&router, case, row.stack).await;
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_ne!(
            reached, CREATE_COMPLETE,
            "case {case:?}: a declaration the owning service refuses cannot leave the stack \
             {CREATE_COMPLETE}, got {described}"
        );

        let (status, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackEvents must answer HTTP 200, got {status} with {events}"
        );
        // The event that names the failing declaration: read from the member carrying BOTH its
        // logical id and its status, so no other resource's event can stand in for it.
        let failure = blocks_of(&events, "member").into_iter().find(|member| {
            elements(member, "LogicalResourceId")
                .first()
                .is_some_and(|logical_id| logical_id == row.failing)
                && elements(member, "ResourceStatus")
                    .first()
                    .is_some_and(|status| status == RESOURCE_CREATE_FAILED)
        });
        let failure = failure.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a {RESOURCE_CREATE_FAILED} event for {}, got {events}",
                row.failing
            )
        });
        let reason = elements(failure, "ResourceStatusReason")
            .into_iter()
            .next()
            .unwrap_or_default();
        assert!(
            !reason.trim().is_empty(),
            "case {case:?}: the {RESOURCE_CREATE_FAILED} event for {} must carry the reason the \
             owning service refused it, got {failure:?}",
            row.failing
        );
    }
}

/// The mapping's own event source refused: AWS Lambda cannot resolve a stream nothing created.
#[tokio::test]
async fn a_mapping_over_an_event_source_nothing_created_records_its_reason() {
    the_failure_is_recorded(&Row {
        case: "a mapping over an event source nothing created",
        stack: "diagnosable-source-missing",
        failing: MAPPING,
        broken: json!({
            "EventSourceArn":
                "arn:aws:dynamodb:us-west-2:000000000000:table/absent/stream/2026-01-01T00:00:00.000",
            "FunctionName": {"Ref": FUNCTION},
            "StartingPosition": "TRIM_HORIZON",
            "BatchSize": 1,
        }),
    })
    .await;
}

/// The function refused: AWS Lambda does not offer the runtime the declaration names.
#[tokio::test]
async fn a_function_asking_for_a_runtime_lambda_does_not_offer_records_its_reason() {
    the_failure_is_recorded(&Row {
        case: "a function asking for a runtime AWS Lambda does not offer",
        stack: "diagnosable-runtime-unknown",
        failing: FUNCTION,
        broken: json!({
            "FunctionName": "diagnosable-consumer",
            "Runtime": "python0.1",
            "Handler": "index.handler",
            "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
            "Code": {"ZipFile": "def handler(event, context):\n    return event\n"},
        }),
    })
    .await;
}

/// The table refused: Amazon DynamoDB will not key a table on an attribute it does not define.
#[tokio::test]
async fn a_table_keyed_on_an_undefined_attribute_records_its_reason() {
    the_failure_is_recorded(&Row {
        case: "a table keyed on an attribute it does not define",
        stack: "diagnosable-key-undefined",
        failing: TABLE,
        broken: json!({
            "TableName": "diagnosable-source",
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "absent", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
        }),
    })
    .await;
}
