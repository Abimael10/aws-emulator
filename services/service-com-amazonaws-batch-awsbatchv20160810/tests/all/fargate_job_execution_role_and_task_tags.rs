// SPDX-License-Identifier: CC0-1.0

//! What a PLACED Fargate job asks of its EXECUTION ROLE, what the Amazon ECS task it ran as
//! carries, and how many tags that propagation may carry — driven end to end over the wire
//! against the services that own the facts.
//!
//! The RULE under test: **a job the scheduler places on a MANAGED Fargate compute environment
//! runs as an Amazon ECS task in that environment's backing cluster and SETTLES on the status
//! that task's container exited with, carrying — when the propagation was asked for — the union
//! of its job definition's tags and its own on that task with the job's value winning on a key
//! they share; the only permissions it asks of its execution role are the ones AWS publishes for
//! a task execution role, because the log group AWS Batch delivers a job's output to is the
//! SERVICE's own; and a propagation that would carry more than the documented number of combined
//! tags moves the job to `FAILED` instead, with no task created for it.**
//!
//! `AttemptContainerDetail$taskArn` is "the Amazon Resource Name (ARN) of the Amazon ECS task
//! that's associated with the job attempt" and `$logStreamName` states where the output goes
//! ("The log group for Batch jobs is `/aws/batch/job`"); `JobDetail$propagateTags` is "whether to
//! propagate the tags from the job or job definition to the corresponding Amazon ECS task … Tags
//! can only be propagated to the tasks during task creation. For tags with the same name, job
//! tags are given priority over job definitions tags. If the total number of combined tags from
//! the job and job definition is over 50, the job is moved to the FAILED state";
//! `ContainerProperties$executionRoleArn` is "the Amazon Resource Name (ARN) of the execution
//! role that Batch can assume … for jobs running on Fargate resources, you must provide an
//! execution role", and the policy AWS publishes for one (`AmazonECSTaskExecutionRolePolicy`)
//! grants the agent the log STREAM calls — creating the log group is not among them.
//!
//! The rows of the first test differ along what the rule generalises over: the program the
//! container declares (a utility that exits immediately, a program that declares a WAIT of its
//! own, a program that exits non-zero), and whether the propagation was asked for at all — with
//! one CONTRAST row whose execution role carries no policy, which is what shows the others pass
//! on the published permissions rather than on no permissions being read. The rows of the second
//! test walk the published ceiling: the combined set AT it, one past it, one that only looks past
//! it because both sides wrote the same key, and one past it that propagates nothing.
//!
//! Sources: `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt
//! container's `taskArn`, `exitCode` and `logStreamName`;
//! `smithy_query(query="doc",shape="JobDetail",member="propagateTags")` for the propagation, its
//! tie-break and its ceiling; `smithy_query(query="shape",shape="JobStatus")` for the terminal
//! statuses; `smithy_query(query="shape",shape="TagrisTagsMap")` for the per-side tag bound.
//!
//! What is NOT asserted here, because other suites assert it: which SIDE may ask for the
//! propagation (`core/router/tests/all/batch_propagated_tags_reach_the_ecs_task.rs`) and what a
//! job whose container performed nothing reports (`propagated_task_tags.rs`, beside this file).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, and so the Region of every ARN minted for them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the roles below are named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The policy AWS publishes for an Amazon ECS task execution role, which is the ONLY thing the
/// rows below grant their role. Source: the IAM crate's own catalogue of AWS managed policies
/// (`services/service-com-amazonaws-iam-awsidentitymanagementv20100508`), under the
/// `/service-role/` path AWS files it in.
const PUBLISHED_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The log group AWS Batch delivers a container job's output to when its configuration names no
/// other one. Spelled out here rather than imported, so a wrong constant in the implementation
/// cannot make this suite pass. Source: `AttemptContainerDetail$logStreamName` ("The log group
/// for Batch jobs is `/aws/batch/job`").
const JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The terminal `JobStatus`es and the `CEStatus` a compute environment comes to rest in. Source:
/// `smithy_query(query="shape",shape="JobStatus,CEStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const VALID: &str = "VALID";

/// The error class the Amazon ECS agent reports for an execution resource it could not bring up
/// before starting a container, which is what a job whose role may not open its log stream ends
/// with. Spelled out here rather than imported, for the same reason as the group above.
const RESOURCE_INITIALIZATION_ERROR: &str = "ResourceInitializationError";

/// How many combined tags a propagating job may carry onto its task, and the `statusReason` of
/// one that would carry more. Both spelled out here rather than imported: the number is the one
/// `JobDetail$propagateTags` publishes ("over 50 … the job is moved to the FAILED state") and the
/// sentence names that condition.
const MAX_COMBINED_TAGS: usize = 50;
const OVER_COMBINED_TAGS_REASON: &str =
    "The total number of combined tags from the job and job definition is over 50";

/// The `/16` this suite's VPC is carved out of, the `/24` its one subnet takes, and the
/// destination of the default route that gives that subnet a way to the registry the image below
/// is fetched from.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDR: &str = "10.91.1.0/24";
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The image every job definition below runs: a repository of the Amazon ECR Public gallery, so
/// the fetch is answered by the registry that holds it rather than left undecided.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The tag keys the first test's rows use: one only the job definition carries, one only the job
/// carries, and one BOTH carry — the key whose tie-break the rule names.
const DEFINITION_KEY: &str = "batchExecRoleTagFromDefinition";
const JOB_KEY: &str = "batchExecRoleTagFromJob";
const SHARED_KEY: &str = "batchExecRoleTagShared";

/// The values each side writes, so a merged set says which side a key came from.
const FROM_DEFINITION: &str = "written-by-the-job-definition";
const FROM_JOB: &str = "written-by-the-job";

/// How long a wait below gives the clocks: far above the spans Batch and Amazon ECS give the
/// moves a placed job's task makes, and every wait looks again for the CONDITION rather than
/// sleeping a fixed stretch, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS, Amazon EC2, CloudWatch Logs and IAM on ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed job runs as is Amazon ECS's
/// resource, the subnet and security group its placement names are Amazon VPC's, the log group
/// and stream its output goes to are CloudWatch Logs', and the documents in force on the
/// execution role the agent acts as are IAM's.
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: this harness runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
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

/// One AWS Batch call, as a client makes it: `POST /v1/<operation>` with a JSON body.
async fn batch(router: &MultiServiceRouter, uri: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = decoded(response.into_body().as_ref());
    assert_eq!(
        status, 200,
        "case {uri}: expected HTTP 200, got {status} with {answered}"
    );
    answered
}

/// One `awsJson1_1` call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = decoded(response.into_body().as_ref());
    assert_eq!(
        status, 200,
        "case {signing_name} {operation}: expected HTTP 200, got {status} with {answered}"
    );
    answered
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as XML.
/// Every value below is a literal with nothing form encoding exists for, so the body carries them
/// as they are.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (name, value) in params {
        assert!(
            !value.contains(['&', '=', '+', '%', ' ']),
            "setup: the value of {name} needs form escaping: {value:?}"
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
        .header("authorization", authorization("ec2"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One awsQuery call to IAM that must succeed: the role and the policy attached to it are created
/// through IAM's OWN API, so the documents the agent is judged by are that service's.
///
/// The body carries each value literally, which is what an `application/x-www-form-urlencoded`
/// body of these calls needs: the only bytes form encoding exists for are `&`, `=`, `+`, `%` and
/// whitespace, and no value below holds one — the trust document is written compactly for exactly
/// that reason.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, String)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        assert!(
            !value.contains(['&', '=', '+', '%', ' ']),
            "setup: the value of {name} needs form escaping: {value:?}"
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
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with {text}"
    );
    text
}

/// The text of the first `<name>` element of an XML answer: the id Amazon EC2 minted.
fn element(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let from = xml
        .find(&open)
        .unwrap_or_else(|| panic!("setup: expected a {name} in {xml}"))
        + open.len();
    let rest = &xml[from..];
    let to = rest
        .find(&close)
        .unwrap_or_else(|| panic!("setup: expected a closing {name} in {xml}"));
    rest[..to].to_string()
}

/// Create the IAM role `name`, trusted by the Amazon ECS task runtime the way an execution role
/// is, with the policies `attached` names — and answer its ARN.
async fn execution_role(router: &MultiServiceRouter, name: &str, attached: &[&str]) -> String {
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
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    for policy in attached {
        iam(
            router,
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

/// A queue over a MANAGED Fargate compute environment, with the network its tasks are placed in
/// and the two execution roles the rows run under.
struct Fixture {
    router: MultiServiceRouter,
    /// The queue every job below is submitted to.
    queue: String,
    /// The execution role carrying the policy AWS publishes for one.
    published: String,
    /// The contrast role, which carries no policy at all.
    bare: String,
}

/// Stand up that fixture, with every name prefixed by `prefix` so two of them never collide.
///
/// The network is built through Amazon EC2's own API, so every id the placement names is one that
/// service minted and holds — with a way off the VPC, because the fetch of the image the
/// containers run has to reach the registry that serves it.
async fn fixture(prefix: &str) -> Fixture {
    let router = router();
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
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
        &ec2(
            &router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("{prefix}-group")),
                ("GroupDescription", "the-group-this-suites-tasks-run-under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    let gateway = element(
        &ec2(&router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        &router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let table = element(
        &ec2(&router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        &router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
    )
    .await;
    ec2(
        &router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", DEFAULT_ROUTE),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;

    let published = execution_role(
        &router,
        &format!("{prefix}PublishedExec"),
        &[PUBLISHED_EXECUTION_POLICY],
    )
    .await;
    let bare = execution_role(&router, &format!("{prefix}BareExec"), &[]).await;

    let environment = format!("{prefix}-ce");
    let queue = format!("{prefix}-queue");
    batch(
        &router,
        "/v1/createcomputeenvironment",
        json!({
            "computeEnvironmentName": &environment,
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": [&subnet],
                "securityGroupIds": [&security_group],
            },
        }),
    )
    .await;
    batch(
        &router,
        "/v1/createjobqueue",
        json!({
            "jobQueueName": &queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;

    // The compute environment has to be able to hold a job before one is submitted to it.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            &router,
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [&environment]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(VALID) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the compute environment never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    Fixture {
        router,
        queue,
        published,
        bare,
    }
}

/// The `containerProperties` of a Fargate job definition running `command` as `role`.
fn container(command: &Value, role: &str) -> Value {
    json!({
        "image": IMAGE,
        "command": command,
        // "For jobs that run on Fargate resources, you must provide an execution role"
        // (`ContainerProperties$executionRoleArn`).
        "executionRoleArn": role,
        // The public address an internet gateway carries traffic for, which is what lets the
        // task's interface reach the registry the image names.
        "networkConfiguration": {"assignPublicIp": "ENABLED"},
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
    })
}

/// One tag list as the JSON map both Batch tag members take.
fn tag_map(tags: &[(String, String)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

/// `count` tags whose keys begin with `prefix`, which is how a row fills a side's tag set up to
/// (or past) the combined ceiling.
fn filler_tags(prefix: &str, count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|index| (format!("{prefix}{index:02}"), format!("value-{index:02}")))
        .collect()
}

/// The tags one described Amazon ECS task carries, read into a map so a comparison does not
/// depend on the order the entries came back in.
fn task_tags(task: &Value) -> BTreeMap<String, String> {
    task["tags"]
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

/// Look again until `job_id` reports a terminal `JobStatus`, and answer its detail.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(router, "/v1/describejobs", json!({"jobs": [job_id]})).await;
        let detail = body["jobs"][0].clone();
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

/// What one row of the first test declares and what the job it submits must then report.
struct JobRow {
    case: &'static str,
    /// `ContainerProperties$command`, the program the container declares.
    command: &'static [&'static str],
    /// Whether the submit asks for the tags to be propagated to the task.
    propagate: bool,
    /// Whether the job runs under the role carrying the published execution policy, or the
    /// contrast role that carries no policy at all.
    published_role: bool,
    /// The terminal `JobStatus` the row must settle on.
    status: &'static str,
    /// `ContainerDetail$exitCode`, and `None` where no container of the job ever exited.
    exit_code: Option<i64>,
}

/// The rows: the program the container declares and the status it exits with, whether the
/// propagation was asked for, and the permissions the job's execution role holds.
const JOB_ROWS: &[JobRow] = &[
    JobRow {
        case: "a utility that exits immediately, propagating both sides' tags",
        command: &["echo", "the batch job ran this"],
        propagate: true,
        published_role: true,
        status: SUCCEEDED,
        exit_code: Some(0),
    },
    JobRow {
        case: "a program that declares a wait of its own, propagating both sides' tags",
        command: &["/bin/sh", "-c", "sleep 1; echo done"],
        propagate: true,
        published_role: true,
        status: SUCCEEDED,
        exit_code: Some(0),
    },
    JobRow {
        case: "a program that exits non-zero, propagating both sides' tags",
        command: &["/bin/sh", "-c", "echo refusing; exit 7"],
        propagate: true,
        published_role: true,
        status: FAILED,
        exit_code: Some(7),
    },
    JobRow {
        case: "a job that asks for no propagation at all",
        command: &["echo", "untagged"],
        propagate: false,
        published_role: true,
        status: SUCCEEDED,
        exit_code: Some(0),
    },
    JobRow {
        case: "the contrast: an execution role carrying no policy at all",
        command: &["echo", "the batch job ran this"],
        propagate: true,
        published_role: false,
        status: FAILED,
        exit_code: None,
    },
];

/// RULE: a placed Fargate job runs as an Amazon ECS task of its compute environment's backing
/// cluster, settles on the status that task's container exited with, and carries the union of its
/// job definition's and its own tags on that task — the job's value winning on the shared key —
/// while asking of its execution role nothing beyond the permissions AWS publishes for one,
/// because the log group the job's output goes to is the service's own.
#[tokio::test]
async fn a_placed_fargate_job_settles_as_a_task_tagged_by_both_sides() {
    let fixture = fixture("batchexecrole").await;
    let router = &fixture.router;
    let definition_tags = vec![
        (DEFINITION_KEY.to_string(), FROM_DEFINITION.to_string()),
        (SHARED_KEY.to_string(), FROM_DEFINITION.to_string()),
    ];
    let job_tags = vec![
        (JOB_KEY.to_string(), FROM_JOB.to_string()),
        (SHARED_KEY.to_string(), FROM_JOB.to_string()),
    ];

    // One job definition per row, each tagged on the DEFINITION side.
    for (index, row) in JOB_ROWS.iter().enumerate() {
        let role = if row.published_role {
            &fixture.published
        } else {
            &fixture.bare
        };
        batch(
            router,
            "/v1/registerjobdefinition",
            json!({
                "jobDefinitionName": format!("batchexecrole-jd-{index}"),
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "tags": tag_map(&definition_tags),
                "containerProperties": container(&json!(row.command), role),
            }),
        )
        .await;
    }

    // Every row is submitted BEFORE any is waited on, so the spans the clocks owe them run
    // together rather than one after another.
    let mut submitted: Vec<String> = Vec::with_capacity(JOB_ROWS.len());
    for (index, row) in JOB_ROWS.iter().enumerate() {
        let mut request = json!({
            "jobName": format!("batchexecrole-job-{index}"),
            "jobQueue": &fixture.queue,
            "jobDefinition": format!("batchexecrole-jd-{index}"),
            "tags": tag_map(&job_tags),
        });
        if row.propagate {
            request["propagateTags"] = json!(true);
        }
        let answered = batch(router, "/v1/submitjob", request).await;
        submitted.push(
            answered["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {}: expected a jobId in {answered}", row.case))
                .to_string(),
        );
    }

    for (row, job_id) in JOB_ROWS.iter().zip(submitted.iter()) {
        let case = row.case;

        // The job SETTLES: a client polling `DescribeJobs` gets a terminal status rather than
        // waiting for ever.
        let detail = until_terminal(router, case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the job to settle {}, got {detail}",
            row.status
        );
        assert_eq!(
            detail["container"]["exitCode"].as_i64(),
            row.exit_code,
            "case {case:?}: expected the exit status its container ended with, got {detail}"
        );
        assert_eq!(
            detail["attempts"][0]["container"]["exitCode"].as_i64(),
            row.exit_code,
            "case {case:?}: expected the attempt to report the same exit status, got {detail}"
        );
        let reason = detail["statusReason"].as_str().unwrap_or_default();
        if row.published_role {
            assert!(
                !reason.contains(RESOURCE_INITIALIZATION_ERROR),
                "case {case:?}: the published execution policy is all a job needs, so no \
                 execution resource may be withheld from it, got {reason:?}"
            );
        } else {
            // The contrast: nothing of the container was started, because the resource the agent
            // brings up as the execution role before it could not be brought up — so the job
            // reports that, and no outcome is invented for work nothing did.
            assert!(
                reason.starts_with(RESOURCE_INITIALIZATION_ERROR),
                "case {case:?}: expected the end to begin {RESOURCE_INITIALIZATION_ERROR:?}, got \
                 {reason:?}"
            );
        }

        // The ATTEMPT names the Amazon ECS task the job ran as, in the cluster backing the
        // compute environment — and that service answers for it.
        let task_arn = detail["attempts"][0]["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the task the job ran as, got {detail}")
            })
            .to_string();
        let cluster = task_arn
            .split('/')
            .nth(1)
            .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
            .to_string();
        let answered = rpc(
            router,
            "ecs",
            "AmazonEC2ContainerServiceV20141113",
            "DescribeTasks",
            json!({"cluster": &cluster, "tasks": [&task_arn], "include": ["TAGS"]}),
        )
        .await;
        assert_eq!(
            answered["failures"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: expected Amazon ECS to answer for {task_arn}, got {answered}"
        );
        let task = answered["tasks"][0].clone();
        assert_eq!(
            task["taskArn"].as_str(),
            Some(task_arn.as_str()),
            "case {case:?}: expected the task the attempt names, got {task}"
        );

        // The TAGS on that task: both sides' sets merged with the job's value on the shared key
        // for a propagating job, and neither set for a job that asked for none.
        let expected: BTreeMap<String, String> = if row.propagate {
            [
                (DEFINITION_KEY, FROM_DEFINITION),
                (JOB_KEY, FROM_JOB),
                (SHARED_KEY, FROM_JOB),
            ]
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
        } else {
            BTreeMap::new()
        };
        assert_eq!(
            task_tags(&task),
            expected,
            "case {case:?}: expected {expected:?} on the task, got {task}"
        );

        // The stream that task's container writes to is held where CloudWatch Logs holds it, in
        // the group AWS Batch delivers job output to — which is the SERVICE's own, so the job's
        // execution role never has to create it.
        let stream = detail["attempts"][0]["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a log stream, got {detail}"))
            .to_string();
        let streams = rpc(
            router,
            "logs",
            "Logs_20140328",
            "DescribeLogStreams",
            json!({"logGroupName": JOB_LOG_GROUP, "logStreamNamePrefix": &stream}),
        )
        .await;
        let held: Vec<String> = streams["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|held| held["logStreamName"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            held,
            vec![stream.clone()],
            "case {case:?}: expected {stream:?} in {JOB_LOG_GROUP}, got {streams}"
        );
    }
}

/// What one row of the ceiling table declares: how many tags each side carries, whether they
/// share a key, whether the propagation was asked for, and what the job must then report.
struct CeilingRow {
    case: &'static str,
    definition_tags: usize,
    job_tags: usize,
    /// Whether one of the job's keys is one the definition already carries, so the two sets
    /// merge to one entry fewer than they were written as.
    shares_a_key: bool,
    propagate: bool,
    status: &'static str,
}

/// The rows walk the published ceiling: the combined set AT it, one past it, one that only looks
/// past it because both sides wrote the same key, and one past it that propagates nothing.
const CEILING_ROWS: &[CeilingRow] = &[
    CeilingRow {
        case: "a combined set exactly at the published ceiling",
        definition_tags: 25,
        job_tags: 25,
        shares_a_key: false,
        propagate: true,
        status: SUCCEEDED,
    },
    CeilingRow {
        case: "a combined set one tag past the published ceiling",
        definition_tags: 26,
        job_tags: 25,
        shares_a_key: false,
        propagate: true,
        status: FAILED,
    },
    CeilingRow {
        case: "one tag past the ceiling as written, at it once the shared key is merged",
        definition_tags: 26,
        job_tags: 25,
        shares_a_key: true,
        propagate: true,
        status: SUCCEEDED,
    },
    CeilingRow {
        case: "one tag past the ceiling, propagating nothing",
        definition_tags: 26,
        job_tags: 25,
        shares_a_key: false,
        propagate: false,
        status: SUCCEEDED,
    },
];

/// RULE: the ceiling a propagation is held to is on the COMBINED set it would carry — "if the
/// total number of combined tags from the job and job definition is over 50, the job is moved to
/// the FAILED state" — so a job over it ends `FAILED` naming that condition with no task created
/// and no attempt started for it, while one at it (including one whose two sides share a key)
/// runs, and so does one past it that asked for no propagation at all.
#[tokio::test]
async fn a_propagation_over_the_combined_tag_ceiling_fails_the_job() {
    let fixture = fixture("batchtagceiling").await;
    let router = &fixture.router;

    let mut submitted: Vec<String> = Vec::with_capacity(CEILING_ROWS.len());
    for (index, row) in CEILING_ROWS.iter().enumerate() {
        let definition_tags = filler_tags("definitionTag", row.definition_tags);
        let mut job_tags = filler_tags("jobTag", row.job_tags);
        if row.shares_a_key {
            // The first key the definition carries, written again by the job with a value of its
            // own: the merge decides it once, so the two sets are one entry shorter than the
            // number written.
            job_tags[0] = (definition_tags[0].0.clone(), FROM_JOB.to_string());
        }
        batch(
            router,
            "/v1/registerjobdefinition",
            json!({
                "jobDefinitionName": format!("batchtagceiling-jd-{index}"),
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "tags": tag_map(&definition_tags),
                "containerProperties": container(
                    &json!(["echo", "the batch job ran this"]),
                    &fixture.published,
                ),
            }),
        )
        .await;
        let mut request = json!({
            "jobName": format!("batchtagceiling-job-{index}"),
            "jobQueue": &fixture.queue,
            "jobDefinition": format!("batchtagceiling-jd-{index}"),
            "tags": tag_map(&job_tags),
        });
        if row.propagate {
            request["propagateTags"] = json!(true);
        }
        let answered = batch(router, "/v1/submitjob", request).await;
        let job_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: expected a jobId in {answered}", row.case))
            .to_string();
        submitted.push(job_id);
    }

    for (row, job_id) in CEILING_ROWS.iter().zip(submitted.iter()) {
        let case = row.case;
        let detail = until_terminal(router, case, job_id).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the job to settle {}, got {detail}",
            row.status
        );
        if row.status == FAILED {
            assert_eq!(
                detail["statusReason"].as_str(),
                Some(OVER_COMBINED_TAGS_REASON),
                "case {case:?}: expected the condition the member publishes, got {detail}"
            );
            // Nothing was created for a job the propagation stopped: "tags can only be
            // propagated to the tasks during task creation", and that creation never happened,
            // so the job has had no attempt and names no task.
            assert_eq!(
                detail["attempts"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected no attempt for a job no task was created for, got \
                 {detail}"
            );
            assert!(
                detail["container"]["taskArn"].as_str().is_none(),
                "case {case:?}: expected no task on a job the propagation stopped, got {detail}"
            );
            continue;
        }
        // A row at or under the ceiling RAN, so it has the one attempt its placement started —
        // and the set it propagated is no larger than the ceiling allows.
        assert_eq!(
            detail["attempts"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: expected the one attempt its placement started, got {detail}"
        );
        if row.propagate {
            let task_arn = detail["attempts"][0]["container"]["taskArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the task the job ran as, got {detail}")
                })
                .to_string();
            let cluster = task_arn
                .split('/')
                .nth(1)
                .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
                .to_string();
            let answered = rpc(
                router,
                "ecs",
                "AmazonEC2ContainerServiceV20141113",
                "DescribeTasks",
                json!({"cluster": &cluster, "tasks": [&task_arn], "include": ["TAGS"]}),
            )
            .await;
            let carried = task_tags(&answered["tasks"][0]);
            assert_eq!(
                carried.len(),
                MAX_COMBINED_TAGS,
                "case {case:?}: expected the merged set the propagation carries, got {carried:?}"
            );
        }
    }
}
