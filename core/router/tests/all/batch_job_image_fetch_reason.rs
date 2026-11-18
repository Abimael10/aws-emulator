// SPDX-License-Identifier: CC0-1.0

//! What a placed AWS Batch job REPORTS when the registry will not serve its container image,
//! driven end to end across the services that own the facts: Batch, both Amazon ECR surfaces,
//! IAM and CloudWatch Logs on one engine.
//!
//! The RULE under test: **a container runs an image, and the image is fetched from its
//! registry before the container starts — so what a job whose image the registry will not
//! serve reports is that FETCH failure, worded by the container runtime as a
//! `CannotPullContainerError` naming the reference the pull was attempted with, while a job
//! whose image the registry did serve reports the end a placed container reaches here
//! instead.** `JobDetail$statusReason` is "a short, human-readable string to provide more
//! details about the current status of the job" and `AttemptContainerDetail$reason` is "a
//! short (255 max characters) human-readable string to provide additional details for a
//! running or stopped container", and both are projections of ONE reading of what that
//! attempt ended with, so they agree — with `ContainerDetail$reason` on the job's effective
//! container as the third. Which registry answers is read out of the reference itself: a
//! private Amazon ECR registry of this account decides the pull AS the job's execution role
//! ("for jobs that run on Fargate resources, you must provide an execution role" —
//! `ContainerProperties$executionRoleArn`), so a role no policy grants
//! `ecr:GetAuthorizationToken`/`ecr:BatchGetImage` to is refused the pull it makes; the Amazon
//! ECR Public gallery answers for a reference into the registry alias it was assigned, and an
//! alias it holds no registry for names another account's public registry this emulator has no
//! store for; and a reference naming a registry this emulator does not
//! hold at all is nobody's here to answer, so nothing is decided against it. Either way the
//! placement MADE a task, so the stream the `awslogs` driver opened for that task exists where
//! CloudWatch Logs holds it — holding the lines the container's program wrote, and none at all
//! where the fetch stopped any container from being created, which is also where no `startedAt`
//! and no `exitCode` are reported.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeJobs")` for `JobDetail`'s
//! `statusReason`, `attempts` and `container`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt container's
//! `reason` and `logStreamName` ("the name of the CloudWatch Logs log stream that's associated
//! with the container"); `smithy_query(query="shape",shape="ContainerProperties")` for
//! `image`, `executionRoleArn` and `logConfiguration`.
//!
//! What is NOT asserted here, because the owning crate's own suite asserts it:
//! `services/service-com-amazonaws-batch-awsbatchv20160810/tests/all/batch_ecs_orchestration.rs`'s
//! `an_image_no_registry_here_serves_fails_the_job_at_the_fetch` spans the PUBLIC half of the
//! same rule — a bare name, another registry altogether, the gallery repository served, the
//! gallery repository under a tag or a repository name it does not hold, a gallery alias this
//! registry was not assigned — and the empty stream behind each. What only this suite reaches
//! is the half that needs IAM and the private registry beside Batch: a PRIVATE Amazon ECR
//! repository served and withheld, the DIGEST form of a reference, the pull decided over the
//! documents in force on the job's EXECUTION ROLE, and the agreement of the three projections
//! of one verdict. Its rows therefore differ along the dimensions that half generalises over:
//! which registry the reference names (a private Amazon ECR registry of this account, the
//! public gallery, a registry alias no gallery here holds), WHAT that registry holds for it
//! (the image, another tag, nothing at that digest, no such alias), HOW it addresses the image
//! (by tag or by digest) and WHO the pull is made as (a role granted the calls a pull makes, a
//! role granted none of them).

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

/// The error CLASS a container runtime words a failed image fetch with, spelled out here
/// rather than imported from a service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The log group the job definitions below deliver their container output to, created through
/// CloudWatch Logs' own API before any job is submitted.
const LOG_GROUP: &str = "/sim2real/batch/pull-reasons";

/// The terminal `JobStatus`es a placed job of this emulator reaches — the one a container
/// that exited zero ends on, and the one a job whose image fetch was refused ends on — and
/// the `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape",shape="JobStatus,CEStatus,JQStatus")`.
const FAILED: &str = "FAILED";
const SUCCEEDED: &str = "SUCCEEDED";
const VALID: &str = "VALID";

/// How long this suite waits for something the clock owes — a compute environment reaching
/// `VALID`, a job reaching a terminal status, a stream appearing in CloudWatch Logs. Far above
/// the spans those take, and every wait looks again for the CONDITION rather than sleeping a
/// fixed stretch, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The repository the public gallery holds, and the tag it holds an image under.
const GALLERY_REPOSITORY: &str = "library/busybox";
const GALLERY_TAG: &str = "1.36";

/// The repository this account's private Amazon ECR registry holds, and its one tag.
const PRIVATE_REPOSITORY: &str = "app";
const PRIVATE_TAG: &str = "1";

/// A digest no image of either repository is held under: a well-formed `sha256:<64 hex>`,
/// which is the other way a reference addresses an image.
const ABSENT_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is whether a
/// registry holds an image and may serve it, not which blobs an image is built from — so the
/// layer array the schema its media type names requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// Batch, both Amazon ECR surfaces, IAM and CloudWatch Logs over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the fetch a placed Batch job's container
/// needs is answered out of the registries' OWN stores, the pull is decided over the documents
/// IAM holds for the execution role, and the stream the placement names is created where
/// CloudWatch Logs holds it. Each service claims only requests scoped to its own signing name,
/// so no call below is routed any differently for their presence.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
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

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), answered with the
/// status and the body: a group nothing created is a modelled refusal rather than an empty
/// answer.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    answered(router.dispatch(request).await)
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
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

/// An identity-based document allowing `actions` on every resource.
fn grant(actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": "*"}],
    })
    .to_string()
}

/// Create the role `name` — trusted by the Amazon ECS tasks principal, the way an execution
/// role is — carrying `policy` as its one inline document, and answer its ARN.
async fn role(router: &MultiServiceRouter, name: &str, policy: &str) -> String {
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
        name,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    iam(
        router,
        name,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "pull"),
            ("PolicyDocument", policy),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Poll the reader at `uri` until the string at `pointer` reads `wanted`, and answer what it
/// last read.
async fn until_reads(
    router: &MultiServiceRouter,
    case: &str,
    uri: &str,
    body: &Value,
    pointer: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = batch(router, case, uri, body).await;
        let read = answer
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if read == wanted {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {pointer} to read {wanted:?}, it was still {read:?}: \
             {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeJobs` until `job_id` reports a terminal status, then answer its whole detail.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let request = json!({"jobs": [job_id]});
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = batch(router, case, "/v1/describejobs", &request).await;
        let detail = answer["jobs"][0].clone();
        if matches!(detail["status"].as_str(), Some(FAILED) | Some(SUCCEEDED)) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the job to reach {FAILED} or {SUCCEEDED}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The log streams CloudWatch Logs holds in [`LOG_GROUP`] whose name starts with `prefix`.
///
/// A group this suite created, with no stream in it yet, answers the empty list; the refusal
/// for a group that is not there reads the same way, because neither holds the stream.
async fn streams_named(router: &MultiServiceRouter, prefix: &str) -> Vec<Value> {
    let (status, body) = logs(
        router,
        "DescribeLogStreams",
        &json!({"logGroupName": LOG_GROUP, "logStreamNamePrefix": prefix}),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["logStreams"].as_array().cloned().unwrap_or_default()
}

/// Poll until CloudWatch Logs holds the stream `name` in [`LOG_GROUP`].
async fn until_stream_exists(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if streams_named(router, name)
            .await
            .iter()
            .any(|stream| stream["logStreamName"].as_str() == Some(name))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected CloudWatch Logs to hold the stream {name:?} in \
             {LOG_GROUP:?}, got {:?}",
            streams_named(router, "").await
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the stream `name` in [`LOG_GROUP`] until CloudWatch Logs holds exactly the messages
/// `expected`, in that order.
///
/// A wait for the CONDITION: the events a container's program wrote are delivered by the drain
/// that follows the transition they belong to, so a reader that happens to observe the job's
/// terminal status first looks again rather than concluding the container wrote nothing.
async fn until_events(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &[&str],
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, events) = logs(
            router,
            "GetLogEvents",
            &json!({"logGroupName": LOG_GROUP, "logStreamName": name}),
        )
        .await;
        let written: Vec<String> = events["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        if status == 200 && written == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream {name:?} to hold {expected:?}, last {status} \
             with {events}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: the image a job definition's container runs, the execution role Batch
/// makes its pull as, and what the registry's answer must leave the job reporting.
struct Row {
    case: &'static str,
    /// The image reference, built from the registries the fixture created.
    image: String,
    /// The execution role the pull is made as; a Fargate job must name one.
    execution_role: String,
    /// Whether a registry this emulator HOLDS refuses the reference, which is what ends the
    /// job with the fetch failure rather than with the end a placed container reaches here.
    refused: bool,
    /// A clause the refusal must carry beyond the error class and the reference itself, for a
    /// row that is about WHY the registry withheld the pull.
    says: Option<String>,
}

/// One submitted job, waiting to be read back once its clock has run.
struct Submitted {
    case: &'static str,
    image: String,
    job_id: String,
    refused: bool,
    says: Option<String>,
}

/// RULE: a placed job reports what the fetch of its container image answered — the container
/// runtime's `CannotPullContainerError` naming the reference, for a reference a registry this
/// emulator holds will not serve to the job's execution role, and the end a placed container
/// reaches here for one it serves or one no registry here answers for — and the job's
/// `statusReason`, its attempt's container `reason` and its own container's `reason` are three
/// projections of that one reading. The attempt's `awslogs` stream exists and is empty either
/// way, and nothing of a run is reported.
#[tokio::test]
async fn a_placed_job_reports_what_the_fetch_of_its_image_answered() {
    let router = router();
    let fixture = "the estate";

    // The destination the job definitions' `awslogs` configuration delivers to, created
    // through CloudWatch Logs' own API: the stream of each placed task has to show up in it.
    let (status, created) =
        logs(&router, "CreateLogGroup", &json!({"logGroupName": LOG_GROUP})).await;
    assert_eq!(
        status, 200,
        "case {fixture:?}: expected the log group to be created, got {status} with {created}"
    );

    // One repository in the public gallery holding one image, pushed through that service's
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
    gallery(
        &router,
        fixture,
        "PutImage",
        &json!({
            "repositoryName": GALLERY_REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": GALLERY_TAG,
        }),
    )
    .await;
    // One repository of this account's PRIVATE registry, holding one image under one tag.
    ecr(
        &router,
        fixture,
        "CreateRepository",
        &json!({"repositoryName": PRIVATE_REPOSITORY}),
    )
    .await;
    ecr(
        &router,
        fixture,
        "PutImage",
        &json!({
            "repositoryName": PRIVATE_REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": PRIVATE_TAG,
        }),
    )
    .await;

    // Two execution roles: one carrying the calls a pull of a private image makes, one
    // carrying an unrelated Amazon ECR call and none of them.
    let granted = role(
        &router,
        "batchpullgranted",
        &grant(&[
            "ecr:GetAuthorizationToken",
            "ecr:BatchGetImage",
            "ecr:GetDownloadUrlForLayer",
            "ecr:BatchCheckLayerAvailability",
        ]),
    )
    .await;
    let withheld = role(&router, "batchpullwithheld", &grant(&["ecr:ListImages"])).await;

    // A MANAGED, ENABLED Fargate environment behind one queue: the capacity a job has to be
    // placed on before its image is fetched at all.
    let environment = "ce-pull-reasons";
    batch(
        &router,
        fixture,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": environment,
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": ["subnet-1111"],
                "securityGroupIds": ["sg-1111"],
            },
        }),
    )
    .await;
    until_reads(
        &router,
        fixture,
        "/v1/describecomputeenvironments",
        &json!({"computeEnvironments": [environment]}),
        "/computeEnvironments/0/status",
        VALID,
    )
    .await;
    let queue = member(
        fixture,
        &batch(
            &router,
            fixture,
            "/v1/createjobqueue",
            &json!({
                "jobQueueName": "jq-pull-reasons",
                "state": "ENABLED",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await,
        "/jobQueueArn",
    );
    until_reads(
        &router,
        fixture,
        "/v1/describejobqueues",
        &json!({"jobQueues": [&queue]}),
        "/jobQueues/0/status",
        VALID,
    )
    .await;

    /// The clause a refusal carries when no document in force on the role grants a call that
    /// a pull makes: the pull is refused to the PRINCIPAL it was made as, so the sentence
    /// names that role.
    fn names_the_role(role_arn: &str) -> String {
        format!("User: {role_arn} is not authorized to perform:")
    }
    // A reference into THIS account's private Amazon ECR registry, which `addressed` ends
    // with the tag or digest form of: the host the Amazon ECR User Guide gives for a pull,
    // `<account>.dkr.ecr.<region>.amazonaws.com/<repository>[:<tag>|@<digest>]`.
    let private_image = |addressed: &str| {
        format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{PRIVATE_REPOSITORY}{addressed}")
    };
    let rows = [
        Row {
            case: "the private repository under the tag it holds, by a granted role",
            image: private_image(&format!(":{PRIVATE_TAG}")),
            execution_role: granted.clone(),
            refused: false,
            says: None,
        },
        Row {
            case: "the private repository under a tag it does not hold",
            image: private_image(":2"),
            execution_role: granted.clone(),
            refused: true,
            says: None,
        },
        Row {
            // The other form a reference addresses an image by, which the registry resolves
            // through the same lookup.
            case: "the private repository at a digest it does not hold",
            image: private_image(&format!("@{ABSENT_DIGEST}")),
            execution_role: granted.clone(),
            refused: true,
            says: None,
        },
        Row {
            // The pull of a private image is made AS the job's execution role, so a role no
            // document in force grants those calls to is refused it.
            case: "the private image, by a role no document grants the pull to",
            image: private_image(&format!(":{PRIVATE_TAG}")),
            execution_role: withheld.clone(),
            refused: true,
            says: Some(names_the_role(&withheld)),
        },
        Row {
            // A SECOND registry whose image is served: the end a served fetch leaves a job at
            // cannot depend on which registry answered it.
            case: "the gallery repository under the tag it holds",
            image: format!("{gallery_uri}:{GALLERY_TAG}"),
            execution_role: granted.clone(),
            refused: false,
            says: None,
        },
        Row {
            // The public gallery answers for the registry alias it was ASSIGNED; an alias it
            // holds no registry for names another ACCOUNT's public registry, which this
            // emulator has no store for, so nothing is decided against such a reference.
            case: "a gallery alias this registry was not assigned, under a tag nothing holds",
            image: format!("public.ecr.aws/docker/{GALLERY_REPOSITORY}:no-such-tag"),
            execution_role: granted.clone(),
            refused: false,
            says: None,
        },
    ];

    // Every job is submitted first and read back afterwards: each settles on its own clock,
    // so the whole table costs one run rather than one per row.
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let definition_name = format!("jd-pull-{index}");
        let definition = member(
            case,
            &batch(
                &router,
                case,
                "/v1/registerjobdefinition",
                &json!({
                    "jobDefinitionName": &definition_name,
                    "type": "container",
                    "platformCapabilities": ["FARGATE"],
                    "containerProperties": {
                        "image": &row.image,
                        "command": ["echo", "hello"],
                        "executionRoleArn": &row.execution_role,
                        "resourceRequirements": [
                            {"type": "VCPU", "value": "1"},
                            {"type": "MEMORY", "value": "2048"},
                        ],
                        "logConfiguration": {
                            "logDriver": "awslogs",
                            "options": {"awslogs-group": LOG_GROUP},
                        },
                    },
                }),
            )
            .await,
            "/jobDefinitionArn",
        );
        let job_id = member(
            case,
            &batch(
                &router,
                case,
                "/v1/submitjob",
                &json!({
                    "jobName": format!("job-pull-{index}"),
                    "jobQueue": &queue,
                    "jobDefinition": definition,
                }),
            )
            .await,
            "/jobId",
        );
        submitted.push(Submitted {
            case,
            image: row.image.clone(),
            job_id,
            refused: row.refused,
            says: row.says.clone(),
        });
    }

    // The one end every placed container whose image was served reaches, learned from those
    // rows rather than spelled out here: what the rule requires of it is that it is not the
    // fetch failure and names no image, and that it is the same for all of them, so a client
    // can tell an unpullable image from a container that ran.
    let mut served_reasons: Vec<(&str, String)> = Vec::new();
    for Submitted {
        case,
        image,
        job_id,
        refused,
        says,
    } in &submitted
    {
        let detail = until_terminal(&router, case, job_id).await;
        let reason = member(case, &detail, "/statusReason");
        if *refused {
            assert!(
                reason.starts_with(CANNOT_PULL),
                "case {case:?}: expected a reason opening with {CANNOT_PULL}, got {reason:?}"
            );
            assert!(
                reason.contains(image.as_str()),
                "case {case:?}: expected the reason to name the image {image:?}, got {reason:?}"
            );
            if let Some(says) = says.as_deref() {
                assert!(
                    reason.contains(says),
                    "case {case:?}: expected the reason to carry {says:?}, got {reason:?}"
                );
            }
        } else {
            assert!(
                !reason.contains(CANNOT_PULL),
                "case {case:?}: a served image fails no fetch, got {reason:?}"
            );
            assert!(
                !reason.contains(image.as_str()),
                "case {case:?}: the end a placed container reaches names no image, got \
                 {reason:?}"
            );
            served_reasons.push((case, reason.clone()));
        }

        // The attempt the placement recorded reports the SAME reading, and so does the job's
        // own container: three projections of one verdict cannot disagree.
        let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            attempts.len(),
            1,
            "case {case:?}: a placed job has had exactly one placement, got {detail}"
        );
        assert_eq!(
            attempts[0]["container"]["reason"].as_str(),
            Some(reason.as_str()),
            "case {case:?}: expected the attempt's container reason to be {reason:?}, got {:?}",
            attempts[0]
        );
        assert_eq!(
            detail["container"]["reason"].as_str(),
            Some(reason.as_str()),
            "case {case:?}: expected the container's own reason to be {reason:?}, got {detail}"
        );

        // The task was made either way, so the stream its container's output goes to is
        // named and held where CloudWatch Logs holds it — holding the line the container's
        // program wrote, and nothing when the fetch stopped any container from being created.
        let stream = member(case, &attempts[0], "/container/logStreamName");
        until_stream_exists(&router, case, &stream).await;
        let expected_lines: &[&str] = if *refused { &[] } else { &["hello"] };
        until_events(&router, case, &stream, expected_lines).await;

        // And what a RUN produced is reported only where a container really ran.
        assert_eq!(
            attempts[0]["container"]["exitCode"].as_i64(),
            (!*refused).then_some(0),
            "case {case:?}: the exit code belongs to a container that exited, got {:?}",
            attempts[0]
        );
        assert_eq!(
            attempts[0]["startedAt"].is_null(),
            *refused,
            "case {case:?}: startedAt belongs to an attempt whose container ran, got {:?}",
            attempts[0]
        );
    }

    assert!(
        served_reasons.len() >= 2,
        "the table must span more than one served reference, got {served_reasons:?}"
    );
    let (first_case, first_reason) = served_reasons[0].clone();
    for (case, reason) in &served_reasons[1..] {
        assert_eq!(
            reason, &first_reason,
            "case {case:?}: every job whose image was served reaches the one end case \
             {first_case:?} reached ({first_reason:?}), got {reason:?}"
        );
    }
}
