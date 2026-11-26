// SPDX-License-Identifier: CC0-1.0

//! What CloudFormation does with a NUMBER a declared resource carries outside the closed range
//! its resource type's schema states for that property, over the two services it takes to
//! decide: CloudFormation holds the declaration to the schema, AWS Batch owns the resources
//! that would otherwise be created.
//!
//! RULE under test: **CloudFormation validates each declared resource against its resource
//! type's schema BEFORE invoking any provider, so a property carrying a number outside the
//! `minimum`/`maximum` that schema states fails the operation at that stage — the stack rolls
//! back carrying CloudFormation's own summary over the stage's errors, the failure's own
//! diagnosis is the schema validator's sentence (the rejected number and the bound it broke, at
//! the member's own pointer) on a `VALIDATION_ERROR` operation event `DescribeEvents` reports,
//! and NOTHING of the template is created in the service that owns it; a number the range
//! admits — each published bound included — is deployed and read back there.**
//!
//! That holds whether or not the OWNING API states the bound: AWS Batch types
//! `CreateJobQueue$priority` as a plain required integer with no range, so a priority above the
//! ceiling the `AWS::Batch::JobQueue` schema states is CloudFormation's to refuse and nothing
//! in Batch would, while a `FairsharePolicy$computeReservation` outside "The minimum value is 0
//! and the maximum value is 99" is refused by both — and the stage that runs FIRST is the one
//! whose diagnosis a client reads, which is why a rollback over such a number carries a
//! validation event rather than only a provisioning failure.
//!
//! The rows differ along what the rule generalises over: which end of the range is broken
//! (ceiling, floor), which property and resource type states it (properties the report that
//! led here did not cite among them), how deep in the declaration the member sits (a property
//! of the resource, a member nested inside a structured property), by how much the bound is
//! broken (one past it, far past it), and — as the controls — a number well inside the range
//! and the published bounds themselves.
//!
//! Two crates have to agree for the rule to hold (CloudFormation refuses the declaration, AWS
//! Batch is asked for nothing), so the suite lives here rather than in either crate.
//!
//! Nothing here waits out a span: every stack is polled until it rests in a terminal status or
//! a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{form_encode, members_of, opt_element};

/// The Region every row deploys in, which is the Region the resources would be created in and
/// the one their ARNs carry.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a wait for a stack to reach a terminal status may run before the suite calls it a
/// failure. Generous by design: what the rows are about is WHICH status the stack rests in, not
/// how fast the host reached it.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait before looking at the stack again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Every terminal `StackStatus` a create can rest in, from the model's `StackStatus` enum
/// (`smithy_query(query="shape", shape="StackStatus")`): the `*_IN_PROGRESS` members are the
/// ones a create is still working through, so a status that is none of them is one the stack
/// rests in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// `ResourceStatus` of the stack-level event a failed create records, and the `EventType` a
/// validation result is reported under (the model's `EventType` enum).
const CREATE_FAILED: &str = "CREATE_FAILED";
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema, the `ValidationStatus` of one a declaration did not pass, and the
/// `ValidationFailureMode` of one whose failure was handled by FAILING the operation.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";
const FAILED: &str = "FAILED";
const FAIL: &str = "FAIL";

/// The bounds the `AWS::Batch::SchedulingPolicy` schema states for the two numeric members of
/// a `FairsharePolicy`.
///
/// Source: the bound AWS Batch publishes for the members the properties are passed to —
/// `FairsharePolicy$computeReservation` ("The minimum value is 0 and the maximum value is 99")
/// and `FairsharePolicy$shareDecaySeconds` ("A value of zero (0) indicates the default minimum
/// time window (600 seconds). The maximum supported value is 604800 (1 week)"), which that
/// type's registry schema carries as its `minimum`/`maximum`.
const MIN_COMPUTE_RESERVATION: i64 = 0;
const MAX_COMPUTE_RESERVATION: i64 = 99;
const MAX_SHARE_DECAY_SECONDS: i64 = 604_800;

/// The ceiling the `AWS::Batch::JobQueue` schema states for `Priority`.
///
/// Source: the `Maximum` that type's entry in the "AWS resource and property types reference"
/// publishes for the property. AWS Batch's own `CreateJobQueue$priority` is a plain required
/// integer with no range, so this bound is the resource type's rather than the API's.
const MAX_JOB_QUEUE_PRIORITY: i64 = 1_000;

/// The `ServiceEnvironmentType` an `AWS::Batch::ServiceEnvironment` declares and the
/// `JobQueueType` of a queue that orders one, from AWS Batch's own enums
/// (`smithy_query(query="shape", shape="ServiceEnvironmentType")`: `SAGEMAKER_TRAINING`).
const SAGEMAKER_TRAINING: &str = "SAGEMAKER_TRAINING";

/// The unit a service environment's capacity limit is measured in, from
/// `::service_com_amazonaws_batch_awsbatchv20160810::state::CAPACITY_UNIT_NUM_INSTANCES`.
const NUM_INSTANCES: &str = "NUM_INSTANCES";

/// The only `QuotaShareIdleResourceAssignmentStrategy` AWS Batch models
/// (`smithy_query(query="shape", shape="QuotaShareIdleResourceAssignmentStrategy")`).
const FIFO: &str = "FIFO";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
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
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
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
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
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

/// The ARN a Batch resource of `kind` and `name` carries: the same total function of the Region
/// and the name both services compute it from.
fn batch_arn(kind: &str, name: &str) -> String {
    format!("arn:aws:batch:{REGION}:{ACCOUNT}:{kind}/{name}")
}

/// The stack as `DescribeStacks` answers it once its operation has reached a terminal status —
/// looked at again until it rests there or [`TERMINAL_DEADLINE`] passes, so what the suite
/// asserts never turns on how fast the host ran.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (code, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks must answer 200, got {code} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to rest in a terminal status within \
             {TERMINAL_DEADLINE:?}, last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a stack reports for a failure of any of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the schema validator reports a number above the `maximum` its property's schema states:
/// the member as a pointer into the resource's property document, the number the declaration
/// carried and the ceiling it is held to.
fn not_less_or_equal(member: &str, value: &str, maximum: i64) -> String {
    format!("#/{member}: {value} is not less or equal to {maximum}")
}

/// The same validator's sentence for a number below the `minimum` its property's schema states.
fn not_greater_or_equal(member: &str, value: &str, minimum: i64) -> String {
    format!("#/{member}: {value} is not greater or equal to {minimum}")
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per operation event
/// carrying a validation, as a client reads them.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = cloudformation(router, "DescribeEvents", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|event| opt_element(event, "ValidationName").is_some())
        .map(|event| {
            let read = |name: &str| opt_element(event, name).unwrap_or_default();
            (
                read("LogicalResourceId"),
                read("EventType"),
                read("ValidationName"),
                read("ValidationStatus"),
                read("ValidationFailureMode"),
                read("ValidationPath"),
            )
        })
        .collect()
}

/// The `ValidationStatusReason` of every validation result of `stack`, read the same way.
async fn validation_reasons(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let (code, body) = cloudformation(router, "DescribeEvents", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|event| opt_element(event, "ValidationName").is_some())
        .map(|event| opt_element(event, "ValidationStatusReason").unwrap_or_default())
        .collect()
}

/// The `ResourceStatusReason` of every stack-level `CREATE_FAILED` event of `stack`.
async fn stack_failure_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let (code, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|event| {
            opt_element(event, "LogicalResourceId").as_deref() == Some(stack)
                && opt_element(event, "ResourceStatus").as_deref() == Some(CREATE_FAILED)
        })
        .map(|event| opt_element(event, "ResourceStatusReason").unwrap_or_default())
        .collect()
}

/// What one refused row must report: the member the validation was applied at, and the
/// sentence it carries.
struct Refused {
    /// The chain of members from the resource's property document down to the judged number,
    /// as the pointer and the path both spell it.
    member: &'static str,
    /// The schema validator's own sentence about that number.
    reason: String,
}

/// One scheduling-policy row: the `FairsharePolicy` a template declares, and what the rule
/// says must then happen.
struct PolicyRow {
    case: &'static str,
    stack: &'static str,
    policy: &'static str,
    fairshare: Value,
    /// What the validation must report, or `None` for a declaration the schema admits.
    refused: Option<Refused>,
    /// The `computeReservation` AWS Batch must report for a declaration that IS deployed.
    reserved: Option<i64>,
}

/// The logical id the policy rows declare their resource under.
const POLICY_LOGICAL_ID: &str = "SP";

fn policy_rows() -> Vec<PolicyRow> {
    vec![
        PolicyRow {
            case: "a policy reserving far more compute than the schema admits",
            stack: "batch-range-reservation-far-above",
            policy: "batch-range-reservation-far-above",
            fairshare: json!({
                "ComputeReservation": 150,
                "ShareDecaySeconds": 60,
                "ShareDistribution": [{"ShareIdentifier": "A*", "WeightFactor": 1.0}],
            }),
            refused: Some(Refused {
                member: "FairsharePolicy/ComputeReservation",
                reason: not_less_or_equal(
                    "FairsharePolicy/ComputeReservation",
                    "150",
                    MAX_COMPUTE_RESERVATION,
                ),
            }),
            reserved: None,
        },
        PolicyRow {
            case: "a policy reserving one past that ceiling",
            stack: "batch-range-reservation-one-above",
            policy: "batch-range-reservation-one-above",
            fairshare: json!({"ComputeReservation": 100}),
            refused: Some(Refused {
                member: "FairsharePolicy/ComputeReservation",
                reason: not_less_or_equal(
                    "FairsharePolicy/ComputeReservation",
                    "100",
                    MAX_COMPUTE_RESERVATION,
                ),
            }),
            reserved: None,
        },
        PolicyRow {
            case: "a policy reserving one below the floor the schema states",
            stack: "batch-range-reservation-below",
            policy: "batch-range-reservation-below",
            fairshare: json!({"ComputeReservation": -1}),
            refused: Some(Refused {
                member: "FairsharePolicy/ComputeReservation",
                reason: not_greater_or_equal(
                    "FairsharePolicy/ComputeReservation",
                    "-1",
                    MIN_COMPUTE_RESERVATION,
                ),
            }),
            reserved: None,
        },
        // The OTHER bounded member of the same structure, which the report that led here did
        // not cite: one past its own ceiling, judged at its own member.
        PolicyRow {
            case: "a policy decaying its shares over longer than the schema admits",
            stack: "batch-range-decay-above",
            policy: "batch-range-decay-above",
            fairshare: json!({"ComputeReservation": 50, "ShareDecaySeconds": 604_801}),
            refused: Some(Refused {
                member: "FairsharePolicy/ShareDecaySeconds",
                reason: not_less_or_equal(
                    "FairsharePolicy/ShareDecaySeconds",
                    "604801",
                    MAX_SHARE_DECAY_SECONDS,
                ),
            }),
            reserved: None,
        },
        // The controls: a number well inside the range, and the published bounds themselves.
        PolicyRow {
            case: "a policy reserving compute well inside the range",
            stack: "batch-range-reservation-inside",
            policy: "batch-range-reservation-inside",
            fairshare: json!({
                "ComputeReservation": 50,
                "ShareDecaySeconds": 60,
                "ShareDistribution": [{"ShareIdentifier": "A*", "WeightFactor": 1.0}],
            }),
            refused: None,
            reserved: Some(50),
        },
        PolicyRow {
            case: "a policy sitting on both ceilings the schema states",
            stack: "batch-range-ceilings",
            policy: "batch-range-ceilings",
            fairshare: json!({"ComputeReservation": 99, "ShareDecaySeconds": 604_800}),
            refused: None,
            reserved: Some(99),
        },
        PolicyRow {
            case: "a policy sitting on both floors the schema states",
            stack: "batch-range-floors",
            policy: "batch-range-floors",
            fairshare: json!({"ComputeReservation": 0, "ShareDecaySeconds": 0}),
            refused: None,
            reserved: Some(0),
        },
    ]
}

/// The template one policy row deploys: one scheduling policy, under the row's own name.
fn policy_template(row: &PolicyRow) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            POLICY_LOGICAL_ID: {
                "Type": "AWS::Batch::SchedulingPolicy",
                "Properties": {"Name": row.policy, "FairsharePolicy": row.fairshare},
            },
        },
    })
    .to_string()
}

/// Every policy row: a `FairsharePolicy` number outside the range its type's schema states
/// rolls the stack back with the summary on the stack, the schema validator's own sentence on
/// a `VALIDATION_ERROR` operation event, and NO policy in AWS Batch — while a number the range
/// admits is deployed and read back there.
#[tokio::test]
async fn a_fairshare_number_outside_its_schemas_range_rolls_the_stack_back() {
    for row in policy_rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = build_router();

        let (code, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &policy_template(&row)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        let expected_status = match row.refused {
            Some(_) => ROLLBACK_COMPLETE,
            None => CREATE_COMPLETE,
        };
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // What the STACK reports: the summary over the stage's one error, on its own failure
        // event — never the owning service's refusal, which was never asked for.
        let failures = stack_failure_reasons(&router, case, row.stack).await;
        let expected_failures: Vec<String> =
            row.refused.iter().map(|_| summary(1)).collect();
        assert_eq!(
            failures, expected_failures,
            "case {case:?}: expected the stack's failure events {expected_failures:?}, got \
             {failures:?}"
        );

        // ...and the DETAIL of the failure: one validation result about the resource nothing
        // was done to, naming the validation, its status, how its failure was handled, the
        // member it was applied at and the sentence the schema validator words it in.
        let results = validation_results(&router, case, row.stack).await;
        let reasons = validation_reasons(&router, case, row.stack).await;
        let expected_results: Vec<(String, String, String, String, String, String)> = row
            .refused
            .iter()
            .map(|refused| {
                (
                    POLICY_LOGICAL_ID.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    FAILED.to_string(),
                    FAIL.to_string(),
                    format!(
                        "/Resources/{POLICY_LOGICAL_ID}/Properties/{}",
                        refused.member
                    ),
                )
            })
            .collect();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );
        let expected_reasons: Vec<String> = row
            .refused
            .iter()
            .map(|refused| refused.reason.clone())
            .collect();
        assert_eq!(
            reasons, expected_reasons,
            "case {case:?}: expected the validation reasons {expected_reasons:?}, got \
             {reasons:?}"
        );

        // What AWS Batch holds is the proof: a refused declaration left no policy there at
        // all, and a deployed one reserves the compute its declaration asked for.
        let arn = batch_arn("scheduling-policy", row.policy);
        let (code, read) = batch(
            &router,
            "/v1/describeschedulingpolicies",
            &json!({"arns": [arn]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeSchedulingPolicies must answer 200, got {code} {read}"
        );
        let held = read["schedulingPolicies"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match row.reserved {
            None => assert!(
                held.is_empty(),
                "case {case:?}: a refused declaration must leave no scheduling policy behind, \
                 got {read}"
            ),
            Some(reserved) => {
                assert_eq!(
                    held.len(),
                    1,
                    "case {case:?}: expected exactly one scheduling policy, got {read}"
                );
                assert_eq!(
                    held[0]["fairsharePolicy"]["computeReservation"].as_i64(),
                    Some(reserved),
                    "case {case:?}: expected AWS Batch to report computeReservation \
                     {reserved}, got {read}"
                );
                assert_eq!(
                    held[0]["name"].as_str(),
                    Some(row.policy),
                    "case {case:?}: expected the declared policy name, got {read}"
                );
            }
        }
    }
}

/// One job-queue row: the `Priority` the queue declares, and whether the schema admits it.
struct QueueRow {
    case: &'static str,
    stack: &'static str,
    /// The names the three resources of the estate are declared under, so a refused row can be
    /// asked about each of them by name.
    policy: &'static str,
    environment: &'static str,
    queue: &'static str,
    priority: i64,
    /// What the validation must report, or `None` for a priority the schema admits.
    refused: Option<Refused>,
}

/// The logical ids the queue rows declare their three resources under.
const QUEUE_LOGICAL_ID: &str = "JQ";

fn queue_rows() -> Vec<QueueRow> {
    vec![
        QueueRow {
            case: "a queue whose priority is one past the ceiling the schema states",
            stack: "batch-range-priority-above",
            policy: "batch-range-priority-above-policy",
            environment: "batch-range-priority-above-env",
            queue: "batch-range-priority-above-queue",
            priority: MAX_JOB_QUEUE_PRIORITY + 1,
            refused: Some(Refused {
                member: "Priority",
                reason: not_less_or_equal(
                    "Priority",
                    &(MAX_JOB_QUEUE_PRIORITY + 1).to_string(),
                    MAX_JOB_QUEUE_PRIORITY,
                ),
            }),
        },
        QueueRow {
            case: "a queue whose priority IS that ceiling",
            stack: "batch-range-priority-ceiling",
            policy: "batch-range-priority-ceiling-policy",
            environment: "batch-range-priority-ceiling-env",
            queue: "batch-range-priority-ceiling-queue",
            priority: MAX_JOB_QUEUE_PRIORITY,
            refused: None,
        },
    ]
}

/// The template one queue row deploys: the SageMaker-training estate a queue of that type needs
/// — a scheduling policy, a service environment, and the queue that orders it.
fn queue_template(row: &QueueRow) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "SP": {
                "Type": "AWS::Batch::SchedulingPolicy",
                "Properties": {
                    "Name": row.policy,
                    "QuotaSharePolicy": {"IdleResourceAssignmentStrategy": FIFO},
                },
            },
            "SE": {
                "Type": "AWS::Batch::ServiceEnvironment",
                "Properties": {
                    "ServiceEnvironmentName": row.environment,
                    "ServiceEnvironmentType": SAGEMAKER_TRAINING,
                    "CapacityLimits": [{"MaxCapacity": 2, "CapacityUnit": NUM_INSTANCES}],
                },
            },
            QUEUE_LOGICAL_ID: {
                "Type": "AWS::Batch::JobQueue",
                "Properties": {
                    "JobQueueName": row.queue,
                    "JobQueueType": SAGEMAKER_TRAINING,
                    "Priority": row.priority,
                    "SchedulingPolicyArn": {"Ref": "SP"},
                    "ServiceEnvironmentOrder": [
                        {"Order": 1, "ServiceEnvironment": {"Ref": "SE"}},
                    ],
                },
            },
        },
    })
    .to_string()
}

/// Every queue row: a `Priority` above the ceiling its type's schema states rolls the whole
/// stack back before anything is provisioned — so AWS Batch knows neither the queue nor the
/// service environment nor the policy the template named — while the ceiling itself deploys and
/// the queue reports that priority.
#[tokio::test]
async fn a_job_queue_priority_above_the_schema_maximum_rolls_the_stack_back() {
    for row in queue_rows() {
        let case = row.case;
        let router = build_router();

        let (code, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &queue_template(&row)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {created}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        let expected_status = match row.refused {
            Some(_) => ROLLBACK_COMPLETE,
            None => CREATE_COMPLETE,
        };
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        let failures = stack_failure_reasons(&router, case, row.stack).await;
        let expected_failures: Vec<String> = row.refused.iter().map(|_| summary(1)).collect();
        assert_eq!(
            failures, expected_failures,
            "case {case:?}: expected the stack's failure events {expected_failures:?}, got \
             {failures:?}"
        );

        let results = validation_results(&router, case, row.stack).await;
        let reasons = validation_reasons(&router, case, row.stack).await;
        let expected_results: Vec<(String, String, String, String, String, String)> = row
            .refused
            .iter()
            .map(|refused| {
                (
                    QUEUE_LOGICAL_ID.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    FAILED.to_string(),
                    FAIL.to_string(),
                    format!("/Resources/{QUEUE_LOGICAL_ID}/Properties/{}", refused.member),
                )
            })
            .collect();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );
        let expected_reasons: Vec<String> = row
            .refused
            .iter()
            .map(|refused| refused.reason.clone())
            .collect();
        assert_eq!(
            reasons, expected_reasons,
            "case {case:?}: expected the validation reasons {expected_reasons:?}, got \
             {reasons:?}"
        );

        // A declaration this stage refuses is refused BEFORE anything is provisioned, so AWS
        // Batch knows none of the three resources the template named — not even the ones the
        // schema admits, which the deployment would have created first.
        let (code, queues) = batch(
            &router,
            "/v1/describejobqueues",
            &json!({"jobQueues": [row.queue]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeJobQueues must answer 200, got {code} {queues}"
        );
        let held = queues["jobQueues"].as_array().cloned().unwrap_or_default();
        let (code, environments) = batch(
            &router,
            "/v1/describeserviceenvironments",
            &json!({"serviceEnvironments": [row.environment]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeServiceEnvironments must answer 200, got {code} \
             {environments}"
        );
        let environments_held = environments["serviceEnvironments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (code, policies) = batch(
            &router,
            "/v1/describeschedulingpolicies",
            &json!({"arns": [batch_arn("scheduling-policy", row.policy)]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeSchedulingPolicies must answer 200, got {code} {policies}"
        );
        let policies_held = policies["schedulingPolicies"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        if row.refused.is_some() {
            for (what, found) in [
                ("job queue", &held),
                ("service environment", &environments_held),
                ("scheduling policy", &policies_held),
            ] {
                assert!(
                    found.is_empty(),
                    "case {case:?}: a refused template must leave no {what} behind, got \
                     {found:?}"
                );
            }
            continue;
        }

        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one job queue, got {queues}"
        );
        assert_eq!(
            held[0]["priority"].as_i64(),
            Some(row.priority),
            "case {case:?}: expected AWS Batch to report priority {}, got {queues}",
            row.priority
        );
        assert_eq!(
            held[0]["jobQueueName"].as_str(),
            Some(row.queue),
            "case {case:?}: expected the declared queue name, got {queues}"
        );
        assert_eq!(
            environments_held.len(),
            1,
            "case {case:?}: expected the queue's service environment to exist, got \
             {environments}"
        );
        assert_eq!(
            policies_held.len(),
            1,
            "case {case:?}: expected the queue's scheduling policy to exist, got {policies}"
        );
        // The queue orders the environment the template pointed it at, by the ARN that
        // environment carries — which is what makes the three resources one deployment.
        assert_eq!(
            held[0]["serviceEnvironmentOrder"][0]["serviceEnvironment"].as_str(),
            Some(batch_arn("service-environment", row.environment).as_str()),
            "case {case:?}: expected the queue to order the declared environment, got {queues}"
        );
    }
}
