// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares the AWS Batch SHARING plane —
//! `AWS::Batch::SchedulingPolicy` and `AWS::Batch::QuotaShare`, beside the
//! `AWS::Batch::ServiceEnvironment` and `AWS::Batch::JobQueue` they belong with — really
//! creates all four in AWS Batch, and the handles the stack hands back are the ones that
//! service answers to.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API, and the identifier the stack
//! publishes for it is the one that type's published return values document — so `Ref` and
//! `Fn::GetAtt` resolve to a value the owning service's own readers resolve back to the
//! resource the template declared, and a resource of the same stack that REFERENCES it (a
//! job queue created against `!Ref` of its scheduling policy, a quota share created on `!Ref`
//! of its queue) is created against a handle that service holds something for.** Two crates
//! have to agree for that to hold (CloudFormation makes the calls, AWS Batch serves them), so
//! the suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES each
//! resource or leaves CloudFormation to generate its name, which policy KIND the scheduling
//! policy declares (a quota-share policy or a fair-share one), the share's own sharing
//! strategy and state, whether either resource declares `Tags`, the Region the stack deploys
//! in, whether the template is written in JSON or YAML, and which intrinsic the outputs read
//! each resource with (`Ref`, `Fn::GetAtt`, `Fn::Sub`).
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the GENERATED names of the row that declares none, which
//! are CloudFormation's to compute: what is checked about those is that the ARNs the stack
//! publishes are those names' Batch ARNs and that AWS Batch resolves them.
//!
//! Sibling suite: `core/router/tests/all/cloudformation_provisions_batch_resources.rs` pins
//! the same rule for the compute environment and the job queue.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`, `tests/all/cloudformation_provisions_stack_resources.rs`).
use super::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use super::query_wire::form_encode;

/// How long a settled stack, and a Batch read that has to agree with it, is waited for: a
/// generous deadline the polls below give up at, not a sleep. What a case waits for is the
/// CONDITION — a stack status that is no longer in progress, a resource that is no longer
/// held — so a loaded host only makes the wait longer and never changes the answer.
const SETTLED_DEADLINE: Duration = Duration::from_secs(120);

/// How often those polls look again.
const SETTLED_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The stack status a deployment that created everything it declared reports, and the one a
/// deployment whose resource its owning service refused settles on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `JQStatus`/`QuotaShareStatus` AWS Batch reports for a resource it has finished
/// creating.
const VALID: &str = "VALID";

/// The `ServiceEnvironmentType` and the `JobQueueType` a SageMaker-training estate declares
/// (`smithy_query(query="shape", shape="ServiceEnvironmentType,JobQueueType")`).
const SAGEMAKER_TRAINING: &str = "SAGEMAKER_TRAINING";

/// The capacity unit the service environment and the share allocate, which
/// `QuotaShareCapacityLimit$capacityUnit` documents by example ("For example,
/// `ml.m5.large`").
const CAPACITY_UNIT: &str = "ml.m6i.large";

/// The AWS Batch routes this suite reads over (`smithy_query(query="operation",
/// operation="DescribeSchedulingPolicies,DescribeJobQueues,DescribeServiceEnvironments,\
/// DescribeQuotaShare,ListQuotaShares,ListTagsForResource")`).
const DESCRIBE_SCHEDULING_POLICIES: &str = "/v1/describeschedulingpolicies";
const DESCRIBE_JOB_QUEUES: &str = "/v1/describejobqueues";
const DESCRIBE_SERVICE_ENVIRONMENTS: &str = "/v1/describeserviceenvironments";
const DESCRIBE_QUOTA_SHARE: &str = "/v1/describequotashare";
const LIST_QUOTA_SHARES: &str = "/v1/listquotashares";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its own request either way
/// (the credential scope is what routes it).
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
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one AWS Batch operation over its restJson1 wire, signed for `region`.
async fn batch(router: &MultiServiceRouter, region: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("batch.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// `ListTagsForResource` for `arn`, which AWS Batch binds to `GET /v1/tags/{resourceArn}` —
/// the ARN travels in the URI rather than in a body.
async fn batch_tags(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/v1/tags/{}", form_encode(arn)))
        .header("host", format!("batch.{region}.amazonaws.com"))
        .header("authorization", authorization("batch", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack's `DescribeStacks` body, once its status no longer ends `_IN_PROGRESS`.
async fn settled(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack_name: &str,
) -> String {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
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

/// The ARN a Batch resource of `kind` and `path` carries in `region`: the same total function
/// of the Region, the account and the resource path both services compute it from.
fn batch_arn(region: &str, kind: &str, path: &str) -> String {
    format!(
        "arn:aws:batch:{region}:{}:{kind}/{path}",
        engine::EMULATOR_ACCOUNT_ID,
    )
}

/// The NAME an `arn:…:batch:…:<kind>/<name>` names, which is the name AWS Batch keys the
/// resource by whether the template declared it or CloudFormation generated it.
fn resource_name_in(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

/// The `quotaShares` AWS Batch reports for the queue `queue`, by ARN.
async fn listed_shares(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    queue: &str,
) -> Vec<String> {
    let (status, listed) = batch(
        router,
        region,
        LIST_QUOTA_SHARES,
        &json!({ "jobQueue": queue }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListQuotaShares, got {status} with body {listed}"
    );
    listed["quotaShares"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|share| share["quotaShareArn"].as_str().map(ToString::to_string))
        .collect()
}

/// The `schedulingPolicies` AWS Batch reports for `arn`.
async fn described_policies(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    arn: &str,
) -> Vec<Value> {
    let (status, answered) = batch(
        router,
        region,
        DESCRIBE_SCHEDULING_POLICIES,
        &json!({ "arns": [arn] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSchedulingPolicies, got {status} with \
         body {answered}"
    );
    answered["schedulingPolicies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One row of the table: a template, and what the rule says the four resources it declares
/// must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the resources are created in and
    /// the one their ARNs carry.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The scheduling policy's declared name, or `None` for a template that names none —
    /// whose policy carries the name CloudFormation generated.
    policy_name: Option<&'static str>,
    /// The job queue's declared name, or `None` as above.
    queue_name: Option<&'static str>,
    /// The quota share's declared name, or `None` as above.
    share_name: Option<&'static str>,
    /// The policy member the declaration sets and the one member inside it the read must
    /// report, with the value it must report for it.
    policy_kind: (&'static str, &'static str),
    policy_value: fn() -> Value,
    /// The `QuotaShareResourceSharingStrategy` the share must report.
    share_strategy: &'static str,
    /// The `QuotaShareState` the share must report.
    share_state: &'static str,
    /// The `maxCapacity` the share's one capacity limit must report.
    share_capacity: i64,
    /// The tags the share's own declaration asks for, beside the three CloudFormation records
    /// on every resource it provisions.
    share_tags: &'static [(&'static str, &'static str)],
    /// The output keys whose value must be the policy's ARN, the queue's, the service
    /// environment's and the share's.
    policy_outputs: &'static [&'static str],
    queue_outputs: &'static [&'static str],
    environment_outputs: &'static [&'static str],
    share_outputs: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a named SageMaker-training estate with a quota-share policy in JSON, read with \
               Ref and both attributes",
        region: "us-east-1",
        template: || {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Description": "an AWS Batch SageMaker-training estate with a quota share",
                "Resources": {
                    "Policy": {
                        "Type": "AWS::Batch::SchedulingPolicy",
                        "Properties": {
                            "Name": "training-sp",
                            // `QuotaSharePolicy$idleResourceAssignmentStrategy`'s only
                            // documented value ("Currently, only FIFO is supported").
                            "QuotaSharePolicy": { "IdleResourceAssignmentStrategy": "FIFO" },
                            "Tags": { "Plane": "sharing" },
                        },
                    },
                    "Environment": {
                        "Type": "AWS::Batch::ServiceEnvironment",
                        "Properties": {
                            "ServiceEnvironmentName": "training-se",
                            "ServiceEnvironmentType": "SAGEMAKER_TRAINING",
                            "CapacityLimits": [
                                { "MaxCapacity": 2, "CapacityUnit": "ml.m6i.large" }
                            ],
                        },
                    },
                    "Queue": {
                        "Type": "AWS::Batch::JobQueue",
                        "DependsOn": ["Environment", "Policy"],
                        "Properties": {
                            "JobQueueName": "training-jq",
                            "JobQueueType": "SAGEMAKER_TRAINING",
                            "Priority": 1,
                            "SchedulingPolicyArn": { "Ref": "Policy" },
                            "ServiceEnvironmentOrder": [
                                { "Order": 1, "ServiceEnvironment": { "Ref": "Environment" } }
                            ],
                        },
                    },
                    "Share": {
                        "Type": "AWS::Batch::QuotaShare",
                        "DependsOn": ["Queue"],
                        "Properties": {
                            "QuotaShareName": "training-qs",
                            "JobQueue": { "Ref": "Queue" },
                            "CapacityLimits": [
                                { "MaxCapacity": 1, "CapacityUnit": "ml.m6i.large" }
                            ],
                            "ResourceSharingConfiguration": { "Strategy": "RESERVE" },
                            "PreemptionConfiguration": { "InSharePreemption": "DISABLED" },
                            "Tags": { "Team": "training" },
                        },
                    },
                },
                "Outputs": {
                    "PolicyRef": { "Value": { "Ref": "Policy" } },
                    "PolicyArn": { "Value": { "Fn::GetAtt": ["Policy", "Arn"] } },
                    "EnvironmentRef": { "Value": { "Ref": "Environment" } },
                    "QueueRef": { "Value": { "Ref": "Queue" } },
                    "ShareRef": { "Value": { "Ref": "Share" } },
                    "ShareArn": { "Value": { "Fn::GetAtt": ["Share", "QuotaShareArn"] } },
                },
            })
            .to_string()
        },
        policy_name: Some("training-sp"),
        queue_name: Some("training-jq"),
        share_name: Some("training-qs"),
        policy_kind: ("quotaSharePolicy", "idleResourceAssignmentStrategy"),
        policy_value: || json!("FIFO"),
        share_strategy: "RESERVE",
        // "The default state is ENABLED" (`CreateQuotaShare$state`), so a declaration that
        // states none gets that.
        share_state: "ENABLED",
        share_capacity: 1,
        share_tags: &[("Team", "training")],
        policy_outputs: &["PolicyRef", "PolicyArn"],
        queue_outputs: &["QueueRef"],
        environment_outputs: &["EnvironmentRef"],
        share_outputs: &["ShareRef", "ShareArn"],
    },
    Row {
        case: "an unnamed estate with a fair-share policy and a borrowing, DISABLED share in \
               YAML, read with Fn::Sub",
        region: "eu-west-1",
        template: || {
            [
                "Resources:",
                "  Policy:",
                "    Type: AWS::Batch::SchedulingPolicy",
                "    Properties:",
                "      FairsharePolicy:",
                "        ShareDecaySeconds: 600",
                "        ComputeReservation: 25",
                "        ShareDistribution:",
                "          - ShareIdentifier: 'teamA*'",
                "            WeightFactor: 0.5",
                "  Environment:",
                "    Type: AWS::Batch::ServiceEnvironment",
                "    Properties:",
                "      ServiceEnvironmentType: SAGEMAKER_TRAINING",
                "      CapacityLimits:",
                "        - MaxCapacity: 4",
                "          CapacityUnit: ml.m6i.large",
                "  Queue:",
                "    Type: AWS::Batch::JobQueue",
                "    DependsOn: [Environment, Policy]",
                "    Properties:",
                "      JobQueueType: SAGEMAKER_TRAINING",
                "      Priority: 7",
                "      SchedulingPolicyArn: !Ref Policy",
                "      ServiceEnvironmentOrder:",
                "        - Order: 1",
                "          ServiceEnvironment: !Ref Environment",
                "  Share:",
                "    Type: AWS::Batch::QuotaShare",
                "    DependsOn: [Queue]",
                "    Properties:",
                "      JobQueue: !Ref Queue",
                "      State: DISABLED",
                "      CapacityLimits:",
                "        - MaxCapacity: 3",
                "          CapacityUnit: ml.m6i.large",
                "      ResourceSharingConfiguration:",
                "        Strategy: LEND_AND_BORROW",
                "        BorrowLimit: 200",
                "      PreemptionConfiguration:",
                "        InSharePreemption: ENABLED",
                "Outputs:",
                "  PolicyRef:",
                "    Value: !Ref Policy",
                "  EnvironmentRef:",
                "    Value: !Ref Environment",
                "  QueueSub:",
                "    Value: !Sub '${Queue}'",
                "  ShareSub:",
                "    Value: !Sub '${Share}'",
                "  ShareArn:",
                "    Value: !GetAtt Share.QuotaShareArn",
                "",
            ]
            .join("\n")
        },
        policy_name: None,
        queue_name: None,
        share_name: None,
        policy_kind: ("fairsharePolicy", "computeReservation"),
        policy_value: || json!(25),
        share_strategy: "LEND_AND_BORROW",
        share_state: "DISABLED",
        share_capacity: 3,
        share_tags: &[],
        policy_outputs: &["PolicyRef"],
        queue_outputs: &["QueueSub"],
        environment_outputs: &["EnvironmentRef"],
        share_outputs: &["ShareSub", "ShareArn"],
    },
];

/// Every row: the ARNs the stack reports for its four `AWS::Batch::*` resources are those
/// resources' own, AWS Batch resolves each of them to the resource the template declared, the
/// queue is scheduled by the policy and ordered onto the service environment the template
/// pointed it at, the share is held on that queue, and deleting the stack removes the policy
/// and the share in Batch.
#[tokio::test]
async fn a_stack_provisions_the_batch_scheduling_policy_and_quota_share_it_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("batch-sharing-stack-{index}");
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled(&router, region, case, &stack_name).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} in \
             {described}"
        );

        let outputs: BTreeMap<String, String> = outputs_of(&described);
        let published = |keys: &[&str], what: &str| -> String {
            outputs
                .get(keys[0])
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no {what} output in {described}"))
        };
        let policy_arn = published(row.policy_outputs, "policy");
        let queue_arn = published(row.queue_outputs, "queue");
        let environment_arn = published(row.environment_outputs, "service environment");
        let share_arn = published(row.share_outputs, "share");

        // Each published handle is the ARN that type's return values document, carrying the
        // name the template asked for — or, for a template that named nothing, the one
        // CloudFormation generated. A share's ARN nests the QUEUE it was created on, which is
        // what makes the pair one deployment.
        let policy_name = match row.policy_name {
            Some(declared) => declared.to_string(),
            None => resource_name_in(&policy_arn),
        };
        let queue_name = match row.queue_name {
            Some(declared) => declared.to_string(),
            None => resource_name_in(&queue_arn),
        };
        let environment_name = resource_name_in(&environment_arn);
        let share_name = match row.share_name {
            Some(declared) => declared.to_string(),
            None => resource_name_in(&share_arn),
        };
        for (what, arn, expected) in [
            (
                "scheduling policy",
                &policy_arn,
                batch_arn(region, "scheduling-policy", &policy_name),
            ),
            (
                "job queue",
                &queue_arn,
                batch_arn(region, "job-queue", &queue_name),
            ),
            (
                "service environment",
                &environment_arn,
                batch_arn(region, "service-environment", &environment_name),
            ),
            (
                "quota share",
                &share_arn,
                batch_arn(
                    region,
                    "job-queue",
                    &format!("{queue_name}/quota-share/{share_name}"),
                ),
            ),
        ] {
            assert_eq!(
                arn, &expected,
                "case {case:?}: expected the {what} handle to be {expected}, got {arn:?}"
            );
        }
        for (keys, expected, what) in [
            (row.policy_outputs, &policy_arn, "policy"),
            (row.queue_outputs, &queue_arn, "queue"),
            (row.environment_outputs, &environment_arn, "environment"),
            (row.share_outputs, &share_arn, "share"),
        ] {
            for key in keys {
                assert_eq!(
                    outputs.get(*key),
                    Some(expected),
                    "case {case:?}: expected the {what} output {key:?} to be {expected}, got \
                     {:?}",
                    outputs.get(*key)
                );
            }
        }
        assert!(
            !share_name.is_empty() && share_name.len() <= 128,
            "case {case:?}: expected a quota-share name inside the documented 1..128, got \
             {share_name:?}"
        );

        // AWS Batch holds the policy the stack published, under the name and the ARN it
        // published and with the policy kind the declaration asked for.
        let policies = described_policies(&router, region, case, &policy_arn).await;
        assert_eq!(
            policies.len(),
            1,
            "case {case:?}: expected exactly one scheduling policy for {policy_arn}, got \
             {policies:?}"
        );
        let policy = &policies[0];
        assert_eq!(
            policy["name"].as_str(),
            Some(policy_name.as_str()),
            "case {case:?}: expected the policy to carry the name {policy_name:?}, got {policy}"
        );
        assert_eq!(
            policy["arn"].as_str(),
            Some(policy_arn.as_str()),
            "case {case:?}: expected the policy to carry the published ARN, got {policy}"
        );
        let (kind, inner) = row.policy_kind;
        let expected_value = (row.policy_value)();
        assert_eq!(
            policy[kind][inner], expected_value,
            "case {case:?}: expected {kind}.{inner} to be {expected_value}, got {policy}"
        );

        // The queue is the SageMaker-training queue the template declared, scheduled by that
        // policy and ordered onto that service environment.
        let (status, queues) = batch(
            &router,
            region,
            DESCRIBE_JOB_QUEUES,
            &json!({ "jobQueues": [&queue_arn] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeJobQueues, got {status} with body \
             {queues}"
        );
        let queue = queues["jobQueues"]
            .as_array()
            .and_then(|held| held.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the queue in {queues}"));
        for (member, expected) in [
            ("jobQueueArn", json!(queue_arn)),
            ("jobQueueName", json!(queue_name)),
            ("jobQueueType", json!(SAGEMAKER_TRAINING)),
            ("schedulingPolicyArn", json!(policy_arn)),
            ("status", json!(VALID)),
        ] {
            assert_eq!(
                queue[member], expected,
                "case {case:?}: expected the queue's {member} to be {expected}, got {queue}"
            );
        }
        assert_eq!(
            queue["serviceEnvironmentOrder"][0]["serviceEnvironment"],
            json!(environment_arn),
            "case {case:?}: expected the queue to order {environment_arn}, got {queue}"
        );

        // And the service environment it orders is one that service holds, of the type the
        // declaration asked for.
        let (status, environments) = batch(
            &router,
            region,
            DESCRIBE_SERVICE_ENVIRONMENTS,
            &json!({ "serviceEnvironments": [&environment_arn] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeServiceEnvironments, got {status} \
             with body {environments}"
        );
        let environment = environments["serviceEnvironments"]
            .as_array()
            .and_then(|held| held.first())
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the service environment in {environments}")
            });
        assert_eq!(
            environment["serviceEnvironmentType"],
            json!(SAGEMAKER_TRAINING),
            "case {case:?}: expected the declared ServiceEnvironmentType, got {environment}"
        );

        // The share is AWS Batch's own, read by the only identifier its reader takes, and it
        // reports the configuration the template declared.
        let (status, answered) = batch(
            &router,
            region,
            DESCRIBE_QUOTA_SHARE,
            &json!({ "quotaShareArn": &share_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeQuotaShare, got {status} with body \
             {answered}"
        );
        for (member, expected) in [
            ("quotaShareArn", json!(share_arn)),
            ("quotaShareName", json!(share_name)),
            ("jobQueueArn", json!(queue_arn)),
            ("state", json!(row.share_state)),
            ("status", json!(VALID)),
        ] {
            assert_eq!(
                answered[member], expected,
                "case {case:?}: expected the share's {member} to be {expected}, got {answered}"
            );
        }
        assert_eq!(
            answered["capacityLimits"],
            json!([{ "maxCapacity": row.share_capacity, "capacityUnit": CAPACITY_UNIT }]),
            "case {case:?}: expected the declared capacity limit, got {answered}"
        );
        assert_eq!(
            answered["resourceSharingConfiguration"]["strategy"],
            json!(row.share_strategy),
            "case {case:?}: expected the declared sharing strategy, got {answered}"
        );
        assert!(
            answered["preemptionConfiguration"]["inSharePreemption"].is_string(),
            "case {case:?}: expected the declared preemption configuration, got {answered}"
        );

        // And the queue's own listing reports it, which is the reader a client uses to find
        // the shares of a queue a stack deployed.
        let listed = listed_shares(&router, region, case, &queue_arn).await;
        assert_eq!(
            listed,
            vec![share_arn.clone()],
            "case {case:?}: expected the queue to hold exactly the share the stack created, \
             got {listed:?}"
        );

        // The tags the share's declaration asked for, plus the three CloudFormation records on
        // every resource it provisions, read through Batch's own tagging call.
        let (status, tagged) = batch_tags(&router, region, &share_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with body \
             {tagged}"
        );
        let stack_id = xml_text(&described, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: no <StackId> in {described}"));
        let declared_tags: Vec<(String, String)> = row
            .share_tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .chain([
                ("aws:cloudformation:stack-name".to_string(), stack_name.clone()),
                ("aws:cloudformation:stack-id".to_string(), stack_id.clone()),
                ("aws:cloudformation:logical-id".to_string(), "Share".to_string()),
            ])
            .collect();
        for (key, value) in &declared_tags {
            assert_eq!(
                tagged["tags"][key].as_str(),
                Some(value.as_str()),
                "case {case:?}: expected tag {key:?} to be {value:?}, got {tagged}"
            );
        }

        // Deleting the stack removes both of the sharing plane's resources in AWS Batch.
        let (status, body) = cloudformation(
            &router,
            region,
            "DeleteStack",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let deadline = Instant::now() + SETTLED_DEADLINE;
        loop {
            let shares = listed_shares(&router, region, case, &queue_arn).await;
            let policies = described_policies(&router, region, case, &policy_arn).await;
            if shares.is_empty() && policies.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the stack's deletion to remove the share and the \
                 policy, still holding shares {shares:?} and policies {policies:?}"
            );
            tokio::time::sleep(SETTLED_POLL_INTERVAL).await;
        }
    }
}

/// A stack whose template declares a quota share AWS Batch REFUSES does not report a resource
/// nothing created: the stack fails with the refusal the owning service gave, and the
/// resources it had already created are taken back down in that service.
///
/// The refusal here is Batch's own: a `capacityLimits` entry allocates the share's capacity,
/// and an entry whose `maxCapacity` is not positive allocates none.
#[tokio::test]
async fn a_quota_share_the_owning_service_refuses_fails_the_stack() {
    let router = build_router();
    let case = "a share whose capacity limit allocates nothing";
    let region = "us-east-2";
    let stack_name = "batch-sharing-refused";
    let policy_arn = batch_arn(region, "scheduling-policy", "refused-sp");

    let template = json!({
        "Resources": {
            "Policy": {
                "Type": "AWS::Batch::SchedulingPolicy",
                "Properties": {
                    "Name": "refused-sp",
                    "QuotaSharePolicy": { "IdleResourceAssignmentStrategy": "FIFO" },
                },
            },
            "Environment": {
                "Type": "AWS::Batch::ServiceEnvironment",
                "Properties": {
                    "ServiceEnvironmentName": "refused-se",
                    "ServiceEnvironmentType": "SAGEMAKER_TRAINING",
                    "CapacityLimits": [{ "MaxCapacity": 1, "CapacityUnit": "ml.m6i.large" }],
                },
            },
            "Queue": {
                "Type": "AWS::Batch::JobQueue",
                "DependsOn": ["Environment", "Policy"],
                "Properties": {
                    "JobQueueName": "refused-jq",
                    "JobQueueType": "SAGEMAKER_TRAINING",
                    "Priority": 1,
                    "SchedulingPolicyArn": { "Ref": "Policy" },
                    "ServiceEnvironmentOrder": [
                        { "Order": 1, "ServiceEnvironment": { "Ref": "Environment" } }
                    ],
                },
            },
            "Share": {
                "Type": "AWS::Batch::QuotaShare",
                "DependsOn": ["Queue"],
                "Properties": {
                    "QuotaShareName": "refused-qs",
                    "JobQueue": { "Ref": "Queue" },
                    "CapacityLimits": [{ "MaxCapacity": 0, "CapacityUnit": "ml.m6i.large" }],
                    "ResourceSharingConfiguration": { "Strategy": "RESERVE" },
                    "PreemptionConfiguration": { "InSharePreemption": "DISABLED" },
                },
            },
        },
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let described = settled(&router, region, case, stack_name).await;
    let reported = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, ROLLBACK_COMPLETE,
        "case {case:?}: expected the failed create to be rolled back, got {reported:?} in \
         {described}"
    );
    let reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
    assert!(
        reason.contains("Share"),
        "case {case:?}: expected the failure to name the resource, got {reason:?}"
    );

    // And nothing the rollback took down is left behind in AWS Batch: the policy the stack
    // had created is gone with it.
    let policies = described_policies(&router, region, case, &policy_arn).await;
    assert!(
        policies.is_empty(),
        "case {case:?}: expected no scheduling policy for a stack that rolled back, got \
         {policies:?}"
    );
}
