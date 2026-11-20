// SPDX-License-Identifier: CC0-1.0

//! Which REGISTRY answers the image fetch of a launched Amazon ECS task, and what the task
//! reports once it has — driven end to end across the services that own the facts: Amazon
//! ECS, both Amazon ECR surfaces and IAM on one engine.
//!
//! The RULE under test: **a container runs an image, and the image is fetched from the
//! registry its reference names before the container starts — so the registry that HOLDS the
//! image is the one that decides whether the task runs at all, and the manifest digest that
//! registry resolved is what the container reports for the image it ran.** Two registries are
//! this emulator's to answer for: this account's private Amazon ECR registry, which Amazon
//! ECS pulls from as the task's execution role (`TaskDefinition$executionRoleArn` is "the task
//! execution role that grants the Amazon ECS container agent permission to make Amazon Web
//! Services API calls on your behalf"), and the one public gallery it serves, which answers
//! for the registry alias it was assigned — `Repository$repositoryUri` is "the URI for the
//! repository", built from "a universal prefix and the registry alias", so the alias is part
//! of the reference. A reference that resolves to nothing in either stops the task before it
//! starts: `Task$stopCode` becomes the `TaskStopCode` value for a task that failed before its
//! containers started, `Task$stoppedReason` ("the reason that the task was stopped") carries
//! the container runtime's `CannotPullContainerError` for that reference, `Task$startedAt`
//! ("the time when the task transitioned from the `PENDING` state to the `RUNNING` state")
//! stays absent, and `Container$imageDigest` ("the container image manifest digest") stays
//! absent because no manifest was fetched to digest. A reference naming a registry this
//! emulator does not hold — an alias the gallery was not assigned, which names another public
//! registry, or a bare name a container runtime resolves against Docker Hub — is nobody's here
//! to answer, so nothing is decided against it and the task reaches the end a placed task
//! reaches here, with no digest known for its image.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeTasks")` for `Task`'s
//! `stopCode`, `stoppedReason`, `startedAt` and `containers`;
//! `smithy_query(query="shape",shape="Container")` for `imageDigest`;
//! `smithy_query(query="shape",shape="TaskStopCode")` for the stop codes.
//!
//! The rows differ along what the rule generalises over: WHICH registry the reference names
//! (this account's private registry, the public gallery's own alias, an alias no gallery here
//! holds, no registry at all), WHAT that registry holds for it (the image, another tag, a
//! repository it does not hold) and HOW the reference addresses it (by tag or by digest).
//! Nothing here reaches into a state struct: every digest asserted is read back through the
//! OWNING registry's own `DescribeImages`, and every task member through `DescribeTasks`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them and of the private registry host the references name.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which is also its Amazon ECR registry id.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The cluster every task below is placed in, and the EC2 instance id of the one container
/// instance registered into it: "the EC2 launch type runs your tasks on Amazon EC2 instances
/// registered to your cluster", so a cluster with none has nowhere to place a task.
const CLUSTER: &str = "imagefetch";
const CAPACITY_INSTANCE_ID: &str = "i-0a1b2c3d4e5f60718";

/// `Task$lastStatus` of a task that has stopped, and the two `TaskStopCode` values the rows
/// tell apart: the stop of a task Amazon ECS could not launch the containers of, and the stop
/// of a task whose essential container ran and exited. Spelled out here rather than imported
/// from a service crate, so a wrong constant in the implementation cannot make this suite
/// pass.
const STOPPED: &str = "STOPPED";
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The repository of the PUBLIC gallery the rows pull from, and the tag it holds an image
/// under.
const GALLERY_REPOSITORY: &str = "library/alpine";
const GALLERY_TAG: &str = "3.19";

/// The repository of this account's PRIVATE Amazon ECR registry, and its one tag.
const PRIVATE_REPOSITORY: &str = "fetch/app";
const PRIVATE_TAG: &str = "1";

/// A digest no image of either registry is held under: a well-formed `sha256:<64 hex>`, which
/// is the other way a reference addresses an image.
const ABSENT_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// A reference into the public gallery under an alias this emulator's gallery was NOT
/// assigned: the gallery AWS publishes serves many registries, this emulator holds one, so a
/// reference into another names a registry it does not hold.
const FOREIGN_ALIAS_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// A bare name, which a container runtime resolves against a public registry this emulator
/// does not hold either.
const BARE_NAME_IMAGE: &str = "busybox:latest";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is whether a
/// registry holds an image and what digest it resolved, not which blobs an image is built
/// from — so the layer array the schema its media type names requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// How long this suite waits for a task to reach its terminal status, and how often it looks
/// again. Far above the spans a launch and a stop take, and the wait looks for the CONDITION
/// rather than sleeping a fixed stretch, so a loaded machine delays a row instead of breaking
/// it.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The sentence a container runtime words for a reference the registry resolves to nothing:
/// the manifest fetch is retried before the agent gives up, and the reference it could not
/// resolve is named twice — once as the reference the fetch asked for, and once as the
/// registry's answer about it. The retry count is the agent's own; what differs between two
/// such failures is the reference.
fn cannot_pull_not_found(image: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried 7 time(s): \
         failed to resolve ref {image}: {image}: not found"
    )
}

/// Amazon ECS, both Amazon ECR surfaces and IAM over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the fetch a launched task's containers need
/// is answered out of the registries' OWN stores, and the pull of a private image is decided
/// over the documents IAM holds for the task's execution role. Each service claims only
/// requests scoped to its own signing name, so no call below is routed any differently for
/// their presence.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id
/// and signature are not credentials: the router above runs with request authentication off,
/// so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and the parsed body of one response.
fn answered(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()))
    };
    (status, parsed)
}

/// One `X-Amz-Target` RPC call against the service whose signing name is `signing_name`, which
/// must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon ECS call (`awsJson1_1`, `AmazonEC2ContainerServiceV20141113.<Operation>`).
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await
}

/// One Amazon ECR call (`awsJson1_1`, `AmazonEC2ContainerRegistry_V20150921.<Operation>`).
async fn ecr(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        body,
    )
    .await
}

/// One Amazon ECR Public call (`awsJson1_1`, `SpencerFrontendService.<Operation>`).
async fn gallery(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "ecr-public",
        "SpencerFrontendService",
        operation,
        body,
    )
    .await
}

/// One IAM call: a query-protocol service, so `POST /` with the parameters form-encoded.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {rendered}"
    );
}

/// A string member of a body at `pointer`, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The execution role Amazon ECS pulls as, created through IAM's own API: trusted by this
/// service's task principal, and granted the calls a pull of a private image makes.
async fn execution_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "ecs-tasks.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "ecr:GetAuthorizationToken",
                "ecr:BatchGetImage",
                "ecr:GetDownloadUrlForLayer",
                "ecr:BatchCheckLayerAvailability",
            ],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "pull"),
            ("PolicyDocument", &grant),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// An EC2 instance identity document, as the agent reads it from the instance metadata
/// service, with the signature a registration presents beside it. Nothing here signs
/// anything: what a signature is OF cannot be checked without the key that produced it.
async fn register_capacity(router: &MultiServiceRouter, case: &str) {
    let document = json!({
        "accountId": ACCOUNT,
        "instanceId": CAPACITY_INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string();
    ecs(
        router,
        case,
        "RegisterContainerInstance",
        &json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": document,
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
}

/// Register one revision of `family` running `image` as `execution_role` and place one task
/// of it, answering the task's ARN.
async fn run(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    image: &str,
    execution_role: &str,
) -> String {
    ecs(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "executionRoleArn": execution_role,
            // A program that ENDS: a task is stopped as one whose essential container
            // exited only once that container really exits (`Container$exitCode` is "the
            // exit code returned from the container"), so a row that reads the stop of a
            // task that RAN gives its container something to run to the end. What the
            // program does is beside the point here — every row is about the image fetch.
            "containerDefinitions": [{
                "name": "app",
                "image": image,
                "memory": 128,
                "command": ["sh", "-c", "exit 0"],
            }],
        }),
    )
    .await;
    let placed = ecs(
        router,
        case,
        "RunTask",
        &json!({"cluster": CLUSTER, "taskDefinition": family}),
    )
    .await;
    member(case, &placed, "/tasks/0/taskArn")
}

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the call
/// that placed it.
async fn describe(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let body = ecs(
        router,
        case,
        "DescribeTasks",
        &json!({"cluster": CLUSTER, "tasks": [task_arn]}),
    )
    .await;
    body.pointer("/tasks/0")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// Look at one task until it reaches the terminal `STOPPED` status, and answer the record it
/// stopped as.
async fn stopped(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let task = describe(router, case, task_arn).await;
        if task["lastStatus"].as_str() == Some(STOPPED) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {STOPPED}, it was still {:?}: {task}",
            task["lastStatus"].as_str()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a row expects of the task it placed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    /// No registry here refused the image, so the task ran — and the container reports the
    /// digest the registry that served the pull resolved, or none when no registry here
    /// resolved one.
    Ran { image_digest: Option<String> },
    /// A registry this emulator holds resolved the reference to nothing, so the task never
    /// started.
    FailedToStart,
}

/// One row of the table: the revision it registers and what the task it places must report.
struct Row {
    case: &'static str,
    family: &'static str,
    image: String,
    expected: Expected,
}

/// One task the table placed, waiting to be read back.
struct Placed {
    case: &'static str,
    image: String,
    task_arn: String,
    expected: Expected,
}

/// RULE: the registry an image reference names decides the fetch a launched task's containers
/// need — a reference a registry this emulator HOLDS resolves to nothing stops the task
/// before it starts, with the stop code for a task that failed to start, the container
/// runtime's `CannotPullContainerError` for that reference, no `startedAt` and no
/// `imageDigest`; one it serves lets the task run and reports the manifest digest that
/// registry resolved; and a reference naming a registry this emulator does not hold decides
/// nothing, so the task runs with no digest known for its image.
#[tokio::test]
async fn the_registry_a_reference_names_decides_a_launched_tasks_image_fetch() {
    let router = router();
    let fixture = "the estate";

    // One repository in the PUBLIC gallery holding one image, pushed through that service's
    // own API, so the reference naming it is one the gallery really serves. The URI is the
    // gallery's own (`Repository$repositoryUri`), which is how a client learns the registry
    // alias to pull through.
    let published = gallery(
        &router,
        fixture,
        "CreateRepository",
        &json!({"repositoryName": GALLERY_REPOSITORY}),
    )
    .await;
    let gallery_uri = member(fixture, &published, "/repository/repositoryUri");
    let gallery_digest = member(
        fixture,
        &gallery(
            &router,
            fixture,
            "PutImage",
            &json!({
                "repositoryName": GALLERY_REPOSITORY,
                "imageManifest": MANIFEST,
                "imageTag": GALLERY_TAG,
            }),
        )
        .await,
        "/image/imageId/imageDigest",
    );
    // The gallery repository the registry does NOT hold, addressed under the same alias: the
    // alias is this registry's, so the reference is this registry's to answer.
    let alias_prefix = gallery_uri
        .rsplit_once('/')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_default();

    // One repository of this account's PRIVATE registry, holding one image under one tag.
    ecr(
        &router,
        fixture,
        "CreateRepository",
        &json!({"repositoryName": PRIVATE_REPOSITORY}),
    )
    .await;
    let private_digest = member(
        fixture,
        &ecr(
            &router,
            fixture,
            "PutImage",
            &json!({
                "repositoryName": PRIVATE_REPOSITORY,
                "imageManifest": MANIFEST,
                "imageTag": PRIVATE_TAG,
            }),
        )
        .await,
        "/image/imageId/imageDigest",
    );
    // Each digest is read back through the OWNING registry's own report of what it holds, so
    // the value a container is asserted to report is the registry's and not this suite's.
    let described = ecr(
        &router,
        fixture,
        "DescribeImages",
        &json!({"repositoryName": PRIVATE_REPOSITORY}),
    )
    .await;
    assert_eq!(
        described
            .pointer("/imageDetails/0/imageDigest")
            .and_then(Value::as_str),
        Some(private_digest.as_str()),
        "case {fixture:?}: expected the private registry to report the pushed digest, got \
         {described}"
    );

    let role = execution_role(&router, fixture, "imagefetchexecution").await;
    ecs(
        &router,
        fixture,
        "CreateCluster",
        &json!({"clusterName": CLUSTER}),
    )
    .await;
    register_capacity(&router, fixture).await;

    // A reference into THIS account's private Amazon ECR registry, which `addressed` ends
    // with the tag or digest form of: the host the Amazon ECR User Guide gives for a pull,
    // `<account>.dkr.ecr.<region>.amazonaws.com/<repository>[:<tag>|@<digest>]`.
    let private_image = |addressed: &str| {
        format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{PRIVATE_REPOSITORY}{addressed}")
    };
    let rows = [
        Row {
            case: "the private repository under the tag it holds",
            family: "fetchprivatetag",
            image: private_image(&format!(":{PRIVATE_TAG}")),
            expected: Expected::Ran {
                image_digest: Some(private_digest.clone()),
            },
        },
        Row {
            // The other form a reference addresses an image by, resolved through the same
            // lookup.
            case: "the private repository at the digest it holds",
            family: "fetchprivatedigest",
            image: private_image(&format!("@{private_digest}")),
            expected: Expected::Ran {
                image_digest: Some(private_digest.clone()),
            },
        },
        Row {
            case: "the private repository under a tag it does not hold",
            family: "fetchprivateabsenttag",
            image: private_image(":2"),
            expected: Expected::FailedToStart,
        },
        Row {
            case: "the private repository at a digest it does not hold",
            family: "fetchprivateabsentdigest",
            image: private_image(&format!("@{ABSENT_DIGEST}")),
            expected: Expected::FailedToStart,
        },
        Row {
            case: "a private repository the registry does not hold",
            family: "fetchprivateabsentrepo",
            image: format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/fetch/absent:1"),
            expected: Expected::FailedToStart,
        },
        Row {
            case: "the gallery repository under the tag it holds",
            family: "fetchgallerytag",
            image: format!("{gallery_uri}:{GALLERY_TAG}"),
            expected: Expected::Ran {
                image_digest: Some(gallery_digest.clone()),
            },
        },
        Row {
            case: "the gallery repository at the digest it holds",
            family: "fetchgallerydigest",
            image: format!("{gallery_uri}@{gallery_digest}"),
            expected: Expected::Ran {
                image_digest: Some(gallery_digest.clone()),
            },
        },
        Row {
            case: "the gallery repository under a tag it does not hold",
            family: "fetchgalleryabsenttag",
            image: format!("{gallery_uri}:4.0"),
            expected: Expected::FailedToStart,
        },
        Row {
            case: "a gallery repository the registry does not hold, under its own alias",
            family: "fetchgalleryabsentrepo",
            image: format!("{alias_prefix}/library/absent:{GALLERY_TAG}"),
            expected: Expected::FailedToStart,
        },
        Row {
            // The gallery AWS publishes serves many registries and this emulator holds one,
            // so an alias it was not assigned names a registry it does not hold: nothing is
            // decided, and no digest is known.
            case: "a gallery alias this registry was not assigned",
            family: "fetchforeignalias",
            image: FOREIGN_ALIAS_IMAGE.to_string(),
            expected: Expected::Ran { image_digest: None },
        },
        Row {
            case: "a bare name a container runtime resolves against another registry",
            family: "fetchbarename",
            image: BARE_NAME_IMAGE.to_string(),
            expected: Expected::Ran { image_digest: None },
        },
    ];

    // Every task is placed first and read back afterwards: each settles on its own clock, so
    // the whole table costs one run rather than one per row.
    let mut placed = Vec::new();
    for row in &rows {
        let task_arn = run(&router, row.case, row.family, &row.image, &role).await;
        placed.push(Placed {
            case: row.case,
            image: row.image.clone(),
            task_arn,
            expected: row.expected.clone(),
        });
    }

    for Placed {
        case,
        image,
        task_arn,
        expected,
    } in placed
    {
        let task = stopped(&router, case, &task_arn).await;
        // The fetch was ATTEMPTED either way, so both of its instants are reported either
        // way: `Task$pullStartedAt` is "the time when the container image pull began" and
        // `pullStoppedAt` "the time when the container image pull completed".
        for instant in ["pullStartedAt", "pullStoppedAt"] {
            assert!(
                !task[instant].is_null(),
                "case {case:?}: expected the attempted fetch's {instant}, got {task}"
            );
        }
        let container = task
            .pointer("/containers/0")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the task's container, got {task}"));
        let digest = container["imageDigest"].as_str().map(str::to_string);
        match &expected {
            Expected::Ran { image_digest } => {
                assert_eq!(
                    task["stopCode"].as_str(),
                    Some(ESSENTIAL_CONTAINER_EXITED),
                    "case {case:?}: expected a task that fetched {image} to end with \
                     {ESSENTIAL_CONTAINER_EXITED}, got {task}"
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
                assert_eq!(
                    digest.as_deref(),
                    image_digest.as_deref(),
                    "case {case:?}: expected the container of {image} to report \
                     {image_digest:?} as its imageDigest, got {digest:?}"
                );
            }
            Expected::FailedToStart => {
                assert_eq!(
                    task["stopCode"].as_str(),
                    Some(TASK_FAILED_TO_START),
                    "case {case:?}: expected a task that could not fetch {image} to stop with \
                     {TASK_FAILED_TO_START}, got {task}"
                );
                assert_eq!(
                    task["stoppedReason"].as_str(),
                    Some(cannot_pull_not_found(&image).as_str()),
                    "case {case:?}: expected the runtime's account of a reference that \
                     resolves to nothing, got {:?}",
                    task["stoppedReason"].as_str()
                );
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
                assert_eq!(
                    digest, None,
                    "case {case:?}: expected no imageDigest for an image no manifest was \
                     fetched for, got {digest:?}"
                );
            }
        }
    }
}
