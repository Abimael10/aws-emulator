// SPDX-License-Identifier: CC0-1.0

//! What a stack UPDATE that revises the resources AROUND an `AWS::Lambda::EventSourceMapping`
//! does to the mapping itself, end to end over the wires a client uses.
//!
//! RULE under test: **a stack update acts on a resource only where the update CHANGES that
//! resource's own declaration — a resource whose declared properties the revision restates
//! identically is a no-op, whatever the update does to the other resources of the same stack,
//! including the ones it reads and the one it is wired to.** That is what `UpdateStack`'s own
//! description means by comparing a stack's declarations against what is deployed, and it is
//! the only reading under which a resource whose owning service refuses a SECOND instance of it
//! survives a second deployment at all: `CreateEventSourceMapping` answers
//! `ResourceConflictException` (errorKind client, httpError 409 —
//! `smithy_query(query="operation", operation="CreateEventSourceMapping")`) for a mapping that
//! would wire an event source to a function that is already wired to it, so an update that
//! re-created an unchanged mapping could only ever roll the whole stack back.
//!
//! `minted_identity_survives_a_stack_update.rs` holds the same rule over revisions of the
//! MAPPING's own declaration (nothing, a property `UpdateEventSourceMapping` takes, and a
//! create-only one). This suite holds it over revisions of everything ELSE in the stack, which
//! is the shape a real second deployment has: a client changes its function, its queue or an
//! unrelated resource, never the trigger wiring them together.
//!
//! The rows differ along WHICH sibling the update revises — the function the mapping invokes,
//! the queue it reads, a resource it neither reads nor invokes, all of them at once, and all of
//! them beside a revision of the mapping's own batch size, which is the boundary where the same
//! rule asks the owning service for an update IN PLACE rather than for nothing.
//!
//! Every expected value is computed from what the row declared, except the mapping's `UUID`,
//! which AWS Lambda mints and which is only ever compared with what that service reported
//! earlier.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, settled_stack, xml_text, REGION};
use crate::stack_lifecycle::acknowledging_iam;

/// The `StackStatus` each row's stack has to settle on, before and after its update.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// Where the event source mapping operations are bound
/// (`POST /2015-03-31/event-source-mappings`), which is also the collection ONE mapping is
/// addressed inside and the list the mappings of one function are read from.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// Amazon SQS's `awsJson1_0` wire: the media type and the service shape its `X-Amz-Target`
/// names an operation under.
const SQS_JSON: &str = "application/x-amz-json-1.0";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// How long an observation of work the CLOCK owes — a mapping settling after a change — goes on
/// looking before this suite calls it a hang, and how long it hands the runtime back between two
/// looks.
///
/// A wall-clock DEADLINE on a loop that waits for the CONDITION, with generous slack over the
/// spans the services here choose: a row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A router carrying CloudFormation and the services that own what these templates declare, all
/// on ONE engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `lambda_event_source_mapping_batch_item_failures.rs` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn estate() -> MultiServiceRouter {
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
/// it; it must answer 200, since every call here is one these observations depend on.
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

/// One AWS Lambda read through the same router over its `restJson1` wire: the status it answered
/// and the document it carried.
async fn lambda(router: &MultiServiceRouter, path: &str) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("GET")
        .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
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

/// Every mapping AWS Lambda holds for one function, through its own `ListEventSourceMappings` —
/// which is what says whether a deployment left a SECOND mapping on the same function.
async fn mappings_of(router: &MultiServiceRouter, case: &str, function: &str) -> Vec<Value> {
    let (status, listed) = lambda(router, &format!("{MAPPINGS_URI}?FunctionName={function}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListEventSourceMappings, got {status} with \
         {listed}"
    );
    listed["EventSourceMappings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The mapping AWS Lambda holds under `uuid`, once it is no longer in a transitional state: a
/// change the update applied is in force only once that service reports the mapping settled, so
/// the row looks again until it is.
async fn settled_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) -> Value {
    let settled = ["Enabled", "Disabled"];
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, mapping) = lambda(router, &format!("{MAPPINGS_URI}/{uuid}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetEventSourceMapping {uuid}, got {status} \
             with {mapping}"
        );
        let state = mapping["State"].as_str().unwrap_or_default().to_string();
        if settled.contains(&state.as_str()) || Instant::now() >= deadline {
            return mapping;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `Timeout` AWS Lambda holds for the function, through `GetFunctionConfiguration`.
async fn function_timeout(router: &MultiServiceRouter, case: &str, function: &str) -> i64 {
    let (status, configuration) =
        lambda(router, &format!("/2015-03-31/functions/{function}/configuration")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetFunctionConfiguration, got {status} with \
         {configuration}"
    );
    configuration["Timeout"].as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: {function} reports no Timeout, got {configuration}")
    })
}

/// The `VisibilityTimeout` Amazon SQS holds for the queue called `name`, read the way a client
/// reads it.
async fn queue_visibility(router: &MultiServiceRouter, case: &str, name: &str) -> i64 {
    let located = sqs(router, case, "GetQueueUrl", &json!({ "QueueName": name })).await;
    let url = located["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueUrl {name:?} reports no url, got {located}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["VisibilityTimeout"] }),
    )
    .await;
    attributes["Attributes"]["VisibilityTimeout"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no VisibilityTimeout"))
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {name:?} reports a non-numeric VisibilityTimeout"))
}

/// One revision of a row's template: the values its four declarations state. Every one of them
/// is a property the owning service's own update call takes, so a revision of any of them is a
/// change applied IN PLACE rather than a replacement.
struct Revision {
    /// The mapped function's own `Timeout`, which `UpdateFunctionConfiguration` takes.
    function_timeout: i64,
    /// The `VisibilityTimeout` of the queue the mapping reads, which `SetQueueAttributes` takes.
    source_visibility: i64,
    /// The same, for the queue the mapping neither reads nor invokes.
    spare_visibility: i64,
    /// The mapping's own `BatchSize`, which `UpdateEventSourceMapping` takes.
    batch_size: i64,
}

/// The declaration the FIRST deployment of every row carries.
///
/// AWS Lambda takes a queue mapping only while the queue's `VisibilityTimeout` is at least the
/// mapped function's `Timeout`, so every revision below keeps the source's above the function's.
const DEPLOYED: Revision = Revision {
    function_timeout: 3,
    source_visibility: 30,
    spare_visibility: 30,
    batch_size: 2,
};

/// The template one deployment of a row carries: the queue the mapping reads, a queue nothing
/// reads, the execution role the poller acts as, the function the mapping invokes, and the
/// mapping — whose own declaration names the other two resources and is restated identically by
/// every revision but the last row's.
fn template(slug: &str, revision: &Revision) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Source": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{slug}-source"),
                    "VisibilityTimeout": revision.source_visibility,
                },
            },
            "Spare": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{slug}-spare"),
                    "VisibilityTimeout": revision.spare_visibility,
                },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{slug}-role"),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "read-the-source",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "sqs:ReceiveMessage",
                                    "sqs:DeleteMessage",
                                    "sqs:GetQueueAttributes",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Worker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{slug}-worker"),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Timeout": revision.function_timeout,
                    "Code": { "ZipFile": "def handler(event, context):\n    return {}\n" },
                },
            },
            "Trigger": {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "FunctionName": { "Ref": "Worker" },
                    "EventSourceArn": { "Fn::GetAtt": ["Source", "Arn"] },
                    "BatchSize": revision.batch_size,
                    "Enabled": true,
                },
            },
        },
    })
    .to_string()
}

/// One row: what its update revises, and the names its resources take.
struct Row {
    case: &'static str,
    slug: &'static str,
    revision: Revision,
}

/// The rows: WHICH sibling of the mapping the update revises, up to and including a revision of
/// the mapping's own batch size beside them.
const ROWS: &[Row] = &[
    Row {
        case: "an update that revises the function the mapping invokes",
        slug: "esm-sibling-worker",
        revision: Revision {
            function_timeout: 7,
            ..DEPLOYED
        },
    },
    Row {
        case: "an update that revises the queue the mapping reads",
        slug: "esm-sibling-source",
        revision: Revision {
            source_visibility: 45,
            ..DEPLOYED
        },
    },
    Row {
        case: "an update that revises a resource the mapping neither reads nor invokes",
        slug: "esm-sibling-spare",
        revision: Revision {
            spare_visibility: 45,
            ..DEPLOYED
        },
    },
    Row {
        case: "an update that revises every sibling of the mapping at once",
        slug: "esm-sibling-estate",
        revision: Revision {
            function_timeout: 9,
            source_visibility: 60,
            spare_visibility: 60,
            batch_size: DEPLOYED.batch_size,
        },
    },
    Row {
        case: "an update that revises the mapping's own batch size beside its siblings",
        slug: "esm-sibling-and-batch",
        revision: Revision {
            function_timeout: 5,
            source_visibility: 50,
            spare_visibility: 50,
            batch_size: 4,
        },
    },
];

/// Deploy one revision of a row's template and answer the `StackStatus` the stack settled on,
/// with the reason it carried.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    action: &str,
    revision: &Revision,
    slug: &str,
) -> (String, String) {
    let (status, body) = call(
        router,
        action,
        &acknowledging_iam(&[
            member("StackName", stack),
            member("TemplateBody", &template(slug, revision)),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    let described = settled_stack(case, router, REGION, stack).await;
    (
        xml_text(&described, "StackStatus").unwrap_or_default(),
        xml_text(&described, "StackStatusReason").unwrap_or_default(),
    )
}

#[tokio::test]
async fn an_update_of_the_siblings_leaves_the_unchanged_mapping_alone() {
    for row in ROWS {
        let case = row.case;
        let router = estate();
        let stack = format!("{}-stack", row.slug);
        let function = format!("{}-worker", row.slug);

        // 1. The stack deploys, and the mapping it declared is one AWS LAMBDA holds, under the
        //    id the stack published.
        let (settled, why) = deployed(
            &router,
            case,
            &stack,
            "CreateStack",
            &DEPLOYED,
            row.slug,
        )
        .await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to settle on {CREATE_COMPLETE}, got {settled:?} \
             because {why:?}"
        );
        let minted = physical_id(&router, case, &stack, "Trigger").await;
        let held = mappings_of(&router, case, &function).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected the create to leave AWS Lambda exactly one mapping on \
             {function}, got {held:?}"
        );

        // 2. The update the row revises, which restates the mapping's own declaration
        //    identically in every row but the last.
        let (settled, why) = deployed(
            &router,
            case,
            &stack,
            "UpdateStack",
            &row.revision,
            row.slug,
        )
        .await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to settle on {UPDATE_COMPLETE}, got {settled:?} \
             because {why:?}"
        );

        // 3. The mapping is the SAME one: the id AWS Lambda minted carried forward, and that
        //    service still holding exactly one mapping on the function — nothing was created a
        //    second time, so nothing could be refused as a conflict.
        let after = physical_id(&router, case, &stack, "Trigger").await;
        assert_eq!(
            after, minted,
            "case {case:?}: expected the stack to keep the mapping id AWS Lambda minted \
             ({minted:?}), got {after:?}"
        );
        let held = mappings_of(&router, case, &function).await;
        let uuids: Vec<&str> = held
            .iter()
            .filter_map(|mapping| mapping["UUID"].as_str())
            .collect();
        assert_eq!(
            uuids,
            vec![minted.as_str()],
            "case {case:?}: expected AWS Lambda to hold only the mapping the create made, got \
             {held:?}"
        );

        // 4. ...and the update really applied what it revised, in the services that own it: the
        //    row's own values, read back through their own APIs.
        let mapping = settled_mapping(&router, case, &minted).await;
        let batch = mapping["BatchSize"].as_i64().unwrap_or_default();
        assert_eq!(
            batch, row.revision.batch_size,
            "case {case:?}: expected the declared BatchSize in force, got {batch} in {mapping}"
        );
        let timeout = function_timeout(&router, case, &function).await;
        assert_eq!(
            timeout, row.revision.function_timeout,
            "case {case:?}: expected the declared function Timeout in force, got {timeout}"
        );
        let source = queue_visibility(&router, case, &format!("{}-source", row.slug)).await;
        assert_eq!(
            source, row.revision.source_visibility,
            "case {case:?}: expected the declared source VisibilityTimeout in force, got {source}"
        );
        let spare = queue_visibility(&router, case, &format!("{}-spare", row.slug)).await;
        assert_eq!(
            spare, row.revision.spare_visibility,
            "case {case:?}: expected the declared spare VisibilityTimeout in force, got {spare}"
        );
    }
}
