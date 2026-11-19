// SPDX-License-Identifier: CC0-1.0

//! What a placed AWS Batch job PROPAGATES to the Amazon ECS task it runs as, driven end to end
//! across the services that own the facts: Batch, Amazon ECS and Amazon VPC on one engine.
//!
//! The RULE under test: **a job whose job definition or whose submit asks for tag propagation
//! runs as an Amazon ECS task carrying the job definition's tags AND the job's own tags, the
//! job's value winning on a key they share; a job that asks for none carries neither set** —
//! `JobDetail$propagateTags` is "whether to propagate the tags from the job or job definition to
//! the corresponding Amazon ECS task. If no value is specified, the tags aren't propagated …
//! For tags with the same name, job tags are given priority over job definitions tags". The task
//! is the one the job publishes (`AttemptContainerDetail$taskArn`, "the Amazon Resource Name
//! (ARN) of the Amazon ECS task that's associated with the job attempt") in the cluster the
//! compute environment publishes (`ComputeEnvironmentDetail$ecsClusterArn`), so Amazon ECS
//! answers for that ARN in that cluster with no failure and reports the propagated set under
//! `include: [TAGS]`.
//!
//! Sources: `smithy_query(query="doc",shape="JobDetail",member="propagateTags")` and
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` on the Batch side;
//! `smithy_query(query="operation",operation="DescribeTasks")` for `tasks`/`failures` and
//! `smithy_query(query="doc",shape="DescribeTasksRequest",member="include")` for the `TAGS`
//! field on the Amazon ECS side.
//!
//! The rows differ along what the rule generalises over: WHICH side asks for the propagation
//! (the job definition, the submit, both, neither) and WHICH side carries tags (both with a
//! shared key, the definition alone, the job alone).
//!
//! What is NOT asserted here, because another suite asserts it: that the task resolves at all
//! and what the job reports off its run
//! (`core/router/tests/all/batch_fargate_job_runs_as_an_ecs_task.rs`).

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

/// The `LaunchType` a task of a Fargate compute environment runs under. Source:
/// `smithy_query(query="shape",shape="LaunchType",model_path="services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json")`.
const FARGATE: &str = "FARGATE";

/// The `/16` this suite's VPC is carved out of, and the `/24` its one subnet takes.
const VPC_CIDR: &str = "10.67.0.0/16";
const SUBNET_CIDR: &str = "10.67.1.0/24";

/// The image every job definition below runs. Nothing here is decided by the fetch: the task
/// carries the tags the placement put on it whatever its container then does.
const IMAGE: &str = "registry.example.com/simparity/batch-tagged-worker:1";

/// The tag keys the rows below use: one only the job definition carries, one only the job
/// carries, and one BOTH carry — the key whose tie-break the rule names.
const DEFINITION_KEY: &str = "batchTagFromDefinition";
const JOB_KEY: &str = "batchTagFromJob";
const SHARED_KEY: &str = "batchTagShared";

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
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "batch-propagated-tags"),
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

/// One row of the table: which side asks for the propagation, which side carries tags, and the
/// set the task must end up with.
struct Row {
    case: &'static str,
    /// `RegisterJobDefinition$propagateTags`, left out of the request when false.
    definition_propagates: bool,
    /// `SubmitJob$propagateTags`, left out of the request when `None`.
    job_propagates: Option<bool>,
    definition_tags: &'static [(&'static str, &'static str)],
    job_tags: &'static [(&'static str, &'static str)],
    /// The tags the Amazon ECS task must carry.
    propagated: &'static [(&'static str, &'static str)],
}

/// Both sides' tags, sharing one key — the input the tie-break is read off.
const BOTH_SIDES_DEFINITION: &[(&str, &str)] = &[
    (DEFINITION_KEY, FROM_DEFINITION),
    (SHARED_KEY, FROM_DEFINITION),
];
const BOTH_SIDES_JOB: &[(&str, &str)] = &[(JOB_KEY, FROM_JOB), (SHARED_KEY, FROM_JOB)];
const BOTH_SIDES_MERGED: &[(&str, &str)] = &[
    (DEFINITION_KEY, FROM_DEFINITION),
    (JOB_KEY, FROM_JOB),
    (SHARED_KEY, FROM_JOB),
];

/// The table. Its rows differ along what the rule generalises over — see the module header.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the job definition asks for the propagation, both sides carry tags",
            definition_propagates: true,
            job_propagates: None,
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: BOTH_SIDES_MERGED,
        },
        Row {
            case: "the submit asks for the propagation, both sides carry tags",
            definition_propagates: false,
            job_propagates: Some(true),
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: BOTH_SIDES_MERGED,
        },
        Row {
            case: "both sides ask for the propagation",
            definition_propagates: true,
            job_propagates: Some(true),
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: BOTH_SIDES_MERGED,
        },
        Row {
            case: "a propagating job whose tags only the definition carries",
            definition_propagates: true,
            job_propagates: None,
            definition_tags: &[(DEFINITION_KEY, FROM_DEFINITION)],
            job_tags: &[],
            propagated: &[(DEFINITION_KEY, FROM_DEFINITION)],
        },
        Row {
            case: "a propagating job whose tags only the job carries",
            definition_propagates: false,
            job_propagates: Some(true),
            definition_tags: &[],
            job_tags: &[(JOB_KEY, FROM_JOB)],
            propagated: &[(JOB_KEY, FROM_JOB)],
        },
        Row {
            // The boundary: neither side asks, so neither side's tags travel — "if no value is
            // specified, the tags aren't propagated".
            case: "neither side asks for the propagation",
            definition_propagates: false,
            job_propagates: None,
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: &[],
        },
    ]
}

/// One row's tag list as the JSON map both Batch tag members take.
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

/// The `{key: value}` map a described Batch resource reports its tags as, read into the same
/// shape the comparisons below use.
fn batch_tags(found: &Value) -> BTreeMap<String, String> {
    found
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| (key, value.as_str().unwrap_or_default().to_string()))
        .collect()
}

/// The same pairs as a map, for comparing with what either service reports.
fn expected_map(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Register one row's job definition — a FARGATE container job whose `resourceRequirements`
/// name the smallest Fargate task size — and answer its name.
async fn register(router: &MultiServiceRouter, row: &Row, index: usize) -> String {
    let name = format!("batch-propagated-tags-definition-{index}");
    let mut body = json!({
        "jobDefinitionName": &name,
        "type": "container",
        "platformCapabilities": [FARGATE],
        "containerProperties": {
            "image": IMAGE,
            "command": ["echo", "the batch job ran this"],
            // "For jobs that run on Fargate resources, you must provide an execution role"
            // (`ContainerProperties$executionRoleArn`).
            "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
            "resourceRequirements": [
                {"type": "VCPU", "value": "0.25"},
                {"type": "MEMORY", "value": "512"},
            ],
        },
    });
    // `TagrisTagsMap` is modelled `len 1..50`, so a row carrying no tags sends no `tags`
    // member rather than an empty map, which is not a value the member takes.
    if !row.definition_tags.is_empty() {
        body["tags"] = tag_map(row.definition_tags);
    }
    if row.definition_propagates {
        body["propagateTags"] = json!(true);
    }
    batch(router, row.case, "/v1/registerjobdefinition", &body).await;
    name
}

/// Create one row's compute environment and the queue in front of it: `MANAGED` over `FARGATE`
/// capacity, in the one subnet this suite holds.
async fn capacity(
    router: &MultiServiceRouter,
    row: &Row,
    network: &Network,
    index: usize,
) -> (String, String) {
    let environment = format!("batch-propagated-tags-ce-{index}");
    let queue = format!("batch-propagated-tags-queue-{index}");
    batch(
        router,
        row.case,
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
        row.case,
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
async fn until_environment_valid(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
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

/// Look again until `job_id` has been PLACED — "each container attempt receives a task ARN when
/// they reach the `STARTING` status" — and answer the detail it reports then. Nothing here
/// sleeps for a span: it waits for the CONDITION.
async fn until_placed(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        if detail["attempts"][0]["container"]["taskArn"].is_string() {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job was never placed as a task, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `job_id` reports a terminal `JobStatus`. Source:
/// `smithy_query(query="shape",shape="JobStatus")` for the ladder's members.
async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default().to_string();
        if status == "SUCCEEDED" || status == "FAILED" {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a placed job that asks for tag propagation runs as an Amazon ECS task carrying the job
/// definition's tags and the job's own, the job's value winning on a shared key; one that asks
/// for none carries neither set.
#[tokio::test]
async fn a_propagating_jobs_task_carries_the_definition_and_job_tags_merged() {
    let rows = rows();
    let router = router();
    let network = network_of(&router).await;

    // Every row's estate is created, and every job submitted, BEFORE any of them is waited on,
    // so the spans the clocks owe them run together rather than one after another.
    let mut definitions = Vec::new();
    let mut environments = Vec::new();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        definitions.push(register(&router, row, index).await);
        let (environment, queue) = capacity(&router, row, &network, index).await;
        environments.push(environment);
        queues.push(queue);
    }
    let mut clusters = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        clusters.push(until_environment_valid(&router, row.case, &environments[index]).await);
        until_queue_valid(&router, row.case, &queues[index]).await;
    }
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut body = json!({
            "jobName": format!("batch-propagated-tags-job-{index}"),
            "jobQueue": &queues[index],
            "jobDefinition": &definitions[index],
        });
        if !row.job_tags.is_empty() {
            body["tags"] = tag_map(row.job_tags);
        }
        if let Some(propagates) = row.job_propagates {
            body["propagateTags"] = json!(propagates);
        }
        let answer = batch(&router, row.case, "/v1/submitjob", &body).await;
        submitted.push(
            answer["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {:?}: expected a jobId in {answer}", row.case))
                .to_string(),
        );
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let cluster = clusters[index].clone();
        let detail = until_placed(&router, case, &submitted[index]).await;
        let task_arn = detail["attempts"][0]["container"]["taskArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // The task EXISTS where Amazon ECS holds it, in the cluster the compute environment
        // publishes: that service answers for the ARN AWS Batch published with no failure, which
        // is what makes the handle the job reports usable at all.
        let answer = ecs(
            &router,
            case,
            "DescribeTasks",
            &json!({"cluster": &cluster, "tasks": [&task_arn], "include": ["TAGS"]}),
        )
        .await;
        let failures = answer["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "case {case:?}: Amazon ECS must resolve {task_arn} in {cluster}, got failures \
             {failures:?}"
        );
        let tasks = answer["tasks"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected one described task for {task_arn}, got {answer}"
        );
        assert_eq!(
            tasks[0]["clusterArn"].as_str(),
            Some(cluster.as_str()),
            "case {case:?}: the task must be reported in the environment's own cluster, got \
             {answer}"
        );
        assert_eq!(
            tasks[0]["launchType"].as_str(),
            Some(FARGATE),
            "case {case:?}: a job of a Fargate compute environment runs as a Fargate task, got \
             {answer}"
        );

        // The propagated set itself: both sides' tags where propagation was asked for, the
        // job's value on the key they share, and NOTHING where it was not.
        let found = ecs_tags(&tasks[0]["tags"]);
        let expected = expected_map(row.propagated);
        assert_eq!(
            found, expected,
            "case {case:?}: expected the task's tags to be {expected:?}, got {found:?}"
        );

        // And the merge is made for the TASK alone: each Batch resource still reports its own
        // tags, so nothing wrote the other side's set onto it.
        let job_tags = detail["tags"].clone();
        assert_eq!(
            batch_tags(&job_tags),
            expected_map(row.job_tags),
            "case {case:?}: the job must keep its own tags, got {job_tags}"
        );
        let definitions_answer = batch(
            &router,
            case,
            "/v1/describejobdefinitions",
            &json!({"jobDefinitionName": &definitions[index]}),
        )
        .await;
        let definition_tags = definitions_answer["jobDefinitions"][0]["tags"].clone();
        assert_eq!(
            batch_tags(&definition_tags),
            expected_map(row.definition_tags),
            "case {case:?}: the job definition must keep its own tags, got {definitions_answer}"
        );

        // "Tags can only be propagated to the task during task creation", so the set the
        // placement put on the task is the set it carries for the REST of its life: the job runs
        // to its end and the same answer comes back off the same task.
        let ended = until_terminal(&router, case, &submitted[index]).await;
        let settled = ecs(
            &router,
            case,
            "DescribeTasks",
            &json!({"cluster": &cluster, "tasks": [&task_arn], "include": ["TAGS"]}),
        )
        .await;
        let after = ecs_tags(&settled["tasks"][0]["tags"]);
        assert_eq!(
            after, expected,
            "case {case:?}: expected the task of the ended job ({}) to keep the tags \
             {expected:?}, got {after:?}",
            ended["status"]
        );
    }
}

/// RULE, read the way a client reads it: two jobs of ONE queue differing only in whether they
/// propagate are both tasks of the compute environment's cluster, so ONE `DescribeTasks` call
/// naming both ARNs resolves both with an empty `failures` list — and only the propagating leg's
/// task carries the merged set.
#[tokio::test]
async fn both_legs_of_one_queue_resolve_in_the_environments_cluster() {
    let case = "two jobs of one queue differing only in propagateTags";
    let router = router();
    let network = network_of(&router).await;
    let legs = [
        Row {
            case,
            definition_propagates: true,
            job_propagates: None,
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: BOTH_SIDES_MERGED,
        },
        Row {
            case,
            definition_propagates: false,
            job_propagates: None,
            definition_tags: BOTH_SIDES_DEFINITION,
            job_tags: BOTH_SIDES_JOB,
            propagated: &[],
        },
    ];

    // ONE compute environment and ONE queue, which both legs are submitted to.
    let (environment, queue) = capacity(&router, &legs[0], &network, 100).await;
    let mut definitions = Vec::new();
    for (index, leg) in legs.iter().enumerate() {
        definitions.push(register(&router, leg, 100 + index).await);
    }
    let cluster = until_environment_valid(&router, case, &environment).await;
    until_queue_valid(&router, case, &queue).await;

    let mut submitted = Vec::new();
    for (index, leg) in legs.iter().enumerate() {
        let mut body = json!({
            "jobName": format!("batch-propagated-tags-leg-{index}"),
            "jobQueue": &queue,
            "jobDefinition": &definitions[index],
            "tags": tag_map(leg.job_tags),
        });
        if let Some(propagates) = leg.job_propagates {
            body["propagateTags"] = json!(propagates);
        }
        let answer = batch(&router, case, "/v1/submitjob", &body).await;
        submitted.push(
            answer["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a jobId in {answer}"))
                .to_string(),
        );
    }

    let mut task_arns = Vec::new();
    for job_id in &submitted {
        let detail = until_placed(&router, case, job_id).await;
        task_arns.push(
            detail["attempts"][0]["container"]["taskArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    let answer = ecs(
        &router,
        case,
        "DescribeTasks",
        &json!({"cluster": &cluster, "tasks": &task_arns, "include": ["TAGS"]}),
    )
    .await;
    let failures = answer["failures"].as_array().cloned().unwrap_or_default();
    assert!(
        failures.is_empty(),
        "case {case:?}: Amazon ECS must resolve both of {task_arns:?} in {cluster}, got failures \
         {failures:?}"
    );
    let described: BTreeMap<String, BTreeMap<String, String>> = answer["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|task| {
            Some((
                task["taskArn"].as_str()?.to_string(),
                ecs_tags(&task["tags"]),
            ))
        })
        .collect();
    for (index, leg) in legs.iter().enumerate() {
        let expected = expected_map(leg.propagated);
        let found = described.get(&task_arns[index]).cloned().unwrap_or_default();
        assert_eq!(
            found, expected,
            "case {case:?}: leg {index} must carry {expected:?} on {}, got {found:?} out of \
             {answer}",
            task_arns[index]
        );
    }

    // And both legs run to their end, so the differential is readable off a settled estate.
    for job_id in &submitted {
        until_terminal(&router, case, job_id).await;
    }
}
