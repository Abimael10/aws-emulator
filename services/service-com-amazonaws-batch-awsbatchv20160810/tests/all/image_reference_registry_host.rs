// SPDX-License-Identifier: CC0-1.0

//! WHICH registry a placed job's container image reference is resolved against, driven end to
//! end over the wire against the services that own the facts: Batch, Amazon ECS, Amazon ECR,
//! Amazon ECR Public and CloudWatch Logs on one engine.
//!
//! The RULE under test: **the registry a container image reference NAMES is the registry that
//! resolves it — a reference whose host names the Amazon ECR Public gallery is resolved
//! against that public registry's own repositories and published tags, and a reference whose
//! host names an Amazon ECR private registry against the repositories of that account's
//! registry — so neither reference is ever answered by the other's registry, and only a
//! reference the registry it names resolves to no image fails the job, with the container
//! runtime's `CannotPullContainerError` naming that reference and that registry's own
//! refusal.** A job whose public reference the gallery does serve RUNS: it reaches
//! `SUCCEEDED` with its container exiting 0 and the `awslogs` stream its placement opened
//! holds what the container's program wrote.
//!
//! Sources: `smithy_query(query="shape",shape="ContainerProperties")` spells the two forms
//! apart on `image` — "Images in Amazon ECR Public repositories use the full
//! `registry/repository[:tag]` or `registry/repository[@digest]` naming conventions. For
//! example, `public.ecr.aws/<registry_alias>/<my-web-app>:<latest>`", separately from "Images
//! in Amazon ECR repositories use the full registry and repository URI (for example,
//! `123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>`)" — so a public
//! reference names a repository of the gallery and NOT one of the caller's account;
//! `smithy_query(query="operation",operation="DescribeJobs")` for `JobDetail`'s `status`,
//! `statusReason`, `startedAt`, `attempts` and `container`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt container's
//! `exitCode`, `reason` and `logStreamName`.
//!
//! What is NOT asserted here, because another suite asserts it: that a placed Fargate job runs
//! as a task of its compute environment's Amazon ECS cluster
//! (`core/router/tests/all/batch_fargate_job_runs_as_an_ecs_task.rs`), which is a question
//! about the capacity rather than about which registry answered for the image.
//!
//! The rows differ along what the rule generalises over: the HOST the reference carries (the
//! gallery's, that same host spelled in another case, and this account's private registry),
//! the repository path, and whether the registry that host names holds the tag asked for. The
//! private registry is made to hold the very repository the gallery mirrors, under a tag the
//! gallery publishes no release of — so a row that resolves against the wrong registry of the
//! two ends the opposite way from the one asserted, in both directions, and neither leg can
//! pass by accident.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

/// The Region every call below is signed for, and so the Region of the private registry the
/// references name.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which is the account whose PRIVATE registry
/// a reference on that registry's host addresses.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The host AWS serves the Amazon ECR Public gallery at, as `ContainerProperties$image` spells
/// a public reference (`public.ecr.aws/<registry_alias>/<my-web-app>:<latest>`). Written out
/// here rather than imported from a service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const GALLERY_HOST: &str = "public.ecr.aws";

/// The registry alias the gallery publishes its mirror of the Docker Official Images under,
/// which is the alias segment a reference into one of them carries.
const GALLERY_ALIAS: &str = "docker";

/// The repository the gallery's mirror of the BusyBox official image is published as, and the
/// path this suite also creates in the PRIVATE registry: one path, two registries, which is
/// what makes every row below decide which of the two answered.
const MIRRORED_REPOSITORY: &str = "library/busybox";

/// A release the gallery's listing of that mirror publishes (`gallery.ecr.aws/docker/busybox`,
/// which mirrors the Docker Official Images' own tag list), and one this suite deliberately
/// does NOT push to the private registry.
const GALLERY_RELEASE: &str = "1.36";

/// The tag this suite pushes to the PRIVATE registry, and under which the gallery's mirror
/// publishes no release at all: a tag that exists in exactly one of the two registries.
const PRIVATE_ONLY_TAG: &str = "private-only";

/// How long a poll below gives the clock: far longer than the spans Batch and Amazon ECS give a
/// job and its task, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The line the container's program writes, which is what its log stream holds once it ran.
const MARKER: &str = "registry-host-decides";

/// The error CLASS a container runtime words a failed image fetch with.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// How many times the container agent retried the manifest fetch before it reported a
/// reference it could not resolve, as the reason it words carries it. Spelled out here rather
/// than imported from a service crate.
const MANIFEST_FETCH_RETRIES: u32 = 7;

/// The log group the job definitions below deliver their container output to, created through
/// CloudWatch Logs' own API.
const LOG_GROUP: &str = "/simparity/batch/registry-host";

/// The `awslogs-stream-prefix` those definitions name, which is the first segment of the stream
/// the driver opens for the task.
const STREAM_PREFIX: &str = "batch";

/// The host of this account's Amazon ECR private registry, in the spelling
/// `ContainerProperties$image` documents for a private reference
/// (`<account>.dkr.ecr.<region-name>.amazonaws.com`).
fn private_registry_host() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com")
}

/// The clause an Amazon ECR PRIVATE registry's own not-found answer carries: the registry it
/// looked in, which is the caller's own account. A reference into the public gallery must never
/// be answered with it — that is the whole differential this suite asserts.
fn private_registry_clause() -> String {
    format!("in the registry with id '{ACCOUNT}'")
}

/// The whole sentence a job whose reference the gallery resolves to no image reports, in the
/// container agent's own words: the error class, the retries it spent, the reference the fetch
/// asked for and the answer it got about it. Written out here rather than imported, so a wrong
/// constant in the implementation cannot make this suite pass.
fn image_not_found_reason(image: &str) -> String {
    format!(
        "{CANNOT_PULL}: pull image manifest has been retried {MANIFEST_FETCH_RETRIES} time(s): \
         failed to resolve ref {image}: {image}: not found"
    )
}

/// Batch, Amazon ECS, Amazon ECR, Amazon ECR Public and CloudWatch Logs on ONE shared engine,
/// the way the router mounts every service.
///
/// Each is here because this chain reaches it: the cluster backing the compute environment and
/// the task a placed job runs as are Amazon ECS's, the two registries that answer for an image
/// are Amazon ECR's and Amazon ECR Public's, and the stream the container's output is delivered
/// to is CloudWatch Logs'.
struct Emulator {
    /// The router every request below is dispatched through.
    router: MultiServiceRouter,
}

impl Emulator {
    /// One emulator with every service this chain reaches mounted on one engine.
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
            service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
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

    /// One `awsJson1_1` call to the service signed as `signing_name`.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization(signing_name))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, decoded(response.into_body().as_ref()))
    }

    /// One call to this account's PRIVATE Amazon ECR registry that must be served: the
    /// repository and the image in it are created through THAT service's own API, never written
    /// into a store.
    async fn ecr(&self, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .rpc(
                "ecr",
                "AmazonEC2ContainerRegistry_V20150921",
                operation,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case ecr {operation}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One CloudWatch Logs call; answers the status too, because a stream nothing opened is a
    /// modelled refusal rather than an empty answer.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        self.rpc("logs", "Logs_20140328", operation, body).await
    }
}

/// A SigV4 credential scope naming `service`: it is what routes a request to that service. The
/// key id and signature are not credentials — this harness turns request authentication off, so
/// nothing is verified.
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

/// The prepared emulator: a queue over a MANAGED, ENABLED Fargate environment — the capacity a
/// job has to be placed on before its image is fetched at all — a log group for the container
/// output, and one repository in the PRIVATE registry holding the mirrored path under a tag the
/// gallery publishes no release of.
struct Prepared {
    /// The emulator every call is made against.
    emulator: Emulator,
    /// The ARN of the queue every job below is submitted to.
    queue: String,
}

/// Stand the fixture up.
async fn prepared() -> Prepared {
    let emulator = Emulator::new();
    let environment = "ce-registry-host";
    emulator
        .batch_ok(
            "/v1/createcomputeenvironment",
            json!({
                "computeEnvironmentName": environment,
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": {
                    "type": "FARGATE",
                    "maxvCpus": 64,
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
        let detail = described["computeEnvironments"][0].clone();
        if detail["status"].as_str() == Some(state::STATUS_VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment never reached VALID, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let queue = emulator
        .batch_ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "q-registry-host",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (status, created) = emulator
        .logs("CreateLogGroup", json!({"logGroupName": LOG_GROUP}))
        .await;
    assert_eq!(
        status, 200,
        "setup: expected the log group to be created, got {status} with {created}"
    );

    // The PRIVATE registry is made to hold the very repository path the gallery mirrors, under
    // a tag the gallery publishes no release of — created and pushed through Amazon ECR's own
    // API, so what the private registry holds is that service's own record.
    emulator
        .ecr(
            "CreateRepository",
            json!({"repositoryName": MIRRORED_REPOSITORY}),
        )
        .await;
    emulator
        .ecr(
            "PutImage",
            json!({
                "repositoryName": MIRRORED_REPOSITORY,
                "imageManifest": json!({"schemaVersion": 2}).to_string(),
                "imageTag": PRIVATE_ONLY_TAG,
            }),
        )
        .await;

    Prepared { emulator, queue }
}

/// How a row's job ends, which is the registry the reference named having answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The registry the host names serves the reference, so the container runs and exits 0.
    Runs,
    /// The gallery holds the repository and publishes no release under the tag, so the fetch
    /// failed in the container agent's own words and no part of the caller's private registry
    /// is named in it.
    RefusedByTheGallery,
    /// This account's private registry holds no such image, so the fetch failed with that
    /// registry's own not-found answer, which names the registry it looked in.
    RefusedByThePrivateRegistry,
}

/// One image reference the rule is asserted over.
struct Row {
    /// What the case is.
    case: &'static str,
    /// The reference the job definition's container runs.
    image: String,
    /// How it ends.
    ends: Ends,
}

/// The rows: one repository path held by BOTH registries, addressed through each of their hosts
/// under each of their tags, plus two further gallery repositories and a path the private
/// registry holds nothing of.
fn rows() -> Vec<Row> {
    let private = private_registry_host();
    vec![
        Row {
            case: "a release the gallery publishes, named on the gallery's host",
            image: format!("{GALLERY_HOST}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}:{GALLERY_RELEASE}"),
            ends: Ends::Runs,
        },
        Row {
            // A host is a host name, so its case does not change which registry it names: the
            // tag is one the gallery publishes NO release of, so this row ends the gallery's
            // way only if the upper-case host was read as the gallery's — a host read as some
            // registry this emulator does not hold would leave the reference undecided and the
            // job would run.
            case: "the gallery's host spelled in upper case, on a tag it publishes no release of",
            image: format!(
                "{}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}:{PRIVATE_ONLY_TAG}",
                GALLERY_HOST.to_ascii_uppercase()
            ),
            ends: Ends::RefusedByTheGallery,
        },
        Row {
            case: "a moving channel of another gallery repository",
            image: format!("{GALLERY_HOST}/{GALLERY_ALIAS}/library/nginx:alpine"),
            ends: Ends::Runs,
        },
        Row {
            case: "a variant release of a third gallery repository",
            image: format!("{GALLERY_HOST}/{GALLERY_ALIAS}/library/httpd:2.4-alpine"),
            ends: Ends::Runs,
        },
        Row {
            // The private registry holds exactly this repository under exactly this tag, and
            // the gallery publishes no release of it: a reference resolved against the caller's
            // account instead of the gallery would RUN.
            case: "a tag the gallery publishes no release under, held in the private registry",
            image: format!(
                "{GALLERY_HOST}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}:{PRIVATE_ONLY_TAG}"
            ),
            ends: Ends::RefusedByTheGallery,
        },
        Row {
            // The mirror of the same path, addressed on the private registry's host: the
            // registry that answers is the account's own, which holds this repository.
            case: "the repository and tag the private registry holds, on its own host",
            image: format!("{private}/{MIRRORED_REPOSITORY}:{PRIVATE_ONLY_TAG}"),
            ends: Ends::Runs,
        },
        Row {
            // The gallery publishes this tag and the private registry does not: a reference
            // resolved against the gallery instead of the account would RUN.
            case: "a release only the gallery publishes, named on the private registry's host",
            image: format!("{private}/{MIRRORED_REPOSITORY}:{GALLERY_RELEASE}"),
            ends: Ends::RefusedByThePrivateRegistry,
        },
        Row {
            // The whole gallery path written under the private host names a repository that
            // registry has never held, which is what its not-found answer is for.
            case: "the gallery's own path, named on the private registry's host",
            image: format!("{private}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}:{GALLERY_RELEASE}"),
            ends: Ends::RefusedByThePrivateRegistry,
        },
    ]
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

/// The messages CloudWatch Logs holds in the suite's group for the stream `name`, and the status
/// the read answered with.
async fn events_in(emulator: &Emulator, name: &str) -> (u16, Vec<String>) {
    let (status, body) = emulator
        .logs(
            "GetLogEvents",
            json!({"logGroupName": LOG_GROUP, "logStreamName": name}),
        )
        .await;
    let messages = body["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (status, messages)
}

/// Poll the stream `name` until CloudWatch Logs holds exactly the messages `expected`, in that
/// order.
///
/// A wait for the CONDITION: the stream is created and its events delivered by the drain that
/// follows the transition they belong to, so a reader that happens to observe the job's terminal
/// status first looks again rather than concluding the container wrote nothing.
async fn until_events(emulator: &Emulator, case: &str, name: &str, expected: &[&str]) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, written) = events_in(emulator, name).await;
        if status == 200 && written == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream {name:?} to hold {expected:?}, last {status} with \
             {written:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the registry a container image reference NAMES is the registry that resolves it — a
/// reference on the Amazon ECR Public gallery's host against that gallery's repositories and
/// published tags, a reference on an Amazon ECR private registry's host against that account's
/// repositories — so neither is answered by the other's registry, a reference the registry it
/// names serves lets the job run its program, exit 0 and log what it wrote, and only a
/// reference that registry resolves to no image fails the job, with
/// `CannotPullContainerError` naming the reference and the refusing registry's own answer.
#[tokio::test]
async fn the_registry_a_reference_names_is_the_registry_that_resolves_it() {
    let fixture = prepared().await;
    let emulator = &fixture.emulator;
    let rows = rows();

    // Every job is submitted first and read afterwards: each one's placement is owed by the
    // clock independently, so the suite waits for one lifecycle rather than one per row.
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let definition = emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": format!("jd-registry-host-{index}"),
                    "type": "container",
                    "containerProperties": {
                        "image": &row.image,
                        "command": ["echo", MARKER],
                        "resourceRequirements": [
                            {"type": "VCPU", "value": "1"},
                            {"type": "MEMORY", "value": "512"},
                        ],
                        "logConfiguration": {
                            "logDriver": "awslogs",
                            "options": {
                                "awslogs-group": LOG_GROUP,
                                "awslogs-stream-prefix": STREAM_PREFIX,
                            },
                        },
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
                    "jobName": format!("job-registry-host-{index}"),
                    "jobQueue": &fixture.queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        submitted.push(job_id);
    }

    for (row, job_id) in rows.iter().zip(&submitted) {
        let case = row.case;
        let image = row.image.as_str();
        let detail = until_terminal(emulator, case, job_id).await;
        let expected_status = if row.ends == Ends::Runs {
            state::JOB_SUCCEEDED
        } else {
            state::JOB_FAILED
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?} on {image:?}: expected the job to end {expected_status}, got {detail}"
        );

        let attempt = detail["attempts"][0].clone();
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a statusReason, got {detail}"))
            .to_string();
        // WHICH registry answered, which is the whole point: the private registry's own
        // not-found answer names the registry it looked in, and no reference into the gallery is
        // ever answered with it.
        let clause = private_registry_clause();
        let names_the_private_registry = reason.contains(&clause);
        assert_eq!(
            names_the_private_registry,
            row.ends == Ends::RefusedByThePrivateRegistry,
            "case {case:?} on {image:?}: {clause:?} belongs to the private registry's own \
             answer alone, got {reason:?}"
        );

        match row.ends {
            Ends::Runs => {
                assert!(
                    !reason.contains(CANNOT_PULL),
                    "case {case:?} on {image:?}: the registry served this reference, got \
                     {reason:?}"
                );
                assert_eq!(
                    attempt["container"]["exitCode"].as_i64(),
                    Some(0),
                    "case {case:?} on {image:?}: expected the container to exit 0, got {attempt}"
                );
                assert_eq!(
                    detail["container"]["exitCode"].as_i64(),
                    Some(0),
                    "case {case:?} on {image:?}: the job's container reports that same exit \
                     status, got {detail}"
                );
                assert!(
                    !detail["startedAt"].is_null(),
                    "case {case:?} on {image:?}: a job whose container ran reports when it \
                     started, got {detail}"
                );
            }
            Ends::RefusedByTheGallery => {
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
            }
            Ends::RefusedByThePrivateRegistry => {
                assert!(
                    reason.starts_with(CANNOT_PULL) && reason.contains(image),
                    "case {case:?}: expected a {CANNOT_PULL} naming {image:?}, got {reason:?}"
                );
                assert_eq!(
                    attempt["container"]["reason"].as_str(),
                    Some(reason.as_str()),
                    "case {case:?}: expected the attempt to carry the same reason, got {attempt}"
                );
            }
        }
        if row.ends != Ends::Runs {
            // Nothing was created to run, so no run is reported for it.
            assert!(
                detail["startedAt"].is_null(),
                "case {case:?} on {image:?}: no container started, got {detail}"
            );
            assert!(
                attempt["container"]["exitCode"].is_null(),
                "case {case:?} on {image:?}: no container exited, got {attempt}"
            );
        }

        // The placement made a task either way, so the stream its `awslogs` driver opened is
        // named — holding the line the program wrote, and nothing where nothing ran.
        let stream = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the attempt to name its log stream, got {attempt}")
            })
            .to_string();
        let wanted_prefix = format!("{STREAM_PREFIX}/");
        assert!(
            stream.starts_with(&wanted_prefix),
            "case {case:?}: expected the stream to be named under {wanted_prefix:?}, got \
             {stream:?}"
        );
        if row.ends == Ends::Runs {
            until_events(emulator, case, &stream, &[MARKER]).await;
        } else {
            // The stream is HELD either way — the placement opened it — and holds nothing,
            // because nothing ran to write anything.
            until_events(emulator, case, &stream, &[]).await;
        }
    }
}
