// SPDX-License-Identifier: CC0-1.0

//! What an Automation STEP EXECUTION reports of the step's own declaration: the steps the
//! runbook may take the walk to after it, and the flow members the declaration sets.
//!
//! Two rules, both end to end through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"):
//!
//! 1. `ValidNextSteps` reports the steps the runbook's own graph may take the walk to after
//!    the step — a choice of an `aws:branch` and the branch's default, the step `nextStep`
//!    names or else the step declared after it, and the step a `step:` `onFailure` sends a
//!    failure to — and is absent where the walk can go nowhere: a step that ends the
//!    automation, and the last step of a runbook that names no successor.
//! 2. A step reports the flow members its declaration SETS and nothing more: `MaxAttempts`
//!    and `IsCritical` carry the declared values, and are absent from a step whose
//!    declaration sets neither — the default the walk runs such a step with is not on the
//!    wire.
//!
//! Both are properties of the runbook's declaration rather than of what one step happened
//! to do, so each table row is a whole runbook and every step of it is asserted, the steps
//! the walk entered and the steps it never reached alike. An automation is `Pending` until
//! the service picks it up, so the cases move the clock through the service's own state
//! ([`pick_up`]) rather than sleeping for it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case reads the store through and moves the clock with.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// An instant well in the past, which is what the clock is moved to below.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for, so the walk
/// has entered the steps it can reach by the time a case reads them.
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// One runbook of `steps`, created under `name` and started, answering the execution id.
async fn ran(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    steps: &Value,
) -> String {
    let content = json!({"schemaVersion": "0.3", "mainSteps": steps});
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the runbook {name} must be created first, got {status} with {created:?}"
    );
    let (status, started) = call(
        router,
        "StartAutomationExecution",
        &json!({"DocumentName": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from StartAutomationExecution, got {status} with \
         {started:?}"
    );
    pick_up(engine);
    text(&started, "AutomationExecutionId")
}

/// The step executions of `execution_id`, by step name, as
/// `DescribeAutomationStepExecutions` reports them.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    execution_id: &str,
) -> BTreeMap<String, Value> {
    let (status, report) = call(
        router,
        "DescribeAutomationStepExecutions",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAutomationStepExecutions, got {status} \
         with {report:?}"
    );
    by_name(&report["StepExecutions"])
}

/// The step executions `GetAutomationExecution` reports for `execution_id`, by step name:
/// the same projection, read through the other operation that renders it.
async fn fetched(
    router: &MultiServiceRouter,
    case: &str,
    execution_id: &str,
) -> BTreeMap<String, Value> {
    let (status, got) = call(
        router,
        "GetAutomationExecution",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAutomationExecution, got {status} with {got:?}"
    );
    by_name(&got["AutomationExecution"]["StepExecutions"])
}

/// A reported step execution list, keyed by its steps' names.
fn by_name(steps: &Value) -> BTreeMap<String, Value> {
    steps
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|step| (text(&step, "StepName"), step))
        .collect()
}

/// The `ValidNextSteps` a reported step carries, or `None` where the member is not on the
/// wire at all.
fn valid_next_steps(step: &Value) -> Option<Vec<String>> {
    let listed = step.as_object()?.get("ValidNextSteps")?.as_array()?.clone();
    Some(
        listed
            .iter()
            .map(|name| name.as_str().unwrap_or("<not a name>").to_string())
            .collect(),
    )
}

/// One member of a reported step as it is on the wire: `None` where the step does not carry
/// it at all.
fn member(step: &Value, name: &str) -> Option<Value> {
    step.as_object()?.get(name).cloned()
}

/// What one step of a case's runbook must report: its name, the steps the walk may enter
/// after it, and the two flow members its declaration either sets or leaves out.
struct Expected {
    /// The step's `name` in the runbook.
    name: &'static str,
    /// `ValidNextSteps`, or `None` where the member must be absent.
    next: Option<&'static [&'static str]>,
    /// `MaxAttempts`, or `None` where the member must be absent.
    max_attempts: Option<i64>,
    /// `IsCritical`, or `None` where the member must be absent.
    is_critical: Option<bool>,
}

/// Every step of `expected` as the report `steps` carries it.
fn assert_steps(case: &str, steps: &BTreeMap<String, Value>, expected: &[Expected]) {
    let reported: Vec<String> = steps.keys().cloned().collect();
    let declared: Vec<String> = {
        let mut declared: Vec<String> = expected.iter().map(|step| step.name.to_string()).collect();
        declared.sort();
        declared
    };
    assert_eq!(
        reported, declared,
        "case {case:?}: expected every step of the runbook reported, got {reported:?}"
    );
    for step in expected {
        let reported = &steps[step.name];
        let next: Option<Vec<String>> = step
            .next
            .map(|names| names.iter().map(|name| (*name).to_string()).collect());
        assert_eq!(
            valid_next_steps(reported),
            next,
            "case {case:?} step {:?}: expected ValidNextSteps {next:?}, got {reported}",
            step.name
        );
        assert_eq!(
            member(reported, "MaxAttempts"),
            step.max_attempts.map(|tries| json!(tries)),
            "case {case:?} step {:?}: expected MaxAttempts {:?}, got {reported}",
            step.name,
            step.max_attempts
        );
        assert_eq!(
            member(reported, "IsCritical"),
            step.is_critical.map(|critical| json!(critical)),
            "case {case:?} step {:?}: expected IsCritical {:?}, got {reported}",
            step.name,
            step.is_critical
        );
    }
}

/// A step of `aws:sleep` that waits out no span at all, with whatever flow members `flow`
/// carries declared on it.
fn step(name: &str, flow: Value) -> Value {
    let mut declared = json!({
        "name": name,
        "action": "aws:sleep",
        "inputs": {"Duration": "PT0S"},
    });
    for (member, value) in flow.as_object().cloned().unwrap_or_default() {
        declared[member] = value;
    }
    declared
}

/// RULE: a step reports the steps the runbook's graph may take the walk to after it, and
/// the flow members its own declaration sets — nothing where the graph leads nowhere and
/// nothing where the declaration sets neither.
///
/// The rows differ along the shape of the runbook's graph: document order, a declared
/// `nextStep` that jumps, a branch's choices and default, a step that ends the automation,
/// a failure routed to a named step, a declaration that sets the two flow members, and a
/// failure that carries on to the step after it.
#[tokio::test]
async fn a_step_reports_the_graph_and_the_declaration_it_was_walked_from() {
    let rows: Vec<(&str, Value, Vec<Expected>)> = vec![
        (
            "two steps in the runbook's own order",
            json!([step("first", json!({})), step("second", json!({}))]),
            vec![
                Expected {
                    name: "first",
                    next: Some(&["second"]),
                    max_attempts: None,
                    is_critical: None,
                },
                // The last step of a runbook that names no successor: the walk leaves the
                // runbook after it, so there is no next step to report.
                Expected {
                    name: "second",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a declared nextStep that jumps over the step after it",
            json!([
                step("first", json!({"nextStep": "third"})),
                step("second", json!({})),
                step("third", json!({})),
            ]),
            vec![
                Expected {
                    name: "first",
                    next: Some(&["third"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "second",
                    next: Some(&["third"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "third",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a branch, its choices and its default",
            json!([
                {
                    "name": "decide",
                    "action": "aws:branch",
                    "inputs": {
                        "Choices": [
                            {"NextStep": "left", "Variable": "left", "StringEquals": "left"},
                            {"NextStep": "right", "Variable": "left", "StringEquals": "right"},
                        ],
                        "Default": "neither",
                    },
                },
                step("left", json!({"isEnd": true})),
                step("right", json!({"isEnd": true})),
                step("neither", json!({"isEnd": true})),
            ]),
            vec![
                Expected {
                    name: "decide",
                    next: Some(&["left", "right", "neither"]),
                    max_attempts: None,
                    is_critical: None,
                },
                // A step that ends the automation is followed by nothing, whether the walk
                // entered it or not.
                Expected {
                    name: "left",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "right",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "neither",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a failure routed to a named step",
            json!([
                step("first", json!({"onFailure": "step:recover"})),
                step("second", json!({})),
                step("recover", json!({})),
            ]),
            vec![
                Expected {
                    name: "first",
                    next: Some(&["second", "recover"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "second",
                    next: Some(&["recover"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "recover",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a declaration that sets the tries and the criticality",
            json!([
                step("tries", json!({"maxAttempts": 3, "isCritical": false})),
                step("plain", json!({})),
            ]),
            vec![
                Expected {
                    name: "tries",
                    next: Some(&["plain"]),
                    max_attempts: Some(3),
                    is_critical: Some(false),
                },
                Expected {
                    name: "plain",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a failure that carries on to the step after it",
            json!([
                step("first", json!({"onFailure": "Continue"})),
                step("second", json!({"isEnd": true})),
            ]),
            vec![
                // Succeeding and failing both take the walk to the same place, so the one
                // successor is reported once.
                Expected {
                    name: "first",
                    next: Some(&["second"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "second",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
    ];

    let (router, engine) = service();
    for (at, (case, steps, expected)) in rows.iter().enumerate() {
        let name = format!("Graph{at}");
        let id = ran(&router, &engine, case, &name, steps).await;
        let described = described(&router, case, &id).await;
        assert_steps(case, &described, expected);
    }
}

/// RULE: the two Automation reads render the ONE step projection, so a step carries the
/// same graph and declaration members whichever read reported it.
///
/// The rows differ along what the projection has to carry: a branch with three successors
/// and no declared flow members, and a sequential runbook whose first step declares both.
#[tokio::test]
async fn both_automation_reads_render_the_one_step_projection() {
    let rows: Vec<(&str, Value, Vec<Expected>)> = vec![
        (
            "a branch read through both operations",
            json!([
                {
                    "name": "decide",
                    "action": "aws:branch",
                    "inputs": {
                        "Choices": [
                            {"NextStep": "left", "Variable": "left", "StringEquals": "left"},
                        ],
                        "Default": "right",
                    },
                },
                step("left", json!({"isEnd": true})),
                step("right", json!({"isEnd": true})),
            ]),
            vec![
                Expected {
                    name: "decide",
                    next: Some(&["left", "right"]),
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "left",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
                Expected {
                    name: "right",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
        (
            "a declared tries and criticality read through both operations",
            json!([
                step("tries", json!({"maxAttempts": 7, "isCritical": true})),
                step("plain", json!({})),
            ]),
            vec![
                Expected {
                    name: "tries",
                    next: Some(&["plain"]),
                    max_attempts: Some(7),
                    is_critical: Some(true),
                },
                Expected {
                    name: "plain",
                    next: None,
                    max_attempts: None,
                    is_critical: None,
                },
            ],
        ),
    ];

    let (router, engine) = service();
    for (at, (case, steps, expected)) in rows.iter().enumerate() {
        let name = format!("BothReads{at}");
        let id = ran(&router, &engine, case, &name, steps).await;
        let described = described(&router, case, &id).await;
        assert_steps(&format!("{case} (DescribeAutomationStepExecutions)"), &described, expected);
        let fetched = fetched(&router, case, &id).await;
        assert_steps(&format!("{case} (GetAutomationExecution)"), &fetched, expected);
    }
}
