// SPDX-License-Identifier: CC0-1.0

//! What a compute environment Batch orchestrates with Amazon ECS reports, and what a placed
//! job's container image fetch decides — end to end over the wire, against the services that
//! OWN the resources involved.
//!
//! Two rules are under test here, because they are the two halves of the same chain (a Batch
//! job runs as a task of the Amazon ECS cluster backing its compute environment, and that
//! task's container runs an image a registry has to serve):
//!
//! 1. an environment this service orchestrates with Amazon ECS — managed or unmanaged — is
//!    backed by a cluster of THAT service, and `ComputeEnvironmentDetail$ecsClusterArn` ("the
//!    Amazon Resource Name (ARN) of the underlying Amazon ECS cluster that the compute
//!    environment uses") reports it, so a client that asks Amazon ECS for that cluster finds
//!    it; an environment orchestrated with Amazon EKS has none. The rows span the
//!    orchestration types and both kinds of ECS environment.
//!
//! 2. a placed job's container image is FETCHED before any container starts, and the registry
//!    the reference names answers the fetch where this emulator holds that registry: a
//!    reference it resolves to no image fails the fetch, so the job ends with the container
//!    runtime's `CannotPullContainerError` having run nothing; one it serves, and one naming a
//!    registry this emulator does not hold, let the job RUN the program its container declares
//!    and end on the status that program exited with. Either way the placement made a task, so
//!    the stream the `awslogs` driver opened for it is held — holding what that program wrote.
//!    The rows span the registries (a private Amazon ECR registry, the Amazon ECR Public
//!    gallery, registries this emulator holds nothing of) and what each holds for the reference
//!    (the image, a different tag, no repository, no registry alias).
//!
//! 3. the stream a placed container writes to is named the way the `awslogs` driver names one,
//!    from the prefix the container's configuration carries — or the job definition's own name
//!    when it carries none — the container's name and the id of the task the placement made.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

/// The Region every call below is signed for, and so the Region of every ARN minted for them.
const REGION: &str = "us-east-1";

/// The account id this emulator mints its ARNs under, which is also the registry id of its
/// Amazon ECR registries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The log group a job's container logs go to when its configuration names no other one.
/// Spelled out rather than imported, so a wrong constant in the implementation cannot make a
/// test pass. Source: `AttemptContainerDetail$logStreamName` ("The log group for Batch jobs is
/// `/aws/batch/job`").
const DEFAULT_LOG_GROUP: &str = "/aws/batch/job";

/// How long a poll below gives the clock: far longer than the spans Batch gives a job's moves,
/// so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The error class a container runtime words a failed image fetch with, which is what a job
/// whose image could not be fetched reports as its `statusReason`. Spelled out here rather
/// than imported.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The IAM API version an awsQuery body carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// Batch, Amazon ECS, both Amazon ECR surfaces, CloudWatch Logs and IAM on ONE shared engine,
/// the way the router mounts every service.
///
/// Each of the five is here because this chain reaches it: the cluster backing a compute
/// environment is Amazon ECS's resource, the image a job's container runs is served by the
/// registry that holds it (a private Amazon ECR registry or the ECR Public gallery), the log
/// stream a placed container writes to is CloudWatch Logs', and the policies in force on the
/// execution role a private pull is made as are IAM's. Each claims only requests scoped to its
/// own signing name, so no Batch call below is routed any differently for their presence.
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
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
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

    /// One `awsJson1_1` call against the service whose signing name is `signing_name`.
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

    /// One Amazon ECS call, as a client makes it.
    async fn ecs(&self, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .rpc(
                "ecs",
                "AmazonEC2ContainerServiceV20141113",
                operation,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case ecs {operation}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One Amazon ECR Public call, as a client makes it.
    async fn gallery(&self, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .rpc("ecr-public", "SpencerFrontendService", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case ecr-public {operation}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One call to the private Amazon ECR registry, as a client makes it: a repository and the
    /// image in it are created through THAT service's own API, never written into a store.
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

    /// One awsQuery call to IAM that must succeed, answering its XML body: a role and the
    /// documents in force on it are created through IAM's OWN API.
    ///
    /// The body carries each value literally, which is what an
    /// `application/x-www-form-urlencoded` body of these calls needs: the only bytes that
    /// form encoding exists for are `&`, `=`, `+`, `%` and whitespace, and no value below
    /// holds one — the policy documents are written compactly for exactly that reason
    /// ([`grant`], [`role`]). A suite whose values need escaping takes the maintained
    /// encoder instead, the way this crate's manifest takes a crate for anything it would
    /// otherwise write by hand.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
        for (name, value) in params {
            assert!(
                !value.contains(['&', '=', '+', '%', ' ']),
                "case iam {action}: the value of {name} needs form escaping: {value:?}"
            );
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(value);
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case iam {action}: expected HTTP 200, got {status} with {text}"
        );
        text
    }

    /// One CloudWatch Logs call, as a client makes it; answers the status and the body, because
    /// a group nothing has created is a modelled refusal rather than an empty answer.
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

/// The one record `DescribeComputeEnvironments` reports for `name`.
async fn described_environment(emulator: &Emulator, name: &str) -> Value {
    let body = emulator
        .batch_ok(
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [name]}),
        )
        .await;
    let listed = body["computeEnvironments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "case describe {name}: expected exactly one record, got {body}"
    );
    listed[0].clone()
}

/// Poll `DescribeComputeEnvironments` until `name` reports `VALID`, then answer its record.
async fn until_environment_valid(emulator: &Emulator, name: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = described_environment(emulator, name).await;
        if described["status"].as_str() == Some(state::STATUS_VALID) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {name}: the compute environment never reached VALID, last {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The compute resources a `MANAGED` Fargate environment is created with here: the two
/// required members of `ComputeResource` (`type`, `maxvCpus`) plus the networking a Fargate
/// environment needs.
fn fargate_compute_resources() -> Value {
    json!({
        "type": "FARGATE",
        "maxvCpus": 16,
        "subnets": ["subnet-1111"],
        "securityGroupIds": ["sg-1111"],
    })
}

/// The compute resources a `MANAGED` EC2 environment is created with here.
fn ec2_compute_resources() -> Value {
    json!({
        "type": "EC2",
        "maxvCpus": 16,
        "minvCpus": 0,
        "subnets": ["subnet-1111"],
        "instanceRole": format!("arn:aws:iam::{ACCOUNT}:instance-profile/ecsInstanceRole"),
    })
}

/// One row of the compute environment table: what the create asks for, and what the
/// environment must then report.
struct EnvironmentRow {
    case: &'static str,
    /// The `CreateComputeEnvironment` request body, minus the name.
    request: fn() -> Value,
    /// The `containerOrchestrationType` the environment must report.
    orchestration: &'static str,
    /// Whether an Amazon ECS cluster has to back it.
    backed_by_a_cluster: bool,
}

/// RULE: every compute environment this service orchestrates with Amazon ECS is backed by a
/// cluster of that service and reports its ARN, and a client that asks Amazon ECS for that
/// cluster finds it; an environment orchestrated with Amazon EKS reports no `ecsClusterArn`.
#[tokio::test]
async fn an_ecs_orchestrated_compute_environment_reports_the_cluster_amazon_ecs_holds() {
    let rows: &[EnvironmentRow] = &[
        EnvironmentRow {
            case: "managed Fargate",
            request: || json!({"type": "MANAGED", "computeResources": fargate_compute_resources()}),
            orchestration: "ECS",
            backed_by_a_cluster: true,
        },
        EnvironmentRow {
            case: "managed EC2",
            request: || json!({"type": "MANAGED", "computeResources": ec2_compute_resources()}),
            orchestration: "ECS",
            backed_by_a_cluster: true,
        },
        EnvironmentRow {
            // The case `DescribeComputeEnvironments`' own documentation names: "if you're
            // using an unmanaged compute environment, you can use the
            // DescribeComputeEnvironment operation to determine the ecsClusterArn that you
            // launch your Amazon ECS container instances into".
            case: "unmanaged",
            request: || json!({"type": "UNMANAGED"}),
            orchestration: "ECS",
            backed_by_a_cluster: true,
        },
        EnvironmentRow {
            case: "managed and orchestrated with Amazon EKS",
            request: || {
                json!({
                    "type": "MANAGED",
                    "computeResources": ec2_compute_resources(),
                    "eksConfiguration": {
                        "eksClusterArn": format!("arn:aws:eks:{REGION}:{ACCOUNT}:cluster/analytics"),
                        "kubernetesNamespace": "batch-jobs",
                    },
                })
            },
            orchestration: "EKS",
            backed_by_a_cluster: false,
        },
    ];

    let emulator = Emulator::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("ce-cluster-{index}");
        let mut request = (row.request)();
        request["computeEnvironmentName"] = json!(name);
        emulator
            .batch_ok("/v1/createcomputeenvironment", request)
            .await;
        let described = until_environment_valid(&emulator, &name).await;
        assert_eq!(
            described["containerOrchestrationType"].as_str(),
            Some(row.orchestration),
            "case {case}: expected the {} orchestration type, got {described}",
            row.orchestration
        );
        let reported = described["ecsClusterArn"].as_str();
        if !row.backed_by_a_cluster {
            assert!(
                reported.is_none(),
                "case {case}: an environment Amazon ECS does not orchestrate has no cluster \
                 ARN, got {described}"
            );
            continue;
        }
        let uuid = described["uuid"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a uuid, got {described}"));
        // AWS Batch names the cluster of an environment `AWSBatch-<name>-<environment id>`,
        // and an Amazon ECS cluster ARN is `arn:aws:ecs:<region>:<account>:cluster/<name>`.
        let cluster_name = format!("AWSBatch-{name}-{uuid}");
        let expected = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{cluster_name}");
        assert_eq!(
            reported,
            Some(expected.as_str()),
            "case {case}: expected the backing cluster's ARN, got {described}"
        );

        // The cluster EXISTS where Amazon ECS holds it: that service's own reader answers for
        // it, which is what makes the reported ARN reachable.
        let answered = emulator
            .ecs("DescribeClusters", json!({"clusters": [expected]}))
            .await;
        let clusters = answered["clusters"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            clusters.len(),
            1,
            "case {case}: Amazon ECS must hold the cluster the environment names, got {answered}"
        );
        assert_eq!(
            clusters[0]["clusterName"].as_str(),
            Some(cluster_name.as_str()),
            "case {case}: expected the cluster to carry its own name, got {answered}"
        );
        assert_eq!(
            clusters[0]["status"].as_str(),
            Some("ACTIVE"),
            "case {case}: expected an ACTIVE cluster, got {answered}"
        );
    }
}

/// RULE: the deletion of a compute environment takes its backing Amazon ECS cluster with it,
/// so the cluster that service holds for a removed environment is no longer ACTIVE.
#[tokio::test]
async fn deleting_a_compute_environment_deletes_its_backing_cluster() {
    let emulator = Emulator::new();
    let name = "ce-cluster-deleted";
    emulator
        .batch_ok(
            "/v1/createcomputeenvironment",
            json!({
                "computeEnvironmentName": name,
                "type": "MANAGED",
                // DISABLED from the start, because a compute environment is deletable only
                // in that state (`DeleteComputeEnvironment`).
                "state": "DISABLED",
                "computeResources": fargate_compute_resources(),
            }),
        )
        .await;
    let described = until_environment_valid(&emulator, name).await;
    let cluster = described["ecsClusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case delete: expected a cluster ARN, got {described}"))
        .to_string();

    emulator
        .batch_ok(
            "/v1/deletecomputeenvironment",
            json!({"computeEnvironment": name}),
        )
        .await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let answered = emulator
            .ecs("DescribeClusters", json!({"clusters": [&cluster]}))
            .await;
        let active = answered["clusters"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|held| held["status"].as_str() == Some("ACTIVE"));
        if !active {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case delete: the backing cluster is still ACTIVE in Amazon ECS, got {answered}"
        );
        // The removal is owed by the clock, so the environment's own reader is what
        // materialises it; looking again is what drives it.
        emulator
            .batch_ok("/v1/describecomputeenvironments", json!({}))
            .await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One emulator with a queue over a MANAGED, ENABLED Fargate environment — the capacity a job
/// has to be placed on before its image is fetched at all.
struct Placeable {
    emulator: Emulator,
    queue: String,
    /// The `repositoryUri` of the gallery repository this fixture pushed one image into:
    /// `public.ecr.aws/<this registry's alias>/<repository>`, as the gallery itself reports it.
    gallery_uri: String,
}

/// The tag the gallery repository below holds an image under.
const HELD_TAG: &str = "v1";

/// A fresh emulator with capacity to place a job on, and one image in the public gallery.
async fn placeable() -> Placeable {
    let emulator = Emulator::new();
    let environment = "ce-pull";
    emulator
        .batch_ok(
            "/v1/createcomputeenvironment",
            json!({
                "computeEnvironmentName": environment,
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": fargate_compute_resources(),
            }),
        )
        .await;
    until_environment_valid(&emulator, environment).await;
    let queue = emulator
        .batch_ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "q-pull",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // One repository in the public gallery holding one image, pushed through that service's
    // own API, so the reference that names it is one the gallery really serves. The URI is
    // the gallery's own (`Repository$repositoryUri`), which is how a client learns the
    // registry alias to pull through.
    let created = emulator
        .gallery("CreateRepository", json!({"repositoryName": "team/app"}))
        .await;
    let gallery_uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| panic!("case gallery create: expected a repositoryUri, got {created}"))
        .to_string();
    emulator
        .gallery(
            "PutImage",
            json!({
                "repositoryName": "team/app",
                "imageManifest": json!({"schemaVersion": 2}).to_string(),
                "imageTag": HELD_TAG,
            }),
        )
        .await;

    Placeable {
        emulator,
        queue,
        gallery_uri,
    }
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
            "case {case}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the stream `name` of `group` until CloudWatch Logs holds exactly the messages
/// `expected`, in that order.
///
/// A wait for the CONDITION: the stream is created and its events delivered by the drain that
/// follows the transition they belong to, so a reader that happens to observe the job's
/// terminal status first looks again rather than concluding the container wrote nothing.
async fn until_events(emulator: &Emulator, case: &str, group: &str, name: &str, expected: &[&str]) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, events) = emulator
            .logs(
                "GetLogEvents",
                json!({"logGroupName": group, "logStreamName": name}),
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
            "case {case}: expected the stream {name:?} to hold {expected:?}, last {status} with \
             {events}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The log streams CloudWatch Logs holds in `group` whose name starts with `prefix`.
///
/// A group CloudWatch Logs holds nothing for holds no such stream either, so the refusal for an
/// absent group reads as the empty list.
async fn streams_in(emulator: &Emulator, group: &str, prefix: &str) -> Vec<Value> {
    let (status, body) = emulator
        .logs(
            "DescribeLogStreams",
            json!({"logGroupName": group, "logStreamNamePrefix": prefix}),
        )
        .await;
    if status != 200 {
        return Vec::new();
    }
    body["logStreams"].as_array().cloned().unwrap_or_default()
}

/// One row of the log-stream naming table: the `logConfiguration` the job definition carries,
/// and what the stream must then be called.
struct StreamNameRow {
    case: &'static str,
    /// The container's `logConfiguration`, or `None` for a container that configures no log
    /// driver at all (which gets the `awslogs` default).
    log_configuration: Option<fn() -> Value>,
    /// The first segment of the stream name: `None` when it is the job definition's own name,
    /// which is what AWS Batch uses when the configuration names no prefix.
    expected_prefix: Option<&'static str>,
}

/// RULE: the stream a placed job's container writes to is named the way the `awslogs` driver
/// names one — `<awslogs-stream-prefix>/<container name>/<ECS task id>` — with the prefix the
/// container's configuration carries, or the job definition's own name when it carries none,
/// and the id of the task the placement made.
#[tokio::test]
async fn a_placed_job_s_log_stream_is_named_the_way_the_awslogs_driver_names_one() {
    let rows: &[StreamNameRow] = &[
        StreamNameRow {
            case: "a container that configures no log driver",
            log_configuration: None,
            expected_prefix: None,
        },
        StreamNameRow {
            case: "awslogs with no options at all",
            log_configuration: Some(|| json!({"logDriver": "awslogs"})),
            expected_prefix: None,
        },
        StreamNameRow {
            case: "awslogs naming a stream prefix",
            log_configuration: Some(|| {
                json!({
                    "logDriver": "awslogs",
                    "options": {"awslogs-stream-prefix": "batch"},
                })
            }),
            expected_prefix: Some("batch"),
        },
        StreamNameRow {
            case: "awslogs naming a stream prefix and a group it creates",
            log_configuration: Some(|| {
                json!({
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-stream-prefix": "analytics/nightly",
                        "awslogs-group": "/driver/made/this",
                        "awslogs-create-group": "true",
                    },
                })
            }),
            expected_prefix: Some("analytics/nightly"),
        },
        StreamNameRow {
            // An option present but empty names no prefix, so the default stands rather than
            // a stream name beginning with a separator.
            case: "awslogs naming an empty stream prefix",
            log_configuration: Some(|| {
                json!({"logDriver": "awslogs", "options": {"awslogs-stream-prefix": ""}})
            }),
            expected_prefix: None,
        },
    ];

    let fixture = placeable().await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let definition_name = format!("jd-stream-{index}");
        let mut container = json!({
            "image": "busybox",
            "command": ["echo", "hi"],
            "resourceRequirements": [
                {"type": "VCPU", "value": "1"},
                {"type": "MEMORY", "value": "512"},
            ],
        });
        if let Some(build) = row.log_configuration {
            container["logConfiguration"] = build();
        }
        let definition = fixture
            .emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": &definition_name,
                    "type": "container",
                    "containerProperties": container,
                }),
            )
            .await["jobDefinitionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let job_id = fixture
            .emulator
            .batch_ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("job-stream-{index}"),
                    "jobQueue": &fixture.queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let detail = until_terminal(&fixture.emulator, case, &job_id).await;
        let attempt = detail["attempts"][0].clone();
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the attempt must name its task, got {attempt}"));
        let task_id = task_arn.rsplit('/').next().unwrap_or_default();
        let prefix = row.expected_prefix.unwrap_or(&definition_name);
        // The `awslogs` naming: the prefix, the name of the container the output belongs to
        // (`default` for the single container of a container job) and the task's id.
        let wanted = format!("{prefix}/default/{task_id}");
        assert_eq!(
            attempt["container"]["logStreamName"].as_str(),
            Some(wanted.as_str()),
            "case {case}: expected the stream {wanted:?}, got {attempt}"
        );
        let group = row
            .log_configuration
            .map(|build| build())
            .and_then(|configuration| {
                configuration["options"]["awslogs-group"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_LOG_GROUP.to_string());
        let found = streams_in(&fixture.emulator, &group, &wanted).await;
        assert!(
            found
                .iter()
                .any(|held| held["logStreamName"].as_str() == Some(wanted.as_str())),
            "case {case}: expected CloudWatch Logs to hold {wanted:?} in {group:?}, got {found:?}"
        );
    }
}
struct PullRow {
    case: &'static str,
    /// The image reference the job definition's container runs, built from the fixture (the
    /// gallery URI is the registry's own).
    image: fn(&Placeable) -> String,
    /// Whether the registry refuses to serve the reference, which is what ends the job with a
    /// pull failure and leaves it with no container output at all.
    refused: bool,
}

/// RULE: a placed job's container image is fetched from the registry its reference names, and a
/// reference a registry THIS emulator holds resolves to no image fails that fetch — the job
/// ends with the container runtime's `CannotPullContainerError` naming the reference and that
/// registry's own refusal, having run nothing. A reference the registry serves, and one naming
/// a registry this emulator does not hold, let the placed job RUN the program its container
/// declares. Either way the placement made a task, so the stream the driver opened for it is
/// held — holding what that program wrote, and nothing when nothing ran.
#[tokio::test]
async fn an_image_no_registry_here_serves_fails_the_job_at_the_fetch() {
    let rows: &[PullRow] = &[
        PullRow {
            // A bare name a container runtime resolves against a public registry this
            // emulator does not hold: nothing is decided against the fetch.
            case: "a bare name",
            image: |_| "busybox".to_string(),
            refused: false,
        },
        PullRow {
            case: "another registry altogether",
            image: |_| "quay.io/org/app:1".to_string(),
            refused: false,
        },
        PullRow {
            case: "the gallery repository that holds the tag",
            image: |fixture| format!("{}:{HELD_TAG}", fixture.gallery_uri),
            refused: false,
        },
        PullRow {
            case: "the gallery repository with a tag it does not hold",
            image: |fixture| format!("{}:no-such-tag", fixture.gallery_uri),
            refused: true,
        },
        PullRow {
            case: "a gallery repository the registry does not hold",
            image: |fixture| {
                let alias_prefix = fixture
                    .gallery_uri
                    .rsplit_once('/')
                    .map(|(prefix, _)| prefix.to_string())
                    .unwrap_or_default();
                format!("{alias_prefix}/absent:{HELD_TAG}")
            },
            refused: true,
        },
        PullRow {
            // The alias names another ACCOUNT's public registry in the gallery AWS serves,
            // which this emulator holds no store for: nothing is decided against the fetch,
            // exactly as for another registry host.
            case: "a gallery alias this registry was not assigned",
            image: |_| "public.ecr.aws/docker/library/busybox:latest".to_string(),
            refused: false,
        },
        PullRow {
            case: "a private registry repository that is not there",
            image: |_| format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/absent:1"),
            refused: true,
        },
    ];

    let fixture = placeable().await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let image = (row.image)(&fixture);
        let definition_name = format!("jd-pull-{index}");
        let definition = fixture
            .emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": &definition_name,
                    "type": "container",
                    "containerProperties": {
                        "image": &image,
                        "command": ["echo", "hi"],
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
        let job_id = fixture
            .emulator
            .batch_ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("job-pull-{index}"),
                    "jobQueue": &fixture.queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let detail = until_terminal(&fixture.emulator, case, &job_id).await;
        let expected_status = if row.refused {
            state::JOB_FAILED
        } else {
            state::JOB_SUCCEEDED
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case}: expected the job to end {expected_status}, got {detail}"
        );
        let reason = detail["statusReason"].as_str().unwrap_or_else(|| {
            panic!("case {case}: expected a statusReason, got {detail}")
        });
        if row.refused {
            assert!(
                reason.starts_with(CANNOT_PULL) && reason.contains(&image),
                "case {case}: expected a {CANNOT_PULL} naming {image:?}, got {reason:?}"
            );
        } else {
            assert_eq!(
                Some(reason),
                Some(state::ESSENTIAL_CONTAINER_EXITED_REASON),
                "case {case}: expected the reason a container that exited carries, got \
                 {reason:?}"
            );
        }
        // The task was made either way, so the stream its container's output goes to is named
        // and held — holding the line the program wrote, and nothing at all when the fetch
        // stopped the container from being created.
        let attempt = detail["attempts"][0].clone();
        let named = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case}: expected the attempt to name its log stream, got {attempt}")
            })
            .to_string();
        let found = streams_in(&fixture.emulator, DEFAULT_LOG_GROUP, &named).await;
        assert!(
            found
                .iter()
                .any(|held| held["logStreamName"].as_str() == Some(named.as_str())),
            "case {case}: expected CloudWatch Logs to hold {named:?}, got {found:?}"
        );
        let expected_lines: &[&str] = if row.refused { &[] } else { &["hi"] };
        until_events(
            &fixture.emulator,
            case,
            DEFAULT_LOG_GROUP,
            &named,
            expected_lines,
        )
        .await;
        // What a RUN produced is reported only for a container that ran: a fetch the registry
        // refused created none. The job's `stoppedAt` is not that — it is when the task the
        // placement made stopped, which every placed job reports.
        assert_eq!(
            attempt["startedAt"].is_null(),
            row.refused,
            "case {case}: startedAt belongs to an attempt whose container ran, got {attempt}"
        );
        // `JobDetail$startedAt` is modelled `@required`, so the job reports the Long default
        // where its container never started and the instant it began where it did.
        assert_eq!(
            detail["startedAt"].as_i64() == Some(0),
            row.refused,
            "case {case}: no startedAt is invented for a job nothing ran, got {detail}"
        );
        assert!(
            detail["stoppedAt"].as_i64().is_some(),
            "case {case}: a placed job reports when its task stopped, got {detail}"
        );
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            (!row.refused).then_some(0),
            "case {case}: the exit code belongs to a container that exited, got {attempt}"
        );
    }
}

/// The fixed text of the `statusReason` the agent words for the Amazon ECR registry
/// AUTHENTICATION it could not retrieve — the execution resource it obtains before any
/// manifest is fetched — followed by the class Amazon ECR refuses an unauthorized call with.
/// Spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const REGISTRY_AUTH_RETRIEVAL_FAILED: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve ecr registry auth: service call has been retried 1 \
     time(s): AccessDeniedException: ";

/// What the refusal inside that reason names: the registry-level authorization token, decided
/// on no resource narrower than the registry — not the manifest call the fetch itself makes.
const WITHHELD_REGISTRY_CALL: &str =
    "is not authorized to perform: ecr:GetAuthorizationToken on resource: *";

/// An identity-based document allowing `actions` on every resource.
///
/// Rendered compactly (`to_string` writes no whitespace), so the form body that carries it
/// to IAM needs no escaping ([`Emulator::iam`]).
fn grant(actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
    .to_string()
}

/// Create the IAM role `name`, trusted by Amazon ECS's task runtime and carrying `policy` as
/// its one inline policy, through IAM's own API — and answer its ARN.
///
/// The trust policy is rendered the same compact way as [`grant`].
async fn role(emulator: &Emulator, name: &str, policy: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    emulator
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
    emulator
        .iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// One row of the registry-authentication table: what the job's execution role is granted,
/// what its container runs, and the step the job must fail at — `None` for a row that runs.
struct RegistryAuthRow {
    case: &'static str,
    /// Which of the prepared roles the job runs as, and `None` for a job that names none.
    role: Option<usize>,
    /// The repository of this account's private registry the reference names, with its tag.
    reference: &'static str,
    /// The step the job fails at.
    fails_at: Option<Step>,
}

/// Where a job's launch failed: the execution-resource retrieval that precedes every fetch,
/// or the fetch itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The registry withheld the authorization token, so no image was reached.
    RegistryAuthRetrieval,
    /// The token was obtained and the registry would not serve the reference.
    ImageFetch,
}

/// RULE: the authorization token for an Amazon ECR registry is an EXECUTION RESOURCE retrieved
/// as the job's execution role before any image is fetched, and it is granted on the registry
/// rather than on a repository — so a job whose role no policy permits that registry-level
/// call fails in that retrieval, reporting the agent's `ResourceInitializationError` for the
/// registry authentication it could not get and naming no image, whatever the registry holds
/// for the reference. A role that DID obtain the token and is refused the manifest fails at
/// the fetch instead, which is the container runtime's account of an image it could not pull.
#[tokio::test]
async fn a_job_whose_role_cannot_get_the_registry_token_fails_the_resource_retrieval() {
    let fixture = placeable().await;
    let emulator = &fixture.emulator;

    // One repository of this account's private registry, holding one tagged image, created
    // and pushed through Amazon ECR's own API.
    emulator
        .ecr("CreateRepository", json!({"repositoryName": "app"}))
        .await;
    emulator
        .ecr(
            "PutImage",
            json!({
                "repositoryName": "app",
                "imageManifest": json!({"schemaVersion": 2}).to_string(),
                "imageTag": HELD_TAG,
            }),
        )
        .await;

    // The roles, each carrying a different part of what a pull takes.
    let roles = [
        role(
            emulator,
            "batchmanifestonly",
            &grant(&["ecr:BatchGetImage"]),
        )
        .await,
        role(emulator, "batchtokenonly", &grant(&["ecr:GetAuthorizationToken"])).await,
        role(
            emulator,
            "batchfullpull",
            &grant(&["ecr:GetAuthorizationToken", "ecr:BatchGetImage"]),
        )
        .await,
    ];
    // The rows differ along what the rule generalises over: which half of the pull the role
    // holds, and what the registry holds for the reference — which the registry-level call is
    // not decided on.
    let rows: &[RegistryAuthRow] = &[
        RegistryAuthRow {
            case: "a role granted the manifest call but not the registry token",
            role: Some(0),
            reference: "app:v1",
            fails_at: Some(Step::RegistryAuthRetrieval),
        },
        RegistryAuthRow {
            case: "the same role, pulling a repository this registry does not hold",
            role: Some(0),
            reference: "absent:v1",
            fails_at: Some(Step::RegistryAuthRetrieval),
        },
        RegistryAuthRow {
            case: "a role granted the registry token but not the manifest",
            role: Some(1),
            reference: "app:v1",
            fails_at: Some(Step::ImageFetch),
        },
        RegistryAuthRow {
            case: "a role granted both calls a pull makes",
            role: Some(2),
            reference: "app:v1",
            fails_at: None,
        },
        RegistryAuthRow {
            // A job that names no role states nothing for a policy to be read against, so
            // nothing is withheld and the registry answers about the image alone.
            case: "no execution role at all",
            role: None,
            reference: "app:v1",
            fails_at: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let image = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{}", row.reference);
        let mut container = json!({
            "image": &image,
            "command": ["echo", "hi"],
            "resourceRequirements": [
                {"type": "VCPU", "value": "1"},
                {"type": "MEMORY", "value": "512"},
            ],
        });
        if let Some(which) = row.role {
            container["executionRoleArn"] = json!(roles[which]);
        }
        let definition = emulator
            .batch_ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": format!("jd-auth-{index}"),
                    "type": "container",
                    "containerProperties": container,
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
                    "jobName": format!("job-auth-{index}"),
                    "jobQueue": &fixture.queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let detail = until_terminal(emulator, case, &job_id).await;
        let expected_status = if row.fails_at.is_some() {
            state::JOB_FAILED
        } else {
            state::JOB_SUCCEEDED
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case}: expected the job to end {expected_status}, got {detail}"
        );
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a statusReason, got {detail}"));
        match row.fails_at {
            Some(Step::RegistryAuthRetrieval) => {
                assert!(
                    reason.starts_with(REGISTRY_AUTH_RETRIEVAL_FAILED),
                    "case {case}: expected the statusReason to begin \
                     {REGISTRY_AUTH_RETRIEVAL_FAILED:?}, got {reason:?}"
                );
                assert!(
                    reason.contains(WITHHELD_REGISTRY_CALL),
                    "case {case}: expected the statusReason to carry \
                     {WITHHELD_REGISTRY_CALL:?}, got {reason:?}"
                );
                // A fetch that was never made is not reported: neither the runtime's class
                // for a failed fetch nor the reference itself is in it.
                for absent in [CANNOT_PULL, image.as_str()] {
                    assert!(
                        !reason.contains(absent),
                        "case {case}: expected no {absent:?} in the statusReason of a job that \
                         reached no image, got {reason:?}"
                    );
                }
            }
            Some(Step::ImageFetch) => {
                assert!(
                    reason.starts_with(CANNOT_PULL) && reason.contains(&image),
                    "case {case}: expected a {CANNOT_PULL} naming {image:?}, got {reason:?}"
                );
            }
            None => {
                assert_eq!(
                    Some(reason),
                    Some(state::ESSENTIAL_CONTAINER_EXITED_REASON),
                    "case {case}: expected the reason a container that exited carries, got \
                     {reason:?}"
                );
            }
        }
        // Nothing ran for a job that failed before its container was created, whichever step
        // failed: the exit code belongs to a container that exited.
        let attempt = detail["attempts"][0].clone();
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            row.fails_at.is_none().then_some(0),
            "case {case}: the exit code belongs to a container that exited, got {attempt}"
        );
    }
}
