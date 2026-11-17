// SPDX-License-Identifier: CC0-1.0

//! Registering a scalable target RESOLVES the resource it scales in the service that owns it,
//! driven through the WHOLE assembled router.
//!
//! RULE: a scalable target is a capacity dimension OF A RESOURCE — "the resource that you want
//! to scale" — so `RegisterScalableTarget` resolves the `ResourceId` in the service that owns
//! that resource before it stores anything, and refuses a resource that service does not hold
//! with HTTP 400 `ValidationException`; nothing is stored, so `DescribeScalableTargets` lists
//! no such target. The refusal's message is the one Application Auto Scaling words for the
//! namespace: for most of them a sentence naming the resource kind and the resource id, and
//! where the owning service states what it could not find, that statement relayed as the
//! `Reason` of a validation envelope.
//!
//! Why this suite is HERE rather than beside the service's own: the resource is resolved by
//! CALLING the service that owns it, so the rule can only be driven on an emulator where those
//! services are mounted beside Application Auto Scaling — which is what `build_router`
//! assembles. A registration against an engine that does not hold the owning service resolves
//! nothing and is answered on its own terms; that is what the service's own suites exercise.
//!
//! The rows span the namespaces whose resource is resolved this way and the three wire
//! protocols those services speak (`awsQuery`, `ec2Query`, `awsJson1_1`, `restJson1`), each
//! with a resource that was never created — plus the other direction, a registration whose
//! resource the owning service DOES hold, which is accepted and reads back.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call is signed for, and the account every emulated ARN carries.
const REGION: &str = "us-east-1";
const ACCOUNT: &str = "000000000000";

/// The capacity window every row registers. Both bounds are above every documented floor and
/// the window is not inverted, so no row is refused for its capacities.
const MIN_CAPACITY: i64 = 1;
const MAX_CAPACITY: i64 = 4;

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
    /// The `__type` discriminator an awsJson error body carries, without its namespace.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
            .unwrap_or_default()
    }

    /// The `Message` an error body carries, whichever casing the protocol rendered it in.
    fn error_message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The `Authorization` header a call carries while the router authenticates nothing: the
/// credential scope, which is what names the target service and its region.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request through the router.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> Answer {
    let (parts, body) = request.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(body));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    Answer {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

/// One Application Auto Scaling call, the way a client makes one: an `awsJson1_1` POST naming
/// the operation in `X-Amz-Target`.
async fn application_auto_scaling(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> Answer {
    let host = format!("application-autoscaling.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AnyScaleFrontendService.{operation}"),
        )
        .header("authorization", credential_scope("application-autoscaling"))
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// One `awsQuery` call, the way a client makes one: a form body carrying the action, the API
/// version and the operation's parameters, each value percent-encoded through this test
/// crate's one encoder.
async fn query_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", credential_scope(signing_name))
        .body(body.into_bytes())
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// One `awsJson1_1` call to the service `signing_name` names, the way a client makes one.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", credential_scope(signing_name))
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// One `restJson1` read of the service `signing_name` names.
async fn rest_get(router: &MultiServiceRouter, signing_name: &str, path: &str) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("accept", "application/json")
        .header("authorization", credential_scope(signing_name))
        .body(Vec::new())
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// The read of the OWNING service that states what could not be found: the same question
/// Application Auto Scaling puts to it when it resolves the resource.
enum OwnerRead {
    /// An `awsJson1_1` operation of that service.
    Json {
        signing_name: &'static str,
        target_prefix: &'static str,
        operation: &'static str,
        body: Value,
    },
    /// A `restJson1` read of that service.
    RestGet {
        signing_name: &'static str,
        path: String,
    },
}

/// How the refusal's message is composed.
enum Expected {
    /// Application Auto Scaling's own sentence for the namespace, which this emulator words:
    /// the resource kind it scales and the resource id the request supplied.
    Sentence(String),
    /// The owning service's own statement, RELAYED as the `Reason` of the envelope that names
    /// the resource and the dimension. The row carries the read rather than that service's
    /// words, so the test asks that service the same question and expects its answer back.
    RelayedForResource(OwnerRead),
    /// The same relay, in the envelope that names no resource of its own.
    RelayedForTarget(OwnerRead),
}

/// One row of the catalog: a registration, and what the emulator must answer it with.
struct Row {
    case: &'static str,
    namespace: &'static str,
    resource_id: String,
    dimension: &'static str,
    /// The `ValidationException` message the registration is refused with.
    expected: Expected,
}

/// The sentence the owning service states for `read`, which must itself be a refusal: the
/// `Reason` the relaying refusal carries.
async fn owner_sentence(router: &MultiServiceRouter, case: &str, read: &OwnerRead) -> String {
    let answer = match read {
        OwnerRead::Json {
            signing_name,
            target_prefix,
            operation,
            body,
        } => json_call(router, signing_name, target_prefix, operation, body.clone()).await,
        OwnerRead::RestGet { signing_name, path } => rest_get(router, signing_name, path).await,
    };
    assert!(
        !(200..300).contains(&answer.status),
        "case {case:?}: expected the owning service to refuse the resource the registration \
         names, got {} {}",
        answer.status,
        answer.text
    );
    let sentence = answer.error_message();
    assert!(
        !sentence.is_empty(),
        "case {case:?}: expected the owning service to state why, got {}",
        answer.text
    );
    sentence
}

/// The message one row must be refused with.
async fn expected_message(router: &MultiServiceRouter, row: &Row) -> String {
    match &row.expected {
        Expected::Sentence(sentence) => sentence.clone(),
        Expected::RelayedForResource(read) => format!(
            "Validation failed for resource: {}, scalable dimension: {}. Reason: {}",
            row.resource_id,
            row.dimension,
            owner_sentence(router, row.case, read).await
        ),
        Expected::RelayedForTarget(read) => format!(
            "Validation failed for scalable target. Reason: {}",
            owner_sentence(router, row.case, read).await
        ),
    }
}

/// The catalog: one absent resource per namespace whose resource is resolved in the service
/// that owns it, spanning that service's own wire protocol and the two shapes the refusal
/// takes — Application Auto Scaling's own sentence, and the owning service's statement relayed
/// as the `Reason` of a validation envelope.
fn catalog() -> Vec<Row> {
    let comprehend_endpoint =
        format!("arn:aws:comprehend:{REGION}:{ACCOUNT}:document-classifier-endpoint/absent-doc");
    let msk_cluster = format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/absent-cluster/\
         11111111-1111-1111-1111-111111111111-1"
    );
    vec![
        Row {
            case: "an Aurora DB cluster Amazon RDS does not hold",
            namespace: "rds",
            resource_id: "cluster:absent-orders-db".to_string(),
            dimension: "rds:cluster:ReadReplicaCount",
            expected: Expected::Sentence(
                "RDS cluster doesn't exist: cluster:absent-orders-db".to_string(),
            ),
        },
        Row {
            case: "a DB cluster Amazon Neptune does not hold",
            namespace: "neptune",
            resource_id: "cluster:absent-graph".to_string(),
            dimension: "neptune:cluster:ReadReplicaCount",
            expected: Expected::Sentence(
                "Neptune cluster doesn't exist: cluster:absent-graph".to_string(),
            ),
        },
        Row {
            case: "a replication group Amazon ElastiCache does not hold",
            namespace: "elasticache",
            resource_id: "replication-group/absent-group".to_string(),
            dimension: "elasticache:replication-group:Replicas",
            expected: Expected::Sentence(
                "ElastiCache replication group doesn't exist: replication-group/absent-group"
                    .to_string(),
            ),
        },
        Row {
            case: "a cache cluster Amazon ElastiCache does not hold",
            namespace: "elasticache",
            resource_id: "cache-cluster/absent-cache".to_string(),
            dimension: "elasticache:cache-cluster:Nodes",
            expected: Expected::Sentence(
                "ElastiCache cache cluster doesn't exist: cache-cluster/absent-cache".to_string(),
            ),
        },
        Row {
            case: "a Spot Fleet request Amazon EC2 does not hold",
            namespace: "ec2",
            resource_id: "spot-fleet-request/sfr-11111111-1111-1111-1111-111111111111".to_string(),
            dimension: "ec2:spot-fleet-request:TargetCapacity",
            expected: Expected::Sentence(
                "Spot fleet request doesn't exist: \
                 spot-fleet-request/sfr-11111111-1111-1111-1111-111111111111"
                    .to_string(),
            ),
        },
        Row {
            case: "a fleet Amazon AppStream 2.0 does not hold",
            namespace: "appstream",
            resource_id: "fleet/absent-fleet".to_string(),
            dimension: "appstream:fleet:DesiredCapacity",
            expected: Expected::Sentence(
                "Appstream fleet doesn't exist: fleet/absent-fleet".to_string(),
            ),
        },
        Row {
            case: "an instance group Amazon EMR does not hold",
            namespace: "elasticmapreduce",
            resource_id: "instancegroup/j-ABSENTCLUSTER/ig-ABSENTGROUP".to_string(),
            dimension: "elasticmapreduce:instancegroup:InstanceCount",
            expected: Expected::Sentence(
                "EMR instance group doesn't exist: instancegroup/j-ABSENTCLUSTER/ig-ABSENTGROUP"
                    .to_string(),
            ),
        },
        Row {
            case: "an endpoint Amazon SageMaker does not hold, whose own statement is relayed",
            namespace: "sagemaker",
            resource_id: "endpoint/absent-endpoint/variant/AllTraffic".to_string(),
            dimension: "sagemaker:variant:DesiredInstanceCount",
            expected: Expected::RelayedForResource(OwnerRead::Json {
                signing_name: "sagemaker",
                target_prefix: "SageMaker",
                operation: "DescribeEndpoint",
                body: json!({"EndpointName": "absent-endpoint"}),
            }),
        },
        Row {
            case: "an inference component Amazon SageMaker does not hold",
            namespace: "sagemaker",
            resource_id: "inference-component/absent-component".to_string(),
            dimension: "sagemaker:inference-component:DesiredCopyCount",
            expected: Expected::RelayedForResource(OwnerRead::Json {
                signing_name: "sagemaker",
                target_prefix: "SageMaker",
                operation: "DescribeInferenceComponent",
                body: json!({"InferenceComponentName": "absent-component"}),
            }),
        },
        Row {
            case: "a cluster Amazon MSK does not hold, whose own statement is relayed",
            namespace: "kafka",
            resource_id: msk_cluster.clone(),
            dimension: "kafka:broker-storage:VolumeSize",
            expected: Expected::RelayedForTarget(OwnerRead::RestGet {
                signing_name: "kafka",
                // The cluster ARN is an `@httpLabel`, so it travels as ONE path segment:
                // its `:` and `/` are encoded through this test crate's one encoder.
                path: format!("/v1/clusters/{}", form_encode(&msk_cluster)),
            }),
        },
        Row {
            case: "an endpoint Amazon Comprehend does not hold, whose own statement is relayed",
            namespace: "comprehend",
            resource_id: comprehend_endpoint.clone(),
            dimension: "comprehend:document-classifier-endpoint:DesiredInferenceUnits",
            expected: Expected::RelayedForResource(OwnerRead::Json {
                signing_name: "comprehend",
                target_prefix: "Comprehend_20171127",
                operation: "DescribeEndpoint",
                body: json!({"EndpointArn": comprehend_endpoint}),
            }),
        },
    ]
}

/// Register one row's target.
async fn register(router: &MultiServiceRouter, row: &Row) -> Answer {
    application_auto_scaling(
        router,
        "RegisterScalableTarget",
        json!({
            "ServiceNamespace": row.namespace,
            "ResourceId": row.resource_id,
            "ScalableDimension": row.dimension,
            "MinCapacity": MIN_CAPACITY,
            "MaxCapacity": MAX_CAPACITY,
        }),
    )
    .await
}

/// The targets `DescribeScalableTargets` reports for one namespace.
async fn described(router: &MultiServiceRouter, namespace: &str) -> Vec<Value> {
    let answer = application_auto_scaling(
        router,
        "DescribeScalableTargets",
        json!({"ServiceNamespace": namespace}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {namespace:?}: expected DescribeScalableTargets to answer 200, got {} {}",
        answer.status, answer.text
    );
    answer.body["ScalableTargets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_registration_is_refused_where_the_owning_service_holds_no_such_resource() {
    std::thread::Builder::new()
        .name("owning-service-resource-resolution".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                router.set_request_authentication(false);

                for row in catalog() {
                    let case = row.case;
                    // What the refusal must say, settled BEFORE the registration: for the
                    // namespaces whose reason is relayed, this asks the owning service the
                    // same question and takes its own sentence, so the rule under test is the
                    // relay rather than another crate's wording copied in here.
                    let expected = expected_message(&router, &row).await;
                    let answer = register(&router, &row).await;
                    assert_eq!(
                        answer.status, 400,
                        "case {case:?}: expected HTTP 400, got {} {}",
                        answer.status, answer.text
                    );
                    assert_eq!(
                        answer.error_code(),
                        "ValidationException",
                        "case {case:?}: expected a ValidationException, got {}",
                        answer.text
                    );
                    assert_eq!(
                        answer.error_message(),
                        expected,
                        "case {case:?}: expected the refusal {expected:?}, got {:?}",
                        answer.error_message()
                    );

                    // A refused registration stores nothing, so the describe that follows it
                    // reports no target at all.
                    let listed = described(&router, row.namespace).await;
                    assert!(
                        listed.is_empty(),
                        "case {case:?}: expected a refused registration to persist nothing, got \
                         {listed:?}"
                    );
                }
            });
        })
        .expect("spawn the suite thread")
        .join()
        .expect("the suite thread completes");
}

#[test]
fn a_registration_whose_resource_the_owning_service_holds_is_accepted_and_reads_back() {
    std::thread::Builder::new()
        .name("owning-service-resource-accepted".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                router.set_request_authentication(false);

                // The resource the target scales, created in the service that OWNS it: an
                // Aurora DB cluster, through Amazon RDS' own API.
                let identifier = "orders-db";
                let created = query_call(
                    &router,
                    "rds",
                    "2014-10-31",
                    "CreateDBCluster",
                    &[
                        ("DBClusterIdentifier", identifier),
                        ("Engine", "aurora-postgresql"),
                    ],
                )
                .await;
                assert_eq!(
                    created.status, 200,
                    "case \"create the cluster\": expected CreateDBCluster to answer 200, got {} \
                     {}",
                    created.status, created.text
                );

                let row = Row {
                    case: "an Aurora DB cluster Amazon RDS holds",
                    namespace: "rds",
                    resource_id: format!("cluster:{identifier}"),
                    dimension: "rds:cluster:ReadReplicaCount",
                    expected: Expected::Sentence(String::new()),
                };
                let case = row.case;
                let answer = register(&router, &row).await;
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the registration to be accepted, got {} {}",
                    answer.status, answer.text
                );
                let arn = answer.body["ScalableTargetARN"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    arn.starts_with("arn:aws:application-autoscaling:"),
                    "case {case:?}: expected a ScalableTargetARN, got {}",
                    answer.text
                );

                // And it reads back through the describe, which is what a reconciliation loop
                // confirms the registration with.
                let listed = described(&router, row.namespace).await;
                assert_eq!(
                    listed.len(),
                    1,
                    "case {case:?}: expected exactly the registered target, got {listed:?}"
                );
                let target = &listed[0];
                for (member, want) in [
                    ("ResourceId", json!(row.resource_id)),
                    ("ScalableDimension", json!(row.dimension)),
                    ("ServiceNamespace", json!(row.namespace)),
                    ("MinCapacity", json!(MIN_CAPACITY)),
                    ("MaxCapacity", json!(MAX_CAPACITY)),
                    ("ScalableTargetARN", json!(arn)),
                ] {
                    assert_eq!(
                        target[member], want,
                        "case {case:?}: expected {member} {want:?}, got {target:?}"
                    );
                }

                // The same identity in another cluster's name is still resolved: the name the
                // request names is the one that is looked for, so a cluster this account does
                // not hold is refused even beside one it does.
                let absent = Row {
                    case: "another cluster, which Amazon RDS does not hold",
                    namespace: "rds",
                    resource_id: "cluster:other-db".to_string(),
                    dimension: "rds:cluster:ReadReplicaCount",
                    expected: Expected::Sentence(
                        "RDS cluster doesn't exist: cluster:other-db".to_string(),
                    ),
                };
                let expected = expected_message(&router, &absent).await;
                let answer = register(&router, &absent).await;
                assert_eq!(
                    answer.status, 400,
                    "case {:?}: expected HTTP 400, got {} {}",
                    absent.case, answer.status, answer.text
                );
                assert_eq!(
                    answer.error_message(),
                    expected,
                    "case {:?}: expected the refusal {expected:?}, got {:?}",
                    absent.case,
                    answer.error_message()
                );
                let listed = described(&router, absent.namespace).await;
                assert_eq!(
                    listed.len(),
                    1,
                    "case {:?}: expected the refused registration to persist nothing beside the \
                     accepted one, got {listed:?}",
                    absent.case
                );
            });
        })
        .expect("spawn the suite thread")
        .join()
        .expect("the suite thread completes");
}
