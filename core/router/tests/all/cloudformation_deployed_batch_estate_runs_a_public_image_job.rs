// SPDX-License-Identifier: CC0-1.0

//! A Batch estate DEPLOYED BY CLOUDFORMATION — an execution role, a `MANAGED`/`FARGATE`
//! compute environment, an `ENABLED` job queue and container job definitions — runs a job whose
//! container image lives in a PUBLIC registry, driven end to end across the services that own
//! the facts: CloudFormation, IAM, Amazon EC2, AWS Batch, Amazon ECS, Amazon ECR Public and
//! CloudWatch Logs on one engine.
//!
//! The RULE under test: **the registry an image reference NAMES is the registry the fetch is
//! answered by, so a reference whose host is a public registry is resolved there and never
//! rewritten into a private-registry lookup in the caller's own account — a public reference
//! the registry holds an image for is fetched and the container runs its command, and only a
//! reference that genuinely resolves to nothing fails the job, in the words of the registry
//! that was actually consulted rather than in a sentence naming the caller's account.**
//! `ContainerProperties$image` documents the forms separately: "Images in Amazon ECR Public
//! repositories use the full `registry/repository[:tag]` or `registry/repository[@digest]`
//! naming conventions. For example, `public.ecr.aws/<registry_alias>/<my-web-app>:<latest>`",
//! against "Images in Amazon ECR repositories use the full registry and repository URI (for
//! example, `123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>`)" — two
//! different registries, so a public reference states nothing about the account's own one.
//!
//! Sources: `smithy_query(query="shape",shape="ContainerProperties")` for `image`,
//! `command`, `executionRoleArn`, `resourceRequirements` and `networkConfiguration`;
//! `smithy_query(query="operation",operation="DescribeJobs")` for `JobDetail`'s `status`,
//! `statusReason` and `attempts`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt container's
//! `reason`, `exitCode` and `logStreamName`;
//! `smithy_query(query="operation",operation="DescribeJobQueues")` for the queue's `status`
//! and `state`.
//!
//! The rows differ along what the rule generalises over: WHICH public registry the reference
//! addresses (the Amazon ECR Public gallery, the registry a Docker Hub reference names by host,
//! and the same registry a reference naming no host at all is resolved against), WHICH
//! repository of it the reference names (one the gallery mirrors, one under an alias this
//! emulator holds no store for), and WHETHER the reference resolves to an image at all — the
//! boundary at which the job really does fail, and where this suite pins that the failure names
//! the registry consulted instead of the account's private one.
//!
//! Sibling suites: `batch_stack_registered_job_definition_is_submittable.rs` pins that the
//! stack's job definition is registered and submittable at all; this one carries the submit
//! through to the job's terminal status. The owning crate's
//! `tests/all/public_gallery_image_releases.rs` pins the gallery's own release listing over a
//! directly registered definition.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element, outputs_of};

/// The Region every call below is signed for, and the Region of every ARN minted for them.
const REGION: &str = "us-west-2";

/// The account this emulator mints its ARNs under. A public reference is answered by a
/// registry that is NOT this account's, so no failure about one may name it.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The stack status a deployment that created everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `JQStatus` a job queue comes to rest in, and the `JQState` the template asks for.
/// Spelled out here rather than imported from a service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const VALID: &str = "VALID";
const ENABLED: &str = "ENABLED";

/// The terminal `JobStatus` members. Source: `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The error CLASS a container runtime words a failed image fetch with.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The sentence a PRIVATE Amazon ECR registry words a repository it does not hold with, which
/// names the registry's account — so it is the sentence a PUBLIC reference may never be
/// answered with, whatever that reference resolves to.
const PRIVATE_REGISTRY_NOT_FOUND: &str = "does not exist in the registry with id";

/// What the registry answers about a reference it holds no image for, which is the condition
/// the agent's own failure reports.
const NOT_FOUND: &str = "not found";

/// The line each container's command writes, which is what its log stream holds once it ran.
const MARKER: &str = "hello-from-cfn-batch";

/// The log group AWS Batch delivers a job container's output to when its configuration names
/// none: "the log group for Batch jobs is `/aws/batch/job`"
/// (`AttemptContainerDetail$logStreamName`).
const JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The managed policy AWS publishes for a task execution role, which the role the stack
/// declares carries ("AmazonECSTaskExecutionRolePolicy", the task execution role of the Amazon
/// ECS Developer Guide).
const TASK_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The names the template declares for the resources this suite addresses by name.
const ROLE_NAME: &str = "cfnBatchPublicImageExecution";
const COMPUTE_ENVIRONMENT_NAME: &str = "cfn-batch-public-ce";
const QUEUE_NAME: &str = "cfn-batch-public-queue";

/// The `/16` this suite's VPC is carved out of, and the `/24` its subnet takes.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDR: &str = "10.71.1.0/24";

/// The destination of a default route, which is what carries a task's traffic to a registry on
/// the internet.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// How long a poll below gives the clock: far longer than the spans CloudFormation, AWS Batch
/// and Amazon ECS give the moves a deployed estate and a placed job make. Every wait looks
/// again for the CONDITION rather than sleeping a span, so a loaded host only makes a wait
/// longer and never changes the answer.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The services this chain reaches, on ONE shared engine exactly as `core/router/src/lib.rs`
/// mounts them: CloudFormation deploys the estate, IAM holds the execution role, Amazon EC2
/// holds the network the tasks are placed in, AWS Batch owns the jobs, Amazon ECS runs their
/// containers, Amazon ECR Public answers for a gallery reference and CloudWatch Logs holds what
/// a container wrote.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The parsed body of one response, or the text it carried when that is not JSON.
fn decoded(payload: &[u8]) -> Value {
    if payload.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(payload)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()))
}

/// One CloudFormation call over its awsQuery wire, answered as the XML text a client reads.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
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
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {text}"
    );
    text
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = decoded(response.body());
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, with the status it answered: a group nothing opened yet is a
/// modelled refusal rather than an empty answer, which the poll below looks again after.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, decoded(response.body()))
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as
/// XML — returned as text, which the element reader pulls the minted ids out of.
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
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
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
/// gateway, a subnet whose route table sends the default route to that gateway, and the
/// security group the placement names — the public subnet a Fargate task reaches a registry
/// from.
struct Network {
    subnet: String,
    security_group: String,
}

/// Build that network through Amazon EC2's own API, so every id the template carries is one
/// that service minted and holds.
async fn network_of(router: &MultiServiceRouter) -> Network {
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
                ("GroupName", "cfn-batch-public-image-jobs"),
                ("GroupDescription", "the group the jobs of this suite run under"),
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

/// One row of the table: the public image reference one job definition of the stack runs, and
/// whether the registry it names resolves it to an image at all.
struct Row {
    case: &'static str,
    /// The reference the declaration's `ContainerProperties.Image` carries.
    image: &'static str,
    /// Whether a reference this emulator's registries resolve to NO image, which is the only
    /// case that fails the job.
    unresolved: bool,
}

/// The rows. The published tags come from the repositories' own gallery listings
/// (`gallery.ecr.aws/docker/…`, the gallery's mirror of the Docker Official Images): `1.36` is
/// a BusyBox release and `2.4-alpine` an Apache HTTP Server variant, while nothing is published
/// under the tag the last row asks for.
const ROWS: &[Row] = &[
    Row {
        case: "an Amazon ECR Public release of a repository the gallery mirrors",
        image: "public.ecr.aws/docker/library/busybox:1.36",
        unresolved: false,
    },
    Row {
        case: "an Amazon ECR Public variant release of another mirrored repository",
        image: "public.ecr.aws/docker/library/httpd:2.4-alpine",
        unresolved: false,
    },
    Row {
        case: "an Amazon ECR Public alias this emulator holds no registry store for",
        image: "public.ecr.aws/amazonlinux/amazonlinux:2023",
        unresolved: false,
    },
    Row {
        case: "a Docker Hub reference naming that registry by host",
        image: "docker.io/library/busybox:1.36",
        unresolved: false,
    },
    Row {
        case: "a Docker Hub short name, which names no registry at all",
        image: "busybox:1.36",
        unresolved: false,
    },
    Row {
        case: "an Amazon ECR Public tag the mirrored repository publishes no release under",
        image: "public.ecr.aws/docker/library/busybox:no-such-release",
        unresolved: true,
    },
];

/// The logical id of the job definition one row is deployed as.
fn logical_id(index: usize) -> String {
    format!("JobDefinition{index}")
}

/// The name the row's job definition is declared under.
fn definition_name(index: usize) -> String {
    format!("cfn-batch-public-jd-{index}")
}

/// The stack template: the whole estate the issue's client deploys — an execution role trusting
/// the Amazon ECS tasks service principal, a `MANAGED`/`FARGATE` compute environment in the
/// public subnet above, an `ENABLED` job queue in front of it, and one container job definition
/// per row, each running the same command on its own image reference.
fn template(network: &Network) -> String {
    let mut resources = json!({
        "TaskExecutionRole": {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": ROLE_NAME,
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "ecs-tasks.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                },
                "ManagedPolicyArns": [TASK_EXECUTION_POLICY],
            },
        },
        "ComputeEnvironment": {
            "Type": "AWS::Batch::ComputeEnvironment",
            "Properties": {
                "ComputeEnvironmentName": COMPUTE_ENVIRONMENT_NAME,
                "Type": "MANAGED",
                "State": ENABLED,
                "ComputeResources": {
                    "Type": "FARGATE",
                    "MaxvCpus": 16,
                    "Subnets": [&network.subnet],
                    "SecurityGroupIds": [&network.security_group],
                },
            },
        },
        "JobQueue": {
            "Type": "AWS::Batch::JobQueue",
            "Properties": {
                "JobQueueName": QUEUE_NAME,
                "Priority": 1,
                "State": ENABLED,
                "ComputeEnvironmentOrder": [
                    { "Order": 1, "ComputeEnvironment": { "Ref": "ComputeEnvironment" } }
                ],
            },
        },
    });
    let mut outputs = json!({ "JobQueueArn": { "Value": { "Ref": "JobQueue" } } });
    for (index, row) in ROWS.iter().enumerate() {
        let logical = logical_id(index);
        resources[logical.as_str()] = json!({
            "Type": "AWS::Batch::JobDefinition",
            "Properties": {
                "JobDefinitionName": definition_name(index),
                "Type": "container",
                "PlatformCapabilities": ["FARGATE"],
                "ContainerProperties": {
                    "Image": row.image,
                    "Command": ["echo", MARKER],
                    // "For jobs that run on Fargate resources, you must provide an execution
                    // role" (`ContainerProperties$executionRoleArn`).
                    "ExecutionRoleArn": { "Fn::GetAtt": ["TaskExecutionRole", "Arn"] },
                    "ResourceRequirements": [
                        { "Type": "VCPU", "Value": "0.25" },
                        { "Type": "MEMORY", "Value": "512" }
                    ],
                    "NetworkConfiguration": { "AssignPublicIp": ENABLED },
                },
            },
        });
        outputs[format!("{logical}Arn").as_str()] = json!({
            "Value": { "Fn::GetAtt": [logical.as_str(), "JobDefinitionArn"] }
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an AWS Batch estate whose jobs run images from public registries",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// The stack's `DescribeStacks` answer, once its status no longer ends `_IN_PROGRESS`.
async fn settled(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, case, "DescribeStacks", &[("StackName", stack_name)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled, last status {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the job queue the stack deployed reports `VALID`, and answer the record it
/// reported then.
async fn until_queue_valid(router: &MultiServiceRouter, case: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describejobqueues",
            &json!({ "jobQueues": [QUEUE_NAME] }),
        )
        .await;
        let queue = body["jobQueues"][0].clone();
        if queue["status"].as_str() == Some(VALID) {
            return queue;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployed job queue never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer the detail reported
/// then. Nothing here sleeps for a span: it waits for the CONDITION.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(router, case, "/v1/describejobs", &json!({ "jobs": [job_id] })).await;
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

/// Look again until the stream `name` holds exactly the lines `expected`, in that order.
///
/// A wait for the CONDITION: the stream and its events are delivered by the drain that follows
/// the transition they belong to, so a reader that observed the job's terminal status first
/// looks again rather than concluding the container wrote nothing.
async fn until_events(router: &MultiServiceRouter, case: &str, name: &str, expected: &[&str]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, events) = logs(
            router,
            "GetLogEvents",
            &json!({ "logGroupName": JOB_LOG_GROUP, "logStreamName": name }),
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

/// RULE: a job the CloudFormation-deployed estate submits fetches its container image from the
/// registry its reference NAMES — so every public reference those registries hold an image for
/// runs its command and the job ends `SUCCEEDED` with the container's exit status 0, and the one
/// reference that resolves to no image ends `FAILED` in the words of the registry consulted,
/// naming the reference and never the caller's own account or a private registry's
/// repository-not-found sentence.
#[tokio::test]
async fn a_cloudformation_deployed_batch_estate_runs_its_public_image_job() {
    let router = router();
    let network = network_of(&router).await;
    let stack_name = "batch-public-image-estate";
    let setup = "the deployed estate";

    cloudformation(
        &router,
        setup,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &template(&network)),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;

    let described = settled(&router, setup, stack_name).await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {setup:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} in \
         {described}"
    );
    let outputs = outputs_of(&described);
    let queue_arn = outputs
        .get("JobQueueArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {setup:?}: no JobQueueArn output in {described}"));

    // The queue the deployment published is one AWS Batch itself reports, and it is in service.
    let queue = until_queue_valid(&router, setup).await;
    assert_eq!(
        queue["state"].as_str(),
        Some(ENABLED),
        "case {setup:?}: expected the deployed queue to be {ENABLED}, got {queue}"
    );
    assert_eq!(
        queue["jobQueueArn"].as_str(),
        Some(queue_arn.as_str()),
        "case {setup:?}: expected the queue the stack published, got {queue}"
    );

    // Every row's job is submitted first, so the jobs settle alongside each other rather than
    // one estate's worth of spans per row.
    let mut submitted = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let logical = logical_id(index);
        let definition_arn = outputs
            .get(&format!("{logical}Arn"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no {logical}Arn output in {described}"));
        let answered = batch(
            &router,
            case,
            "/v1/submitjob",
            &json!({
                "jobName": format!("cfn-batch-public-job-{index}"),
                "jobQueue": &queue_arn,
                "jobDefinition": &definition_arn,
            }),
        )
        .await;
        let job_id = answered["jobId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| panic!("case {case:?}: expected a jobId, got {answered}"))
            .to_string();
        submitted.push(job_id);
    }

    for (row, job_id) in ROWS.iter().zip(submitted.iter()) {
        let case = row.case;
        let image = row.image;
        let detail = until_terminal(&router, case, job_id).await;
        let expected_status = if row.unresolved { FAILED } else { SUCCEEDED };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected the job on {image:?} to end {expected_status}, got {detail}"
        );

        let attempt = detail["attempts"][0].clone();
        let reason = detail["statusReason"].as_str().unwrap_or_default().to_string();
        let attempt_reason = attempt["container"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // Whatever the verdict, it is never the private registry's: that sentence names an
        // account's own registry, which no public reference addresses.
        for (member, text) in [("statusReason", &reason), ("attempts[0].container.reason", &attempt_reason)] {
            assert!(
                !text.contains(PRIVATE_REGISTRY_NOT_FOUND),
                "case {case:?}: {member} may not answer the public reference {image:?} with a \
                 private registry's {PRIVATE_REGISTRY_NOT_FOUND:?}, got {text:?}"
            );
            assert!(
                !text.contains(ACCOUNT),
                "case {case:?}: {member} may not name the caller's account {ACCOUNT} for the \
                 public reference {image:?}, got {text:?}"
            );
        }

        if row.unresolved {
            // The one reference that resolves to nothing: the container runtime's own class,
            // naming the reference it could not resolve and what the registry answered about it.
            for (member, text) in [
                ("statusReason", &reason),
                ("attempts[0].container.reason", &attempt_reason),
            ] {
                assert!(
                    text.starts_with(CANNOT_PULL) && text.contains(image) && text.contains(NOT_FOUND),
                    "case {case:?}: expected {member} to be a {CANNOT_PULL} naming {image:?} and \
                     {NOT_FOUND:?}, got {text:?}"
                );
            }
            // Nothing ran, so no exit status is claimed for a container that never started.
            assert!(
                attempt["container"]["exitCode"].is_null(),
                "case {case:?}: expected no exit code for a container that never started, got \
                 {attempt}"
            );
            continue;
        }

        // A reference the registry served: no fetch failure is reported, and the job carries the
        // container's own exit status.
        assert!(
            !reason.contains(CANNOT_PULL),
            "case {case:?}: no fetch failure belongs to the served reference {image:?}, got \
             {reason:?}"
        );
        assert_eq!(
            attempt["container"]["exitCode"].as_i64(),
            Some(0),
            "case {case:?}: expected the container of {image:?} to exit 0, got {attempt}"
        );

        // And the container really ran its command: its log stream holds the line the program
        // wrote, which is the backing for the exit status above.
        let stream = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the attempt to name its log stream, got {attempt}")
            })
            .to_string();
        until_events(&router, case, &stream, &[MARKER]).await;
    }
}
