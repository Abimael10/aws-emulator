// SPDX-License-Identifier: CC0-1.0

//! WHO provisions the log group a container job's output is delivered to, and what that
//! leaves the job's EXECUTION ROLE having to hold — driven end to end over the wire against
//! the services that own the facts (Amazon ECS holds the task, Amazon EC2 the network it is
//! placed in, CloudWatch Logs the group and the stream, IAM the documents in force on the
//! role).
//!
//! RULE under test: **the log group AWS Batch delivers a container job's output to when the
//! job's configuration names no other one is the group AWS Batch itself holds for job output,
//! so THIS SERVICE provisions it before the job's container is launched; a job's task
//! execution role is therefore never required to hold the group-creating permission for it,
//! only the stream-level ones. A group the job's own configuration names instead is the
//! caller's: the service provisions none such, so a job delivering into a group nobody
//! created never starts its container.**
//!
//! `AttemptContainerDetail$logStreamName` is "the name of the CloudWatch Logs log stream
//! that's associated with the container. The log group for Batch jobs is `/aws/batch/job`"
//! (`smithy_query(query="shape",shape="AttemptContainerDetail")`), and
//! `ContainerProperties$executionRoleArn` is "the Amazon Resource Name (ARN) of the execution
//! role that Batch can assume ... for jobs running on Fargate resources, you must provide an
//! execution role" — in Amazon ECS's words "the task execution role that grants the Amazon
//! ECS container agent permission to make Amazon Web Services API calls on your behalf"
//! (`TaskDefinition$executionRoleArn`). The policy AWS publishes for one
//! (`AmazonECSTaskExecutionRolePolicy`, read here through IAM's own catalogue of AWS managed
//! policies) grants that agent `logs:CreateLogStream` and `logs:PutLogEvents` and no group
//! creation at all, which is what makes provisioning the group the service's own job.
//!
//! The rows differ along what the rule generalises over: what the execution role is granted
//! (the published policy, exactly the two stream actions, a document that explicitly DENIES
//! the group creation, and one that allows it) and how the container's configuration names
//! the group (naming none, naming Batch's own group, naming it and asking the driver to
//! create it too) — with
//! one CONTRAST row naming a group of the CALLER's that nobody created, which is what shows
//! the successes come from the service provisioning ITS group rather than from provisioning
//! whatever a configuration names. Every row's job is asserted to run as a task Amazon ECS
//! answers for, so the permissions each row grants are really the ones the agent's own
//! delivery was decided against.
//!
//! What is NOT asserted here, because sibling suites assert it: where a stream is named and
//! what it holds (`job_log_streams.rs`, `batch_ecs_orchestration.rs`) and what a propagating
//! job puts on its task (`fargate_job_execution_role_and_task_tags.rs`).
//!
//! `core/router/tests/all/batch_job_log_group_is_the_services_own.rs` drives the same rule
//! across the services from the router, over roles whose documents it writes itself. What
//! this suite adds, from the owning crate, are the branches that one leaves open: the AWS
//! MANAGED policy a task execution role actually carries (read out of IAM's own catalogue
//! rather than hand-written), a document that explicitly DENIES the group creation, a
//! configuration that names Batch's own group ITSELF, which is where the GROUP rather than
//! the absence of a configuration has to decide whose provisioning it is; a group of the
//! caller's that nobody created at all; and the group's own existence read back through
//! `DescribeLogGroups`. Whether a driver asked to create a group that is already there still
//! makes that call is not decided here: that is the driver's own order, which
//! `core/router/tests/all/ecs_awslogs_created_log_group_authorization.rs` owns, so the row
//! carrying `awslogs-create-group` runs under a role that may create groups.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state;

// The reading of an id Amazon EC2 minted, from the sibling suite that already holds it: both
// suites stand their network up through that service's own `ec2Query` API, and every
// `tests/all/*.rs` module is compiled into this one test binary.
use crate::fargate_job_execution_role_and_task_tags::element;

/// The Region every call below is signed for, and so the Region of every ARN minted for them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the roles below are named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The IAM API version an awsQuery body carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The `/16` this suite's VPC is carved out of, the `/24` its one subnet takes, and the
/// destination of the default route that gives that subnet a way to the registry the image
/// below is fetched from.
const VPC_CIDR: &str = "10.93.0.0/16";
const SUBNET_CIDR: &str = "10.93.1.0/24";
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The image every job definition below runs: a repository of the Amazon ECR Public gallery,
/// so the fetch is answered by the registry that holds it rather than left undecided.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The log group AWS Batch delivers a container job's output to when its configuration names
/// no other one. Spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make this suite pass. Source:
/// `AttemptContainerDetail$logStreamName` ("The log group for Batch jobs is
/// `/aws/batch/job`").
const DEFAULT_LOG_GROUP: &str = "/aws/batch/job";

/// A log group of the CALLER's: not the one AWS Batch holds for job output, so not this
/// service's to provision.
const CALLER_LOG_GROUP: &str = "/caller/holds/this";

/// The policy AWS publishes for an Amazon ECS task execution role, under the
/// `/service-role/` path AWS files it in. Source: IAM's own catalogue of AWS managed
/// policies (`services/service-com-amazonaws-iam-awsidentitymanagementv20100508`), whose
/// document for it grants `logs:CreateLogStream` and `logs:PutLogEvents` and no group
/// creation.
const PUBLISHED_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The two CloudWatch Logs actions a task execution role holds for the delivery, named as
/// IAM publishes them ("Actions defined by Amazon CloudWatch Logs").
const STREAM_ACTIONS: &[&str] = &["logs:CreateLogStream", "logs:PutLogEvents"];

/// The actions the pull of the image above is made with, which every role written here holds
/// — the published policy of the first row grants them itself — so that no row is decided by
/// the fetch instead of by the delivery.
const PULL_ACTIONS: &[&str] = &["ecr:GetAuthorizationToken", "ecr:BatchGetImage"];

/// The group-creating action no task execution role is required to hold for Batch's own
/// group, and the one the denied role below denies outright.
const CREATE_LOG_GROUP_ACTION: &str = "logs:CreateLogGroup";

/// The error class the Amazon ECS agent reports for a resource it could not bring up before
/// starting a container, which is what a job whose delivery has no log group ends with.
/// Spelled out here rather than imported, for the same reason as the group above.
const RESOURCE_INITIALIZATION_ERROR: &str = "ResourceInitializationError";

/// How long a wait below gives the clocks: far above the spans Batch and Amazon ECS give the
/// moves a placed job's task makes, and every wait looks again for the CONDITION rather than
/// sleeping a fixed stretch, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS, Amazon EC2, CloudWatch Logs and IAM on ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed job runs as is Amazon
/// ECS's resource, the subnet and security group its placement names are Amazon VPC's, the
/// group and stream its output goes to are CloudWatch Logs', and the documents in force on
/// the execution role the agent acts as are IAM's.
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
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
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

    /// One AWS Batch call that must be served: `POST /v1/<operation>` with a JSON body.
    async fn batch(&self, uri: &str, body: Value) -> Value {
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
        let answered = decoded(response.into_body().as_ref());
        assert_eq!(
            status, 200,
            "case {uri}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One `awsJson1_1` call against the service whose signing name is `signing_name`,
    /// answering its status and body — a group nothing created is a modelled refusal rather
    /// than an empty answer.
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

    /// One Amazon ECS call that must be served.
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

    /// One CloudWatch Logs call.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        self.rpc("logs", "Logs_20140328", operation, body).await
    }

    /// One Amazon EC2 call that must succeed: an `ec2Query` service, so a form body naming
    /// the action, answered as XML. Every value below is a literal with nothing form encoding
    /// exists for, so the body carries them as they are.
    async fn ec2(&self, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2016-11-15");
        for (name, value) in params {
            assert!(
                !value.contains(['&', '=', '+', '%', ' ']),
                "case ec2 {action}: the value of {name} needs form escaping: {value:?}"
            );
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(value);
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("ec2.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("ec2"))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case ec2 {action}: expected HTTP 200, got {status} with {text}"
        );
        text
    }

    /// One awsQuery call to IAM that must succeed.
    ///
    /// The body carries each value literally, which is what an
    /// `application/x-www-form-urlencoded` body of these calls needs: the only bytes form
    /// encoding exists for are `&`, `=`, `+`, `%` and whitespace, and no value below holds
    /// one — every document here is rendered compactly for exactly that reason.
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
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id
/// and signature are not credentials: this harness runs with request authentication off.
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

/// An identity-based document allowing `allowed` on every resource, and — where `denied`
/// names any — denying those outright.
///
/// Rendered compactly (`to_string` writes no whitespace), so the form body that carries it to
/// IAM needs no escaping ([`Emulator::iam`]).
fn document(allowed: &[&str], denied: &[&str]) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": allowed,
        "Resource": "*",
    })];
    if !denied.is_empty() {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denied,
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the IAM role `name`, trusted by the Amazon ECS task runtime the way an execution
/// role is, carrying `inline` as its one inline document and `attached` as its attachments —
/// and answer its ARN.
async fn execution_role(
    emulator: &Emulator,
    name: &str,
    inline: Option<String>,
    attached: &[&str],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "ecs-tasks.amazonaws.com"},
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
    if let Some(inline_document) = inline {
        emulator
            .iam(
                "PutRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyName", "inline".to_string()),
                    ("PolicyDocument", inline_document),
                ],
            )
            .await;
    }
    for policy in attached {
        emulator
            .iam(
                "AttachRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyArn", (*policy).to_string()),
                ],
            )
            .await;
    }
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A queue over a MANAGED, ENABLED Fargate compute environment placed in a network built
/// through Amazon EC2's own API — so every id the placement names is one that service minted
/// and holds, with a way off the VPC, because the fetch of the image the containers run has
/// to reach the registry that serves it.
async fn placeable(emulator: &Emulator) -> String {
    let vpc = element(
        &emulator.ec2("CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &emulator
            .ec2(
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
    let security_group = element(
        &emulator
            .ec2(
                "CreateSecurityGroup",
                &[
                    ("GroupName", "batch-log-group-suite"),
                    ("GroupDescription", "the-group-these-tasks-run-under"),
                    ("VpcId", vpc.as_str()),
                ],
            )
            .await,
        "groupId",
    );
    let gateway = element(
        &emulator.ec2("CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    emulator
        .ec2(
            "AttachInternetGateway",
            &[
                ("InternetGatewayId", gateway.as_str()),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await;
    let table = element(
        &emulator
            .ec2("CreateRouteTable", &[("VpcId", vpc.as_str())])
            .await,
        "routeTableId",
    );
    emulator
        .ec2(
            "AssociateRouteTable",
            &[
                ("RouteTableId", table.as_str()),
                ("SubnetId", subnet.as_str()),
            ],
        )
        .await;
    emulator
        .ec2(
            "CreateRoute",
            &[
                ("RouteTableId", table.as_str()),
                ("DestinationCidrBlock", DEFAULT_ROUTE),
                ("GatewayId", gateway.as_str()),
            ],
        )
        .await;

    let environment = "ce-log-group";
    emulator
        .batch(
            "/v1/createcomputeenvironment",
            json!({
                "computeEnvironmentName": environment,
                "type": "MANAGED",
                "state": state::STATE_ENABLED,
                "computeResources": {
                    "type": "FARGATE",
                    "maxvCpus": 16,
                    "subnets": [&subnet],
                    "securityGroupIds": [&security_group],
                },
            }),
        )
        .await;
    // The compute environment has to be able to hold a job before one is submitted to it.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = emulator
            .batch(
                "/v1/describecomputeenvironments",
                json!({"computeEnvironments": [environment]}),
            )
            .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(state::STATUS_VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment never became usable, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    emulator
        .batch(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "q-log-group",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
            }),
        )
        .await["jobQueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected the queue's ARN"))
        .to_string()
}

/// Poll `DescribeJobs` until `job_id` reports a terminal status, then answer its detail.
async fn until_terminal(emulator: &Emulator, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = emulator
            .batch("/v1/describejobs", json!({"jobs": [job_id]}))
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

/// Whether CloudWatch Logs holds the log group `name`, read through that service's own API.
async fn holds_group(emulator: &Emulator, name: &str) -> bool {
    let (status, body) = emulator
        .logs("DescribeLogGroups", json!({"logGroupNamePrefix": name}))
        .await;
    assert_eq!(
        status, 200,
        "case describe groups {name:?}: expected 200, got {status} {body}"
    );
    body["logGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|group| group["logGroupName"].as_str() == Some(name))
}

/// Poll until CloudWatch Logs holds the stream `stream` of `group`.
async fn until_stream_exists(emulator: &Emulator, case: &str, group: &str, stream: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = emulator
            .logs(
                "DescribeLogStreams",
                json!({"logGroupName": group, "logStreamNamePrefix": stream}),
            )
            .await;
        if status == 200
            && body["logStreams"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|held| held["logStreamName"].as_str() == Some(stream))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected CloudWatch Logs to hold {stream:?} in {group:?}, got {status} \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Which prepared execution role a row's job runs as.
#[derive(Debug, Clone, Copy)]
enum Role {
    /// Carrying the policy AWS publishes for a task execution role, and nothing else.
    Published,
    /// Carrying exactly the two stream-level actions that policy grants for the delivery.
    StreamOnly,
    /// Carrying those two and an explicit DENY of the group creation.
    GroupCreationDenied,
    /// Carrying those two AND the group creation, which is what a configuration asking the
    /// DRIVER to create a group needs of the role it is made as.
    MayCreateGroup,
}

/// What one row declares, and what its job must then report.
struct Row {
    case: &'static str,
    /// The container's `logConfiguration`, or `None` for one that configures no log driver at
    /// all — the default Batch log configuration, which is the case the rule is about.
    log_configuration: Option<fn() -> Value>,
    role: Role,
    /// The log group the job's output is delivered to.
    group: &'static str,
    /// Whether the job must SUCCEED; a row that must not reports the agent's
    /// resource-initialisation failure instead, having started no container.
    succeeds: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "no log configuration at all, under the published execution policy",
        log_configuration: None,
        role: Role::Published,
        group: DEFAULT_LOG_GROUP,
        succeeds: true,
    },
    Row {
        case: "no log configuration, under a role granted only the two stream actions",
        log_configuration: None,
        role: Role::StreamOnly,
        group: DEFAULT_LOG_GROUP,
        succeeds: true,
    },
    Row {
        case: "no log configuration, under a role explicitly denied the group creation",
        log_configuration: None,
        role: Role::GroupCreationDenied,
        group: DEFAULT_LOG_GROUP,
        succeeds: true,
    },
    Row {
        case: "awslogs naming Batch's own group, under that same denied role",
        log_configuration: Some(|| {
            json!({
                "logDriver": "awslogs",
                "options": {"awslogs-group": DEFAULT_LOG_GROUP},
            })
        }),
        role: Role::GroupCreationDenied,
        group: DEFAULT_LOG_GROUP,
        succeeds: true,
    },
    Row {
        // The driver is asked to create a group the service already holds. The role here MAY
        // create groups, so the row turns on the delivery reaching the service's group rather
        // than on whether the driver still makes a call for a group that is already there —
        // which is the question `core/router/tests/all/ecs_awslogs_created_log_group_authorization.rs`
        // owns, over the driver's own order.
        case: "awslogs naming Batch's own group and asking the driver to create it too",
        log_configuration: Some(|| {
            json!({
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": DEFAULT_LOG_GROUP,
                    "awslogs-create-group": "true",
                },
            })
        }),
        role: Role::MayCreateGroup,
        group: DEFAULT_LOG_GROUP,
        succeeds: true,
    },
    Row {
        // The CONTRAST: a group of the caller's that nobody created is not the service's to
        // provision, so the delivery has nowhere to go and the container never starts.
        case: "awslogs naming a group of the caller's that nobody created",
        log_configuration: Some(|| {
            json!({
                "logDriver": "awslogs",
                "options": {"awslogs-group": CALLER_LOG_GROUP},
            })
        }),
        role: Role::Published,
        group: CALLER_LOG_GROUP,
        succeeds: false,
    },
];

/// RULE: the log group a container job's output is delivered to when its configuration names
/// no other one is the group AWS Batch holds for job output, so the SERVICE provisions it and
/// the job's execution role is never required to hold the group-creating permission for it —
/// while a group of the caller's is not provisioned for it at all.
#[tokio::test]
async fn batch_provisions_its_own_job_log_group_and_asks_the_role_for_no_group_creation() {
    let emulator = Emulator::new();
    let queue = placeable(&emulator).await;
    // Every role holds what the PULL takes, so no row is decided by the image fetch that
    // precedes the delivery; what they differ in is the delivery's own permissions.
    // The issue's own prescription: the published policy and nothing else, which already
    // grants what the pull takes beside the two delivery actions.
    let published = execution_role(
        &emulator,
        "batchloggrouppublished",
        None,
        &[PUBLISHED_EXECUTION_POLICY],
    )
    .await;
    let stream_only = execution_role(
        &emulator,
        "batchloggroupstreamonly",
        Some(document(&[STREAM_ACTIONS, PULL_ACTIONS].concat(), &[])),
        &[],
    )
    .await;
    let denied = execution_role(
        &emulator,
        "batchloggroupdenied",
        Some(document(
            &[STREAM_ACTIONS, PULL_ACTIONS].concat(),
            &[CREATE_LOG_GROUP_ACTION],
        )),
        &[],
    )
    .await;
    let may_create = execution_role(
        &emulator,
        "batchloggroupmaycreate",
        Some(document(
            &[STREAM_ACTIONS, PULL_ACTIONS, &[CREATE_LOG_GROUP_ACTION]].concat(),
            &[],
        )),
        &[],
    )
    .await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let role = match row.role {
            Role::Published => &published,
            Role::StreamOnly => &stream_only,
            Role::GroupCreationDenied => &denied,
            Role::MayCreateGroup => &may_create,
        };
        let definition_name = format!("jd-log-group-{index}");
        let mut container = json!({
            "image": IMAGE,
            "command": ["echo", "the batch job ran this"],
            "executionRoleArn": role,
            // The public address an internet gateway carries traffic for, which is what lets
            // the task's interface reach the registry the image names.
            "networkConfiguration": {"assignPublicIp": "ENABLED"},
            "resourceRequirements": [
                {"type": "VCPU", "value": "0.25"},
                {"type": "MEMORY", "value": "512"},
            ],
        });
        if let Some(build) = row.log_configuration {
            container["logConfiguration"] = build();
        }
        let definition = emulator
            .batch(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": &definition_name,
                    "type": "container",
                    "platformCapabilities": ["FARGATE"],
                    "containerProperties": container,
                }),
            )
            .await["jobDefinitionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected the definition's ARN"))
            .to_string();
        let job_id = emulator
            .batch(
                "/v1/submitjob",
                json!({
                    "jobName": format!("job-log-group-{index}"),
                    "jobQueue": &queue,
                    "jobDefinition": definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a jobId"))
            .to_string();

        let detail = until_terminal(&emulator, case, &job_id).await;
        let expected_status = if row.succeeds {
            state::JOB_SUCCEEDED
        } else {
            state::JOB_FAILED
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected the job to settle {expected_status}, got {detail}"
        );
        let reason = detail["statusReason"].as_str().unwrap_or_default();
        let attempt = detail["attempts"][0].clone();

        // The job ran as a task AMAZON ECS answers for, so the delivery really was the
        // agent's to initialise as the role this row granted.
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected the attempt's task, got {attempt}"))
            .to_string();
        let cluster = task_arn
            .split('/')
            .nth(1)
            .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
            .to_string();
        let described = emulator
            .ecs(
                "DescribeTasks",
                json!({"cluster": &cluster, "tasks": [&task_arn]}),
            )
            .await;
        assert_eq!(
            described["tasks"][0]["taskArn"].as_str(),
            Some(task_arn.as_str()),
            "case {case:?}: expected Amazon ECS to answer for {task_arn:?}, got {described}"
        );

        if !row.succeeds {
            // Nothing started, and nothing provisioned the caller's group on its behalf: the
            // service holds one group for job output, and this is not it.
            assert!(
                reason.starts_with(RESOURCE_INITIALIZATION_ERROR),
                "case {case:?}: expected the end to begin {RESOURCE_INITIALIZATION_ERROR:?}, got \
                 {reason:?}"
            );
            assert!(
                attempt["container"]["exitCode"].is_null(),
                "case {case:?}: no exit status belongs to a container that never started, got \
                 {attempt}"
            );
            assert!(
                !holds_group(&emulator, row.group).await,
                "case {case:?}: a group of the caller's is not this service's to provision, but \
                 CloudWatch Logs now holds {:?}",
                row.group
            );
            continue;
        }

        // The role was asked for NOTHING it does not hold: the group was there to deliver
        // into, so the launch reports no resource it could not initialise — and in
        // particular no group creation.
        assert!(
            !reason.contains(RESOURCE_INITIALIZATION_ERROR)
                && !reason.contains(CREATE_LOG_GROUP_ACTION),
            "case {case:?}: the delivery may ask the execution role for no group creation, got \
             {reason:?}"
        );
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            Some(0),
            "case {case:?}: expected the status its container exited with, got {attempt}"
        );
        // And the group the output went to is HELD where CloudWatch Logs holds one, with the
        // attempt's own stream in it: the service provisioned it.
        assert!(
            holds_group(&emulator, row.group).await,
            "case {case:?}: expected CloudWatch Logs to hold {:?}",
            row.group
        );
        let stream = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected the attempt's stream, got {attempt}"))
            .to_string();
        until_stream_exists(&emulator, case, row.group, &stream).await;
    }
}
