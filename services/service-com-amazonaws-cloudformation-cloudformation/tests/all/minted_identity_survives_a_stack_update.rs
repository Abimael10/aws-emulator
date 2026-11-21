// SPDX-License-Identifier: CC0-1.0

//! What a stack UPDATE does to a resource whose physical id AWS LAMBDA minted, end to end over
//! the wires a client uses.
//!
//! The rule every row here holds the service to: **a resource the stack already has, whose
//! re-declaration states every property the resource cannot take a new value for identically,
//! is that SAME resource — so the update carries the identifier its owning service minted
//! forward and asks that service to create nothing; only a declaration that changes one of
//! those create-only properties asks for a new resource, which the update replaces.** Such an
//! identifier is not a function of the template (AWS Lambda mints an event source mapping's
//! `UUID`, and the version number in a layer version's ARN), so a deployment cannot recompute
//! it: the record in force is the only place it comes from, and a re-declaration read as a
//! different resource would be CREATED all over again — which the owning service answers with
//! its own conflict for a mapping that already wires the same source to the same function.
//!
//! `minted_identity_across_updates.rs` holds the same rule over a type ANOTHER service mints an
//! identifier for; this suite drives the two AWS Lambda types this emulator creates under one.
//!
//! The rows span what the rule generalises over — WHAT the update revises: nothing about any
//! resource at all (the stack's own `Description`), a property the owning service's update call
//! takes (an event source mapping's `BatchSize`), and a create-only property (the event source
//! the mapping reads, a layer version's content), which is the boundary where the same rule
//! asks for a NEW resource.
//!
//! Every expected value is computed from what the row declared, except the two identifiers AWS
//! Lambda assigns — the mapping's UUID and the layer's version number — which are that
//! service's to mint and are only ever compared with what it reported earlier.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, settled_stack_status, xml_text, REGION};
use crate::hotswapped_function_code::package;
use crate::stack_lifecycle::acknowledging_iam;
use crate::stack_resource_provisioning::s3;

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

/// How long the queues hide a record they handed a consumer, in seconds, and the mapped
/// function's own `Timeout`: AWS Lambda takes a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`.
const HOLD_SECONDS: i64 = 30;
const FUNCTION_TIMEOUT_SECONDS: i64 = 3;

/// How long an observation of work the CLOCK owes — a mapping settling, a replaced resource
/// being taken down — goes on looking before this suite calls it a hang, and how long it hands
/// the runtime back between two looks.
///
/// A wall-clock DEADLINE on a loop that waits for the CONDITION, with generous slack over the
/// spans the services here choose: a row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The bucket the layer rows name their content in, and the two keys they name in it.
const CODE_BUCKET: &str = "layer-content-bucket";
const FIRST_CONTENT_KEY: &str = "first.zip";
const SECOND_CONTENT_KEY: &str = "second.zip";

/// A router carrying CloudFormation and the services that own what these templates declare,
/// all on ONE engine — the shape `core/router/src/lib.rs` mounts every service in.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
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
/// it answered: what the stack published is read back through Lambda's own API.
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

/// The physical id the stack published for one of its logical resources — for a mapping, "Ref
/// returns the event source mapping's ID", and for a layer version, the ARN the publish
/// answered.
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

/// The ARN Amazon SQS itself reports for the queue called `name`, read the way a client reads
/// it.
async fn queue_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let read = sqs(router, case, "GetQueueUrl", &json!({ "QueueName": name })).await;
    let url = read["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueUrl {name:?} reports no url, got {read}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no QueueArn"))
        .to_string()
}

/// Every mapping AWS Lambda holds for one function, through its own
/// `ListEventSourceMappings` — which is what says whether a deployment left a SECOND mapping
/// on the same function.
async fn mappings_of(router: &MultiServiceRouter, case: &str, function: &str) -> Vec<Value> {
    let (status, listed) = lambda(
        router,
        &format!("{MAPPINGS_URI}?FunctionName={function}"),
    )
    .await;
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

/// The UUIDs of those mappings, in the order Lambda listed them.
fn listed_uuids(mappings: &[Value]) -> Vec<String> {
    mappings
        .iter()
        .filter_map(|mapping| mapping["UUID"].as_str())
        .map(ToString::to_string)
        .collect()
}

/// Look again until the mappings AWS Lambda holds for `function` are exactly `wanted`, or
/// [`BUDGET`] passes — the wait a deployment that took a replaced mapping down owes, since
/// "when you delete an event source mapping, it enters a Deleting state and might not be
/// completely deleted for several seconds".
async fn mappings_until(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    wanted: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = listed_uuids(&mappings_of(router, case, function).await);
        if held == wanted || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until AWS Lambda answers `wanted` for `path`, or [`BUDGET`] passes, and answer
/// the last status it gave — the same wait for a resource a deployment took down.
async fn status_until(
    router: &MultiServiceRouter,
    path: &str,
    wanted: u16,
) -> (u16, Value) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, answered) = lambda(router, path).await;
        if status == wanted || Instant::now() >= deadline {
            return (status, answered);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The execution role every mapping row declares — the identity the poller reads the queue as.
fn role(slug: &str) -> Value {
    json!({
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
    })
}

/// One revision of a mapping row's template: the stack's own `Description`, the mapping's
/// declared `BatchSize`, and WHICH of the two queues the template declares the mapping reads.
struct Mapping {
    description: &'static str,
    batch_size: i64,
    reads: &'static str,
}

/// The template a mapping row deploys: two queues, the execution role, the function the
/// mapping invokes, and the mapping itself over the queue `revision.reads` names.
fn mapping_template(slug: &str, revision: &Mapping) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": revision.description,
        "Resources": {
            "Source": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{slug}-source"),
                    "VisibilityTimeout": HOLD_SECONDS,
                },
            },
            "Other": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{slug}-other"),
                    "VisibilityTimeout": HOLD_SECONDS,
                },
            },
            "Role": role(slug),
            "Worker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{slug}-worker"),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT_SECONDS,
                    "Code": { "ZipFile": "def handler(event, context):\n    return {}\n" },
                },
            },
            "Esm": {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "FunctionName": { "Ref": "Worker" },
                    "EventSourceArn": { "Fn::GetAtt": [revision.reads, "Arn"] },
                    "BatchSize": revision.batch_size,
                    "Enabled": true,
                },
            },
        },
    })
    .to_string()
}

/// One mapping row: what its update revises, and what the rule says that leaves.
struct Row {
    case: &'static str,
    slug: &'static str,
    /// The template the update submits.
    revision: Mapping,
    /// Whether the mapping the stack already has is the one the revision declares, so the
    /// identifier AWS Lambda minted for it carries forward.
    keeps_the_minted_id: bool,
}

/// The declaration the first deployment of every row carries.
const DEPLOYED: Mapping = Mapping {
    description: "the deployment the stack was created from",
    batch_size: 2,
    reads: "Source",
};

/// The rows: what an update revises, from nothing about any resource to the one property that
/// makes the declaration ask for a different mapping.
const ROWS: &[Row] = &[
    Row {
        case: "an update that revises nothing about any resource",
        slug: "esm-untouched",
        revision: Mapping {
            description: "the same resources, redeployed",
            batch_size: DEPLOYED.batch_size,
            reads: DEPLOYED.reads,
        },
        keeps_the_minted_id: true,
    },
    Row {
        case: "an update that changes a property UpdateEventSourceMapping takes",
        slug: "esm-rebatched",
        revision: Mapping {
            description: DEPLOYED.description,
            batch_size: 7,
            reads: DEPLOYED.reads,
        },
        keeps_the_minted_id: true,
    },
    Row {
        case: "an update that changes the event source, which that call does not take",
        slug: "esm-resourced",
        revision: Mapping {
            description: DEPLOYED.description,
            batch_size: DEPLOYED.batch_size,
            reads: "Other",
        },
        keeps_the_minted_id: false,
    },
];

/// RULE: an `AWS::Lambda::EventSourceMapping` the stack already has and whose re-declaration
/// states its create-only properties identically is the SAME mapping — the update carries the
/// `UUID` AWS Lambda minted forward, asks that service for no second mapping on the pair, and
/// the stack reaches `UPDATE_COMPLETE`; a declaration that states the event source differently
/// asks for a mapping of its own, which the update creates and whose predecessor it takes down.
#[tokio::test]
async fn an_update_keeps_the_mapping_identifier_aws_lambda_minted() {
    for row in ROWS {
        let case = row.case;
        let router = estate();
        let stack = format!("{}-stack", row.slug);
        let function = format!("{}-worker", row.slug);

        // 1. The stack deploys, and the mapping it declared is one AWS Lambda holds under the
        //    id the stack published, reading the queue the template named.
        let (status, created) = call(
            &router,
            "CreateStack",
            &acknowledging_iam(&[
                member("StackName", &stack),
                member("TemplateBody", &mapping_template(row.slug, &DEPLOYED)),
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
        let minted = physical_id(&router, case, &stack, "Esm").await;
        let source_arn = queue_arn(&router, case, &format!("{}-source", row.slug)).await;
        let other_arn = queue_arn(&router, case, &format!("{}-other", row.slug)).await;
        let held = mappings_until(&router, case, &function, std::slice::from_ref(&minted)).await;
        assert_eq!(
            held,
            vec![minted.clone()],
            "case {case:?}: expected AWS Lambda to hold exactly the mapping the stack \
             published, got {held:?}"
        );

        // 2. The update the row revises, which the stack has to carry out.
        let (status, updated) = call(
            &router,
            "UpdateStack",
            &acknowledging_iam(&[
                member("StackName", &stack),
                member("TemplateBody", &mapping_template(row.slug, &row.revision)),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
        );
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to settle on {UPDATE_COMPLETE}, got {settled:?}"
        );

        // 3. The identity in force: the minted one where the declaration asks for the same
        //    mapping, and one of its own where it does not.
        let in_force = physical_id(&router, case, &stack, "Esm").await;
        if row.keeps_the_minted_id {
            assert_eq!(
                in_force, minted,
                "case {case:?}: expected the stack to keep the mapping id AWS Lambda minted \
                 ({minted:?}), got {in_force:?}"
            );
        } else {
            assert_ne!(
                in_force, minted,
                "case {case:?}: expected the replaced mapping to carry an id of its own, got \
                 the one the first deployment minted ({minted:?})"
            );
        }

        // ...and AWS Lambda holds exactly ONE mapping for the function: the one now in force,
        // reading the queue the revision named and carrying the batch size it declared.
        let held =
            mappings_until(&router, case, &function, std::slice::from_ref(&in_force)).await;
        assert_eq!(
            held,
            vec![in_force.clone()],
            "case {case:?}: expected AWS Lambda to hold exactly the mapping in force \
             ({in_force:?}), got {held:?}"
        );
        let (status, mapping) = lambda(&router, &format!("{MAPPINGS_URI}/{in_force}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetEventSourceMapping to resolve the id in force, got \
             {status} with {mapping}"
        );
        let expected_arn = match row.revision.reads {
            "Other" => &other_arn,
            _ => &source_arn,
        };
        assert_eq!(
            mapping["EventSourceArn"].as_str(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the mapping to read {expected_arn:?}, got {mapping}"
        );
        assert_eq!(
            mapping["BatchSize"].as_i64(),
            Some(row.revision.batch_size),
            "case {case:?}: expected the declared batch size {}, got {mapping}",
            row.revision.batch_size
        );
    }
}

/// The two revisions of a layer row's template: the stack's own `Description` and the Amazon S3
/// key its `Content` reads the archive from.
struct Layer {
    description: &'static str,
    content_key: &'static str,
}

/// The template a layer row deploys: one layer version, published under the layer the row
/// names, out of the object its `Content` points at.
fn layer_template(slug: &str, revision: &Layer) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": revision.description,
        "Resources": {
            "Shared": {
                "Type": "AWS::Lambda::LayerVersion",
                "Properties": {
                    "LayerName": format!("{slug}-deps"),
                    "Content": { "S3Bucket": CODE_BUCKET, "S3Key": revision.content_key },
                    "CompatibleRuntimes": ["python3.12"],
                },
            },
        },
    })
    .to_string()
}

/// Stage the two archives the layer rows name, the way a client uploads them before deploying:
/// AWS Lambda READS the archive out of the object a `Content` names.
async fn stage_the_content(router: &MultiServiceRouter, case: &str) {
    let created = s3(router, "PUT", &format!("/{CODE_BUCKET}"), b"").await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: the content bucket must be created first, got {} {}",
        created.status,
        created.body
    );
    for (key, source) in [
        (FIRST_CONTENT_KEY, "shared = 'the first archive'\n"),
        (SECOND_CONTENT_KEY, "shared = 'the second archive'\n"),
    ] {
        let archive = package("python/shared.py", source);
        let staged = s3(router, "PUT", &format!("/{CODE_BUCKET}/{key}"), &archive).await;
        assert!(
            (200..300).contains(&staged.status),
            "case {case:?}: the archive {key} must be staged first, got {} {}",
            staged.status,
            staged.body
        );
    }
}

/// The layer rows: the same two revisions of the rule, over the other kind of identifier AWS
/// Lambda mints — the version number its publish allocated.
const LAYER_ROWS: &[(&str, &str, Layer, bool)] = &[
    (
        "an update that revises nothing about the layer",
        "layer-untouched",
        Layer {
            description: "the same layer, redeployed",
            content_key: FIRST_CONTENT_KEY,
        },
        true,
    ),
    (
        "an update that changes the content, which no call of that service edits",
        "layer-republished",
        Layer {
            description: "the deployment the stack was created from",
            content_key: SECOND_CONTENT_KEY,
        },
        false,
    ),
];

/// RULE, over the other type whose identifier AWS Lambda mints: an `AWS::Lambda::LayerVersion`
/// the stack already has and whose re-declaration states every property identically is the SAME
/// version — the update keeps the ARN that publish answered and publishes nothing; a
/// declaration that states its content differently asks for a version of its own, which the
/// update publishes and whose predecessor it takes down.
#[tokio::test]
async fn an_update_keeps_the_layer_version_aws_lambda_published() {
    for (case, slug, revision, keeps_the_minted_id) in LAYER_ROWS {
        let router = estate();
        stage_the_content(&router, case).await;
        let stack = format!("{slug}-stack");
        let deployed = Layer {
            description: "the deployment the stack was created from",
            content_key: FIRST_CONTENT_KEY,
        };

        let (status, created) = call(
            &router,
            "CreateStack",
            &[
                member("StackName", &stack),
                member("TemplateBody", &layer_template(slug, &deployed)),
            ],
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
        let published = physical_id(&router, case, &stack, "Shared").await;

        let (status, updated) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", &stack),
                member("TemplateBody", &layer_template(slug, revision)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
        );
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to settle on {UPDATE_COMPLETE}, got {settled:?}"
        );

        // The identity in force, and the version AWS Lambda really holds under it.
        let in_force = physical_id(&router, case, &stack, "Shared").await;
        let (layer, version) = split_version(case, &in_force);
        if *keeps_the_minted_id {
            assert_eq!(
                in_force, published,
                "case {case:?}: expected the stack to keep the version ARN AWS Lambda answered \
                 the publish with ({published:?}), got {in_force:?}"
            );
        } else {
            assert_ne!(
                in_force, published,
                "case {case:?}: expected the republished version to carry an ARN of its own, \
                 got the one the first deployment published ({published:?})"
            );
        }
        let (status, held) = lambda(
            &router,
            &format!("/2018-10-31/layers/{layer}/versions/{version}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AWS Lambda to hold the version in force, got {status} \
             {held}"
        );
        assert_eq!(
            held["LayerVersionArn"].as_str(),
            Some(in_force.as_str()),
            "case {case:?}: expected the version to report {in_force:?}, got {held}"
        );

        // ...and nothing beyond it: an unchanged declaration published no second version, and a
        // changed one left no first version standing.
        let (next_layer, next_version) = split_version(case, &published);
        let unwanted = if *keeps_the_minted_id {
            // The version a second publish under the same layer would have allocated.
            format!(
                "/2018-10-31/layers/{next_layer}/versions/{}",
                next_version + 1
            )
        } else {
            format!("/2018-10-31/layers/{next_layer}/versions/{next_version}")
        };
        let (status, answered) = status_until(&router, &unwanted, 404).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected AWS Lambda to hold no version at {unwanted:?}, got {status} \
             {answered}"
        );
    }
}

/// The layer and the version number one version ARN names — the ARN's own two trailing
/// segments, which is how both services spell a version.
fn split_version(case: &str, arn: &str) -> (String, u64) {
    let (head, version) = arn.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected a version ARN ending in a version number, got {arn:?}")
    });
    let layer = head.rsplit(':').next().unwrap_or_default();
    let version = version.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: expected a numeric version in {arn:?} ({error})")
    });
    (layer.to_string(), version)
}
