// SPDX-License-Identifier: CC0-1.0

//! That a PLACED AWS Batch job SETTLES, whatever its container's declared program turns out to
//! be — driven end to end across the services that own the facts: Batch, Amazon ECS and
//! Amazon VPC on one engine.
//!
//! The RULE under test: **a job placed on the capacity behind its queue reaches a terminal
//! `JobStatus`, and that end is read off the task it was placed as — the status its container
//! exited with where the container produced one, and the detail Amazon ECS reports for a
//! container that produced NONE where it did not, with no exit code invented for it.** A
//! container's `exitCode` is "the exit code returned from the container" and
//! `AttemptContainerDetail$reason` is "a short (255 max characters) human-readable string to
//! provide additional details for a running or stopped container", which is the same detail
//! Amazon ECS publishes for the container (`Container$reason`); so a container the task reports
//! a detail for while reporting no exit status has produced the only account of its run there
//! will be, and the job's work is over: it ends `FAILED` carrying that detail. The task the job
//! was placed as is then stopped with the job, so nothing of the account's is left running for
//! a job that has ended.
//!
//! Why it matters: `JobStatus` is `[SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED,
//! FAILED]`, and every client of AWS Batch waits for one of the last two — `SubmitJob` followed
//! by `DescribeJobs` until the job settles. A job that never settles hangs that client for ever
//! instead of telling it what became of the work.
//!
//! The rows differ along what the rule generalises over: WHAT the container declares (an
//! argument vector this emulator performs, a shell program that exits non-zero, a command named
//! in no catalog of it, a command named in no catalog reached part way through a shell program,
//! and no vector at all — the program inside the image) and therefore whether the run produced
//! an exit status. Every row also asks for TAG PROPAGATION from both sides, so the task of a
//! job whose container exited is read back for the merged set as well: the tags a propagating
//! job puts on its task are observable exactly because the job settles.
//!
//! Sources: `smithy_query(query="shape",shape="JobStatus")` for the ladder's members;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for `taskArn`, `exitCode` and
//! `reason`; `smithy_query(query="doc",shape="JobDetail",member="propagateTags")` for the
//! propagation and its tie-break; `smithy_query(query="operation",operation="DescribeTasks")`
//! for `tasks`/`failures` and `smithy_query(query="doc",shape="DescribeTasksRequest",
//! member="include")` for the `TAGS` field on the Amazon ECS side.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the execution role below is named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape",shape="CEStatus,JQStatus")`.
const VALID: &str = "VALID";

/// The terminal `JobStatus` members, spelled out here rather than imported from a service
/// crate, so a wrong constant in the implementation cannot make this suite pass. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The `platformCapabilities` member a Fargate job definition carries. Source:
/// `smithy_query(query="shape",shape="PlatformCapability")`.
const FARGATE: &str = "FARGATE";

/// The `/16` this suite's VPC is carved out of, and the `/24` its one subnet takes.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDR: &str = "10.71.1.0/24";

/// The destination of a default route, which is what gives the subnet below a way off its VPC
/// — and so a way to the registry a container's image is fetched from.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The image every job definition below runs. It names a registry this emulator holds none of,
/// so nothing here is decided by the fetch: what the rows vary is the program the container
/// declares.
const IMAGE: &str = "registry.example.com/simparity/batch-settling-worker:1";

/// The tag keys the rows use: one only the job definition carries, one only the job carries,
/// and one BOTH carry — the key whose tie-break the propagation rule names.
const DEFINITION_KEY: &str = "batchSettlesFromDefinition";
const JOB_KEY: &str = "batchSettlesFromJob";
const SHARED_KEY: &str = "batchSettlesShared";

/// The values each side writes, so a merged set says which side a key came from.
const FROM_DEFINITION: &str = "written-by-the-job-definition";
const FROM_JOB: &str = "written-by-the-job";

/// How long a wait below gives the clocks: far above the spans Batch and Amazon ECS give the
/// moves a placed job's task makes, and every wait looks again for the CONDITION rather than
/// sleeping a fixed stretch, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS and Amazon EC2 over ONE shared engine, the way `core/router/src/lib.rs`
/// mounts every service: the task a placed job runs as is Amazon ECS's resource, and the subnets
/// and security groups its placement names are Amazon VPC's. Each service claims only requests
/// scoped to its own signing name.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: the router above runs with request authentication off.
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

/// One Amazon ECS call: an `awsJson1_1` service, so `POST /` under the operation's target.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ecs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as
/// XML — returned as text, which [`element`] pulls the minted ids out of.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={}&Version=2016-11-15", form_encode(action));
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
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// The network every placement below names: one VPC, one subnet in it, and the security group
/// governing the interface each task is created with.
struct Network {
    subnet: String,
    security_group: String,
}

/// Build that network through Amazon EC2's own API, so every id the placements name is one that
/// service minted and holds.
async fn network_of(router: &MultiServiceRouter) -> Network {
    let vpc = element(&ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await, "vpcId");
    // The way off the VPC, so the registry the containers' image names is reachable from the
    // subnet their tasks are placed in: every row's run waits on an image fetch first.
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", SUBNET_CIDR),
                ("AvailabilityZone", &format!("{REGION}a")),
            ],
        )
        .await,
        "subnetId",
    );
    let table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[("RouteTableId", table.as_str()), ("SubnetId", subnet.as_str())],
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", DEFAULT_ROUTE),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "batch-settling-jobs"),
                ("GroupDescription", "the group the tasks of this suite run under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnet,
        security_group,
    }
}

/// How one row's run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The container's program ran and exited with this status.
    Exited(i64),
    /// The container produced no exit status, because this emulator performed none of the
    /// program it declared: the detail the task reports for it names this word of the program.
    NothingPerformedNaming(&'static str),
    /// The container produced no exit status and the detail names no command of the program:
    /// there was none to name.
    NothingPerformed,
}

/// One row of the table: what the container declares, and how the run of it ends.
struct Row {
    case: &'static str,
    /// The container's `command`, and `None` for a definition that states none — a container
    /// whose program is the one inside its image.
    command: Option<&'static [&'static str]>,
    ends: Ends,
}

/// The table. Its rows differ along what the rule generalises over — see the module header.
const ROWS: &[Row] = &[
    Row {
        case: "an argument vector this emulator performs, exiting zero",
        command: Some(&["echo", "the batch job ran this"]),
        ends: Ends::Exited(0),
    },
    Row {
        case: "a shell program that exits non-zero",
        command: Some(&["sh", "-c", "echo leaving && exit 13"]),
        ends: Ends::Exited(13),
    },
    Row {
        case: "a vector naming a program no catalog of this emulator holds",
        command: Some(&["simparity-batch-worker", "--once"]),
        ends: Ends::NothingPerformedNaming("simparity-batch-worker"),
    },
    Row {
        // Part way through: the program runs until it reaches a command nothing performs.
        case: "a shell program reaching a program no catalog of this emulator holds",
        command: Some(&["sh", "-c", "echo starting && simparity-batch-step --now"]),
        ends: Ends::NothingPerformedNaming("simparity-batch-step"),
    },
    Row {
        // The program is the one inside the image, which this emulator starts none of.
        case: "a definition stating no command at all",
        command: None,
        ends: Ends::NothingPerformed,
    },
];

/// The tags the job definition of every row carries, and the ones its job carries: both sides
/// name one key of their own and SHARE one, which is the input the tie-break is read off.
const DEFINITION_TAGS: &[(&str, &str)] = &[
    (DEFINITION_KEY, FROM_DEFINITION),
    (SHARED_KEY, FROM_DEFINITION),
];
const JOB_TAGS: &[(&str, &str)] = &[(JOB_KEY, FROM_JOB), (SHARED_KEY, FROM_JOB)];
const MERGED_TAGS: &[(&str, &str)] = &[
    (DEFINITION_KEY, FROM_DEFINITION),
    (JOB_KEY, FROM_JOB),
    (SHARED_KEY, FROM_JOB),
];

/// One tag list as the JSON map both Batch tag members take.
fn tag_map(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// The `{key, value}` list a described Amazon ECS resource reports its tags as, read into a map
/// so a comparison does not depend on the order the entries came back in.
fn ecs_tags(found: &Value) -> BTreeMap<String, String> {
    found
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|tag| {
            Some((
                tag["key"].as_str()?.to_string(),
                tag["value"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// The same pairs as a map, for comparing with what Amazon ECS reports.
fn expected_map(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Register one row's job definition and answer its name: a FARGATE container job whose
/// `resourceRequirements` name the smallest Fargate task size, carrying the definition's tags.
async fn register(router: &MultiServiceRouter, row: &Row, index: usize) -> String {
    let name = format!("batch-settles-definition-{index}");
    let mut container = json!({
        "image": IMAGE,
        // "For jobs that run on Fargate resources, you must provide an execution role"
        // (`ContainerProperties$executionRoleArn`).
        "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
        // The public address an internet gateway carries traffic for, which is what lets the
        // task's interface reach the registry the image names.
        "networkConfiguration": {"assignPublicIp": "ENABLED"},
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
    });
    if let Some(command) = row.command {
        container["command"] = json!(command);
    }
    batch(
        router,
        row.case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": [FARGATE],
            "tags": tag_map(DEFINITION_TAGS),
            "containerProperties": container,
        }),
    )
    .await;
    name
}

/// Create the ONE compute environment and the queue in front of it that every row's job is
/// submitted to: `MANAGED` over `FARGATE` capacity, in the one subnet this suite holds. One
/// estate for the whole table, so the rows' jobs run together on one cluster rather than each
/// waiting for an estate of its own. Answers both names.
async fn capacity(router: &MultiServiceRouter, network: &Network) -> (String, String) {
    let environment = "batch-settles-ce".to_string();
    let queue = "batch-settles-queue".to_string();
    batch(
        router,
        "the estate every row is submitted to",
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": &environment,
            "type": "MANAGED",
            "computeResources": {
                "type": FARGATE,
                "maxvCpus": 16,
                "subnets": [&network.subnet],
                "securityGroupIds": [&network.security_group],
            },
        }),
    )
    .await;
    batch(
        router,
        "the estate every row is submitted to",
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": &queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;
    (environment, queue)
}

/// Look again until the compute environment `name` reports `VALID`, and answer the Amazon ECS
/// cluster it publishes then.
async fn until_environment_valid(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [name]}),
        )
        .await;
        let described = &body["computeEnvironments"][0];
        if described["status"].as_str() == Some(VALID) {
            return described["ecsClusterArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a VALID environment publishes its cluster, got {body}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the compute environment never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the job queue `name` reports `VALID`.
async fn until_queue_valid(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describejobqueues",
            &json!({"jobQueues": [name]}),
        )
        .await;
        if body["jobQueues"][0]["status"].as_str() == Some(VALID) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job queue never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one `JobDetail` `DescribeJobs` reports for `job_id`.
async fn described(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
    let jobs = body["jobs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        jobs.len(),
        1,
        "case {case:?}: expected exactly one described job for {job_id}, got {body}"
    );
    jobs[0].clone()
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer the detail it reports
/// then. Nothing here sleeps for a span: it waits for the CONDITION.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the task `arn` is one Amazon ECS no longer wants running, and answer it:
/// nothing of the account's is left running for a job that has ended.
///
/// Each look also reads the job, because reading the job plane is what drives the steps AWS
/// Batch owes Amazon ECS for its jobs — the same thing a client polling `DescribeJobs` does.
async fn until_the_task_is_not_wanted(
    router: &MultiServiceRouter,
    case: &str,
    job_id: &str,
    cluster: &str,
    arn: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = ecs(
            router,
            case,
            "DescribeTasks",
            &json!({"cluster": cluster, "tasks": [arn], "include": ["TAGS"]}),
        )
        .await;
        let task = body["tasks"][0].clone();
        if task["desiredStatus"].as_str() == Some("STOPPED") {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task of an ended job is still wanted running, last {task}"
        );
        described(router, case, job_id).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a placed Fargate job settles whatever its container's program turns out to be, and the
/// end it reports — the status its container exited with, or the detail reported for a container
/// that produced none — is the task's own. The task stays readable in the cluster, carrying the
/// tags the propagating job put on it, and is stopped with the job.
#[tokio::test]
async fn a_placed_job_settles_whatever_its_container_performed() {
    let router = router();
    let network = network_of(&router).await;

    // ONE estate for the whole table, and every row's job submitted BEFORE any of them is
    // waited on, so the spans the clocks owe them run together rather than one after another.
    let mut definitions = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        definitions.push(register(&router, row, index).await);
    }
    let (environment, queue) = capacity(&router, &network).await;
    let cluster = until_environment_valid(&router, "the estate", &environment).await;
    until_queue_valid(&router, "the estate", &queue).await;
    let mut submitted = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let answer = batch(
            &router,
            row.case,
            "/v1/submitjob",
            &json!({
                "jobName": format!("batch-settles-job-{index}"),
                "jobQueue": &queue,
                "jobDefinition": &definitions[index],
                "propagateTags": true,
                "tags": tag_map(JOB_TAGS),
            }),
        )
        .await;
        submitted.push(
            answer["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {:?}: expected a jobId in {answer}", row.case))
                .to_string(),
        );
    }

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let job_id = &submitted[index];
        // IT SETTLES: every row reaches a terminal status, which is the whole point.
        let detail = until_terminal(&router, case, job_id).await;
        let expected_status = match row.ends {
            Ends::Exited(0) => SUCCEEDED,
            _ => FAILED,
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {detail}"
        );
        let attempt = detail["attempts"][0].clone();
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !reason.is_empty(),
            "case {case:?}: expected the job to report why it ended, got {detail}"
        );
        // The exit status is the container's own, and ABSENT where the run produced none: no
        // status is invented for a container that produced nothing.
        let expected_exit = match row.ends {
            Ends::Exited(status) => Some(status),
            _ => None,
        };
        for (where_, found) in [
            ("the attempt's container", &attempt["container"]["exitCode"]),
            ("the job's container", &detail["container"]["exitCode"]),
        ] {
            assert_eq!(
                found.as_i64(),
                expected_exit,
                "case {case:?}: expected {expected_exit:?} as {where_}'s exitCode, got {found}"
            );
        }
        // The attempt's own account of the end agrees with the job's: both are projections of
        // one reading of what the attempt ended with.
        for (member, found) in [
            ("statusReason", &attempt["statusReason"]),
            ("container.reason", &attempt["container"]["reason"]),
        ] {
            assert_eq!(
                found.as_str(),
                Some(reason.as_str()),
                "case {case:?}: expected the attempt's {member} to be {reason:?}, got {found}"
            );
        }
        // Where the run produced no status, the detail NAMES the program nothing performed.
        if let Ends::NothingPerformedNaming(named) = row.ends {
            assert!(
                reason.contains(named),
                "case {case:?}: expected the end to name {named:?}, got {reason:?}"
            );
        }
        // The job stopped, and it names the task it ran as.
        assert!(
            detail["stoppedAt"].as_i64().is_some(),
            "case {case:?}: expected a stoppedAt on a job whose work is over, got {detail}"
        );
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the attempt must name the task it ran as, got {detail}")
            })
            .to_string();
        // That task is the one Amazon ECS holds, and it is no longer wanted running, because
        // the job it was placed for has ended.
        let task = until_the_task_is_not_wanted(&router, case, job_id, &cluster, &task_arn).await;
        assert_eq!(
            task["taskArn"].as_str(),
            Some(task_arn.as_str()),
            "case {case:?}: expected Amazon ECS to answer for the task the job reports, got {task}"
        );
        // The propagated set is on the task of a job whose container EXITED: such a task
        // stopped because its essential container did, so the tags the placement put on it
        // are still there to be read. A task AWS Batch had to stop itself — the task of a job
        // whose container produced no outcome — carries none by then, because `StopTask`
        // deletes them ("stops a running task. Any tags associated with the task will be
        // deleted"), so the propagation of such a row is not read off the stopped task.
        if matches!(row.ends, Ends::Exited(_)) {
            assert_eq!(
                ecs_tags(&task["tags"]),
                expected_map(MERGED_TAGS),
                "case {case:?}: expected the merged tag set on the task, got {task}"
            );
        }
    }
}
