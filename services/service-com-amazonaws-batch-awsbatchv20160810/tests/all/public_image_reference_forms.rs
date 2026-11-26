// SPDX-License-Identifier: CC0-1.0

//! Every FORM a placed job's container image reference may name a PUBLIC registry in, driven
//! end to end over the wire against the services that own the facts: Batch, Amazon ECS, Amazon
//! ECR, Amazon ECR Public and CloudWatch Logs on one engine.
//!
//! The RULE under test: **the registry an image reference NAMES is the registry that answers
//! the fetch, so a reference that names any registry other than this account's own Amazon ECR
//! private registry is never resolved as a repository of that private registry — whatever
//! spelling it carries — and no such job is ever answered with that registry's
//! repository-not-found sentence or with the account the registry belongs to; only a reference
//! the registry it names resolves to no image fails the job, and then with the container
//! agent's own account of the reference it could not resolve.**
//!
//! `smithy_query(query="shape",shape="ContainerProperties")` documents the accepted forms of
//! `image` separately: "Images in Amazon ECR Public repositories use the full
//! `registry/repository[:tag]` or `registry/repository[@digest]` naming conventions. For
//! example, `public.ecr.aws/<registry_alias>/<my-web-app>:<latest>`"; "Images in Amazon ECR
//! repositories use the full registry and repository URI (for example,
//! `123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>`)"; "Images in official
//! repositories on Docker Hub use a single name (for example, `ubuntu` or `mongo`)"; "Images in
//! other repositories on Docker Hub are qualified with an organization name (for example,
//! `amazon/amazon-ecs-agent`)"; "Images in other online repositories are qualified further by a
//! domain name (for example, `quay.io/assemblyline/ubuntu`)" — five forms, only ONE of which
//! addresses the caller's own private registry.
//!
//! Further sources: `smithy_query(query="operation",operation="DescribeJobs")` for
//! `JobDetail`'s `status`, `statusReason`, `startedAt` and `attempts`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt container's
//! `reason` and `exitCode`.
//!
//! The rows differ along what the rule generalises over: the FORM the reference is written in
//! (the gallery's host with a tag, with no tag at all, with a digest, a Docker Hub host, a
//! single Docker Hub name, an organization-qualified one, a domain-qualified host carrying a
//! port), how many path segments follow the host, and whether the registry the form names
//! resolves the reference to an image at all. Nothing passes by accident: the private registry
//! is made to hold EXACTLY the repository path and tag the failing row's reference would be
//! looked up under if its host were read as that registry's, so a reference rewritten into a
//! private lookup would RUN where this suite requires it to fail — and every row whose registry
//! this emulator holds no store for would fail with the private registry's own not-found
//! sentence where this suite requires it to run.
//!
//! Sibling suites: `image_reference_registry_host.rs` pins the gallery host against the private
//! registry's host over one repository path both registries hold, and
//! `public_gallery_image_releases.rs` pins which releases of a gallery repository resolve. This
//! one pins the SPELLINGS of a reference, which is what decides whether the private registry is
//! consulted at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

/// The Region every call below is signed for, and so the Region of the private registry whose
/// host none of these references carries.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under: the owner of the PRIVATE registry, which is
/// the registry none of the references below addresses — so no answer about one of them may name
/// it.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The host AWS serves the Amazon ECR Public gallery at, as `ContainerProperties$image` spells a
/// public reference (`public.ecr.aws/<registry_alias>/<my-web-app>:<latest>`). Written out here
/// rather than imported from a service crate, so a wrong constant in the implementation cannot
/// make this suite pass.
const GALLERY_HOST: &str = "public.ecr.aws";

/// The registry alias the gallery publishes its mirror of the Docker Official Images under,
/// which is the alias segment a reference into one of them carries.
const GALLERY_ALIAS: &str = "docker";

/// The repository path of the gallery's mirror of the BusyBox official image, which is also the
/// path the FAILING row's reference carries.
const MIRRORED_REPOSITORY: &str = "library/busybox";

/// A tag the gallery's listing of that mirror publishes no release under
/// (`gallery.ecr.aws/docker/busybox`, which mirrors the Docker Official Images' own tag list), and
/// the tag this suite pushes into the PRIVATE registry instead.
const UNPUBLISHED_TAG: &str = "no-such-release";

/// The repository name the private registry is made to hold: everything the failing row's
/// reference carries after the gallery host, which is the name a reference rewritten into a
/// private-registry lookup would be resolved under.
fn rewritten_private_path() -> String {
    format!("{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}")
}

/// How long a poll below gives the clock: far longer than the spans Batch and Amazon ECS give a
/// job and its task, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The line the container's program writes, which is what a job that ran reports an exit status
/// for.
const MARKER: &str = "public-reference-form";

/// The error CLASS a container runtime words a failed image fetch with.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// How many times the container agent retried the manifest fetch before it reported a reference
/// it could not resolve, as the reason it words carries it. Spelled out here rather than imported
/// from a service crate.
const MANIFEST_FETCH_RETRIES: u32 = 7;

/// The clause an Amazon ECR PRIVATE registry's own not-found answer carries: the registry it
/// looked in, which is the caller's own account. A reference that names another registry must
/// never be answered with it — that is the whole differential this suite asserts.
const PRIVATE_REGISTRY_CLAUSE: &str = "does not exist in the registry with id";

/// The whole sentence a job whose reference the registry resolves to no image reports, in the
/// container agent's own words: the error class, the retries it spent, the reference the fetch
/// asked for and the answer it got about it. Written out here rather than imported, so a wrong
/// constant in the implementation cannot make this suite pass.
fn image_not_found_reason(image: &str) -> String {
    format!(
        "{CANNOT_PULL}: pull image manifest has been retried {MANIFEST_FETCH_RETRIES} time(s): \
         failed to resolve ref {image}: {image}: not found"
    )
}

/// Batch, Amazon ECS, Amazon ECR, Amazon ECR Public and CloudWatch Logs on ONE shared engine, the
/// way the router mounts every service.
///
/// Each is here because this chain reaches it: the cluster backing the compute environment and
/// the task a placed job runs as are Amazon ECS's, the two registries that could answer for an
/// image are Amazon ECR's and Amazon ECR Public's, and the stream a container's output is
/// delivered to is CloudWatch Logs'.
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

    /// One call to this account's PRIVATE Amazon ECR registry that must be served: the repository
    /// and the image in it are created through THAT service's own API, never written into a store.
    async fn ecr(&self, operation: &str, body: Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("ecr.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
            )
            .header("authorization", authorization("ecr"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = decoded(response.into_body().as_ref());
        assert_eq!(
            status, 200,
            "setup ecr {operation}: expected HTTP 200, got {status} with {answered}"
        );
        answered
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
/// job has to be placed on before its image is fetched at all — and one repository in the PRIVATE
/// registry holding exactly the path and tag a rewritten public reference would resolve to.
async fn prepared() -> (Emulator, String) {
    let emulator = Emulator::new();
    let environment = "ce-reference-forms";
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
                "jobQueueName": "q-reference-forms",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // The differential: this account's private registry is made to hold the path a public
    // reference would be looked up under if its host were read as that registry's, under the very
    // tag the gallery publishes no release of. A reference resolved against the account instead of
    // the registry it names would therefore be SERVED.
    let path = rewritten_private_path();
    emulator
        .ecr("CreateRepository", json!({"repositoryName": &path}))
        .await;
    emulator
        .ecr(
            "PutImage",
            json!({
                "repositoryName": &path,
                "imageManifest": json!({"schemaVersion": 2}).to_string(),
                "imageTag": UNPUBLISHED_TAG,
            }),
        )
        .await;
    // ... and it really does resolve there, so the failing row below fails because the registry
    // its reference NAMED holds no such image, not because nothing anywhere holds one.
    let held = emulator
        .ecr(
            "BatchGetImage",
            json!({
                "repositoryName": &path,
                "imageIds": [{"imageTag": UNPUBLISHED_TAG}],
            }),
        )
        .await;
    assert_eq!(
        held["images"][0]["imageId"]["imageTag"].as_str(),
        Some(UNPUBLISHED_TAG),
        "setup: expected the private registry to resolve {path}:{UNPUBLISHED_TAG}, got {held}"
    );

    (emulator, queue)
}

/// How a row's job ends, which is the registry the reference named having answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The registry the reference names serves it — or holds no store here to decide it — so the
    /// container runs its program and exits 0.
    Runs,
    /// The registry the reference names resolves it to no image, so no container is created and
    /// the job carries the container agent's own account of the reference.
    Unresolved,
}

/// One image reference form the rule is asserted over.
struct Row {
    /// What the case is.
    case: &'static str,
    /// The reference the job definition's container runs.
    image: String,
    /// How it ends.
    ends: Ends,
}

/// The rows: one per documented form of `ContainerProperties$image` that names a registry other
/// than this account's own, plus the one reference among them that resolves to nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // A reference naming no tag asks for the moving `latest` channel, which this
            // repository's gallery listing publishes.
            case: "a gallery reference naming no tag at all",
            image: format!("{GALLERY_HOST}/{GALLERY_ALIAS}/library/httpd"),
            ends: Ends::Runs,
        },
        Row {
            // A digest names content by its own hash rather than a release the repository
            // published, so the tags it publishes state nothing about it.
            case: "a gallery reference addressing a digest",
            image: format!(
                "{GALLERY_HOST}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}@sha256:\
                 3fbc632167424a6d997e74f52b878d7cc478225cffac6bc977eedfe51c7f4e79"
            ),
            ends: Ends::Runs,
        },
        Row {
            // Another publishing account's public registry, which this emulator holds no store
            // for: nothing is decided against such a reference, and the private registry is not
            // consulted for it either.
            case: "a gallery alias this emulator holds no registry for",
            image: format!("{GALLERY_HOST}/amazonlinux/amazonlinux:2023"),
            ends: Ends::Runs,
        },
        Row {
            case: "a Docker Hub reference naming that registry by host",
            image: "docker.io/library/busybox:1.36".to_string(),
            ends: Ends::Runs,
        },
        Row {
            // "Images in official repositories on Docker Hub use a single name".
            case: "a single Docker Hub name, which carries no host at all",
            image: "busybox:1.36".to_string(),
            ends: Ends::Runs,
        },
        Row {
            // "Images in other repositories on Docker Hub are qualified with an organization
            // name" — one path segment ahead of the repository, exactly as a gallery alias is.
            case: "an organization-qualified Docker Hub name",
            image: "simparity/batch-reference-probe:1".to_string(),
            ends: Ends::Runs,
        },
        Row {
            // "Images in other online repositories are qualified further by a domain name", and
            // such a host may carry a port.
            case: "a domain-qualified host carrying a port",
            image: "registry.example.com:5000/team/batch-reference-probe:1".to_string(),
            ends: Ends::Runs,
        },
        Row {
            // The gallery publishes no release under this tag and the PRIVATE registry holds
            // exactly this path under exactly it: a reference rewritten into a private lookup
            // would run, and this one has to fail in the gallery's own terms.
            case: "a gallery tag the mirrored repository publishes no release under",
            image: format!(
                "{GALLERY_HOST}/{GALLERY_ALIAS}/{MIRRORED_REPOSITORY}:{UNPUBLISHED_TAG}"
            ),
            ends: Ends::Unresolved,
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

/// RULE: a container image reference that names any registry other than this account's own
/// Amazon ECR private registry — in every form `ContainerProperties$image` documents — is
/// answered by the registry it names and never by a lookup in that private registry, so no such
/// job reports that registry's repository-not-found sentence or the account it belongs to; a
/// reference the named registry resolves to no image fails the job in the container agent's own
/// words, and every other one lets the container run its program and exit 0.
#[tokio::test]
async fn a_reference_naming_another_registry_is_never_a_private_registry_lookup() {
    let (emulator, queue) = prepared().await;
    let rows = rows();

    // Every job is submitted first and read afterwards: each one's placement is owed by the clock
    // independently, so the suite waits for one lifecycle rather than one per row.
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let definition = emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": format!("jd-reference-form-{index}"),
                    "type": "container",
                    "containerProperties": {
                        "image": &row.image,
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
                    "jobName": format!("job-reference-form-{index}"),
                    "jobQueue": &queue,
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
        let detail = until_terminal(&emulator, case, job_id).await;
        let expected_status = match row.ends {
            Ends::Runs => state::JOB_SUCCEEDED,
            Ends::Unresolved => state::JOB_FAILED,
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?} on {image:?}: expected the job to end {expected_status}, got {detail}"
        );

        let attempt = detail["attempts"][0].clone();
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?} on {image:?}: expected a statusReason, got {detail}")
            })
            .to_string();
        // The private registry was not the registry any of these references named, so neither its
        // own not-found sentence nor the account it belongs to may appear in what the job reports.
        assert!(
            !reason.contains(PRIVATE_REGISTRY_CLAUSE),
            "case {case:?} on {image:?}: {PRIVATE_REGISTRY_CLAUSE:?} belongs to the private \
             registry's own answer, which this reference never addressed, got {reason:?}"
        );
        assert!(
            !reason.contains(ACCOUNT),
            "case {case:?} on {image:?}: the caller's account {ACCOUNT} owns no registry this \
             reference named, got {reason:?}"
        );

        match row.ends {
            Ends::Runs => {
                assert!(
                    !reason.contains(CANNOT_PULL),
                    "case {case:?} on {image:?}: no fetch failure is decided for this reference, \
                     got {reason:?}"
                );
                assert_eq!(
                    attempt["container"]["exitCode"].as_i64(),
                    Some(0),
                    "case {case:?} on {image:?}: expected the container to exit 0, got {attempt}"
                );
                assert!(
                    !detail["startedAt"].is_null(),
                    "case {case:?} on {image:?}: a job whose container ran reports when it \
                     started, got {detail}"
                );
            }
            Ends::Unresolved => {
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
        }
    }
}
