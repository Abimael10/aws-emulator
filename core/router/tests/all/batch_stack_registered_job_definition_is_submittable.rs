// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` for an estate of
//! `AWS::Batch::ComputeEnvironment` + `AWS::Batch::JobQueue` + `AWS::Batch::JobDefinition` has
//! really REGISTERED that job definition in AWS Batch, so the next call a deployment makes —
//! `SubmitJob` addressed by the handles the stack declared or published — is served rather
//! than refused as naming nothing.
//!
//! The rule under test: **a resource a stack reports created exists in the service that OWNS
//! it, addressable by every handle that service's own readers take — so once the stack is
//! settled the job definition an `AWS::Batch::JobDefinition` declares is one
//! `DescribeJobDefinitions` answers, `ACTIVE` at the revision AWS Batch allocated, and
//! `SubmitJob` accepts it referenced "by name, name:revision, or ARN"
//! (`SubmitJob$jobDefinition`) exactly as it accepts a revision registered through
//! `RegisterJobDefinition` itself.** Two crates have to agree for that to hold —
//! CloudFormation makes the registration and AWS Batch serves it — so the suite lives here
//! rather than in either crate.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the job
//! definition and the queue (through template parameters) or leaves CloudFormation to generate
//! both names. Inside a row the submits differ along the handle form the client addresses the
//! registration with (the bare name, `name:revision`, the published ARN) and the form it
//! addresses the queue with (its name, its ARN), which are the forms `SubmitJob` documents for
//! each member.
//!
//! Every expected value is computed here from what the row declares, except the GENERATED
//! names of the row that declares none, which are CloudFormation's to compute: those are read
//! off the handles the stack published and then required to resolve in AWS Batch.
//!
//! Sibling suites: `core/router/tests/all/cloudformation_provisions_batch_job_definitions.rs`
//! pins the identifier a stack publishes for the type (and its tags, properties and teardown);
//! this one pins that the registration behind that identifier is one the Batch JOB APIs serve.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::state::job_arn;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`, `tests/all/cloudformation_provisions_stack_resources.rs`).
use super::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use super::query_wire::form_encode;

/// The Region the stacks here deploy in, which is the Region every ARN they publish carries.
const REGION: &str = "us-west-2";

/// How long a settled stack is waited for: a generous deadline the poll below gives up at, not
/// a sleep. What a case waits for is the CONDITION — a stack status that is no longer in
/// progress — so a loaded host only makes the wait longer and never changes the answer.
const SETTLED_DEADLINE: Duration = Duration::from_secs(120);

/// How often that poll looks again.
const SETTLED_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `JobDefinition$status` AWS Batch reports for a revision that can be submitted.
const ACTIVE: &str = "ACTIVE";

/// The stack status a deployment that created everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The segment a job-definition ARN carries its name and revision after.
const JOB_DEFINITION_ARN_SEGMENT: &str = "job-definition/";

/// The revision AWS Batch allocates for a name no earlier registration in this emulator used:
/// "the revision of the job definition" starts at 1.
const FIRST_REVISION: i64 = 1;

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its own request either way (the
/// credential scope is what routes it).
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
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
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

/// Call one AWS Batch operation over its restJson1 wire.
async fn batch(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The ARN AWS Batch mints for revision `revision` of the job definition `name`.
fn job_definition_arn(name: &str, revision: i64) -> String {
    format!(
        "arn:aws:batch:{REGION}:{}:{JOB_DEFINITION_ARN_SEGMENT}{name}:{revision}",
        engine::EMULATOR_ACCOUNT_ID,
    )
}

/// The NAME a job-definition ARN carries (`…:job-definition/<name>:<revision>`).
fn name_in(case: &str, arn: &str) -> String {
    let path = arn
        .split_once(JOB_DEFINITION_ARN_SEGMENT)
        .map(|(_, path)| path)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a job-definition ARN, got {arn:?}");
        });
    match path.rsplit_once(':') {
        Some((name, _revision)) => name.to_string(),
        None => path.to_string(),
    }
}

/// The NAME an `arn:aws:batch:…:<kind>/<name>` names, which is how AWS Batch keys the queue a
/// stack published the ARN of.
fn resource_name_in(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

/// The stack's `DescribeStacks` body, once its status no longer ends `_IN_PROGRESS`.
async fn settled(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled, last status {reported:?}"
        );
        tokio::time::sleep(SETTLED_POLL_INTERVAL).await;
    }
}

/// The template both rows deploy: the three Batch resources an estate is deployed as, with the
/// two names taken from template PARAMETERS so a row can either declare them or leave the
/// properties out altogether.
///
/// `declared` says whether the two name properties are present at all; a row that leaves them
/// out is one CloudFormation generates both names for.
fn template(declared: bool) -> String {
    let mut job_queue = json!({
        "Priority": 1,
        "State": "ENABLED",
        "ComputeEnvironmentOrder": [
            { "Order": 1, "ComputeEnvironment": { "Ref": "ComputeEnvironment" } }
        ],
    });
    let mut job_definition = json!({
        "Type": "container",
        "PlatformCapabilities": ["FARGATE"],
        "ContainerProperties": {
            "Image": "public.ecr.aws/example/stack-worker:1",
            // "For jobs that run on Fargate resources, you must provide an execution role"
            // (`ContainerProperties$executionRoleArn`).
            "ExecutionRoleArn": format!(
                "arn:aws:iam::{}:role/batchStackExecution",
                engine::EMULATOR_ACCOUNT_ID,
            ),
            "ResourceRequirements": [
                { "Type": "VCPU", "Value": "0.25" },
                { "Type": "MEMORY", "Value": "512" }
            ],
        },
    });
    if declared {
        job_queue["JobQueueName"] = json!({ "Ref": "JobQueueName" });
        job_definition["JobDefinitionName"] = json!({ "Ref": "JobDefinitionName" });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an AWS Batch estate deployed by CloudFormation",
        "Parameters": {
            "JobDefinitionName": { "Type": "String" },
            "JobQueueName": { "Type": "String" },
        },
        "Resources": {
            "ComputeEnvironment": {
                "Type": "AWS::Batch::ComputeEnvironment",
                "Properties": {
                    "Type": "MANAGED",
                    "State": "ENABLED",
                    "ComputeResources": {
                        "Type": "FARGATE",
                        "MaxvCpus": 16,
                        "Subnets": ["subnet-0123456789abcdef0"],
                        "SecurityGroupIds": ["sg-0123456789abcdef0"],
                    },
                },
            },
            "JobQueue": {
                "Type": "AWS::Batch::JobQueue",
                "Properties": job_queue,
            },
            "JobDefinition": {
                "Type": "AWS::Batch::JobDefinition",
                "Properties": job_definition,
            },
        },
        "Outputs": {
            "JobDefinitionArn": {
                "Value": { "Fn::GetAtt": ["JobDefinition", "JobDefinitionArn"] }
            },
            "JobQueueArn": { "Value": { "Ref": "JobQueue" } },
        },
    })
    .to_string()
}

/// One row of the table: a stack, and whether the names the two addressable resources carry are
/// the template's own or CloudFormation's.
struct Row {
    case: &'static str,
    /// Whether the template declares `JobDefinitionName` and `JobQueueName`.
    declared: bool,
    /// The value the `JobDefinitionName` parameter is given, which a declaring row is then
    /// required to have registered the revision under.
    definition_parameter: &'static str,
    /// The value the `JobQueueName` parameter is given.
    queue_parameter: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a stack naming its job definition and its queue",
        declared: true,
        definition_parameter: "orders-stack-jd",
        queue_parameter: "orders-stack-queue",
    },
    Row {
        case: "a stack leaving CloudFormation to name both",
        declared: false,
        definition_parameter: "unused-stack-jd",
        queue_parameter: "unused-stack-queue",
    },
];

/// How one submit addresses the two resources the stack deployed.
struct Handles {
    case: &'static str,
    /// The `SubmitJob$jobDefinition` form: "by name, name:revision, or ARN".
    definition: fn(&str, &str) -> String,
    /// The `SubmitJob$jobQueue` form: "the name or the Amazon Resource Name (ARN)".
    queue: fn(&str, &str) -> String,
}

/// Every handle form of the pair, so the registration is required to answer to each one the
/// operation documents rather than only to the identifier the stack happened to publish.
const HANDLES: &[Handles] = &[
    Handles {
        case: "the bare job-definition name, the queue by name",
        definition: |name, _arn| name.to_string(),
        queue: |name, _arn| name.to_string(),
    },
    Handles {
        case: "name:revision, the queue by name",
        definition: |name, _arn| format!("{name}:{FIRST_REVISION}"),
        queue: |name, _arn| name.to_string(),
    },
    Handles {
        case: "the published job-definition ARN, the queue by ARN",
        definition: |_name, arn| arn.to_string(),
        queue: |_name, arn| arn.to_string(),
    },
    Handles {
        case: "the bare job-definition name, the queue by ARN",
        definition: |name, _arn| name.to_string(),
        queue: |_name, arn| arn.to_string(),
    },
];

/// RULE: the job definition a settled stack declared is registered in AWS Batch — one
/// `DescribeJobDefinitions` answers, `ACTIVE` at the revision that service allocated — and
/// `SubmitJob` accepts it under every handle form the operation documents, on the queue the
/// same stack deployed, answering 200 with a job that service then reports against the
/// registered revision.
#[tokio::test]
async fn a_settled_stacks_batch_job_definition_is_registered_and_submittable() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("batch-estate-{index}");
        let body = template(row.declared);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &body),
                ("Parameters.member.1.ParameterKey", "JobDefinitionName"),
                ("Parameters.member.1.ParameterValue", row.definition_parameter),
                ("Parameters.member.2.ParameterKey", "JobQueueName"),
                ("Parameters.member.2.ParameterValue", row.queue_parameter),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );

        let described = settled(&router, case, &stack_name).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} in \
             {described}"
        );

        let outputs = outputs_of(&described);
        let definition_arn = outputs
            .get("JobDefinitionArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no JobDefinitionArn output in {described}"));
        let queue_arn = outputs
            .get("JobQueueArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no JobQueueArn output in {described}"));

        // The name the revision belongs to: the declared one, and otherwise the one
        // CloudFormation generated, which is the ARN's own name segment.
        let definition_name = name_in(case, &definition_arn);
        if row.declared {
            assert_eq!(
                definition_name, row.definition_parameter,
                "case {case:?}: expected the declared name to be the registration's, got \
                 {definition_name:?} in {definition_arn:?}"
            );
        }
        let queue_name = resource_name_in(&queue_arn);
        if row.declared {
            assert_eq!(
                queue_name, row.queue_parameter,
                "case {case:?}: expected the declared queue name, got {queue_name:?} in \
                 {queue_arn:?}"
            );
        }
        let expected_arn = job_definition_arn(&definition_name, FIRST_REVISION);
        assert_eq!(
            definition_arn, expected_arn,
            "case {case:?}: expected the stack to publish {expected_arn:?}, got \
             {definition_arn:?}"
        );

        // The registration is AWS Batch's own: the reader a client runs after a deployment
        // answers for the BARE NAME the template declared.
        let (status, answered) = batch(
            &router,
            "/v1/describejobdefinitions",
            &json!({ "jobDefinitions": [&definition_name] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeJobDefinitions on \
             {definition_name:?}, got {status} with body {answered}"
        );
        let listed = answered["jobDefinitions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one registered revision for {definition_name:?}, \
             got {answered}"
        );
        let record = &listed[0];
        assert_eq!(
            record["jobDefinitionName"].as_str(),
            Some(definition_name.as_str()),
            "case {case:?}: expected the revision to carry the name, got {record}"
        );
        assert_eq!(
            record["jobDefinitionArn"].as_str(),
            Some(definition_arn.as_str()),
            "case {case:?}: expected the revision to carry the published ARN, got {record}"
        );
        assert_eq!(
            record["revision"].as_i64(),
            Some(FIRST_REVISION),
            "case {case:?}: expected revision {FIRST_REVISION}, got {record}"
        );
        assert_eq!(
            record["status"].as_str(),
            Some(ACTIVE),
            "case {case:?}: expected the revision to be {ACTIVE}, got {record}"
        );

        // And every handle form the operation documents addresses it, on the queue the same
        // stack deployed.
        for (slot, handles) in HANDLES.iter().enumerate() {
            let case = format!("{case} submitted with {}", handles.case);
            let case = case.as_str();
            let job_name = format!("stack-job-{index}-{slot}");
            let (status, submitted) = batch(
                &router,
                "/v1/submitjob",
                &json!({
                    "jobName": &job_name,
                    "jobQueue": (handles.queue)(&queue_name, &queue_arn),
                    "jobDefinition": (handles.definition)(&definition_name, &definition_arn),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from SubmitJob, got {status} with body \
                 {submitted}"
            );
            assert_eq!(
                submitted["jobName"].as_str(),
                Some(job_name.as_str()),
                "case {case:?}: expected the submitted job name, got {submitted}"
            );
            let job_id = submitted["jobId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| panic!("case {case:?}: expected a jobId, got {submitted}"))
                .to_string();
            let expected_job_arn = job_arn(REGION, &job_id);
            assert_eq!(
                submitted["jobArn"].as_str(),
                Some(expected_job_arn.as_str()),
                "case {case:?}: expected the job's own ARN {expected_job_arn:?}, got {submitted}"
            );

            // The job AWS Batch stored is backed by the registered revision: its reader reports
            // the job against the ARN the stack published, whichever handle named it.
            let (status, jobs) =
                batch(&router, "/v1/describejobs", &json!({ "jobs": [&job_id] })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeJobs, got {status} with body {jobs}"
            );
            let detail = jobs["jobs"]
                .as_array()
                .and_then(|held| held.first())
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: expected the job in {jobs}"));
            assert_eq!(
                detail["jobDefinition"].as_str(),
                Some(definition_arn.as_str()),
                "case {case:?}: expected the job to run the registered revision \
                 {definition_arn:?}, got {detail}"
            );
            assert_eq!(
                detail["jobQueue"].as_str(),
                Some(queue_arn.as_str()),
                "case {case:?}: expected the job to be queued on {queue_arn:?}, got {detail}"
            );
        }
    }
}
