// SPDX-License-Identifier: CC0-1.0

//! WHO creates the log group an AWS Batch job's container output is delivered into, and what
//! that decides about whether the job can run at all.
//!
//! RULE: the log group a compute service holds for its own workloads' output is created by
//! that service, before the workload's containers are launched — for AWS Batch that is
//! `/aws/batch/job`, "the log group for Batch jobs is `/aws/batch/job`"
//! (`AttemptContainerDetail$logStreamName`) — so the workload's log driver is configured to
//! DELIVER into it rather than to create it, and the job's execution role is never required
//! to hold the group-creation permission to run. A group the job's OWN `logConfiguration`
//! asks the driver to create (`awslogs-create-group`) is the driver's creation, made as that
//! execution role ("the task execution role that grants the Amazon ECS container agent
//! permission to make Amazon Web Services API calls on your behalf",
//! `TaskDefinition$executionRoleArn`), so a role that may not create it fails the job to
//! start; and the delivery itself — the STREAM the driver creates in the group — is made as
//! that role either way, so a role allowed no CloudWatch Logs action fails the job to start
//! whoever created the group.
//!
//! The rows span what the rule generalises over: WHICH group the configuration delivers into
//! (the service's own default, one the configuration has the driver create, one a client
//! created first) against WHAT the execution role is allowed (the driver's delivery actions
//! alone, those plus the group creation, and none of them) — the boundary being which of the
//! driver's two creations each row's role has to be allowed to make.
//!
//! It lives beside the router because the stores the rule spans are four services': the job,
//! its queue and its compute environment are AWS Batch's, the task the job runs as is Amazon
//! ECS's, the execution role and the documents in force on it are IAM's, and the log group
//! and its streams are CloudWatch Logs'. Every fact is created through the owning service's
//! own API and every value asserted is read back through `DescribeJobs`,
//! `DescribeLogGroups` and `DescribeLogStreams`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region of every ARN minted for
/// them.
const REGION: &str = "us-east-2";

/// The account this emulator mints its ARNs under, which every role and group ARN below is
/// named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape",shape="CEStatus,JQStatus")`.
const VALID: &str = "VALID";

/// The terminal `JobStatus` members, spelled out here rather than imported from a service
/// crate, so a wrong constant in the implementation cannot make this suite pass. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The log group AWS Batch delivers a job container's output to when the job's configuration
/// names none: "the log group for Batch jobs is `/aws/batch/job`"
/// (`AttemptContainerDetail$logStreamName`).
const DEFAULT_JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The image every job definition below runs, and the program its container runs: one line
/// and exit 0, so a job whose containers were launched at all ends `SUCCEEDED`.
const IMAGE: &str = "registry.example.com/simparity/batch-logger:1";
const PROGRAM: [&str; 2] = ["echo", "the batch job ran this"];

/// The `/16` this suite's VPC is carved out of, the `/24` its subnet takes, and the
/// destination of the default route that gives the subnet a way to the registry.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDR: &str = "10.71.1.0/24";
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The actions the `awslogs` driver performs under the execution role: it creates the stream
/// it delivers to, writes the container's lines into it, and creates the log group itself
/// when the configuration asked it to (IAM Service Authorization Reference, "Actions defined
/// by Amazon CloudWatch Logs").
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";
const CREATE_LOG_GROUP: &str = "logs:CreateLogGroup";

/// How the container agent's account of a logger it could not validate OPENS and CLOSES, and
/// the class a refused call carries — spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a row pass. The step the agent retried is the
/// stream creation, whichever of the calls that creation takes is the one that failed.
const LOGGER_FAILURE_OPENING: &str =
    "ResourceInitializationError: failed to validate logger args: create stream has been \
     retried 1 times: failed to create Cloudwatch log ";
const LOGGER_FAILURE_CLOSING: &str = ": exceeded maximum number of attempts, 1";
const ACCESS_DENIED: &str = "AccessDeniedException";

/// How often a poll re-reads the emulator, and how long one keeps trying before it gives up.
/// Generous: every transition this suite waits for is one of three services' spans, and the
/// suite runs beside every other test of the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(60);
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// What one row's execution role is allowed, as one identity-based document of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The driver's delivery actions alone: nothing in force allows a group creation.
    DeliveryOnly,
    /// The delivery actions AND the group creation, on every resource.
    MayCreateGroup,
    /// No CloudWatch Logs action at all: the role may not even create the stream.
    NoLogsAction,
}

/// Which group a row's job delivers into, and who makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The job states no `logConfiguration`, so its output goes to the group AWS Batch holds
    /// for job logs, which that service creates itself.
    HeldByTheService,
    /// The configuration names a group of the caller's and asks the DRIVER to create it.
    MadeByTheDriver,
    /// The configuration names a group a client created before the job ran, and asks the
    /// driver to create nothing.
    MadeByTheCaller,
}

/// What one row's job ends as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The logger was initialised, so the container ran and the job succeeded.
    Runs,
    /// The logger could not be initialised: the job failed to start, on the creation of this
    /// resource kind (the word the agent's account names it with), refused for this action.
    FailsToStart {
        resource_kind: &'static str,
        action: &'static str,
    },
}

/// One row: where the job's output is delivered, what its execution role may do, and what the
/// job ends as.
struct Row {
    case: &'static str,
    destination: Destination,
    grant: Grant,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        // The regression this suite pins: the group is the SERVICE's, so a role holding only
        // the delivery actions — what AWS's own task execution policy grants — runs the job.
        case: "the service's own log group, with a role allowed only the delivery actions",
        destination: Destination::HeldByTheService,
        grant: Grant::DeliveryOnly,
        outcome: Outcome::Runs,
    },
    Row {
        case: "the service's own log group, with a role that may also create groups",
        destination: Destination::HeldByTheService,
        grant: Grant::MayCreateGroup,
        outcome: Outcome::Runs,
    },
    Row {
        // The delivery itself is still the role's: the group being the service's does not
        // stand in for the stream the driver has to create in it.
        case: "the service's own log group, with a role allowed no CloudWatch Logs action",
        destination: Destination::HeldByTheService,
        grant: Grant::NoLogsAction,
        outcome: Outcome::FailsToStart {
            resource_kind: "stream",
            action: CREATE_LOG_STREAM,
        },
    },
    Row {
        // A group the configuration asked the DRIVER to create is the driver's creation, made
        // as the execution role, so this row's role has to be allowed it — and is not.
        case: "a group the configuration has the driver create, with a delivery-only role",
        destination: Destination::MadeByTheDriver,
        grant: Grant::DeliveryOnly,
        outcome: Outcome::FailsToStart {
            resource_kind: "group",
            action: CREATE_LOG_GROUP,
        },
    },
    Row {
        case: "a group the configuration has the driver create, with a role that may create it",
        destination: Destination::MadeByTheDriver,
        grant: Grant::MayCreateGroup,
        outcome: Outcome::Runs,
    },
    Row {
        // A group that is already there is never created, so nothing is decided against this
        // row's role beyond the stream it delivers through.
        case: "a group the caller created, which the driver only delivers into",
        destination: Destination::MadeByTheCaller,
        grant: Grant::DeliveryOnly,
        outcome: Outcome::Runs,
    },
];

/// The log group row `index` delivers into: the service's own for a row that states no
/// configuration, and otherwise one of the row's own, so no row's delivery meets another's.
fn group_of(index: usize, row: &Row) -> String {
    match row.destination {
        Destination::HeldByTheService => DEFAULT_JOB_LOG_GROUP.to_string(),
        _ => format!("/emulator/batch/job-logs-{index}"),
    }
}

/// The name of row `index`'s execution role.
fn role_name_of(index: usize) -> String {
    format!("batchLoggerRole{index}")
}

/// AWS Batch, Amazon ECS, Amazon EC2, IAM and CloudWatch Logs over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed job runs as is Amazon
/// ECS's resource, the subnet it runs in is Amazon VPC's, the execution role the launch is
/// decided against is IAM's, and the group its container's output goes to is CloudWatch
/// Logs'.
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
        Arc::clone(&engine),
    ));
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
/// and signature are not credentials: the router above runs with request authentication off.
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

/// One `awsJson1_1` call to the service signing as `service`, under `prefix.<operation>`,
/// which must answer 200.
async fn json_rpc(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(service))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service} {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    json_rpc(router, case, "logs", "Logs_20140328", operation, body).await
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as
/// XML — returned as text, which `element` pulls the minted ids out of.
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

/// One awsQuery call to IAM that must succeed. The role and the documents in force on it are
/// created through IAM's OWN API, never written into its store.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, String)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
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
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
}

/// One statement of an identity-based policy: `effect` on `actions`, for every resource.
fn statement(effect: &str, actions: &[&str]) -> Value {
    json!({"Effect": effect, "Action": actions, "Resource": "*"})
}

/// The identity-based document one row's execution role carries. Every row's role may be
/// assumed and may pull its image; what the rows vary is the CloudWatch Logs half.
fn policy_document(grant: Grant) -> String {
    let delivery = [CREATE_LOG_STREAM, PUT_LOG_EVENTS];
    let everything = [CREATE_LOG_STREAM, PUT_LOG_EVENTS, CREATE_LOG_GROUP];
    let statements = match grant {
        Grant::DeliveryOnly => vec![statement("Allow", &delivery)],
        Grant::MayCreateGroup => vec![statement("Allow", &everything)],
        // A document that allows a CloudWatch Logs action the driver never makes: the role
        // EXISTS and carries a policy this account holds, so the decision rests on it — and
        // it states nothing about either of the driver's own calls.
        Grant::NoLogsAction => vec![statement("Allow", &["logs:DescribeLogGroups"])],
    };
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create row `index`'s execution role through IAM's own API — a trust policy that lets
/// Amazon ECS assume it, and the inline document the row's grant calls for — and answer its
/// ARN.
async fn execution_role(router: &MultiServiceRouter, index: usize, row: &Row) -> String {
    let name = role_name_of(index);
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": [ECS_TASKS_PRINCIPAL]},
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string();
    iam(
        router,
        row.case,
        "CreateRole",
        &[
            ("RoleName", name.clone()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    iam(
        router,
        row.case,
        "PutRolePolicy",
        &[
            ("RoleName", name.clone()),
            ("PolicyName", "logger".to_string()),
            ("PolicyDocument", policy_document(row.grant)),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The network every compute environment below places its tasks in: one VPC with an attached
/// internet gateway and a subnet whose route table sends the default route to it, so a task
/// given a public address reaches the registry its image names.
async fn network_of(router: &MultiServiceRouter) -> (String, String) {
    let vpc = element(&ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await, "vpcId");
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
                ("GroupName", "batch-logger-jobs"),
                ("GroupDescription", "the group the tasks of this suite run under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (subnet, security_group)
}

/// Register row `index`'s job definition and answer its name: a FARGATE container job whose
/// `logConfiguration` is the one the row's destination calls for — and none at all for the
/// row delivering to the service's own group, which is the default a container job gets.
async fn register(router: &MultiServiceRouter, index: usize, row: &Row, role_arn: &str) -> String {
    let name = format!("batch-logger-def-{index}");
    let mut container = json!({
        "image": IMAGE,
        "command": PROGRAM,
        // "For jobs that run on Fargate resources, you must provide an execution role"
        // (`ContainerProperties$executionRoleArn`) — the role the image pull and the log
        // driver's own calls are made as.
        "executionRoleArn": role_arn,
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
        "networkConfiguration": {"assignPublicIp": "ENABLED"},
    });
    if row.destination != Destination::HeldByTheService {
        let mut options = json!({"awslogs-group": group_of(index, row)});
        if row.destination == Destination::MadeByTheDriver {
            options["awslogs-create-group"] = json!("true");
        }
        container["logConfiguration"] = json!({"logDriver": "awslogs", "options": options});
    }
    batch(
        router,
        row.case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": container,
        }),
    )
    .await;
    name
}

/// Create row `index`'s compute environment and the queue in front of it, and answer the
/// queue's name. The environment is `MANAGED` over `FARGATE` capacity.
async fn capacity(
    router: &MultiServiceRouter,
    index: usize,
    row: &Row,
    subnet: &str,
    security_group: &str,
) -> String {
    let environment = environment_of(index);
    let queue = format!("batch-logger-queue-{index}");
    batch(
        router,
        row.case,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": &environment,
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
        row.case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": &queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;
    queue
}

/// The name of row `index`'s compute environment.
fn environment_of(index: usize) -> String {
    format!("batch-logger-ce-{index}")
}

/// Look again until the compute environment `name` reports `VALID`.
async fn until_environment_valid(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [name]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(VALID) {
            return;
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
    let deadline = Instant::now() + POLL_BUDGET;
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

/// Look again until `job_id` reports a terminal `JobStatus`, and answer the detail it reports
/// then. Nothing here sleeps for a span: it waits for the CONDITION.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
        let detail = body["jobs"][0].clone();
        let status = detail["status"].as_str().unwrap_or_default().to_string();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether this account holds a log group named `group`, read back through
/// `DescribeLogGroups`.
async fn holds_group(router: &MultiServiceRouter, case: &str, group: &str) -> bool {
    let body = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": group}),
    )
    .await;
    body["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|held| held["logGroupName"].as_str() == Some(group))
}

/// Whether `group` holds a stream named `stream`, read back through `DescribeLogStreams`.
async fn holds_stream(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
) -> bool {
    let body = logs(
        router,
        case,
        "DescribeLogStreams",
        &json!({"logGroupName": group}),
    )
    .await;
    body["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|held| held["logStreamName"].as_str() == Some(stream))
}

/// The ARN of the session Amazon ECS initialises the logger in: an assumed-role session of
/// the task's execution role, named after the task it is for.
fn session_arn(role_name: &str, task_id: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{task_id}")
}

/// The `statusReason` a row whose logger could not be initialised must report, assembled from
/// the request and from the emulator's own records: the fixed text the agent words such a
/// stop with, around the refusal CloudWatch Logs gave for the call the driver made — the
/// session the execution role was assumed in for this task, the action, and the resource it
/// addressed.
fn expected_reason(
    resource_kind: &str,
    action: &str,
    session: &str,
    resource: &str,
) -> String {
    format!(
        "{LOGGER_FAILURE_OPENING}{resource_kind}: {ACCESS_DENIED}: User: {session} is not \
         authorized to perform: {action} on resource: {resource} because no identity-based \
         policy allows the {action} action{LOGGER_FAILURE_CLOSING}"
    )
}

/// RULE: the group a Batch job's output is delivered into is created by the service that
/// holds it — Batch's own `/aws/batch/job`, before the job's container is launched — so the
/// job's execution role is required to hold only the driver's delivery actions; a group the
/// job's configuration has the driver create is the driver's creation, made as that role.
#[tokio::test]
async fn a_batch_job_delivers_into_the_group_its_own_service_holds() {
    let router = router();
    let (subnet, security_group) = network_of(&router).await;

    // Every row's estate is created, and every job submitted, BEFORE any of them is waited
    // on, so the spans the three services' clocks owe them run together.
    let mut queues = Vec::new();
    let mut definitions = Vec::new();
    let mut roles = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_arn = execution_role(&router, index, row).await;
        definitions.push(register(&router, index, row, &role_arn).await);
        queues.push(capacity(&router, index, row, &subnet, &security_group).await);
        roles.push(role_arn);
        // The group a CLIENT created before the job ran, which the driver then only delivers
        // into.
        if row.destination == Destination::MadeByTheCaller {
            logs(
                &router,
                row.case,
                "CreateLogGroup",
                &json!({"logGroupName": group_of(index, row)}),
            )
            .await;
        }
    }
    for (index, row) in ROWS.iter().enumerate() {
        until_environment_valid(&router, row.case, &environment_of(index)).await;
        until_queue_valid(&router, row.case, &queues[index]).await;
    }
    let mut submitted = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let answer = batch(
            &router,
            row.case,
            "/v1/submitjob",
            &json!({
                "jobName": format!("batch-logger-job-{index}"),
                "jobQueue": &queues[index],
                "jobDefinition": &definitions[index],
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
        let group = group_of(index, row);
        let detail = until_terminal(&router, case, &submitted[index]).await;
        let reported = detail["statusReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let attempt = detail["attempts"][0].clone();
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        match row.outcome {
            Outcome::Runs => {
                assert_eq!(
                    detail["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: a job whose logger was initialised runs its container, got \
                     {detail}"
                );
                assert_eq!(
                    detail["container"]["exitCode"].as_i64(),
                    Some(0),
                    "case {case:?}: expected the exit status the program produced, got {detail}"
                );
                // The delivery is REAL: the group the output went to is held by CloudWatch
                // Logs, and it holds the stream the job reports for the attempt.
                assert!(
                    holds_group(&router, case, &group).await,
                    "case {case:?}: expected CloudWatch Logs to hold {group} after the job ran"
                );
                let stream = attempt["container"]["logStreamName"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a logStreamName on the attempt, got {detail}")
                    })
                    .to_string();
                assert!(
                    holds_stream(&router, case, &group, &stream).await,
                    "case {case:?}: expected {group} to hold the stream {stream} the attempt \
                     reports"
                );
            }
            Outcome::FailsToStart {
                resource_kind,
                action,
            } => {
                assert_eq!(
                    detail["status"].as_str(),
                    Some(FAILED),
                    "case {case:?}: a job whose logger could not be initialised fails, got \
                     {detail}"
                );
                assert!(
                    detail["container"]["exitCode"].is_null(),
                    "case {case:?}: no container ran, so no exit status is reported, got {detail}"
                );
                let task_id = task_arn.rsplit('/').next().unwrap_or_default();
                let session = session_arn(&role_name_of(index), task_id);
                let resource = if resource_kind == "group" {
                    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{group}")
                } else {
                    format!(
                        "arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{group}:log-stream:{}",
                        attempt["container"]["logStreamName"]
                            .as_str()
                            .unwrap_or_default()
                    )
                };
                assert_eq!(
                    reported,
                    expected_reason(resource_kind, action, &session, &resource),
                    "case {case:?}: expected the agent's account of the logger it could not \
                     validate, got {reported:?} from {detail}"
                );
            }
        }
    }
}
