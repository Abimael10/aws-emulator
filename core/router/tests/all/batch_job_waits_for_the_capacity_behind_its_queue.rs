// SPDX-License-Identifier: CC0-1.0

//! What a submitted AWS Batch job WAITS for before it is placed, and where a client sees it
//! while it waits — driven across the two crates the issue's deployment shape spans
//! (CloudFormation declares the estate, AWS Batch runs the job) on one engine.
//!
//! The RULE under test: **a job the service has accepted is placed only once the compute
//! environment behind its queue HAS the capacity to hold it, so a job whose environment must
//! provision that capacity first stays `RUNNABLE` — where `GetJobQueueSnapshot` reports it,
//! "the Amazon Resource Names (ARNs) of the first 100 `RUNNABLE` jobs in a named job queue" —
//! for as long as the provisioning takes, and the environment reports the capacity it scaled
//! out to as `computeResources.desiredvCpus`, "the desired number of vCPUS in the compute
//! environment. Batch modifies this value between the minimum and maximum values based on job
//! queue demand"; capacity that is ALREADY running — the vCPUs a `minvCpus` above zero makes
//! the environment "maintain", or Fargate capacity, which is no instance of the account's own
//! — holds the job up for nothing.** This holds for an estate CloudFormation declared exactly
//! as for one the Batch API created: once the stack reports `CREATE_COMPLETE` its
//! `AWS::Batch::JobQueue` is `VALID` and ordered onto its `AWS::Batch::ComputeEnvironment`, so
//! a job submitted to it is queued there and then placed.
//!
//! Sources: `smithy_query(query="doc",shape="FrontOfQueueDetail",member="jobs")` for what the
//! snapshot lists, `smithy_query(query="shape",shape="FrontOfQueueJobSummary")` for its
//! `jobArn`/`earliestTimeAtPosition`, and `smithy_query(query="shape",shape="ComputeResource")`
//! for `minvCpus`, `maxvCpus` and `desiredvCpus`.
//!
//! The rows differ along what the rule generalises over: WHICH capacity is behind the queue
//! (an EC2 compute environment that maintains none, one that maintains more than the job
//! asks for, a Fargate one that provisions nothing), how much the job DEMANDS (one vCPU, two,
//! and a container that states no `resourceRequirements` at all) and who declared the estate
//! (a CloudFormation stack, the Batch API).
//!
//! Nothing here asserts on elapsed time: a case counts how many of its own one-second reads of
//! the queue found the job outstanding, and reads how long the job was held off the instants the
//! SERVICE reports for it (`createdAt`, `stoppedAt`), so a loaded host makes fewer reads rather
//! than a different answer.
//!
//! The last suite is the other end of the same list: a `computeEnvironmentOrder` is stored and
//! walked every time a job of the queue is placed or the capacity behind it is scaled, so the
//! entries one create may name are bounded, and a create past that bound is refused rather than
//! kept.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state::limits;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region of every ARN minted here.
const REGION: &str = "us-east-1";

/// The interval a client polls the queue's snapshot at: whole seconds, which is what a client
/// waiting for capacity does. A job that is outstanding only between two of these reads is a
/// job no client can observe at the front of its queue.
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often the other waits here look again — a stack settling, a queue reaching `VALID`.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The deadline every wait here gives up at: generous, and never asserted on. What each wait
/// waits for is a CONDITION, so a slow host only makes it later.
const DEADLINE: Duration = Duration::from_secs(120);

/// The `JQStatus` a queue a client may submit to reports.
const VALID: &str = "VALID";

/// The terminal `JobStatus` values (`smithy_query(query="shape",shape="JobStatus")`): what a
/// job that has been placed and has run to an end reports. WHICH of the two a placed container
/// of this emulator reaches is another contract's business; what these cases watch for is the
/// job leaving its queue's ordering for good.
const TERMINAL: &[&str] = &["SUCCEEDED", "FAILED"];

/// The least a job that has to WAIT for capacity may spend between its submit and the stop of
/// the task it was eventually placed as — several of a client's poll intervals, so a client
/// polling the queue's snapshot has reads to find it with.
///
/// Read off the instants the SERVICE reports (`JobDetail$createdAt`, `JobDetail$stoppedAt`),
/// never off this test's own clock: those instants are the ones the job's transitions fell due
/// at, so a loaded host does not move them.
const WAIT_FOR_CAPACITY_MILLIS: i64 = 3_000;

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in the other suites
/// here; the in-process callback CloudFormation makes signs its own request either way.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    // The cluster backing a compute environment is created in Amazon ECS, so that service is
    // mounted beside Batch for the create to reach.
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// How much of a job's life the service itself reports between accepting it
/// (`JobDetail$createdAt`, "when the job was created") and the stop of the task the placement
/// made (`JobDetail$stoppedAt`) — which for a job that had to wait for capacity is dominated
/// by that wait.
fn reported_lifetime_millis(case: &str, job: &Value) -> i64 {
    let created = job["createdAt"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a createdAt, got {job}"));
    let stopped = job["stoppedAt"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a stoppedAt, got {job}"));
    stopped - created
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one AWS Batch operation over its restJson1 wire, answering the status and the body.
async fn batch_call(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The same call, which must answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let (status, answered) = batch_call(router, uri, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {answered}"
    );
    answered
}

/// The one `JobDetail` `DescribeJobs` answers for `job_id`.
async fn described_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    batch(
        router,
        case,
        "/v1/describejobs",
        &json!({"jobs": [job_id]}),
    )
    .await["jobs"][0]
        .clone()
}

/// The one `ComputeEnvironmentDetail` `DescribeComputeEnvironments` answers for `reference`.
async fn described_environment(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
) -> Value {
    batch(
        router,
        case,
        "/v1/describecomputeenvironments",
        &json!({"computeEnvironments": [reference]}),
    )
    .await["computeEnvironments"][0]
        .clone()
}

/// Wait until the job queue `reference` reports [`VALID`], as a client that has just been
/// handed one does, and answer the record it then reports.
async fn until_queue_valid(router: &MultiServiceRouter, case: &str, reference: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let queue = batch(
            router,
            case,
            "/v1/describejobqueues",
            &json!({"jobQueues": [reference]}),
        )
        .await["jobQueues"][0]
            .clone();
        if queue["status"].as_str() == Some(VALID) {
            return queue;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job queue never reached {VALID}, last {queue}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Register one container job definition that reserves `vcpus`, or — for `None` — states no
/// `resourceRequirements` at all, and answer its ARN.
async fn registered_definition(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    vcpus: Option<&str>,
) -> String {
    let mut container = json!({
        "image": "public.ecr.aws/docker/library/busybox:1.36",
        "command": ["echo", "hello"],
    });
    if let Some(vcpus) = vcpus {
        container["resourceRequirements"] = json!([
            {"type": "VCPU", "value": vcpus},
            {"type": "MEMORY", "value": "512"},
        ]);
    }
    let registered = batch(
        router,
        case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": name,
            "type": "container",
            "containerProperties": container,
        }),
    )
    .await;
    registered["jobDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobDefinitionArn, got {registered}"))
        .to_string()
}

/// What watching ONE job out of its queue saw.
struct Watched {
    /// How many of the one-second reads of `GetJobQueueSnapshot` listed the job's ARN at the
    /// front of its queue.
    reads_listing_the_job: usize,
    /// The `earliestTimeAtPosition` the snapshot reported for it, from the first read that
    /// listed it.
    earliest_time_at_position: Option<i64>,
    /// The `computeResources.desiredvCpus` the environment reported while the job was
    /// outstanding, from that same read.
    desiredv_cpus_while_waiting: Option<i64>,
    /// The job, as `DescribeJobs` reported it once it reached a terminal status.
    settled: Value,
}

/// Poll `GetJobQueueSnapshot` once a second, exactly as a client waiting for its job to be
/// placed does, until the job reaches a terminal status — and count the reads that found it
/// outstanding at the front of its queue.
///
/// The counting is what makes this independent of the wall clock: a read that takes longer on a
/// loaded host is one read fewer, never a different verdict.
async fn watch_out_of_the_queue(
    router: &MultiServiceRouter,
    case: &str,
    queue: &str,
    environment: &str,
    job_arn: &str,
    job_id: &str,
) -> Watched {
    let deadline = Instant::now() + DEADLINE;
    let mut watched = Watched {
        reads_listing_the_job: 0,
        earliest_time_at_position: None,
        desiredv_cpus_while_waiting: None,
        settled: Value::Null,
    };
    loop {
        let snapshot = batch(
            router,
            case,
            "/v1/getjobqueuesnapshot",
            &json!({"jobQueue": queue}),
        )
        .await;
        let listed = snapshot["frontOfQueue"]["jobs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(entry) = listed
            .iter()
            .find(|entry| entry["jobArn"].as_str() == Some(job_arn))
        {
            watched.reads_listing_the_job += 1;
            if watched.earliest_time_at_position.is_none() {
                watched.earliest_time_at_position = entry["earliestTimeAtPosition"].as_i64();
                watched.desiredv_cpus_while_waiting =
                    described_environment(router, case, environment).await
                        ["computeResources"]["desiredvCpus"]
                        .as_i64();
            }
        }
        let described = described_job(router, case, job_id).await;
        let status = described["status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&status.as_str()) {
            watched.settled = described;
            return watched;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never left its queue for a terminal status, last \
             {described}"
        );
        tokio::time::sleep(SNAPSHOT_POLL_INTERVAL).await;
    }
}

/// Assert that the job the watch followed was really PLACED: a placement starts a job attempt
/// carrying the ARN of the task the job was placed as ("each container attempt receives a task
/// ARN when they reach the `STARTING` status").
fn assert_placed(case: &str, watched: &Watched) {
    let attempts = watched.settled["attempts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let placed = attempts
        .iter()
        .any(|attempt| attempt["container"]["taskArn"].as_str().is_some());
    assert!(
        placed,
        "case {case:?}: expected the job to have been placed as a task, got {}",
        watched.settled
    );
}

/// The template the issue's deployment shape declares: a MANAGED compute environment whose
/// compute resources are Amazon EC2 instances it maintains NONE of, and an `ENABLED` queue
/// ordered onto it, with both ARNs as outputs.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ComputeEnvironment": {
                "Type": "AWS::Batch::ComputeEnvironment",
                "Properties": {
                    "Type": "MANAGED",
                    "ServiceRole": format!(
                        "arn:aws:iam::{}:role/service-role/AWSBatchServiceRole",
                        engine::EMULATOR_ACCOUNT_ID,
                    ),
                    "ComputeResources": {
                        "Type": "EC2",
                        "MinvCpus": 0,
                        "MaxvCpus": 2,
                        "InstanceTypes": ["optimal"],
                        "Subnets": ["subnet-0123456789abcdef0"],
                        "SecurityGroupIds": ["sg-0123456789abcdef0"],
                        "InstanceRole": format!(
                            "arn:aws:iam::{}:instance-profile/ecsInstanceRole",
                            engine::EMULATOR_ACCOUNT_ID,
                        ),
                    },
                },
            },
            "JobQueue": {
                "Type": "AWS::Batch::JobQueue",
                "Properties": {
                    "Priority": 1,
                    "State": "ENABLED",
                    "ComputeEnvironmentOrder": [
                        {"Order": 1, "ComputeEnvironment": {"Ref": "ComputeEnvironment"}},
                    ],
                },
            },
        },
        "Outputs": {
            "ComputeEnvironmentArn": {"Value": {"Ref": "ComputeEnvironment"}},
            "JobQueueArn": {"Value": {"Ref": "JobQueue"}},
        },
    })
    .to_string()
}

/// RULE, on the estate CloudFormation declares: a job submitted to the stack's queue is
/// reported at the front of that queue by a client polling `GetJobQueueSnapshot` while the
/// stack's MANAGED EC2 compute environment provisions the capacity for it, the environment
/// reports the `desiredvCpus` that demand raised, and the job is then placed and runs to an
/// end — with the capacity scaled back to the vCPUs the environment maintains once it has.
#[tokio::test]
async fn a_job_on_a_cloudformation_deployed_ec2_queue_is_reported_at_the_front_of_its_queue() {
    let router = build_router();
    let case = "a CloudFormation-deployed MANAGED EC2 estate";
    let stack = "batch-ec2-capacity";

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );

    let deadline = Instant::now() + DEADLINE;
    let described = loop {
        let (status, body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            assert_eq!(
                reported, "CREATE_COMPLETE",
                "case {case:?}: expected the stack to complete, got {body}"
            );
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled, last status {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let outputs = outputs_of(&described);
    let queue = outputs
        .get("JobQueueArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no JobQueueArn output in {described}"));
    let environment = outputs
        .get("ComputeEnvironmentArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ComputeEnvironmentArn output in {described}"));

    // The queue the stack published is one a client may submit to, ordered onto the compute
    // environment the stack declared.
    let reported = until_queue_valid(&router, case, &queue).await;
    assert_eq!(
        reported["state"].as_str(),
        Some("ENABLED"),
        "case {case:?}: expected the declared state, got {reported}"
    );
    assert_eq!(
        reported["computeEnvironmentOrder"][0]["computeEnvironment"].as_str(),
        Some(environment.as_str()),
        "case {case:?}: expected the queue to order the stack's environment, got {reported}"
    );

    let definition = registered_definition(&router, case, "batch-ec2-capacity-jd", Some("1")).await;
    let submitted = batch(
        &router,
        case,
        "/v1/submitjob",
        &json!({
            "jobName": "batch-ec2-capacity-job",
            "jobQueue": &queue,
            "jobDefinition": &definition,
        }),
    )
    .await;
    let job_arn = submitted["jobArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobArn, got {submitted}"))
        .to_string();
    let job_id = submitted["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobId, got {submitted}"))
        .to_string();

    let watched =
        watch_out_of_the_queue(&router, case, &queue, &environment, &job_arn, &job_id).await;
    assert!(
        watched.reads_listing_the_job >= 1,
        "case {case:?}: expected a client polling once a second to find the outstanding job at \
         the front of the queue, got {} read(s) listing it and the job {}",
        watched.reads_listing_the_job,
        watched.settled
    );
    // And the service's own instants say the job really was held for the capacity, rather than
    // caught in a window too short for a client to poll.
    let lifetime = reported_lifetime_millis(case, &watched.settled);
    assert!(
        lifetime >= WAIT_FOR_CAPACITY_MILLIS,
        "case {case:?}: expected the job to be held for the capacity its environment had to \
         provision (at least {WAIT_FOR_CAPACITY_MILLIS} ms between createdAt and stoppedAt), got \
         {lifetime} ms in {}",
        watched.settled
    );
    assert!(
        watched
            .earliest_time_at_position
            .is_some_and(|at| at > 0),
        "case {case:?}: expected the snapshot entry to carry the instant the job reached its \
         position, got {:?}",
        watched.earliest_time_at_position
    );
    assert_eq!(
        watched.desiredv_cpus_while_waiting,
        Some(1),
        "case {case:?}: expected the environment to have scaled out to the vCPU the queued job \
         asks for, got {:?}",
        watched.desiredv_cpus_while_waiting
    );
    assert_placed(case, &watched);

    // Nothing of the queue's is outstanding any more, so the capacity scaled out for it is
    // back to the vCPUs the environment maintains.
    let settled = described_environment(&router, case, &environment).await;
    assert_eq!(
        settled["computeResources"]["desiredvCpus"].as_i64(),
        Some(0),
        "case {case:?}: expected the environment to scale back to the vCPUs it maintains, got \
         {settled}"
    );
}

/// One row of the capacity table: the compute environment behind the queue, what the job asks
/// for, and what the rule says the job's wait and the environment's report must then be.
struct Row {
    case: &'static str,
    /// The `computeResources` the environment is created with.
    resources: fn() -> Value,
    /// The `VCPU` the job definition reserves, or `None` for a container that states no
    /// `resourceRequirements`.
    vcpus: Option<&'static str>,
    /// Whether the environment has to PROVISION capacity for the job, and so whether a client
    /// polling the queue once a second finds the job outstanding.
    waits_for_capacity: bool,
    /// The `desiredvCpus` the environment must report while the job is outstanding — `None`
    /// for an environment whose capacity it provisions none of.
    desiredv_cpus: Option<i64>,
}

/// The compute resources of a MANAGED EC2 environment: `type` and the required `maxvCpus`,
/// the vCPUs it maintains, and the networking such an environment is created with.
fn ec2_resources(maintained: i64) -> Value {
    json!({
        "type": "EC2",
        "minvCpus": maintained,
        "maxvCpus": 16,
        "instanceTypes": ["optimal"],
        "subnets": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "an EC2 environment that maintains no vCPUs",
        resources: || ec2_resources(0),
        vcpus: Some("1"),
        waits_for_capacity: true,
        desiredv_cpus: Some(1),
    },
    Row {
        case: "an EC2 environment that maintains no vCPUs and a job asking for two",
        resources: || ec2_resources(0),
        vcpus: Some("2"),
        waits_for_capacity: true,
        desiredv_cpus: Some(2),
    },
    Row {
        case: "an EC2 environment that maintains no vCPUs and a container stating no \
               resourceRequirements",
        resources: || ec2_resources(0),
        vcpus: None,
        waits_for_capacity: true,
        desiredv_cpus: Some(1),
    },
    Row {
        case: "an EC2 environment maintaining more vCPUs than the job asks for",
        resources: || ec2_resources(2),
        vcpus: Some("1"),
        waits_for_capacity: false,
        // `desiredvCpus` is held "between the minimum and maximum values", so a demand below
        // what the environment maintains leaves it at that minimum.
        desiredv_cpus: Some(2),
    },
    Row {
        case: "a Fargate environment, whose capacity is no instance of this account's",
        resources: || {
            json!({
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroupIds": ["sg-0123456789abcdef0"],
            })
        },
        vcpus: Some("1"),
        waits_for_capacity: false,
        desiredv_cpus: None,
    },
];

/// RULE, over the capacity behind the queue: a job waits in its queue — where the snapshot
/// reports it — exactly while the compute environment behind that queue has to provision the
/// capacity to hold it, and the environment reports what it scaled out to; capacity that is
/// already running, or that is no instance of this account's at all, holds nothing up.
#[tokio::test]
async fn how_long_a_job_waits_in_its_queue_is_decided_by_the_capacity_behind_it() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let environment = format!("ce-capacity-{index}");
        let queue_name = format!("jq-capacity-{index}");

        let created = batch(
            &router,
            case,
            "/v1/createcomputeenvironment",
            &json!({
                "computeEnvironmentName": &environment,
                "type": "MANAGED",
                "state": "ENABLED",
                "serviceRole": format!(
                    "arn:aws:iam::{}:role/service-role/AWSBatchServiceRole",
                    engine::EMULATOR_ACCOUNT_ID,
                ),
                "computeResources": (row.resources)(),
            }),
        )
        .await;
        let environment_arn = created["computeEnvironmentArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {created}"))
            .to_string();
        let created = batch(
            &router,
            case,
            "/v1/createjobqueue",
            &json!({
                "jobQueueName": &queue_name,
                "state": "ENABLED",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment_arn}],
            }),
        )
        .await;
        let queue = created["jobQueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {created}"))
            .to_string();
        until_queue_valid(&router, case, &queue).await;

        let definition = registered_definition(
            &router,
            case,
            &format!("jd-capacity-{index}"),
            row.vcpus,
        )
        .await;
        let submitted = batch(
            &router,
            case,
            "/v1/submitjob",
            &json!({
                "jobName": format!("job-capacity-{index}"),
                "jobQueue": &queue,
                "jobDefinition": &definition,
            }),
        )
        .await;
        let job_arn = submitted["jobArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a jobArn, got {submitted}"))
            .to_string();
        let job_id = submitted["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a jobId, got {submitted}"))
            .to_string();

        let watched = watch_out_of_the_queue(
            &router,
            case,
            &queue,
            &environment_arn,
            &job_arn,
            &job_id,
        )
        .await;
        let lifetime = reported_lifetime_millis(case, &watched.settled);
        if row.waits_for_capacity {
            assert!(
                watched.reads_listing_the_job >= 1,
                "case {case:?}: expected a client polling once a second to find the job \
                 outstanding while its capacity is provisioned, got {} read(s) listing it and \
                 the job {}",
                watched.reads_listing_the_job,
                watched.settled
            );
            assert!(
                lifetime >= WAIT_FOR_CAPACITY_MILLIS,
                "case {case:?}: expected the job to be held for the capacity its environment had \
                 to provision (at least {WAIT_FOR_CAPACITY_MILLIS} ms between createdAt and \
                 stoppedAt), got {lifetime} ms in {}",
                watched.settled
            );
            assert_eq!(
                watched.desiredv_cpus_while_waiting, row.desiredv_cpus,
                "case {case:?}: expected the environment to report desiredvCpus {:?} while the \
                 job waited, got {:?}",
                row.desiredv_cpus, watched.desiredv_cpus_while_waiting
            );
        } else {
            assert!(
                watched.reads_listing_the_job <= 1,
                "case {case:?}: capacity that is already running holds no job in its queue, yet \
                 {} read(s) one second apart found it there; the job settled as {}",
                watched.reads_listing_the_job,
                watched.settled
            );
            assert!(
                lifetime < WAIT_FOR_CAPACITY_MILLIS,
                "case {case:?}: capacity that is already running holds a job for nothing, yet \
                 the service reports {lifetime} ms between its createdAt and its stoppedAt in {}",
                watched.settled
            );
            let reported = described_environment(&router, case, &environment_arn).await;
            assert_eq!(
                reported["computeResources"]["desiredvCpus"].as_i64(),
                row.desiredv_cpus,
                "case {case:?}: expected desiredvCpus {:?}, got {reported}",
                row.desiredv_cpus
            );
        }
        assert_placed(case, &watched);
    }
}

/// RULE, at the other end of the same list: a `computeEnvironmentOrder` is stored and walked
/// whenever a job of the queue is placed or the capacity behind it is scaled, so a create that
/// names more entries than this service keeps is refused with the modelled `ClientException`
/// (HTTP 400) rather than stored — the bound its `serviceEnvironmentOrder` twin already has.
#[tokio::test]
async fn a_queue_naming_more_compute_environments_than_this_service_stores_is_refused() {
    let router = build_router();
    let case = "a computeEnvironmentOrder one entry past the maximum";
    let environment = "ce-order-bound";

    batch(
        &router,
        case,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": environment,
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": ec2_resources(0),
        }),
    )
    .await;

    // One entry past what the store keeps, every one of them naming the one environment that
    // exists: the list's LENGTH is what is refused.
    let maximum = limits::MAX_COMPUTE_ENVIRONMENT_ORDER_ENTRIES;
    let order: Vec<Value> = (0..=maximum)
        .map(|index| json!({"order": index, "computeEnvironment": environment}))
        .collect();
    let (status, answered) = batch_call(
        &router,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "jq-order-bound",
            "state": "ENABLED",
            "priority": 1,
            "computeEnvironmentOrder": order,
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the modelled ClientException's 400, got {status} with {answered}"
    );
    let message = answered["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("computeEnvironmentOrder") && message.contains(&maximum.to_string()),
        "case {case:?}: expected the refusal to name the member and the maximum, got {answered}"
    );

    // And nothing was stored for it: a refused create leaves no queue behind.
    let described = batch(
        &router,
        case,
        "/v1/describejobqueues",
        &json!({"jobQueues": ["jq-order-bound"]}),
    )
    .await;
    assert_eq!(
        described["jobQueues"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected no job queue for a refused create, got {described}"
    );

    // One entry fewer is inside the bound, so the same request is accepted.
    let order: Vec<Value> = (0..maximum)
        .map(|index| json!({"order": index, "computeEnvironment": environment}))
        .collect();
    batch(
        &router,
        case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "jq-order-at-bound",
            "state": "ENABLED",
            "priority": 1,
            "computeEnvironmentOrder": order,
        }),
    )
    .await;

    // The same bound holds on the other way INTO the stored list: `UpdateJobQueue` replaces a
    // queue's order, so a replacement past the maximum is refused too, and the queue keeps the
    // order it had.
    let order: Vec<Value> = (0..=maximum)
        .map(|index| json!({"order": index, "computeEnvironment": environment}))
        .collect();
    let (status, answered) = batch_call(
        &router,
        "/v1/updatejobqueue",
        &json!({
            "jobQueue": "jq-order-at-bound",
            "computeEnvironmentOrder": order,
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected UpdateJobQueue to answer the modelled ClientException's 400, got \
         {status} with {answered}"
    );
    let message = answered["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("computeEnvironmentOrder") && message.contains(&maximum.to_string()),
        "case {case:?}: expected the update's refusal to name the member and the maximum, got \
         {answered}"
    );
    let described = batch(
        &router,
        case,
        "/v1/describejobqueues",
        &json!({"jobQueues": ["jq-order-at-bound"]}),
    )
    .await;
    assert_eq!(
        described["jobQueues"][0]["computeEnvironmentOrder"]
            .as_array()
            .map(Vec::len),
        Some(maximum),
        "case {case:?}: expected the refused update to leave the stored order alone, got \
         {described}"
    );
}
