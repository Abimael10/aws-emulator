// SPDX-License-Identifier: CC0-1.0

//! The `dependsOn` GATE of the AWS Batch job plane, driven end to end across the services
//! that own the facts: Batch, Amazon ECS, Amazon VPC and CloudWatch Logs on one engine.
//!
//! The RULE under test, in two halves that meet in one scenario:
//!
//! **A job whose `dependsOn` names other jobs is HELD in `PENDING` — echoing those ids, with
//! no `attempts` and no `startedAt` — until every one of them reaches a terminal status; it is
//! then RELEASED when all of them `SUCCEEDED` (and runs its own container, so its `startedAt`
//! is no earlier than the last dependency's `stoppedAt`), or reaches `FAILED` having never
//! been dispatched when any of them `FAILED` — no `attempts`, no `startedAt`, no
//! `container.exitCode`, and a `statusReason` explaining the dependency failure.** Sources:
//! `smithy_query(query="operation",operation="SubmitJob")` for `dependsOn`
//! (`JobDependency{jobId,type}`), `smithy_query(query="shape",shape="JobStatus")` for the
//! statuses a job takes, and `JobDetail`'s own `dependsOn` ("a list of job IDs that this job
//! depends on"), `attempts`, `startedAt`, `stoppedAt` and `statusReason`.
//!
//! **And the gate REACHES that verdict whatever duration a container's program states.** The
//! seconds in a container's `sleep` are a caller's word about how long its program took, not
//! time this emulator spends: a program that states a wait and then goes on settles as
//! promptly as one that states none, so a dependent is released, or failed, instead of being
//! held for as long as some program's words. A wait a program ENDS on is the other thing: the
//! container is waiting for something else to end it, which is how a container stays up, so it
//! is honoured — which is what makes the HOLD observable here at all, because such a
//! dependency is still `RUNNING` while its dependent waits behind it.
//!
//! Only the assembled services can pin this: the jobs are placed on a MANAGED/FARGATE compute
//! environment, each one's container RUNS AS an Amazon ECS task of that environment's cluster
//! in the subnets Amazon VPC holds, and the statuses, instants and exit codes the gate turns
//! on are read off those tasks.
//!
//! The rows differ along what the rule generalises over: HOW MANY jobs a dependent names
//! (one, two, and a chain of three where the middle job is both), WHICH way each of them ends
//! (a container that exits 0, one that exits non-zero, and a mix of the two), and WHAT the
//! containers' programs state (a wait between two steps, a wait they end on, and no wait at
//! all) — including a wait far longer than this suite's own deadline, which is what would hold
//! the gate if a stated duration were spent.
//!
//! What is NOT asserted here, because the owning crate's own suites assert it: the release of
//! a dependent whose dependency a CALLER ended (`services/service-com-amazonaws-batch-
//! awsbatchv20160810/tests/all/job_lifecycle.rs`), the `PENDING` rest of a dependent on a
//! queue with no capacity (`job_rests_in_runnable.rs`), and the per-index dependencies of an
//! array job (`list_jobs_by_array_parent.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the execution role is named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape",shape="CEStatus,JQStatus")`.
const VALID: &str = "VALID";

/// The `JobStatus` members this suite names, spelled out here rather than imported from a
/// service crate, so a wrong constant in the implementation cannot make the suite pass.
/// Source: `smithy_query(query="shape",shape="JobStatus")`.
const SUBMITTED: &str = "SUBMITTED";
pub(crate) const PENDING: &str = "PENDING";
pub(crate) const SUCCEEDED: &str = "SUCCEEDED";
pub(crate) const FAILED: &str = "FAILED";

/// The `statusReason` Amazon ECS words for a task whose essential container exited, which is
/// what AWS Batch reports for a job that RAN. A job the gate failed without dispatching it
/// must not carry it: nothing of that job ran.
pub(crate) const ESSENTIAL_CONTAINER_EXITED: &str = "Essential container in task exited";

/// The image every job definition below runs: the public gallery release this emulator holds.
pub(crate) const IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The `/16` this suite's VPC is carved out of and the `/24` its subnet takes.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDR: &str = "10.71.1.0/24";

/// The destination of a default route.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// How long a wait below gives the clocks of Batch, Amazon ECS and Amazon VPC: far above the
/// spans they owe the moves a placed job's task makes, and every wait looks again for the
/// CONDITION rather than sleeping a stretch, so a loaded machine delays a row instead of
/// breaking it.
pub(crate) const DEADLINE: Duration = Duration::from_secs(180);

/// How long one look waits before the next.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The compute environment, the queue over it and the name of this suite's one estate.
pub(crate) const ENVIRONMENT: &str = "batch-depends-ce";
pub(crate) const QUEUE: &str = "batch-depends-queue";

/// AWS Batch, Amazon ECS, Amazon EC2 and CloudWatch Logs over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed job runs as is Amazon
/// ECS's resource, the interface it is reached on is Amazon VPC's, and the stream its
/// container writes to is CloudWatch Logs'.
pub(crate) fn router() -> MultiServiceRouter {
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
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
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

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
pub(crate) async fn batch(
    router: &MultiServiceRouter,
    case: &str,
    uri: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()));
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
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

/// The network the compute environment places its tasks in: one VPC with an attached internet
/// gateway, a subnet whose route table sends the default route to that gateway (so a task
/// given a public address reaches the registry its image comes from) and the security group
/// the placement names — every id minted by Amazon EC2 itself.
pub(crate) async fn network_of(router: &MultiServiceRouter) -> (String, String) {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
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
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
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
                ("GroupName", "batch-dependency-jobs"),
                (
                    "GroupDescription",
                    "the group the tasks of this suite run under",
                ),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (subnet, security_group)
}

/// What one job's container PROGRAM is, and what it does.
///
/// The `command` of each is an argument vector naming a shell and a program, which is how a
/// job definition writes a script. They differ in WHAT the program states about its own time:
/// one states a wait and then goes on, one ends on a wait, and one states none at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Program {
    /// States a wait BETWEEN its steps and then exits 0: a duration the program states, which
    /// this emulator does not spend — the wait is far longer than [`DEADLINE`], so a job
    /// running it settles only because the duration is stated rather than spent.
    StatesAWaitThenSucceeds,
    /// The same, exiting non-zero: the status this container exited with is the status its job
    /// ends on.
    StatesAWaitThenFails,
    /// ENDS on a wait, which is how a container stays up: the job running it is `RUNNING` for
    /// as long as its program says before exiting 0. This is the dependency a row uses when it
    /// wants the HOLD to be observable.
    EndsOnAWait,
    /// States no wait at all and exits 0.
    ExitsAtOnce,
}

impl Program {
    /// The job definition name this program is registered under.
    fn definition(self) -> &'static str {
        match self {
            Self::StatesAWaitThenSucceeds => "jd-states-a-wait-then-succeeds",
            Self::StatesAWaitThenFails => "jd-states-a-wait-then-fails",
            Self::EndsOnAWait => "jd-ends-on-a-wait",
            Self::ExitsAtOnce => "jd-exits-at-once",
        }
    }

    /// The container's `command`.
    fn command(self) -> Vec<&'static str> {
        match self {
            // 600 s is far above this suite's own deadline: a run that SPENT the wait could
            // never settle inside it, so the row that depends on this job proves the wait is
            // stated rather than spent.
            Self::StatesAWaitThenSucceeds => vec!["/bin/sh", "-c", "sleep 600; exit 0"],
            Self::StatesAWaitThenFails => vec!["/bin/sh", "-c", "sleep 900; exit 9"],
            // Three seconds is long enough for a dependent submitted beside this job to be
            // seen waiting behind it (one span of the job plane's clock), and it is the
            // program's LAST word, so it is honoured.
            Self::EndsOnAWait => vec!["/bin/sh", "-c", "echo holding the container up; sleep 3"],
            Self::ExitsAtOnce => vec!["/bin/sh", "-c", "exit 0"],
        }
    }

    /// The `JobStatus` a job running this program comes to rest in, and the exit status its
    /// container produced.
    fn ends(self) -> (&'static str, i64) {
        match self {
            Self::StatesAWaitThenFails => (FAILED, 9),
            _ => (SUCCEEDED, 0),
        }
    }
}

/// Every program a row names, registered once each.
const PROGRAMS: [Program; 4] = [
    Program::StatesAWaitThenSucceeds,
    Program::StatesAWaitThenFails,
    Program::EndsOnAWait,
    Program::ExitsAtOnce,
];

/// One row: the jobs a dependent waits on, what that dependent runs, and how the gate ends it.
struct Row {
    case: &'static str,
    /// The programs of the jobs the dependent names in `dependsOn`, in submission order. The
    /// FIRST one is the job whose own run the hold is observed against.
    dependencies: &'static [Program],
    /// What the dependent itself runs, once it is released.
    dependent: Program,
    /// Whether the dependent is RELEASED (every dependency succeeded) or FAILED by the gate
    /// without ever being dispatched (one of them failed).
    released: bool,
}

/// The table. Its rows differ along what the rule generalises over — see the module header.
const ROWS: &[Row] = &[
    Row {
        case: "one dependency that succeeds",
        dependencies: &[Program::EndsOnAWait],
        dependent: Program::StatesAWaitThenSucceeds,
        released: true,
    },
    Row {
        case: "one dependency that fails",
        dependencies: &[Program::StatesAWaitThenFails],
        dependent: Program::StatesAWaitThenSucceeds,
        released: false,
    },
    Row {
        case: "two dependencies that both succeed",
        dependencies: &[Program::EndsOnAWait, Program::ExitsAtOnce],
        dependent: Program::ExitsAtOnce,
        released: true,
    },
    Row {
        case: "two dependencies, the second of which fails",
        dependencies: &[Program::EndsOnAWait, Program::StatesAWaitThenFails],
        dependent: Program::ExitsAtOnce,
        released: false,
    },
    Row {
        case: "a dependent whose own container fails",
        dependencies: &[Program::EndsOnAWait],
        dependent: Program::StatesAWaitThenFails,
        released: true,
    },
];

/// Register the job definition `name` running `command`: a FARGATE container job whose
/// `resourceRequirements` name the smallest Fargate task size and whose `networkConfiguration`
/// asks for the public address the internet gateway carries traffic for.
pub(crate) async fn register_definition(
    router: &MultiServiceRouter,
    name: &str,
    command: &[&str],
) {
    batch(
        router,
        "setup",
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": {
                "image": IMAGE,
                "command": command,
                // "For jobs that run on Fargate resources, you must provide an execution role"
                // (`ContainerProperties$executionRoleArn`).
                "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
                "resourceRequirements": [
                    {"type": "VCPU", "value": "0.25"},
                    {"type": "MEMORY", "value": "512"},
                ],
                "networkConfiguration": {"assignPublicIp": "ENABLED"},
            },
        }),
    )
    .await;
}

/// Register the job definition of `program` ([`register_definition`]).
async fn register(router: &MultiServiceRouter, program: Program) {
    register_definition(router, program.definition(), &program.command()).await;
}

/// Create the compute environment and the queue in front of it, and poll both to `VALID`.
pub(crate) async fn estate(router: &MultiServiceRouter, subnet: &str, security_group: &str) {
    batch(
        router,
        "setup",
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": ENVIRONMENT,
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": [subnet],
                "securityGroupIds": [security_group],
            },
        }),
    )
    .await;
    batch(
        router,
        "setup",
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": QUEUE,
            "priority": 1,
            "state": "ENABLED",
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": ENVIRONMENT}],
        }),
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let environments = batch(
            router,
            "setup",
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [ENVIRONMENT]}),
        )
        .await;
        let queues = batch(
            router,
            "setup",
            "/v1/describejobqueues",
            &json!({"jobQueues": [QUEUE]}),
        )
        .await;
        if environments["computeEnvironments"][0]["status"].as_str() == Some(VALID)
            && queues["jobQueues"][0]["status"].as_str() == Some(VALID)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment and the queue must both reach {VALID}, last \
             {environments} and {queues}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Submit one job running the definition `definition`, waiting on `depends_on`; answers its
/// job id.
pub(crate) async fn submit(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &str,
    depends_on: &[String],
) -> String {
    let mut body = json!({
        "jobName": name,
        "jobQueue": QUEUE,
        "jobDefinition": definition,
    });
    if !depends_on.is_empty() {
        body["dependsOn"] = Value::Array(
            depends_on
                .iter()
                .map(|id| json!({"jobId": id}))
                .collect(),
        );
    }
    let answered = batch(router, case, "/v1/submitjob", &body).await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobId in {answered}"))
        .to_string()
}

/// The one `JobDetail` `DescribeJobs` reports for `job_id`.
pub(crate) async fn described(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
    let jobs = body["jobs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        jobs.len(),
        1,
        "case {case:?}: expected exactly one described job for {job_id}, got {body}"
    );
    jobs[0].clone()
}

/// Whether `status` is one of the two terminal `JobStatus` members.
pub(crate) fn terminal(status: &str) -> bool {
    status == SUCCEEDED || status == FAILED
}

/// Look again until `job_id` reports a terminal status, and answer the detail it reports then.
/// Waits for the CONDITION, never for a span.
pub(crate) async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        if terminal(detail["status"].as_str().unwrap_or_default()) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `job_id` has LEFT `SUBMITTED`, and answer the first detail that reports
/// another status beside the status its first dependency was in at that moment.
pub(crate) async fn until_left_submitted(
    router: &MultiServiceRouter,
    case: &str,
    job_id: &str,
    first_dependency: &str,
) -> (Value, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        if detail["status"].as_str() != Some(SUBMITTED) {
            let dependency = described(router, case, first_dependency).await;
            return (
                detail,
                dependency["status"].as_str().unwrap_or_default().to_string(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the dependent never left {SUBMITTED}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The epoch-millisecond member `name` of `detail`, or `None` where it is absent.
pub(crate) fn instant(detail: &Value, name: &str) -> Option<i64> {
    detail.get(name).and_then(Value::as_i64)
}

/// RULE: a job's `dependsOn` holds it in `PENDING` until every job it names is terminal, then
/// releases it on success and fails it — undispatched — on failure, whatever duration the
/// containers' programs state.
#[tokio::test]
async fn a_dependents_gate_holds_then_releases_or_fails_it() {
    let router = router();
    let (subnet, security_group) = network_of(&router).await;
    estate(&router, &subnet, &security_group).await;
    for program in PROGRAMS {
        register(&router, program).await;
    }

    // Every row's jobs are submitted BEFORE any of them is waited on, so the spans the clocks
    // owe them run together rather than one after another.
    let mut dependencies: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, String> = BTreeMap::new();
    for (index, row) in ROWS.iter().enumerate() {
        let mut ids = Vec::new();
        for (position, program) in row.dependencies.iter().enumerate() {
            ids.push(
                submit(
                    &router,
                    row.case,
                    &format!("dependency-{index}-{position}"),
                    program.definition(),
                    &[],
                )
                .await,
            );
        }
        let dependent = submit(
            &router,
            row.case,
            &format!("dependent-{index}"),
            row.dependent.definition(),
            &ids,
        )
        .await;
        dependencies.insert(row.case, ids);
        dependents.insert(row.case, dependent);
    }
    // One CHAIN beside the table: a job that is both a dependent and a dependency, so the
    // gate has to hold the third job behind a job that is itself held.
    let chain_case = "a chain three deep";
    let first = submit(
        &router,
        chain_case,
        "chain-first",
        Program::EndsOnAWait.definition(),
        &[],
    )
    .await;
    let middle = submit(
        &router,
        chain_case,
        "chain-middle",
        Program::StatesAWaitThenSucceeds.definition(),
        std::slice::from_ref(&first),
    )
    .await;
    let last = submit(
        &router,
        chain_case,
        "chain-last",
        Program::ExitsAtOnce.definition(),
        std::slice::from_ref(&middle),
    )
    .await;

    // THE HOLD, for every dependent at once and before anything is waited on: a dependent
    // leaves `SUBMITTED` for `PENDING` — not for `RUNNABLE` — while the job it waits on is
    // still unfinished, it echoes every id it waits on, and nothing of a run is reported for
    // it. The job each hold is observed against is one whose program ENDS on a wait, so it is
    // still running by the time this walk reaches its row.
    for (case, dependent_id, waited_on) in ROWS
        .iter()
        .map(|row| (row.case, &dependents[row.case], dependencies[row.case].clone()))
        .chain([(chain_case, &last, vec![middle.clone()])])
    {
        let (held, dependency_status) =
            until_left_submitted(&router, case, dependent_id, &waited_on[0]).await;
        assert_eq!(
            held["status"].as_str(),
            Some(PENDING),
            "case {case:?}: a dependent whose dependencies are unsatisfied is held in \
             {PENDING}, got {held}"
        );
        assert!(
            !terminal(&dependency_status),
            "case {case:?}: the job the dependent waits on must still have been unfinished \
             while the dependent was held, got {dependency_status:?} for it"
        );
        let echoed: Vec<String> = held["dependsOn"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["jobId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            echoed, waited_on,
            "case {case:?}: the held job must echo the ids it waits on, got {held}"
        );
        assert_eq!(
            instant(&held, "startedAt"),
            None,
            "case {case:?}: a held job has not started, so it reports no startedAt, got {held}"
        );
        assert_eq!(
            held["attempts"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a held job has had no attempt, so its attempts are the empty \
             list, got {held}"
        );
    }

    for row in ROWS {
        let case = row.case;
        let waited_on = &dependencies[case];
        let dependent_id = &dependents[case];

        // THE DEPENDENCIES. Each one settles on its own container's exit status, whatever its
        // program stated about its own time — the row's first one, which the hold was observed
        // against, ENDS on a wait and so really ran for it.
        let mut latest_stop = 0_i64;
        for (position, program) in row.dependencies.iter().enumerate() {
            let detail = until_terminal(&router, case, &waited_on[position]).await;
            let (status, exit) = program.ends();
            assert_eq!(
                detail["status"].as_str(),
                Some(status),
                "case {case:?}: dependency {position} runs {program:?}, so it must reach \
                 {status}, got {detail}"
            );
            assert_eq!(
                detail["container"]["exitCode"].as_i64(),
                Some(exit),
                "case {case:?}: dependency {position} must report the status its container \
                 exited with, got {detail}"
            );
            let stopped = instant(&detail, "stoppedAt").unwrap_or_else(|| {
                panic!("case {case:?}: a job that ran reports stoppedAt, got {detail}")
            });
            latest_stop = latest_stop.max(stopped);
            if *program == Program::EndsOnAWait {
                let started = instant(&detail, "startedAt").unwrap_or_else(|| {
                    panic!("case {case:?}: a job that ran reports startedAt, got {detail}")
                });
                // A wait the program ENDS on is honoured, so the run really covers it: the
                // bound is far under the three seconds the program states, which is what
                // makes it safe on a loaded host.
                assert!(
                    stopped - started >= 1_000,
                    "case {case:?}: a container that ends on a wait stays up for it, so its \
                     job's run must span it, got startedAt {started} and stoppedAt {stopped}"
                );
            }
        }

        // THE VERDICT. Released once they have all succeeded, or failed without ever being
        // dispatched when one of them failed.
        let settled = until_terminal(&router, case, dependent_id).await;
        let reason = settled["statusReason"].as_str().unwrap_or_default().to_string();
        if row.released {
            let (status, exit) = row.dependent.ends();
            assert_eq!(
                settled["status"].as_str(),
                Some(status),
                "case {case:?}: a released dependent runs its own container, so it ends \
                 {status}, got {settled}"
            );
            assert_eq!(
                settled["container"]["exitCode"].as_i64(),
                Some(exit),
                "case {case:?}: a released dependent reports the status its container exited \
                 with, got {settled}"
            );
            assert_eq!(
                settled["attempts"].as_array().map(Vec::len),
                Some(1),
                "case {case:?}: a released dependent is placed once, so it reports one \
                 attempt, got {settled}"
            );
            let started = instant(&settled, "startedAt").unwrap_or_else(|| {
                panic!("case {case:?}: a dependent that ran reports startedAt, got {settled}")
            });
            assert!(
                started >= latest_stop,
                "case {case:?}: a dependent starts no earlier than the last dependency \
                 stopped, got startedAt {started} against {latest_stop}"
            );
        } else {
            assert_eq!(
                settled["status"].as_str(),
                Some(FAILED),
                "case {case:?}: a dependent whose dependency failed reaches {FAILED}, got \
                 {settled}"
            );
            assert_eq!(
                settled["attempts"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: a dependent the gate failed was never dispatched, so it \
                 reports no attempt, got {settled}"
            );
            assert_eq!(
                instant(&settled, "startedAt"),
                None,
                "case {case:?}: a dependent the gate failed never started, got {settled}"
            );
            assert!(
                settled["container"]["exitCode"].is_null(),
                "case {case:?}: no exit code is reported for a container that never ran, got \
                 {settled}"
            );
            assert!(
                !reason.is_empty() && reason != ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: the failure must explain the dependency rather than report a \
                 container that exited, got {reason:?}"
            );
        }
    }

    // THE CHAIN. The middle job is held behind the first and the last behind the middle, so
    // each one starts after the one before it stopped.
    let mut stops = Vec::new();
    for (name, job_id) in [("first", &first), ("middle", &middle), ("last", &last)] {
        let detail = until_terminal(&router, chain_case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(SUCCEEDED),
            "case {chain_case:?}: the {name} job of the chain succeeds, got {detail}"
        );
        let started = instant(&detail, "startedAt").unwrap_or_else(|| {
            panic!("case {chain_case:?}: the {name} job ran, so it reports startedAt, got {detail}")
        });
        let stopped = instant(&detail, "stoppedAt").unwrap_or_else(|| {
            panic!("case {chain_case:?}: the {name} job ran, so it reports stoppedAt, got {detail}")
        });
        if let Some(previous) = stops.last() {
            assert!(
                started >= *previous,
                "case {chain_case:?}: the {name} job starts no earlier than the job it \
                 depended on stopped, got startedAt {started} against {previous}"
            );
        }
        stops.push(stopped);
    }
    // The middle job of the chain was both held and released, so it ran its own container:
    // one attempt, which a job the gate had failed would not have.
    let middle_detail = described(&router, chain_case, &middle).await;
    assert_eq!(
        middle_detail["attempts"].as_array().map(Vec::len),
        Some(1),
        "case {chain_case:?}: the released middle job was placed once, got {middle_detail}"
    );
}
