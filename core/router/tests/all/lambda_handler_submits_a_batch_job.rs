// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING Lambda handler can do with AWS Batch through the AWS SDK its managed runtime
//! supplies: submit the work, and read back every fact about what it submitted.
//!
//! RULE: a managed `python3.x` runtime ships the AWS SDK for Python on the function's import
//! path with a client for every service the runtime's call seam binds, so `boto3.client(<token>)`
//! for such a service resolves and each of its operations is carried over that service's own
//! protocol into the service that owns the state — the answer the handler reads is that
//! service's own answer, success or refusal, judged against the policies in force on the
//! function's execution role. An invocation whose handler then RETURNS is a successful
//! `RequestResponse` invocation: HTTP 200, NO `X-Amz-Function-Error` header, and the handler's
//! own return value as the payload (`InvocationResponse$FunctionError` is set only "if the
//! function returns an error", which is why it is an in-band header on an otherwise successful
//! 200 rather than a modelled error shape).
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the execution role and
//! the wrapper function together, IAM holds the role and its inline document, STS mints the
//! session the handler's calls are signed with, Lambda runs the handler and supplies it the SDK,
//! and AWS Batch takes the decision and owns the job — so the suite lives here: a crate-local
//! suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: WHICH SERVICE the handler builds a
//! client for and WHICH operation of it it asks for, and so which binding of which protocol the
//! runtime carries — of the job service (`restJson1`) a write that mints a resource
//! (`SubmitJob`), a read of one resource by the id that write minted (`DescribeJobs`), a read of
//! a collection (`DescribeJobQueues`), a listing scoped to a resource (`ListJobs`), a second
//! write that mints a revision (`RegisterJobDefinition`), and a call the service itself refuses
//! (a submit naming a queue the account does not hold); of the container registry and the
//! container service (both `awsJson1_1`) a read of the repository an image lives in and of the
//! cluster a task runs on; plus one call the execution role does not grant (`TerminateJob`),
//! which the service refuses before it touches the job. Every served row asserts BOTH halves:
//! what the handler reported, and what the owning service answers an independent reader
//! afterwards.
//!
//! What the neighbours pin and this one does not repeat: `batch_identity_authorization` drives
//! the authorization decision over a table of principals and documents for a client that signed
//! directly, `batch_fargate_job_runs_as_an_ecs_task` drives what a PLACED job reports, and
//! `lambda_in_function_cloudwatch_authorization` drives the in-function seam against a service
//! whose actions are judged over every resource.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::batch_identity_authorization::{
    batch, served, ACCESS_DENIED, ACCESS_DENIED_STATUS, ACCOUNT_KEY, REGION,
};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};

/// The account this emulator serves, which the session ARN a refusal names carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the execution role and the wrapper function are deployed by, and the status it
/// must reach.
const STACK: &str = "xsvc-fn-batch-wrapper";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The compute environment, the queue in front of it and the job definition the wrapper submits
/// against — created through AWS Batch's own API as the account, so every ARN below is one that
/// service minted.
const ENVIRONMENT: &str = "xsvc-fn-batch-ce";
const QUEUE: &str = "xsvc-fn-batch-queue";
const DEFINITION: &str = "xsvc-fn-batch-def";

/// The definition the handler itself registers, for the row whose probe is a write.
const HANDLER_DEFINITION: &str = "xsvc-fn-batch-handler-def";

/// A job name the account holds nothing for, for the row the service itself refuses.
const ABSENT_QUEUE: &str = "xsvc-fn-batch-no-such-queue";

/// The container repository and the container cluster the other two services hold, created as
/// the account so the facts the handler reads are those services' own.
const REPOSITORY: &str = "xsvc-fn-batch-images";
const CLUSTER: &str = "xsvc-fn-batch-cluster";

/// The `X-Amz-Target` prefixes those two services' models declare
/// (`smithy_query(query="service_info")`: their own service shapes), and their signing names,
/// which is what routes a request to each.
const ECR_TARGET: &str = "AmazonEC2ContainerRegistry_V20150921";
const ECS_TARGET: &str = "AmazonEC2ContainerServiceV20141113";
const ECR_SIGNING_NAME: &str = "ecr";
const ECS_SIGNING_NAME: &str = "ecs";

/// The `status` a created container cluster reports.
const CLUSTER_ACTIVE: &str = "ACTIVE";

/// The job the wrapper submits.
const JOB_NAME: &str = "xsvc-fn-batch-job";

/// The image and the reservation every definition below states: an EC2-capability container
/// job, so neither an execution role nor a Fargate-supported size pair is required of it.
const IMAGE: &str = "registry.example.com/simparity/xsvc-batch-worker:1";
const VCPU: &str = "1";
const MEMORY: &str = "2048";

/// The role and the function the stack declares. Both are named here because the refusal of the
/// ungranted row names both: the identity a handler's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
const ROLE_NAME: &str = "xsvc-fn-batch-role";
const FUNCTION_NAME: &str = "xsvc-fn-batch-wrapper-fn";

/// The logical id the function is declared under in the template.
const FUNCTION_LOGICAL: &str = "Wrapper";

/// The runtime the wrapper runs, and the module and attribute its package carries the handler
/// as. A managed Python runtime ships the AWS SDK for Python, which is what the handler imports.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what any run
/// needs on any host, because what is under test is the call the handler made and not how fast
/// an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// The signal an invocation whose handler raised carries, and the only value this emulator sets
/// it to for an unhandled handler or init failure.
const UNHANDLED: &str = "Unhandled";

/// The `JQStatus` a created queue settles at, and the `JQState` it is created in.
const QUEUE_VALID: &str = "VALID";
const QUEUE_ENABLED: &str = "ENABLED";

/// Every value `JobStatus` may carry, from the called service's own model
/// (`smithy_query(query="shape", shape="JobStatus", model_path="services/\
/// service-com-amazonaws-batch-awsbatchv20160810/smithy-model/\
/// com-amazonaws-batch-awsbatchv20160810.json")`). A job's status moves as the clock brings its
/// phases due, so what a read of one asserts is that it is a status this service models.
const JOB_STATUS_VALUES: &[&str] = &[
    "SUBMITTED",
    "PENDING",
    "RUNNABLE",
    "STARTING",
    "RUNNING",
    "SUCCEEDED",
    "FAILED",
];

/// The `JobStatus` a job submitted onto a queue whose capacity nothing provisions comes to rest
/// at: "RUNNABLE - The job resides in the queue and is not currently scheduled to run on a
/// compute resource" (`JobStatus`). `ListJobs` reports the jobs of ONE status, and "if you don't
/// specify a status, only `RUNNING` jobs are returned" (`ListJobs$jobStatus`), so a listing of a
/// resting job names that status.
const RESTING_STATUS: &str = "RUNNABLE";

/// The error class AWS Batch refuses a request about something the account does not hold with,
/// and the status its model gives that shape (`ClientException`, `httpError` 400).
const CLIENT_EXCEPTION: &str = "ClientException";
const CLIENT_EXCEPTION_STATUS: u16 = 400;

/// How often to look while waiting for work the clock owes — a compute environment or a queue
/// settling, a created function reaching `Active` — and how long to keep looking. The spans are
/// the owning services' own, so no fixed sleep names the instant one ends, and the deadline is
/// generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(240);

/// The wrapper every row invokes: it builds ONE AWS Batch client at module scope — which is
/// where a managed runtime's handler builds one — performs the operation the event names, and
/// answers the document a client of such a wrapper parses: a status and a `body` carrying the
/// JSON it has to report. A refusal is caught and reported as the code, the sentence and the
/// status the service answered, unless the event asks for it to propagate.
const WRAPPER_HANDLER: &str = r#"
import json
import os

import boto3
from botocore.exceptions import ClientError

BATCH = boto3.client("batch")
REGISTRY = boto3.client("ecr")
CONTAINERS = boto3.client("ecs")


def submit(event):
    answered = BATCH.submit_job(
        jobName=event["jobName"],
        jobQueue=event.get("jobQueue") or os.environ["JOB_QUEUE"],
        jobDefinition=os.environ["JOB_DEFINITION"],
        parameters=event.get("parameters") or {},
    )
    return {"jobId": answered["jobId"], "jobName": answered["jobName"]}


def describe_jobs(event):
    reported = []
    for job in BATCH.describe_jobs(jobs=event["jobs"])["jobs"]:
        reported.append({
            "jobId": job["jobId"],
            "jobName": job["jobName"],
            "jobQueue": job["jobQueue"],
            "status": job["status"],
        })
    return {"jobs": reported}


def describe_queues(event):
    reported = []
    answered = BATCH.describe_job_queues(jobQueues=[os.environ["JOB_QUEUE"]])
    for queue in answered["jobQueues"]:
        reported.append({
            "jobQueueName": queue["jobQueueName"],
            "jobQueueArn": queue["jobQueueArn"],
            "status": queue["status"],
            "state": queue["state"],
        })
    return {"jobQueues": reported}


def list_jobs(event):
    ids = []
    names = []
    answered = BATCH.list_jobs(
        jobQueue=os.environ["JOB_QUEUE"], jobStatus=event["jobStatus"]
    )
    for summary in answered["jobSummaryList"]:
        ids.append(summary["jobId"])
        names.append(summary["jobName"])
    return {"jobIds": ids, "jobNames": names}


def register(event):
    answered = BATCH.register_job_definition(
        jobDefinitionName=event["jobDefinitionName"],
        type="container",
        containerProperties=event["containerProperties"],
    )
    return {
        "jobDefinitionName": answered["jobDefinitionName"],
        "jobDefinitionArn": answered["jobDefinitionArn"],
        "revision": answered["revision"],
    }


def terminate(event):
    BATCH.terminate_job(jobId=event["jobId"], reason=event["reason"])
    return {"terminated": event["jobId"]}


def describe_repositories(event):
    reported = []
    answered = REGISTRY.describe_repositories(repositoryNames=[event["repository"]])
    for repository in answered["repositories"]:
        reported.append({
            "repositoryName": repository["repositoryName"],
            "repositoryUri": repository["repositoryUri"],
            "registryId": repository["registryId"],
        })
    return {"repositories": reported}


def describe_clusters(event):
    reported = []
    for cluster in CONTAINERS.describe_clusters(clusters=[event["cluster"]])["clusters"]:
        reported.append({
            "clusterName": cluster["clusterName"],
            "clusterArn": cluster["clusterArn"],
            "status": cluster["status"],
        })
    return {"clusters": reported}


CALLS = {
    "submit": submit,
    "describe_jobs": describe_jobs,
    "describe_queues": describe_queues,
    "list_jobs": list_jobs,
    "register": register,
    "terminate": terminate,
    "describe_repositories": describe_repositories,
    "describe_clusters": describe_clusters,
}


def handler(event, context):
    call = CALLS[event["op"]]
    if not event.get("catch"):
        return {"statusCode": 200, "body": json.dumps(call(event))}
    try:
        answered = call(event)
    except ClientError as refused:
        return {
            "statusCode": 502,
            "body": json.dumps({
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }),
        }
    return {"statusCode": 200, "body": json.dumps(answered)}
"#;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the job it submits is the job this suite reads back.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request and the access key id is what names the caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, carrying the account's
/// own published key — so the setup and read-back legs below are the account's calls and never
/// any row's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One `awsJson1_1` call of the service signing as `signing_name`, under
/// `<target prefix>.<operation>`, made as the account — which must succeed: these are the
/// suite's setup and read-back legs.
async fn json_rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization(signing_name))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// The container a definition below describes: an image and the reservation an EC2-capability
/// container job states.
fn container_properties() -> Value {
    json!({
        "image": IMAGE,
        "resourceRequirements": [
            {"type": "VCPU", "value": VCPU},
            {"type": "MEMORY", "value": MEMORY},
        ],
    })
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document the execution role carries: the log actions every execution role needs,
/// and the five AWS Batch actions the served rows ask for — and NOT the termination the last row
/// asks for, so one document decides both halves.
fn role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:PutLogEvents",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": [
                    "batch:SubmitJob",
                    "batch:DescribeJobs",
                    "batch:DescribeJobQueues",
                    "batch:ListJobs",
                    "batch:RegisterJobDefinition",
                    "ecr:DescribeRepositories",
                    "ecs:DescribeClusters",
                ],
                "Resource": "*",
            },
        ],
    })
}

/// The template the suite deploys: the execution role carrying that document, and the wrapper
/// function running as it, carrying the queue and the definition it submits against as the two
/// environment variables its handler reads. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE_NAME,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "wrapper-grants",
                        "PolicyDocument": role_document(),
                    }],
                },
            },
            "Wrapper": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {"JOB_QUEUE": QUEUE, "JOB_DEFINITION": DEFINITION},
                    },
                    "Code": {"ZipFile": WRAPPER_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy that stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares a named IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures {failures:?}",
        reported.1
    );
    let (status, described) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {described}"
    );
    physical_ids_of(&described)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {name:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The AWS Batch estate the wrapper submits against, created as the account: an UNMANAGED
/// compute environment (so nothing of EC2's has to exist for it), the queue in front of it, and
/// the container definition the submit names. Answers the queue's own ARN.
async fn batch_estate(router: &MultiServiceRouter) -> String {
    let case = "setup";
    served(
        router,
        case,
        "/v1/createcomputeenvironment",
        &json!({"computeEnvironmentName": ENVIRONMENT, "type": "UNMANAGED"}),
    )
    .await;
    let queue = served(
        router,
        case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": QUEUE,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": ENVIRONMENT}],
        }),
    )
    .await;
    served(
        router,
        case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": DEFINITION,
            "type": "container",
            "containerProperties": container_properties(),
        }),
    )
    .await;
    queue["jobQueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected the queue's ARN in {queue}"))
        .to_string()
}

/// The container registry's repository and the container service's cluster, created as the
/// account so that what the handler reads of each is that service's own record. Answers the
/// repository's URI and the cluster's ARN, both minted by the service that holds them.
async fn neighbouring_estate(router: &MultiServiceRouter) -> (String, String) {
    let case = "setup";
    let created = json_rpc(
        router,
        case,
        ECR_SIGNING_NAME,
        ECR_TARGET,
        "CreateRepository",
        &json!({"repositoryName": REPOSITORY}),
    )
    .await;
    let repository_uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected the repository's URI in {created}"))
        .to_string();
    let cluster = json_rpc(
        router,
        case,
        ECS_SIGNING_NAME,
        ECS_TARGET,
        "CreateCluster",
        &json!({"clusterName": CLUSTER}),
    )
    .await;
    let cluster_arn = cluster["cluster"]["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected the cluster's ARN in {cluster}"))
        .to_string();
    (repository_uri, cluster_arn)
}

/// Look again until the queue reports [`QUEUE_VALID`]: the status is owed from the instant the
/// clock brings the create due, which no fixed sleep can name.
async fn wait_until_queue_valid(router: &MultiServiceRouter) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = served(
            router,
            "setup",
            "/v1/describejobqueues",
            &json!({"jobQueues": [QUEUE]}),
        )
        .await;
        if body["jobQueues"][0]["status"].as_str() == Some(QUEUE_VALID) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the job queue must reach {QUEUE_VALID}, last {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// WHICH operation of the called service one row's handler asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// `SubmitJob`: the write that mints the job the wrapper exists to start.
    Submit,
    /// `DescribeJobs`: the read of one resource, by the id that write minted.
    DescribeJobs,
    /// `DescribeJobQueues`: the read of a collection.
    DescribeQueues,
    /// `ListJobs`: the listing scoped to one resource.
    ListJobs,
    /// `RegisterJobDefinition`: a second write, which mints a revision.
    Register,
    /// `SubmitJob` naming a queue the account holds nothing for: the call the service itself
    /// refuses.
    SubmitToAbsentQueue,
    /// `TerminateJob`: the call the execution role does not grant.
    Terminate,
    /// `DescribeRepositories`, of the container registry: another service, over another
    /// protocol.
    DescribeRepositories,
    /// `DescribeClusters`, of the container service: a third service, over the same protocol as
    /// the registry but a different model.
    DescribeClusters,
}

impl Probe {
    /// The word the event names it by, which the handler dispatches on.
    fn op(self) -> &'static str {
        match self {
            Self::Submit | Self::SubmitToAbsentQueue => "submit",
            Self::DescribeJobs => "describe_jobs",
            Self::DescribeQueues => "describe_queues",
            Self::ListJobs => "list_jobs",
            Self::Register => "register",
            Self::Terminate => "terminate",
            Self::DescribeRepositories => "describe_repositories",
            Self::DescribeClusters => "describe_clusters",
        }
    }

    /// The IAM action the operation performs, which a statement grants and a refusal names:
    /// Batch's actions are published as `batch:<OperationName>`.
    fn action(self) -> &'static str {
        match self {
            Self::Submit | Self::SubmitToAbsentQueue => "batch:SubmitJob",
            Self::DescribeJobs => "batch:DescribeJobs",
            Self::DescribeQueues => "batch:DescribeJobQueues",
            Self::ListJobs => "batch:ListJobs",
            Self::Register => "batch:RegisterJobDefinition",
            Self::Terminate => "batch:TerminateJob",
            Self::DescribeRepositories => "ecr:DescribeRepositories",
            Self::DescribeClusters => "ecs:DescribeClusters",
        }
    }
}

/// One row: the probe the invocation carries, and the case it is reported under.
struct Row {
    case: &'static str,
    probe: Probe,
}

/// The table. Its rows differ along which operation of the called service the handler asks for,
/// and so which binding of that service's protocol the runtime carries: two writes, three reads
/// of different arity, a call the service refuses and a call the role does not grant.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a submit, the write the wrapper exists to make",
            probe: Probe::Submit,
        },
        Row {
            case: "a read of one job by the id the submit minted",
            probe: Probe::DescribeJobs,
        },
        Row {
            case: "a read of the queue collection",
            probe: Probe::DescribeQueues,
        },
        Row {
            case: "a listing scoped to the queue",
            probe: Probe::ListJobs,
        },
        Row {
            case: "a write that registers a definition revision",
            probe: Probe::Register,
        },
        Row {
            case: "a read of the repository an image lives in, of another service",
            probe: Probe::DescribeRepositories,
        },
        Row {
            case: "a read of the cluster a task runs on, of a third service",
            probe: Probe::DescribeClusters,
        },
        Row {
            case: "a submit naming a queue the account does not hold",
            probe: Probe::SubmitToAbsentQueue,
        },
        Row {
            case: "a termination the execution role does not grant",
            probe: Probe::Terminate,
        },
    ]
}

/// The event one row's invocation carries. `job_id` is the id the submit row minted, which the
/// rows that address one job are stated against.
fn event(probe: Probe, job_id: &str, catch: bool) -> Value {
    let mut carried = json!({"op": probe.op(), "catch": catch});
    match probe {
        Probe::Submit => {
            carried["jobName"] = json!(JOB_NAME);
            carried["parameters"] = json!({"shard": "0"});
        }
        Probe::SubmitToAbsentQueue => {
            carried["jobName"] = json!(JOB_NAME);
            carried["jobQueue"] = json!(ABSENT_QUEUE);
        }
        Probe::DescribeJobs => carried["jobs"] = json!([job_id]),
        Probe::Terminate => {
            carried["jobId"] = json!(job_id);
            carried["reason"] = json!("asked for by the suite");
        }
        Probe::Register => {
            carried["jobDefinitionName"] = json!(HANDLER_DEFINITION);
            carried["containerProperties"] = container_properties();
        }
        Probe::ListJobs => carried["jobStatus"] = json!(RESTING_STATUS),
        Probe::DescribeRepositories => carried["repository"] = json!(REPOSITORY),
        Probe::DescribeClusters => carried["cluster"] = json!(CLUSTER),
        Probe::DescribeQueues => {}
    }
    carried
}

/// The `X-Amz-Function-Error` an invocation carries, which is set exactly when the run failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Invoke the wrapper with `probe` and answer the document its handler returned, asserting the
/// successful-invocation half of the rule: HTTP 200, no function-error header, and the handler's
/// own return value as the payload.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    probe: Probe,
    job_id: &str,
) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event(probe, job_id, true).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    assert_eq!(
        function_error(&headers),
        None,
        "case {case:?}: a handler that returns runs to completion, so no function error is \
         reported; got {:?} with {payload}",
        function_error(&headers)
    );
    payload
}

/// The document the wrapper's `body` carries, with its envelope asserted: the two keys a client
/// of such a wrapper parses, and the status this row must report.
fn body_of(case: &str, payload: &Value, expected_status: u64) -> Value {
    let object = payload
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected the handler's document, got {payload}"));
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["body", "statusCode"],
        "case {case:?}: expected the wrapper's two keys, got {keys:?} in {payload}"
    );
    assert_eq!(
        payload["statusCode"].as_u64(),
        Some(expected_status),
        "case {case:?}: expected statusCode {expected_status}, got {payload}"
    );
    let text = payload["body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected body to carry text, got {payload}"));
    serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("case {case:?}: expected body to carry JSON, got {text:?}"))
}

/// The one `JobDetail` AWS Batch reports an independent reader for `job_id`.
async fn described(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let body = served(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
    let jobs = body["jobs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        jobs.len(),
        1,
        "case {case:?}: expected exactly one described job for {job_id:?}, got {body}"
    );
    jobs[0].clone()
}

/// The ids of every job AWS Batch reports at rest on the suite's queue, read as the account.
async fn ids_on_the_queue(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = served(
        router,
        case,
        "/v1/listjobs",
        &json!({"jobQueue": QUEUE, "jobStatus": RESTING_STATUS}),
    )
    .await;
    body["jobSummaryList"]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .filter_map(|summary| summary["jobId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The revisions AWS Batch reports for the definition `name`, read as the account.
async fn revisions_of(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<i64> {
    let body = served(
        router,
        case,
        "/v1/describejobdefinitions",
        &json!({"jobDefinitionName": name}),
    )
    .await;
    body["jobDefinitions"]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .filter_map(|definition| definition["revision"].as_i64())
                .collect()
        })
        .unwrap_or_default()
}

/// Look again until the job `job_id` comes to rest at [`RESTING_STATUS`]: the phases a submit
/// opens are owed by the clock, which no fixed sleep can name.
async fn wait_until_job_rests(router: &MultiServiceRouter, case: &str, job_id: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        if detail["status"].as_str() == Some(RESTING_STATUS) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the job must come to rest at {RESTING_STATUS}, last {detail}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// RULE: a handler reaches a service the runtime's SDK binds over that service's own protocol,
/// the answer it reads is that service's own, and an invocation whose handler returns is
/// answered HTTP 200 with no function-error header and the handler's document as the payload.
#[tokio::test]
async fn a_wrapper_handler_submits_and_reads_batch_work_through_the_runtimes_sdk() {
    let router = build_router();
    let queue_arn = batch_estate(&router).await;
    let (repository_uri, cluster_arn) = neighbouring_estate(&router).await;
    let physical = deploy(&router).await;
    let function = physical
        .get(FUNCTION_LOGICAL)
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must report a physical id for the wrapper"));
    assert_eq!(
        function, FUNCTION_NAME,
        "setup: the stack must name the function {FUNCTION_NAME:?}, got {function:?}"
    );
    wait_until_queue_valid(&router).await;
    wait_until_active(&router, &function).await;

    // The id the submit row mints, which the rows addressing one job are stated against.
    let mut submitted = String::new();

    for row in rows() {
        let case = row.case;
        match row.probe {
            Probe::Submit => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let job_id = body["jobId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: expected a jobId in {body}"));
                assert!(
                    !job_id.is_empty(),
                    "case {case:?}: expected the id AWS Batch minted, got {body}"
                );
                assert_eq!(
                    body["jobName"],
                    json!(JOB_NAME),
                    "case {case:?}: expected the job name the handler submitted, got {body}"
                );
                // The other half: the job EXISTS where the service that owns it holds it, on the
                // queue the function's own environment named.
                let detail = described(&router, case, job_id).await;
                assert_eq!(
                    detail["jobName"],
                    json!(JOB_NAME),
                    "case {case:?}: expected the submitted job's name, got {detail}"
                );
                assert_eq!(
                    detail["jobQueue"],
                    json!(queue_arn),
                    "case {case:?}: expected the job on the queue the wrapper's environment \
                     named, got {detail}"
                );
                submitted = job_id.to_string();
            }
            Probe::DescribeJobs => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let reported = body["jobs"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?}: expected the one job the id names, got {body}"
                );
                assert_eq!(
                    reported[0]["jobId"],
                    json!(submitted),
                    "case {case:?}: expected the job the handler asked about, got {body}"
                );
                assert_eq!(
                    reported[0]["jobName"],
                    json!(JOB_NAME),
                    "case {case:?}: expected the submitted job's name, got {body}"
                );
                assert_eq!(
                    reported[0]["jobQueue"],
                    json!(queue_arn),
                    "case {case:?}: expected the queue the job was placed on, got {body}"
                );
                let status = reported[0]["status"].as_str().unwrap_or_default();
                assert!(
                    JOB_STATUS_VALUES.contains(&status),
                    "case {case:?}: expected a status this service models, got {status:?} in \
                     {body}"
                );
            }
            Probe::DescribeQueues => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let reported = body["jobQueues"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?}: expected the one queue the handler asked about, got {body}"
                );
                assert_eq!(
                    reported[0]["jobQueueName"],
                    json!(QUEUE),
                    "case {case:?}: expected the queue's own name, got {body}"
                );
                assert_eq!(
                    reported[0]["jobQueueArn"],
                    json!(queue_arn),
                    "case {case:?}: expected the ARN AWS Batch minted for the queue, got {body}"
                );
                assert_eq!(
                    reported[0]["status"],
                    json!(QUEUE_VALID),
                    "case {case:?}: expected the settled queue's status, got {body}"
                );
                assert_eq!(
                    reported[0]["state"],
                    json!(QUEUE_ENABLED),
                    "case {case:?}: expected the queue's own state, got {body}"
                );
            }
            Probe::ListJobs => {
                wait_until_job_rests(&router, case, &submitted).await;
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let ids: Vec<String> = body["jobIds"]
                    .as_array()
                    .map(|listed| {
                        listed
                            .iter()
                            .filter_map(|id| id.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(
                    ids.contains(&submitted),
                    "case {case:?}: expected the submitted job in the listing, got {body}"
                );
                assert_eq!(
                    ids,
                    ids_on_the_queue(&router, case).await,
                    "case {case:?}: expected the listing the service answers an independent \
                     reader, got {body}"
                );
            }
            Probe::Register => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                assert_eq!(
                    body["jobDefinitionName"],
                    json!(HANDLER_DEFINITION),
                    "case {case:?}: expected the definition the handler registered, got {body}"
                );
                let revision = body["revision"].as_i64().unwrap_or_default();
                let wanted_arn = format!(
                    "arn:aws:batch:{REGION}:{ACCOUNT}:job-definition/{HANDLER_DEFINITION}:\
                     {revision}"
                );
                assert_eq!(
                    body["jobDefinitionArn"].as_str(),
                    Some(wanted_arn.as_str()),
                    "case {case:?}: expected the ARN AWS Batch minted for the revision, got \
                     {body}"
                );
                // The other half: the revision is in the service that owns it.
                assert_eq!(
                    revisions_of(&router, case, HANDLER_DEFINITION).await,
                    vec![revision],
                    "case {case:?}: expected the registered revision to be the one the service \
                     holds, got {body}"
                );
            }
            Probe::DescribeRepositories => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let reported = body["repositories"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?}: expected the one repository the handler asked about, got \
                     {body}"
                );
                assert_eq!(
                    reported[0]["repositoryName"],
                    json!(REPOSITORY),
                    "case {case:?}: expected the repository's own name, got {body}"
                );
                assert_eq!(
                    reported[0]["repositoryUri"],
                    json!(repository_uri),
                    "case {case:?}: expected the URI the registry minted for it, got {body}"
                );
                assert_eq!(
                    reported[0]["registryId"],
                    json!(ACCOUNT),
                    "case {case:?}: expected the registry the repository belongs to, got {body}"
                );
            }
            Probe::DescribeClusters => {
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 200);
                let reported = body["clusters"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?}: expected the one cluster the handler asked about, got {body}"
                );
                assert_eq!(
                    reported[0]["clusterName"],
                    json!(CLUSTER),
                    "case {case:?}: expected the cluster's own name, got {body}"
                );
                assert_eq!(
                    reported[0]["clusterArn"],
                    json!(cluster_arn),
                    "case {case:?}: expected the ARN the container service minted for it, got \
                     {body}"
                );
                assert_eq!(
                    reported[0]["status"],
                    json!(CLUSTER_ACTIVE),
                    "case {case:?}: expected the created cluster's status, got {body}"
                );
            }
            Probe::SubmitToAbsentQueue => {
                let before = ids_on_the_queue(&router, case).await;
                // What the SERVICE answers the same call, asked by the account: the sentence the
                // handler must have read is this one and not a wording of this runtime's.
                let direct = batch(
                    &router,
                    ACCOUNT_KEY,
                    "/v1/submitjob",
                    &json!({
                        "jobName": JOB_NAME,
                        "jobQueue": ABSENT_QUEUE,
                        "jobDefinition": DEFINITION,
                    }),
                )
                .await;
                assert_eq!(
                    direct.status, CLIENT_EXCEPTION_STATUS,
                    "case {case:?}: expected the service to refuse the account's own call too, \
                     got {direct:?}"
                );

                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 502);
                assert_eq!(
                    body["code"],
                    json!(CLIENT_EXCEPTION),
                    "case {case:?}: expected the class AWS Batch refused with, got {body}"
                );
                assert_eq!(
                    body["status"].as_u64(),
                    Some(u64::from(CLIENT_EXCEPTION_STATUS)),
                    "case {case:?}: expected the status AWS Batch answered, got {body}"
                );
                assert_eq!(
                    body["message"],
                    json!(direct.message()),
                    "case {case:?}: expected the sentence the service itself answers, got {body}"
                );
                assert_eq!(
                    ids_on_the_queue(&router, case).await,
                    before,
                    "case {case:?}: a refused submit mints no job"
                );
            }
            Probe::Terminate => {
                let before = described(&router, case, &submitted).await;
                let payload = invoked(&router, case, &function, row.probe, &submitted).await;
                let body = body_of(case, &payload, 502);
                assert_eq!(
                    body["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected the class an unauthorized call is refused with, got \
                     {body}"
                );
                assert_eq!(
                    body["status"].as_u64(),
                    Some(u64::from(ACCESS_DENIED_STATUS)),
                    "case {case:?}: expected the status an unauthorized call is refused with, \
                     got {body}"
                );
                let opening = format!(
                    "User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{FUNCTION_NAME} is \
                     not authorized to perform: {}",
                    row.probe.action()
                );
                let message = body["message"].as_str().unwrap_or_default();
                assert!(
                    message.starts_with(&opening),
                    "case {case:?}: expected the refusal to name the function's own role \
                     session and the action it asked for ({opening:?}), got {message:?}"
                );
                // A refused termination leaves the job as it was: no reason of this call's is
                // recorded on it.
                let after = described(&router, case, &submitted).await;
                assert_eq!(
                    after["statusReason"], before["statusReason"],
                    "case {case:?}: a refused termination records no reason on the job, got \
                     {after}"
                );
            }
        }
    }

    assert_an_uncaught_refusal_is_the_functions_own_error(&router, &function).await;
}

/// The other half of the in-band signal: the refusal reaches the handler as its own SDK's
/// exception, so one the handler does NOT catch is the FUNCTION's failure — the `Invoke` is still
/// answered HTTP 200, and THAT is when `X-Amz-Function-Error` carries `Unhandled` and the payload
/// is the run's error document rather than a handler document.
///
/// Driven through the same deployed function, with the event asking for the refusal to propagate.
async fn assert_an_uncaught_refusal_is_the_functions_own_error(
    router: &MultiServiceRouter,
    function: &str,
) {
    let case = "a refusal the handler does not catch";
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event(Probe::SubmitToAbsentQueue, "", false)
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    assert_eq!(
        function_error(&headers).as_deref(),
        Some(UNHANDLED),
        "case {case:?}: an uncaught refusal must surface as the function's own error, got {:?} \
         with {payload}",
        function_error(&headers)
    );
    assert!(
        payload["errorType"]
            .as_str()
            .is_some_and(|kind| !kind.is_empty()),
        "case {case:?}: the error document must name the class that was raised, got {payload}"
    );
    assert!(
        payload["errorMessage"]
            .as_str()
            .is_some_and(|sentence| sentence.contains(ABSENT_QUEUE)),
        "case {case:?}: the error document must carry what AWS Batch refused the call with, got \
         {payload}"
    );
}
