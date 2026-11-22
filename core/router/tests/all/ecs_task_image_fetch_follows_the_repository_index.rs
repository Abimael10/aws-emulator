// SPDX-License-Identifier: CC0-1.0

//! What a launched Amazon ECS task's image fetch is decided against, and WHEN the stop a
//! failed fetch earns becomes readable.
//!
//! RULE: the fetch a launched task's containers need is attempted at the launch and decided
//! against what the registry holds AT THAT INSTANT and against the path the placement has to
//! that registry — so the same revision placed twice is answered twice, and each answer is
//! reported on the task that asked for it. A reference the registry resolves to nothing stops
//! the task before it runs (`Task$stopCode` is the `TaskStopCode` member for a task that
//! failed before its containers started, `Task$stoppedReason` carries the container runtime's
//! `CannotPullContainerError` for that reference, `Task$startedAt` — "the time when the task
//! transitioned from the `PENDING` state to the `RUNNING` state" — stays absent and
//! `Container$imageDigest` with it, because no manifest was fetched to digest); once the
//! registry holds that reference the identical launch is served, and the task runs and reports
//! the manifest digest the registry resolved. A placement with no path to the registry is
//! answered by NOTHING, so that attempt is not decided but WAITED OUT: the fetch's instants
//! (`Task$pullStartedAt`/`pullStoppedAt`) are one attempt apart instead of equal, and the stop
//! falls due at the end of it — a span of this emulator's clock, so a client polling for a
//! verdict reaches one.
//!
//! Which matters because image-pull failure is the ordinary way a rollout breaks: a client
//! reads `stopCode`/`stoppedReason` to tell a broken image reference from a broken network
//! path, and an outcome that never falls due inside a poll makes a misconfigured placement
//! look exactly like a healthy one.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeTasks")` for `Task`'s
//! `stopCode`, `stoppedReason`, `startedAt`, `pullStartedAt`, `pullStoppedAt` and `containers`;
//! `smithy_query(query="shape",shape="Container")` for `imageDigest`, `reason` and `exitCode`;
//! `smithy_query(query="shape",shape="TaskStopCode")` for the stop codes.
//!
//! This suite lives beside the router because the rule spans four services, each owning one
//! half of the decision and all mounted on ONE shared engine as `core/router/src/lib.rs`
//! mounts them: Amazon ECR owns the repository and the index the reference is resolved against,
//! Amazon EC2 owns the subnets and route tables the path is read off, IAM owns the execution
//! role the pull is made as, and Amazon ECS owns the task and the terminal record it stops
//! with. Every fact is created through the owning service's own API and every value asserted
//! is read back through `DescribeTasks`.
//!
//! The rows differ along what the rule generalises over: WHAT the registry holds for the
//! reference when the task is placed (nothing; the image, pushed after an earlier launch of
//! the same revision failed; the same manifest under a second tag), HOW the reference addresses
//! it (by tag, by digest) and WHETHER the placement reaches the registry at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region of every ARN minted for them
/// and of the registry host the references name.
const REGION: &str = "us-west-2";

/// The account this emulator serves, which is also its Amazon ECR registry id.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of Amazon ECS's `awsJson1_1` operations.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The `X-Amz-Target` prefix of Amazon ECR's.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The cluster every task below is placed in, and the one container each revision declares.
const CLUSTER: &str = "indexfetch";
const CONTAINER: &str = "app";

/// The repository of this account's private registry the rows pull from, the tag the push
/// below puts the image under, and the SECOND tag the same manifest is re-tagged with.
const REPOSITORY: &str = "indexfetch/app";
const TAG: &str = "v2";
const SECOND_TAG: &str = "release";

/// A well-formed `sha256:<64 hex>` no image of the repository is held under: the other way a
/// reference addresses an image, and one the push below never makes resolvable.
const ABSENT_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// The execution role Amazon ECS makes the pull as, and the ARN it is named by.
const EXECUTION_ROLE: &str = "indexfetchexecution";

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// The two `TaskStopCode` values the rows tell apart — the stop of a task Amazon ECS could not
/// launch the containers of at all, and the stop of a task whose essential container ran and
/// exited. Spelled out here rather than imported from a service crate, so a wrong constant in
/// the implementation cannot make this suite pass.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is what the
/// registry's index resolves a reference to, not which blobs an image is built from — so the
/// layer array the schema its media type names requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The program every container runs: one that ENDS, so a task whose fetch was served is
/// stopped as one whose essential container exited (`Container$exitCode` is "the exit code
/// returned from the container") and the record it stopped as can be read without racing its
/// own run. What the program does is beside the point here — every row is about the fetch
/// before it.
const PROGRAM: [&str; 3] = ["sh", "-c", "exit 0"];

/// How long a row waits for the clock-owed moves it depends on — the start a placement owes,
/// the attempt a fetch nothing answers is waited out, and the stop that follows — and how often
/// it looks again. Generous on purpose, by well over an order of magnitude over the spans
/// themselves: the wait is for the CONDITION, and the deadline is only here so a failure is a
/// failure rather than a hang.
const POLL_BUDGET: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The attempt the agent spends on a fetch nothing answers before it gives it up, which is what
/// the stop of such a task waits for, and one lifecycle boundary of a task.
const FETCH_ATTEMPT: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::FAILED_IMAGE_FETCH_TIMEOUT;
const TRANSITION_SPAN: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN;

/// How many lifecycle boundaries an attempt nothing answers may be worth and still leave a
/// polling client a verdict: a client that drives a placement and then polls for its outcome
/// gives the whole scenario a budget, so an attempt of the scale a real connect timeout takes
/// is one no such poll ever outlives — the isolation then has no observable effect at all.
const MOST_TRANSITIONS_PER_ATTEMPT: u32 = 8;

/// The sentence a container runtime words for a reference the registry resolves to nothing: the
/// manifest fetch is retried before the agent gives up, and the reference it could not resolve
/// is named twice — once as the reference the fetch asked for, and once as the registry's answer
/// about it. Spelled out here rather than imported, so a wrong wording in the implementation
/// cannot make this suite pass.
fn not_found_reason(image: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried 7 time(s): failed to \
         resolve ref {image}: {image}: not found"
    )
}

/// The sentence it words for a fetch NOTHING answered: the connection to the registry the
/// reference names timed out, so the request the runtime made and the address it made it to are
/// what the failure names.
fn timed_out_reason(image: &str) -> String {
    let host = image.split_once('/').map(|(host, _)| host).unwrap_or(image);
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: failed to do request: Get \
         \"https://{host}/v2/\": dial tcp {host}:443: i/o timeout"
    )
}

/// An image reference into this account's Amazon ECR registry addressing `identifier` of
/// [`REPOSITORY`]: `<account>.dkr.ecr.<region>.amazonaws.com/<repository>[:<tag>|@<digest>]`
/// (Amazon ECR User Guide, the registry URI a pull uses).
fn image(identifier: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{REPOSITORY}{identifier}")
}

/// Amazon ECS, Amazon ECR, Amazon EC2 and IAM over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service. Each claims only requests scoped to its own
/// signing name, so no call below is routed any differently for their presence.
fn emulator() -> MultiServiceRouter {
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
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request here. The
/// key id and the signature are not credentials: the router runs with request authentication
/// off, so nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call that must answer 200, as its parsed body.
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
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({"__unparsed": String::from_utf8_lossy(&payload)}))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected {signing_name} {operation} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon ECS call that must be served.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "ecs", ECS_TARGET_PREFIX, operation, body).await
}

/// One Amazon ECR call that must be served: the repository and its images are created through
/// that service's OWN API.
async fn ecr(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "ecr", ECR_TARGET_PREFIX, operation, body).await
}

/// One ec2Query action that must be served, as its XML body.
async fn ec2(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ec2"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected EC2 {action} to answer 200, got {status} with {rendered}"
    );
    rendered
}

/// One IAM call that must be served: a query-protocol service, so `POST /` with the parameters
/// form-encoded.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
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
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {rendered}"
    );
}

/// A string member of a body at `pointer`, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The two subnets the rows place into: one routed to an attached internet gateway, and one
/// with no route off its VPC at all.
struct Network {
    routed: String,
    isolated: String,
}

/// Build that network through Amazon EC2's OWN operations.
async fn network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc = element(
        &ec2(router, case, "CreateVpc", &[("CidrBlock", "10.70.0.0/16")]).await,
        "vpcId",
    );
    let routed = subnet(router, case, &vpc, "10.70.1.0/24", "a").await;
    let isolated = subnet(router, case, &vpc, "10.70.2.0/24", "b").await;

    // "To use an internet gateway, your subnet's route table must contain a route that directs
    // internet-bound traffic to the internet gateway" (Amazon VPC User Guide).
    let gateway = element(
        &ec2(router, case, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        case,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
    )
    .await;
    // One table per subnet, so what carries a subnet's traffic is the table that subnet is
    // associated with and nothing else.
    for (subnet_id, gateway_id) in [(&routed, Some(&gateway)), (&isolated, None)] {
        let table = element(
            &ec2(router, case, "CreateRouteTable", &[("VpcId", &vpc)]).await,
            "routeTableId",
        );
        ec2(
            router,
            case,
            "AssociateRouteTable",
            &[("RouteTableId", &table), ("SubnetId", subnet_id)],
        )
        .await;
        if let Some(gateway_id) = gateway_id {
            ec2(
                router,
                case,
                "CreateRoute",
                &[
                    ("RouteTableId", &table),
                    ("DestinationCidrBlock", "0.0.0.0/0"),
                    ("GatewayId", gateway_id),
                ],
            )
            .await;
        }
    }
    Network { routed, isolated }
}

/// One subnet of `vpc`, created over the wire.
async fn subnet(
    router: &MultiServiceRouter,
    case: &str,
    vpc: &str,
    cidr_block: &str,
    zone_suffix: &str,
) -> String {
    element(
        &ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc),
                ("CidrBlock", cidr_block),
                ("AvailabilityZone", &format!("{REGION}{zone_suffix}")),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Create the execution role Amazon ECS pulls as, through IAM's own API: trusted by this
/// service's task principal, and granted the calls a pull of a private image makes. Answers its
/// ARN.
async fn execution_role(router: &MultiServiceRouter, case: &str) -> String {
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
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
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
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", "pull"),
            ("PolicyDocument", &grant),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// Register one Fargate `awsvpc` revision of `family` whose one container runs `image` as
/// `role`.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    image: &str,
    role: &str,
) {
    ecs(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": image,
                "essential": true,
                "command": PROGRAM,
            }],
        }),
    )
    .await;
}

/// Place one task of `family` in `subnet_id`, and answer its ARN. A task given a public address
/// is one an internet gateway carries traffic for; the isolated placement asks for none, which
/// is the value `AwsVpcConfiguration$assignPublicIp` documents as the one that leaves the task
/// without internet access.
async fn run(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    subnet_id: &str,
    assign_public_ip: &str,
) -> String {
    let placed = ecs(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [subnet_id],
                    "assignPublicIp": assign_public_ip,
                },
            },
        }),
    )
    .await;
    member(case, &placed, "/tasks/0/taskArn")
}

/// Look at one task until it reaches the terminal `STOPPED` status, and answer the record it
/// stopped as. The wait is for the CONDITION, bounded only so a failure is a failure rather
/// than a hang.
async fn stopped(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ecs(
            router,
            case,
            "DescribeTasks",
            &json!({"cluster": CLUSTER, "tasks": [task_arn]}),
        )
        .await;
        let task = body
            .pointer("/tasks/0")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"));
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

/// The task's one container, as `DescribeTasks` reports it.
fn container(case: &str, task: &Value) -> Value {
    task.pointer("/containers/0")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task's one container, got {task}"))
}

/// How long the fetch of `task` took, as the task's OWN instants report it: `pullStartedAt` is
/// "the Unix timestamp for the time when the container image pull began" and `pullStoppedAt`
/// "the time when the container image pull completed", so their difference is the attempt — not
/// a reading of this suite's clock.
fn fetch_span(case: &str, task: &Value) -> Duration {
    let instant = |member: &str| {
        task[member].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: expected the attempted fetch's {member}, got {task}")
        })
    };
    let (started, stopped) = (instant("pullStartedAt"), instant("pullStoppedAt"));
    assert!(
        stopped >= started,
        "case {case:?}: expected a fetch that stopped no earlier than it started, got \
         {started} .. {stopped}"
    );
    Duration::from_secs_f64(stopped - started)
}

/// What a row expects of the task it placed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    /// The registry served the reference, so the container ran its program to the end and the
    /// task reports the manifest digest that registry resolved.
    Served,
    /// The registry resolved the reference to nothing and said so, so the task never ran and
    /// the attempt ended the moment it was answered.
    ResolvedToNothing,
    /// Nothing answered the fetch at all, so the attempt was waited out and the stop fell due
    /// at the end of it.
    Unreachable,
}

/// One row the table reads back: the reference its revision runs and what the task must report.
struct Placed {
    case: &'static str,
    image: String,
    task_arn: String,
    expected: Expected,
}

/// RULE: a launched task's image fetch is resolved against the registry's index at the instant
/// of the launch and against the path the placement has to that registry — so the same revision
/// fails before it runs while the reference resolves to nothing, is served once the registry
/// holds it, and is waited out where nothing answers it at all.
#[tokio::test]
async fn a_launched_tasks_image_fetch_follows_the_repositorys_index_and_its_path() {
    let setup = "the estate";
    let router = emulator();
    let network = network(&router, setup).await;
    let role = execution_role(&router, setup).await;
    ecs(
        &router,
        setup,
        "CreateCluster",
        &json!({"clusterName": CLUSTER}),
    )
    .await;
    // The repository exists and holds NOTHING: a repository is not an image, so a reference
    // into it resolves to nothing until something is pushed under the identifier it names.
    ecr(
        &router,
        setup,
        "CreateRepository",
        &json!({"repositoryName": REPOSITORY}),
    )
    .await;

    let by_tag = image(&format!(":{TAG}"));
    let by_second_tag = image(&format!(":{SECOND_TAG}"));
    let by_absent_digest = image(&format!("@{ABSENT_DIGEST}"));
    for (family, reference) in [
        ("indexfetchtag", &by_tag),
        ("indexfetchsecondtag", &by_second_tag),
        ("indexfetchdigest", &by_absent_digest),
    ] {
        register(&router, setup, family, reference, &role).await;
    }

    // BEFORE the push: the registry holds nothing for either reference, and says so.
    let mut unpushed = Vec::new();
    for (case, family, reference) in [
        (
            "a tag the repository holds nothing for, before the push",
            "indexfetchtag",
            &by_tag,
        ),
        (
            "a digest the repository holds nothing for, before the push",
            "indexfetchdigest",
            &by_absent_digest,
        ),
    ] {
        let task_arn = run(&router, case, family, &network.routed, "ENABLED").await;
        unpushed.push(Placed {
            case,
            image: reference.clone(),
            task_arn,
            expected: Expected::ResolvedToNothing,
        });
    }
    for placed in &unpushed {
        assert_task(&router, placed).await;
    }

    // The push, through Amazon ECR's own API, and the same manifest re-tagged: the index is
    // what a reference is resolved against, so a second tag pointing at the one manifest
    // resolves the same image.
    let pushed = member(
        setup,
        &ecr(
            &router,
            setup,
            "PutImage",
            &json!({
                "repositoryName": REPOSITORY,
                "imageManifest": MANIFEST,
                "imageTag": TAG,
            }),
        )
        .await,
        "/image/imageId/imageDigest",
    );
    ecr(
        &router,
        setup,
        "PutImage",
        &json!({
            "repositoryName": REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": SECOND_TAG,
        }),
    )
    .await;
    // The digest asserted below is the registry's OWN report of what it holds, not this suite's
    // reading of the manifest it pushed.
    let described = ecr(
        &router,
        setup,
        "DescribeImages",
        &json!({"repositoryName": REPOSITORY}),
    )
    .await;
    assert_eq!(
        member(setup, &described, "/imageDetails/0/imageDigest"),
        pushed,
        "case {setup:?}: expected the registry to report the pushed digest, got {described}"
    );

    // AFTER the push: the identical revision, placed again, and the same reference placed where
    // nothing carries the traffic off the VPC.
    let rows = [
        (
            "the same revision, placed again once the tag is pushed",
            "indexfetchtag",
            &by_tag,
            network.routed.as_str(),
            "ENABLED",
            Expected::Served,
        ),
        (
            "the second tag the one manifest was re-tagged with",
            "indexfetchsecondtag",
            &by_second_tag,
            network.routed.as_str(),
            "ENABLED",
            Expected::Served,
        ),
        (
            "a digest the repository holds nothing for, with the tag now pushed",
            "indexfetchdigest",
            &by_absent_digest,
            network.routed.as_str(),
            "ENABLED",
            Expected::ResolvedToNothing,
        ),
        (
            "the pushed tag, placed in a subnet with no route off its VPC",
            "indexfetchtag",
            &by_tag,
            network.isolated.as_str(),
            "DISABLED",
            Expected::Unreachable,
        ),
    ];
    let mut placed = Vec::new();
    for (case, family, reference, subnet_id, assign_public_ip, expected) in rows {
        let task_arn = run(&router, case, family, subnet_id, assign_public_ip).await;
        placed.push(Placed {
            case,
            image: reference.clone(),
            task_arn,
            expected,
        });
    }
    for row in &placed {
        let task = assert_task(&router, row).await;
        if row.expected == Expected::Served {
            assert_eq!(
                container(row.case, &task)["imageDigest"].as_str(),
                Some(pushed.as_str()),
                "case {:?}: expected the container to report the digest the registry resolved, \
                 got {task}",
                row.case
            );
        }
    }

    // And the attempt a fetch nothing answers is waited out stays a few lifecycle boundaries
    // long, which is what leaves a client polling such a placement a verdict rather than a
    // task that is permanently coming up.
    assert!(
        FETCH_ATTEMPT <= TRANSITION_SPAN * MOST_TRANSITIONS_PER_ATTEMPT,
        "expected an attempt of at most {MOST_TRANSITIONS_PER_ATTEMPT} lifecycle spans \
         ({TRANSITION_SPAN:?} each), got {FETCH_ATTEMPT:?}"
    );
}

/// Read one placed task back once it has stopped — or, for a row the registry served, once its
/// program has run to the end — and assert what the rule says of it. Answers the record, so a
/// row can go on to assert what only it is about.
async fn assert_task(router: &MultiServiceRouter, placed: &Placed) -> Value {
    let Placed {
        case,
        image,
        task_arn,
        expected,
    } = placed;
    let task = stopped(router, case, task_arn).await;
    let container = container(case, &task);
    // The fetch was ATTEMPTED either way, so both of its instants are reported either way.
    let span = fetch_span(case, &task);
    match expected {
        Expected::Served => {
            assert_eq!(
                task["stopCode"].as_str(),
                Some(ESSENTIAL_CONTAINER_EXITED),
                "case {case:?}: expected a task whose fetch of {image} was served to end with \
                 {ESSENTIAL_CONTAINER_EXITED}, got {task}"
            );
            assert!(
                !task["startedAt"].is_null(),
                "case {case:?}: expected a startedAt on a task that ran, got {task}"
            );
            assert_eq!(
                container["exitCode"].as_i64(),
                Some(0),
                "case {case:?}: expected the exit status of the program the container ran, got \
                 {container}"
            );
        }
        Expected::ResolvedToNothing | Expected::Unreachable => {
            assert_eq!(
                task["stopCode"].as_str(),
                Some(TASK_FAILED_TO_START),
                "case {case:?}: expected a task that could not fetch {image} to stop with \
                 {TASK_FAILED_TO_START}, got {task}"
            );
            assert!(
                task["startedAt"].is_null(),
                "case {case:?}: expected no startedAt on a task that never ran, got {task}"
            );
            // Nothing ran to exit, and no manifest was fetched to digest.
            for absent in ["exitCode", "imageDigest"] {
                assert!(
                    container[absent].is_null(),
                    "case {case:?}: expected no {absent} on a container that never ran, got \
                     {container}"
                );
            }
            let reason = match expected {
                Expected::Unreachable => timed_out_reason(image),
                _ => not_found_reason(image),
            };
            assert_eq!(
                task["stoppedReason"].as_str(),
                Some(reason.as_str()),
                "case {case:?}: expected the stoppedReason {reason:?}, got {:?}",
                task["stoppedReason"].as_str()
            );
            // The failure is one image's, so it is reported on the container that runs it too.
            assert_eq!(
                container["reason"].as_str(),
                Some(reason.as_str()),
                "case {case:?}: expected the container's reason {reason:?}, got {container}"
            );
        }
    }
    // A failure the registry ANSWERED ended the moment it was asked for, while a fetch nothing
    // answered was waited out: the task's own instants are what tell the two apart.
    match expected {
        Expected::Unreachable => assert!(
            span.abs_diff(FETCH_ATTEMPT) <= TRANSITION_SPAN,
            "case {case:?}: expected a fetch nothing answered to have been waited out for \
             {FETCH_ATTEMPT:?}, got {span:?}"
        ),
        _ => assert!(
            span < TRANSITION_SPAN,
            "case {case:?}: expected a fetch that was answered to have ended at once, got \
             {span:?}"
        ),
    }
    task
}
