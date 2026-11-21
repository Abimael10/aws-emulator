// SPDX-License-Identifier: CC0-1.0

//! What a placed Batch job reports for a container image held in a repository the Amazon ECR
//! Public GALLERY publishes itself, driven end to end over the wire against the services that
//! own the facts: Batch, Amazon ECS, Amazon ECR Public and CloudWatch Logs on one engine.
//!
//! The RULE under test: **a job's container image is fetched from the registry its reference
//! names before any container starts, and a repository of the gallery whose image this
//! emulator holds is one that registry ANSWERS for — a tag it publishes a release under is
//! fetched and the container runs, while a tag it publishes no release under resolves to no
//! image, so the job never runs at all and ends FAILED with the container agent's
//! `CannotPullContainerError` naming the reference it could not resolve. A repository, an
//! alias or a digest the gallery here states nothing about is left undecided, so such a job
//! runs exactly as one whose registry served it.** `JobDetail$statusReason` is "a short,
//! human-readable string to provide more details about the current status of the job" and
//! `AttemptContainerDetail$reason` is "a short (255 max characters) human-readable string to
//! provide additional details for a running or stopped container", so both carry that one
//! verdict; `AttemptContainerDetail$exitCode` is the status the container exited with, which a
//! container that was never created has none of. Either way the placement MADE a task, so the
//! stream the `awslogs` driver opened for it is held
//! (`AttemptContainerDetail$logStreamName`) — holding what the container's program wrote, and
//! nothing at all where no container was created.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeJobs")` for `JobDetail`'s
//! `status`, `statusReason`, `startedAt` and `attempts`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for `reason`, `exitCode` and
//! `logStreamName`; `smithy_query(query="shape",shape="ContainerProperties")` for `image`,
//! whose documentation spells the gallery form of a reference
//! ("Images in Amazon ECR Public repositories use the full `registry/repository[:tag]` or
//! `registry/repository[@digest]` naming conventions").
//!
//! The rows differ along what the rule generalises over: WHICH repository of the gallery the
//! reference names (one whose image this emulator holds, one it holds none of, an alias it
//! holds none of), WHAT that repository publishes for the tag asked for (a release, a moving
//! channel, a variant, nothing), and HOW the reference addresses the image (by tag or by
//! digest). The repository that holds the tag the suite drives the served path with is not the
//! one it drives the refusal with, so neither case stands on one repository's listing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The log group a job's container output goes to when its configuration names no other.
/// Spelled out rather than imported, so a wrong constant in the implementation cannot make
/// this suite pass. Source: `AttemptContainerDetail$logStreamName` ("The log group for Batch
/// jobs is `/aws/batch/job`").
const DEFAULT_LOG_GROUP: &str = "/aws/batch/job";

/// How long a poll below gives the clock: far longer than the spans Batch and Amazon ECS give
/// a job and its task, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The line the container's program writes, which is what its log stream holds once it ran.
const MARKER: &str = "published-release";

/// How many times the container agent retried the manifest fetch before it reported a
/// reference it could not resolve, as the reason it words carries it. Spelled out here rather
/// than imported from a service crate.
const MANIFEST_FETCH_RETRIES: u32 = 7;

/// The whole sentence a job whose reference the registry resolves to no image reports, in the
/// container agent's own words: the error class, the retries it spent, the reference the fetch
/// asked for and the answer it got about it. Written out here rather than imported, so a wrong
/// constant in the implementation cannot make this suite pass.
fn image_not_found_reason(image: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried \
         {MANIFEST_FETCH_RETRIES} time(s): failed to resolve ref {image}: {image}: not found"
    )
}

/// Batch, Amazon ECS, Amazon ECR Public and CloudWatch Logs on ONE shared engine, the way the
/// router mounts every service.
///
/// Each is here because this chain reaches it: the cluster backing the compute environment and
/// the task a placed job runs as are Amazon ECS's, the registry that answers for the image is
/// Amazon ECR Public's, and the stream the container's output is delivered to is CloudWatch
/// Logs'.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One Batch call, as a client makes it: `POST /v1/<operation>` with a JSON body.
    async fn batch(&self, uri: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
            .header("host", format!("batch.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("batch"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, decoded(response.into_body().as_ref()))
    }

    /// One Batch call that must be served.
    async fn batch_ok(&self, uri: &str, body: Value) -> Value {
        let (status, answered) = self.batch(uri, body).await;
        assert_eq!(
            status, 200,
            "case {uri}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One CloudWatch Logs call; answers the status too, because a group nothing created is a
    /// modelled refusal rather than an empty answer.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("logs.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("Logs_20140328.{operation}"))
            .header("authorization", authorization("logs"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, decoded(response.into_body().as_ref()))
    }
}

/// A SigV4 credential scope naming `service`: it is what routes a request to that service.
/// The key id and signature are not credentials — this harness turns request authentication
/// off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The parsed body of a response, or the text it carried when that is not JSON.
fn decoded(payload: &[u8]) -> Value {
    if payload.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(payload)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()))
}

/// One emulator with a queue over a MANAGED, ENABLED Fargate environment — the capacity a job
/// has to be placed on before its image is fetched at all.
async fn placeable() -> (Emulator, String) {
    let emulator = Emulator::new();
    let environment = "ce-published";
    emulator
        .batch_ok(
            "/v1/createcomputeenvironment",
            json!({
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
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .batch_ok(
                "/v1/describecomputeenvironments",
                json!({"computeEnvironments": [environment]}),
            )
            .await;
        if described["computeEnvironments"][0]["status"].as_str() == Some(state::STATUS_VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment never reached VALID, last {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let queue = emulator
        .batch_ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "q-published",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (emulator, queue)
}

/// Poll `DescribeJobs` until `job_id` reports a terminal status, then answer its detail.
async fn until_terminal(emulator: &Emulator, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = emulator
            .batch_ok("/v1/describejobs", json!({"jobs": [job_id]}))
            .await;
        let detail = body["jobs"][0].clone();
        if detail["status"]
            .as_str()
            .is_some_and(state::is_terminal_status)
        {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the stream `name` until CloudWatch Logs holds exactly the messages `expected`, in that
/// order.
///
/// A wait for the CONDITION: the stream is created and its events delivered by the drain that
/// follows the transition they belong to, so a reader that happens to observe the job's
/// terminal status first looks again rather than concluding the container wrote nothing.
async fn until_events(emulator: &Emulator, case: &str, name: &str, expected: &[&str]) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, events) = emulator
            .logs(
                "GetLogEvents",
                json!({"logGroupName": DEFAULT_LOG_GROUP, "logStreamName": name}),
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

/// One image reference the rule is asserted over.
struct Row {
    case: &'static str,
    /// The reference the job definition's container runs.
    image: &'static str,
    /// Whether the gallery resolves it to no image, which is what ends the job with the
    /// agent's pull failure and leaves its stream with nothing in it.
    unresolved: bool,
}

/// The rows: the gallery's own published repositories decide a reference into them, and
/// everything else is undecided.
///
/// The published tags come from the repositories' gallery listings (`gallery.ecr.aws/docker/…`,
/// the mirror of the Docker Official Images): `1.37` is a BusyBox release, `alpine` a moving
/// nginx channel and `2.4-alpine` an Apache HTTP Server variant, while nothing is published
/// under the two tags below that name no release.
const ROWS: &[Row] = &[
    Row {
        case: "a release the gallery publishes",
        image: "public.ecr.aws/docker/library/busybox:1.37",
        unresolved: false,
    },
    Row {
        case: "a moving channel the gallery publishes",
        image: "public.ecr.aws/docker/library/nginx:alpine",
        unresolved: false,
    },
    Row {
        case: "a variant release the gallery publishes",
        image: "public.ecr.aws/docker/library/httpd:2.4-alpine",
        unresolved: false,
    },
    Row {
        case: "a tag the published repository publishes no release under",
        image: "public.ecr.aws/docker/library/busybox:no-such-release",
        unresolved: true,
    },
    Row {
        case: "a version the published repository publishes no release under",
        image: "public.ecr.aws/docker/library/nginx:0.1",
        unresolved: true,
    },
    Row {
        case: "a repository of that registry this emulator holds no image of",
        image: "public.ecr.aws/docker/library/python:3.12",
        unresolved: false,
    },
    Row {
        case: "an alias this emulator holds no image of",
        image: "public.ecr.aws/amazonlinux/amazonlinux:2023",
        unresolved: false,
    },
    Row {
        case: "a digest into a published repository, which names content and not a release",
        image: "public.ecr.aws/docker/library/busybox@sha256:\
                3fbc632167424a6d997e74f52b878d7cc478225cffac6bc977eedfe51c7f4e79",
        unresolved: false,
    },
];

#[tokio::test]
async fn the_gallerys_published_releases_decide_a_jobs_image_fetch() {
    let (emulator, queue) = placeable().await;
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let image = row.image;
        let definition_name = format!("jd-published-{index}");
        let definition = emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": &definition_name,
                    "type": "container",
                    "containerProperties": {
                        "image": image,
                        "command": ["echo", MARKER],
                        "resourceRequirements": [
                            {"type": "VCPU", "value": "1"},
                            {"type": "MEMORY", "value": "512"},
                        ],
                    },
                }),
            )
            .await["jobDefinitionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let job_id = emulator
            .batch_ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("job-published-{index}"),
                    "jobQueue": &queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let detail = until_terminal(&emulator, case, &job_id).await;
        let expected_status = if row.unresolved {
            state::JOB_FAILED
        } else {
            state::JOB_SUCCEEDED
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected the job to end {expected_status}, got {detail}"
        );

        let attempt = detail["attempts"][0].clone();
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a statusReason, got {detail}"))
            .to_string();
        if row.unresolved {
            let expected = image_not_found_reason(image);
            assert_eq!(
                reason, expected,
                "case {case:?}: expected the reason {expected:?}, got {reason:?}"
            );
            // The job-level reason and the attempt's container reason are two projections of
            // the one verdict, so they agree.
            assert_eq!(
                attempt["container"]["reason"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the attempt to carry the same reason, got {attempt}"
            );
        } else {
            assert_ne!(
                reason, "",
                "case {case:?}: expected the reason a container that exited carries, got \
                 {detail}"
            );
            assert!(
                !reason.contains("CannotPullContainerError"),
                "case {case:?}: no fetch failure is decided for this reference, got {reason:?}"
            );
        }

        // What a RUN produced is reported only where a container was created: a reference the
        // registry resolved to no image created none, so no start and no exit status is
        // claimed for it.
        assert_eq!(
            detail["startedAt"].is_null(),
            row.unresolved,
            "case {case:?}: startedAt belongs to a job whose container started, got {detail}"
        );
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            (!row.unresolved).then_some(0),
            "case {case:?}: the exit code belongs to a container that exited, got {attempt}"
        );

        // The placement made a task either way, so the stream its `awslogs` driver opened is
        // held — holding the line the program wrote, and nothing where nothing ran.
        let named = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the attempt to name its log stream, got {attempt}")
            })
            .to_string();
        let expected_lines: &[&str] = if row.unresolved { &[] } else { &[MARKER] };
        until_events(&emulator, case, &named, expected_lines).await;
    }
}
