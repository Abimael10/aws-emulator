// SPDX-License-Identifier: CC0-1.0

//! Which of a stack's resources a drift read reports at all, end to end over the awsQuery
//! wire.
//!
//! **The rule under test.** Drift detection is supported per resource TYPE, and
//! CloudFormation publishes which types it supports — "resources that don't currently
//! support drift detection can't be checked". So a resource whose type is OUTSIDE that
//! published support is never part of a drift result, however completely the service that
//! owns it reports its configuration back: a whole-stack `DetectStackDrift` leaves it out of
//! `DescribeStackResourceDrifts` entirely rather than reporting any status for it, it counts
//! towards neither the stack's `StackDriftStatus` nor its `DriftedStackResourceCount`,
//! `DetectStackResourceDrift` for it is refused with `ValidationError` naming the type, and
//! the resource reads keep reporting it `NOT_CHECKED` with no check instant. Every resource
//! of a SUPPORTED type in the same stack is checked and reported as usual — including as
//! `MODIFIED` once its configuration really was changed outside the stack.
//!
//! The rows span the class the rule generalises over rather than one recorded case: the two
//! types this emulator provisions that are outside that published support, in two namespaces
//! and with two owning services — an AWS Lambda event source mapping, created in Lambda from
//! a template that wires a queue to a function, and an Amazon Bedrock knowledge base — each
//! declared beside resources of types the support DOES cover (an Amazon SQS queue, a Lambda
//! function, an IAM role), which the same detection has to check.
//!
//! Everything goes through one router carrying CloudFormation and the services that own
//! those resources on ONE engine, over each service's own wire — awsQuery for
//! CloudFormation, `awsJson1_0` for Amazon SQS, `restJson1` for AWS Lambda and Amazon
//! Bedrock Agent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_identity::{
    knowledge_base_resource, opensearch_storage, TITAN_G1,
};
use crate::harness::{call, create_stack, member, settled_stack_status, xml_text, REGION};
use crate::stack_lifecycle::{acknowledging_iam, list_members};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    drift_detection_status, resource_drift_status, DRIFT_STATUS_DRIFTED, DRIFT_STATUS_IN_SYNC,
};

/// The `StackStatus` every stack here has to settle on before anything is read of it.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Amazon SQS's `awsJson1_0` wire: the media type and the service shape its `X-Amz-Target`
/// names an operation under.
const SQS_JSON: &str = "application/x-amz-json-1.0";
/// See [`SQS_JSON`].
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// How long an observation of work the CLOCK owes — a stack coming up, a detection
/// concluding — goes on looking before this suite calls it a hang, and how long it hands the
/// runtime back between two looks.
///
/// A wall-clock DEADLINE on a loop that waits for the CONDITION, generous slack over the
/// spans this service chooses: a case fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How long the declared queue keeps a message for, in seconds, and the value an out-of-band
/// write changes it to: a declared property of a SUPPORTED type, so changing it outside the
/// stack is what makes that resource — and only it — drifted.
const DECLARED_RETENTION: i64 = 3600;
/// See [`DECLARED_RETENTION`].
const RETENTION_SET_OUTSIDE_THE_STACK: i64 = 7200;

/// The sentence CloudFormation answers for a resource whose type drift detection is not
/// supported for, with the type as its one variable part.
fn unsupported_sentence(resource_type: &str) -> String {
    format!("Drift detection is not supported for ResourceType {resource_type}")
}

/// A router carrying CloudFormation and the services that own this suite's resources, all on
/// ONE engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `lambda_event_source_mapping_batch_item_failures.rs` records: mounting Amazon SQS for
/// every suite of this target would change what the accounts those suites build hold.
fn drift_estate_router() -> MultiServiceRouter {
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
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Deploy a template and wait for the create to CONCLUDE, polling the way a client's waiter
/// does.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = create_stack(router, stack, template, &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack}) answered {status} with {body}"
    );
    let settled = settled_stack_status(case, router, REGION, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected {stack} to create cleanly, got {settled:?}"
    );
}

/// What one whole-stack drift detection concluded with: its `StackDriftStatus` and its
/// `DriftedStackResourceCount`.
///
/// The detection is answered while it is still running and concludes at an instant this
/// service records, so this polls `DescribeStackDriftDetectionStatus` for the CONDITION — a
/// detection status that is no longer in progress — under [`BUDGET`].
async fn detect_whole_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String) {
    let (status, body) = call(router, "DetectStackDrift", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetectStackDrift({stack}) answered {status} with {body}"
    );
    let detection_id = xml_text(&body, "StackDriftDetectionId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackDriftDetectionId, got {body}"));

    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, body) = call(
            router,
            "DescribeStackDriftDetectionStatus",
            &[member("StackDriftDetectionId", &detection_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackDriftDetectionStatus answered {status} with {body}"
        );
        let reported = xml_text(&body, "DetectionStatus").unwrap_or_default();
        if reported != drift_detection_status::DETECTION_IN_PROGRESS {
            assert_eq!(
                reported,
                drift_detection_status::DETECTION_COMPLETE,
                "case {case:?}: expected the detection to complete, got {body}"
            );
            return (
                xml_text(&body, "StackDriftStatus").unwrap_or_default(),
                xml_text(&body, "DriftedStackResourceCount").unwrap_or_default(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the detection {detection_id} on {stack} never concluded"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `(logical id, StackResourceDriftStatus)` of every record
/// `DescribeStackResourceDrifts` reports for a stack, in wire order.
async fn reported_drifts(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResourceDrifts({stack}) answered {status} with {body}"
    );
    list_members(&body, "StackResourceDrifts")
        .into_iter()
        .map(|block| {
            (
                xml_text(&block, "LogicalResourceId").unwrap_or_default(),
                xml_text(&block, "StackResourceDriftStatus").unwrap_or_default(),
            )
        })
        .collect()
}

/// `DetectStackResourceDrift` for ONE resource of a stack.
async fn detect_one(router: &MultiServiceRouter, stack: &str, logical_id: &str) -> (u16, String) {
    call(
        router,
        "DetectStackResourceDrift",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await
}

/// `DescribeStackResource` for ONE resource of a stack, which must answer.
async fn describe_resource(
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
        "case {case:?}: DescribeStackResource({logical_id}) answered {status} with {body}"
    );
    body
}

/// The physical id the stack reports for one of its logical resources.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let body = describe_resource(router, case, stack, logical_id).await;
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: the stack reports no physical id for {logical_id}, got {body}")
    })
}

/// One Amazon SQS call over the `awsJson1_0` wire its model declares, which must answer: the
/// out-of-band write of this suite is a call a client makes.
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
    let answered: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {answered}"
    );
    answered
}

/// The handler the declared function carries: a worker that needs no input to be deployed,
/// since nothing here invokes it.
const WORKER: &str = r#"
def handler(event, context):
    return {"handled": len(event.get("Records", []))}
"#;

/// The template that wires a queue to a function through an event source mapping: the queue
/// and the function are types drift detection covers, the mapping is not.
fn queue_worker_template(slug: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a queue, a worker and the mapping that wires them",
        "Resources": {
            "Source": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{slug}-source"),
                    "MessageRetentionPeriod": DECLARED_RETENTION,
                },
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
                        "PolicyName": "consume",
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
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{slug}-consumer"),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
                    "Timeout": 3,
                    "MemorySize": 256,
                    "Code": { "ZipFile": WORKER },
                },
            },
            "Trigger": {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "FunctionName": { "Ref": "Consumer" },
                    "EventSourceArn": { "Fn::GetAtt": ["Source", "Arn"] },
                    "Enabled": true,
                },
            },
        },
    })
    .to_string()
}

/// The template that declares a knowledge base beside the role that reads for it: the role is
/// a type drift detection covers, the knowledge base is not.
fn knowledge_base_template(slug: &str) -> String {
    json!({
        "Resources": {
            "Access": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{slug}-access"),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "bedrock.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Escalation": knowledge_base_resource(
                Some(&format!("{slug}-escalation")),
                None,
                slug,
                TITAN_G1,
                opensearch_storage(slug, &format!("{slug}-index")),
            ),
        },
    })
    .to_string()
}

/// One row of the table: a stack carrying ONE resource of a type outside CloudFormation's
/// published drift-detection support, beside the resources of covered types the same
/// detection has to check.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the resource that cannot be checked, and its type.
    unsupported: (&'static str, &'static str),
    /// The logical ids of the resources that CAN be, each of which an untouched stack reports
    /// `IN_SYNC`.
    supported: &'static [&'static str],
    /// The template the row deploys, built from the row's own stack name.
    template: fn(&str) -> String,
}

/// The rows differ along the type that cannot be checked — the two this emulator provisions
/// that are outside that support, in two namespaces, owned by two different services — and
/// along what is declared beside it.
const ROWS: &[Row] = &[
    Row {
        case: "an event source mapping wiring a queue to a worker",
        stack: "drift-support-mapping",
        unsupported: ("Trigger", "AWS::Lambda::EventSourceMapping"),
        supported: &["Source", "ConsumerRole", "Consumer"],
        template: queue_worker_template,
    },
    Row {
        case: "a knowledge base beside the role that reads for it",
        stack: "drift-support-knowledge-base",
        unsupported: ("Escalation", "AWS::Bedrock::KnowledgeBase"),
        supported: &["Access"],
        template: knowledge_base_template,
    },
];

/// A resource of a type outside CloudFormation's published drift-detection support is in no
/// drift result at all: the whole-stack detection omits it, its own check is refused naming
/// the type, and it stays `NOT_CHECKED` — while every resource of a covered type beside it is
/// checked and reported.
#[tokio::test]
async fn a_type_outside_the_published_drift_support_is_in_no_drift_result() {
    for Row {
        case,
        stack,
        unsupported: (unsupported_id, unsupported_type),
        supported,
        template,
    } in ROWS
    {
        let router = drift_estate_router();
        deploy(&router, case, stack, &template(stack)).await;

        // The whole-stack detection: every covered resource reported, the unsupported one
        // absent, and the stack itself in sync because nothing was changed outside it.
        let (stack_status, drifted) = detect_whole_stack(&router, case, stack).await;
        assert_eq!(
            (stack_status.as_str(), drifted.as_str()),
            (DRIFT_STATUS_IN_SYNC, "0"),
            "case {case:?}: expected the untouched stack to be in sync with nothing drifted, \
             got {stack_status:?} and {drifted:?}"
        );
        let reported = reported_drifts(&router, case, stack).await;
        assert!(
            !reported
                .iter()
                .any(|(logical_id, _)| logical_id == unsupported_id),
            "case {case:?}: expected {unsupported_type} to be omitted from the drift report \
             entirely, got {reported:?}"
        );
        for logical_id in *supported {
            let got = reported
                .iter()
                .find(|(reported_id, _)| reported_id == logical_id)
                .map(|(_, status)| status.as_str());
            assert_eq!(
                got,
                Some(resource_drift_status::IN_SYNC),
                "case {case:?}: expected {logical_id} to be checked and reported in sync, got \
                 {reported:?}"
            );
        }

        // Its own check: refused, naming the type, in CloudFormation's own sentence.
        let (status, body) = detect_one(&router, stack, unsupported_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the check of {unsupported_type} to be refused, got \
             {status} with {body}"
        );
        assert_eq!(
            xml_text(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {case:?}: expected CloudFormation's ValidationError code, got {body}"
        );
        let expected = unsupported_sentence(unsupported_type);
        assert_eq!(
            xml_text(&body, "Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the refusal {expected:?}, got {body}"
        );

        // And nothing was recorded on it by either call.
        let detail = describe_resource(&router, case, stack, unsupported_id).await;
        let got = xml_text(&detail, "StackResourceDriftStatus");
        assert_eq!(
            got.as_deref(),
            Some(resource_drift_status::NOT_CHECKED),
            "case {case:?}: expected {unsupported_id} to be left unchecked, got {got:?}"
        );
        assert!(
            xml_text(&detail, "LastCheckTimestamp").is_none(),
            "case {case:?}: expected no check instant on {unsupported_id}, got {detail}"
        );
        // While a covered resource of the same stack carries the check that did run.
        for logical_id in *supported {
            let detail = describe_resource(&router, case, stack, logical_id).await;
            let got = xml_text(&detail, "StackResourceDriftStatus");
            assert_eq!(
                got.as_deref(),
                Some(resource_drift_status::IN_SYNC),
                "case {case:?}: expected the check of {logical_id} to be recorded, got {got:?}"
            );
        }
    }
}

/// The omission is not "nothing drifted": once a covered resource of the same stack really
/// was changed outside it, that resource is reported `MODIFIED` and the stack is `DRIFTED` —
/// and the mapping beside it is still in no drift result.
#[tokio::test]
async fn a_mapping_stays_out_of_the_report_a_changed_queue_is_in() {
    let case = "a queue changed outside the stack, beside the mapping that reads it";
    let stack = "drift-support-out-of-band";
    let router = drift_estate_router();
    deploy(&router, case, stack, &queue_worker_template(stack)).await;

    // The queue's physical id IS its url ("Ref returns the queue URL"), which is what a
    // client's own `SetQueueAttributes` addresses.
    let queue_url = physical_id(&router, case, stack, "Source").await;
    sqs(
        &router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": queue_url,
            "Attributes": {
                "MessageRetentionPeriod": RETENTION_SET_OUTSIDE_THE_STACK.to_string(),
            },
        }),
    )
    .await;

    let (stack_status, drifted) = detect_whole_stack(&router, case, stack).await;
    assert_eq!(
        (stack_status.as_str(), drifted.as_str()),
        (DRIFT_STATUS_DRIFTED, "1"),
        "case {case:?}: expected the stack to be drifted in exactly the changed queue, got \
         {stack_status:?} and {drifted:?}"
    );

    let reported = reported_drifts(&router, case, stack).await;
    let queue = reported
        .iter()
        .find(|(logical_id, _)| logical_id == "Source")
        .map(|(_, status)| status.as_str());
    assert_eq!(
        queue,
        Some(resource_drift_status::MODIFIED),
        "case {case:?}: expected the changed queue to be reported modified, got {reported:?}"
    );
    assert!(
        !reported.iter().any(|(logical_id, _)| logical_id == "Trigger"),
        "case {case:?}: expected the mapping to stay out of the drift report, got {reported:?}"
    );

    // The difference the report carries is the changed attribute itself, both values and all.
    let (status, body) = detect_one(&router, stack, "Source").await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetectStackResourceDrift(Source) answered {status} with {body}"
    );
    for (tag, wanted) in [
        ("PropertyPath", "/MessageRetentionPeriod".to_string()),
        ("ExpectedValue", DECLARED_RETENTION.to_string()),
        ("ActualValue", RETENTION_SET_OUTSIDE_THE_STACK.to_string()),
    ] {
        let got = xml_text(&body, tag);
        assert_eq!(
            got.as_deref(),
            Some(wanted.as_str()),
            "case {case:?}: expected {tag} to be {wanted:?}, got {body}"
        );
    }
}
