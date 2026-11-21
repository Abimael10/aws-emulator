// SPDX-License-Identifier: CC0-1.0

//! What a workflow step's `CustomActionLambda` execution block DOES, driven over the wire.
//!
//! THE RULE UNDER TEST: a `CustomActionLambda` execution block is PERFORMED — the AWS
//! Lambda functions it names are invoked, and what they answer is what the step and the
//! execution come to. A step whose functions return reports `completed` and lets the clock
//! reach the next one, so an execution whose every step is done reports `completed` and is
//! reported by `ListPlanExecutions` as a completed execution of its plan; a step whose
//! function fails the invocation, or names one that cannot be invoked at all, reports
//! `failed` and fails the execution with what the invocation failed with. Nothing here is
//! claimed for a function that did not run.
//!
//! Two further parts of the block's documented contract are under test with it:
//!
//! * WHAT the function is handed. `Lambdas$payload`: "If you don't specify a payload,
//!   Region switch invokes the function with only the execution metadata. If you provide a
//!   JSON object, Region switch merges execution metadata keys (`InvokedBy`, `PlanArn`,
//!   `ExecutionId`, `StepName`) into the top level. If one of your keys has the same name
//!   as a metadata key, your key takes precedence. If you provide any other JSON type
//!   (string, number, boolean, or array), Region switch forwards it to the Lambda function
//!   unchanged, without adding metadata." Each row's handler RAISES unless the event it
//!   was handed is the one that rule gives it, so an execution that completes is one whose
//!   function saw it.
//! * WHICH function runs. `CustomActionLambdaConfiguration$regionToRun` is "The Amazon Web
//!   Services Region for the function to run in", and the block names one or two functions
//!   (`@length(1, 2)`), so a block naming one per Region runs the one in the Region that
//!   value resolves to. The function that must NOT run raises if it is invoked.
//!
//! Everything is driven over the real wire path: this service and AWS Lambda are mounted on
//! ONE shared engine, every function is created through Lambda's own front door, and the
//! outcome is read back through this service's `GetPlanExecution`, `ListPlanExecutions` and
//! `ListPlanExecutionEvents`.

use std::io::Write;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_arcregionswitch_arcregionswitch::state::{
    EXECUTION_START_SPAN, PERFORMANCE_TIMEOUT_REASON,
};

use crate::switch_support::{
    create_plan, error_message, events_of, get_execution, plan_request, poll_until_state,
    role_arn, start_execution, workflow_of, Emulator, ACCOUNT,
};

/// The two Regions every plan here covers, in the order `CreatePlan` carries them.
const REGIONS: [&str; 2] = ["us-east-1", "us-west-2"];

/// The Region an `activate` execution of such a plan targets — the Region it ACTIVATES, so
/// the one `activatingRegion` resolves to.
const ACTIVATING: &str = "us-west-2";

/// The plan's other Region, which that execution deactivates.
const DEACTIVATING: &str = "us-east-1";

/// The runtime every function here is created with: one this emulator runs a handler for.
const RUNTIME: &str = "python3.12";

/// The handler every function here is created with, inside the module the package holds.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The collection a function is created in: `POST /2015-03-31/functions`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `InvokedBy` the execution metadata carries, from this service's own module — the
/// value under test is the one the code materialises, read from one place.
const INVOKED_BY: &str =
    service_com_amazonaws_arcregionswitch_arcregionswitch::state::custom_action_lambda::INVOKED_BY;

/// A .zip deployment package holding `source` as the handler's module, written in memory
/// the way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The source of a handler that RAISES unless `condition` holds of the event it was handed,
/// and otherwise returns what it checked.
///
/// A handler that raises is reported by Lambda under `FunctionError`, which is what fails
/// the step — so an execution that completes is one whose handler found what it asserted.
fn checking_handler(condition: &str) -> String {
    format!(
        "def lambda_handler(event, context):\n\
         \x20   if not ({condition}):\n\
         \x20       raise Exception(\"unexpected event: %r\" % (event,))\n\
         \x20   return {{\"checked\": True}}\n"
    )
}

/// The source of a handler that always raises, with `reason` as its message: the function a
/// case expects NOT to be invoked, and the one a case about a failed invocation names.
fn raising_handler(reason: &str) -> String {
    format!(
        "def lambda_handler(event, context):\n\
         \x20   raise Exception(\"{reason}\")\n"
    )
}

/// Create the function `name` in `region` running `source`, and bring the activation
/// Lambda's own clock owes it forward so nothing here waits for that window; answers the
/// function's ARN as Lambda minted it.
async fn given_function(
    emulator: &Emulator,
    case: &str,
    region: &str,
    name: &str,
    source: &str,
) -> String {
    let (status, created) = emulator
        .lambda(
            region,
            "POST",
            FUNCTIONS_URI,
            &json!({
                "FunctionName": name,
                "Role": role_arn("handler"),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
                },
            }),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction expected 201, got {status} with {created}"
    );
    activate(emulator);
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string()
}

/// Bring every function's activation forward: the transition is owed by LAMBDA's clock, so
/// nothing here sleeps on that service's behalf.
fn activate(emulator: &Emulator) {
    let state = emulator.engine.state::<
        service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState,
    >();
    let mut store = state.lock();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// One step whose `CustomActionLambda` block names `lambdas` and runs in `region_to_run`.
///
/// Members from `smithy_query(query="shape", shape="CustomActionLambdaConfiguration",
/// model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`:
/// `lambdas`, `retryIntervalMinutes` and `regionToRun` are required, `timeoutMinutes`
/// carries `@default: 60`.
fn lambda_step(name: &str, region_to_run: &str, lambdas: Vec<Value>) -> Value {
    json!({
        "name": name,
        "executionBlockType": "CustomActionLambda",
        "executionBlockConfiguration": {
            "customActionLambdaConfig": {
                "timeoutMinutes": 60,
                "retryIntervalMinutes": 1.0,
                "regionToRun": region_to_run,
                "lambdas": lambdas,
            }
        }
    })
}

/// Create a plan named `name` whose `activate` workflow runs `steps`, and answer its ARN.
async fn plan_with(emulator: &Emulator, case: &str, name: &str, steps: Vec<Value>) -> String {
    create_plan(
        emulator,
        case,
        &plan_request(
            name,
            REGIONS,
            &role_arn("ArcRegionSwitchRole"),
            json!([workflow_of("activate", steps)]),
        ),
    )
    .await
}

/// The status of every step of `execution`, in order.
fn step_statuses(execution: &Value) -> Vec<&str> {
    execution["stepStates"]
        .as_array()
        .map(|steps| {
            steps
                .iter()
                .map(|step| step["status"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// What the plan's execution log reports for the executions in `state`.
async fn listed(emulator: &Emulator, case: &str, plan_arn: &str, state: &str) -> Vec<Value> {
    let (status, page) = emulator
        .call(
            "ListPlanExecutions",
            &json!({"planArn": plan_arn, "state": state}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListPlanExecutions expected 200, got {status} with {page}"
    );
    page["items"].as_array().cloned().unwrap_or_default()
}

/// One row of the payload table: what the block configures for the function, and what the
/// handler asserts of the event it is handed.
struct Payload {
    case: &'static str,
    /// The `payload` member of the block's one function; `None` for a block that
    /// configures none.
    configured: Option<Value>,
    /// The Python expression the handler requires of `event`.
    requires: String,
}

/// RULE: the functions a `CustomActionLambda` step names are invoked with the payload
/// `Lambdas$payload` documents, and a step whose functions return completes the step and
/// the execution — which is then one of the plan's completed executions.
#[tokio::test]
async fn a_custom_action_lambda_step_is_performed_with_the_documented_payload() {
    // The rows differ along what the rule generalises over: the FORM of the configured
    // payload — absent, an object, an object colliding with a metadata key, and a JSON type
    // that is not an object.
    let metadata_present = format!(
        "event[\"InvokedBy\"] == \"{INVOKED_BY}\" \
         and event[\"PlanArn\"].startswith(\"arn:aws:arc-region-switch:\") \
         and \"/\" in event[\"ExecutionId\"] \
         and event[\"StepName\"] == \"Invoke\""
    );
    let cases = [
        Payload {
            case: "no payload: the execution metadata alone",
            configured: None,
            requires: format!("{metadata_present} and len(event) == 4"),
        },
        Payload {
            case: "a JSON object: the metadata merged into it",
            configured: Some(json!({"mine": [1, 2], "other": "kept"})),
            requires: format!(
                "{metadata_present} and event[\"mine\"] == [1, 2] and event[\"other\"] == \"kept\""
            ),
        },
        Payload {
            case: "a key of the caller's with a metadata name takes precedence",
            configured: Some(json!({"PlanArn": "mine", "StepName": "mine"})),
            requires: format!(
                "event[\"PlanArn\"] == \"mine\" and event[\"StepName\"] == \"mine\" \
                 and event[\"InvokedBy\"] == \"{INVOKED_BY}\""
            ),
        },
        Payload {
            case: "a JSON array: forwarded unchanged, with no metadata",
            configured: Some(json!(["only", "mine"])),
            requires: "event == [\"only\", \"mine\"]".to_string(),
        },
        Payload {
            case: "a JSON string: forwarded unchanged, with no metadata",
            configured: Some(json!("plain")),
            requires: "event == \"plain\"".to_string(),
        },
    ];

    let emulator = Emulator::with_lambda();
    for (index, Payload { case, configured, requires }) in cases.into_iter().enumerate() {
        let function = format!("payload-{index}");
        let arn = given_function(
            &emulator,
            case,
            ACTIVATING,
            &function,
            &checking_handler(&requires),
        )
        .await;
        let mut lambda = json!({"arn": arn});
        if let Some(configured) = &configured {
            // The member is the JSON document the caller wants handed over, as the string
            // `LambdaPayload` carries.
            lambda["payload"] = json!(configured.to_string());
        }
        let plan_arn = plan_with(
            &emulator,
            case,
            &format!("payload-{index}"),
            vec![lambda_step("Invoke", "activatingRegion", vec![lambda])],
        )
        .await;
        let execution_id = start_execution(&emulator, case, &plan_arn, ACTIVATING).await;

        let settled = poll_until_state(
            &emulator,
            case,
            &plan_arn,
            &execution_id,
            &["completed", "failed", "planExecutionTimedOut", "canceled"],
        )
        .await;
        assert_eq!(
            settled["executionState"],
            json!("completed"),
            "case {case:?}: expected the execution completed, got {} with the events {:?}",
            settled["executionState"],
            events_of(&emulator, case, &plan_arn, &execution_id).await
        );
        let statuses = step_statuses(&settled);
        assert_eq!(
            statuses,
            vec!["completed"],
            "case {case:?}: expected the performed step completed, got {statuses:?}"
        );
        // A completed execution ended, so it carries the instant it did.
        assert!(
            settled["endTime"].is_number(),
            "case {case:?}: expected a completed execution to carry an endTime, got {}",
            settled["endTime"]
        );

        // The plan's execution log reports it as one of its completed executions.
        let items = listed(&emulator, case, &plan_arn, "completed").await;
        let reported: Vec<&str> = items
            .iter()
            .map(|item| item["executionId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            reported,
            vec![execution_id.as_str()],
            "case {case:?}: expected the finished execution listed as completed, got {items:?}"
        );
        let listed_execution = &items[0];
        for (member, expected) in [
            ("planArn", json!(plan_arn)),
            ("executionState", json!("completed")),
            ("executionAction", json!("activate")),
            ("mode", json!("graceful")),
            ("executionRegion", json!(ACTIVATING)),
        ] {
            assert_eq!(
                listed_execution[member], expected,
                "case {case:?}: expected {member} {expected}, got {}",
                listed_execution[member]
            );
        }
    }
}

/// RULE: a step whose Lambda invocation does not come back as a return FAILS — the step and
/// the execution report `failed`, and the reason is recorded rather than left to guess.
#[tokio::test]
async fn a_custom_action_lambda_step_fails_with_what_the_invocation_failed_with() {
    let emulator = Emulator::with_lambda();

    // The rows differ in HOW the invocation did not come back: a function that raised, and
    // an ARN naming a function Lambda does not hold.
    let raised = given_function(
        &emulator,
        "a function that raises",
        ACTIVATING,
        "raises",
        &raising_handler("the switch could not be made"),
    )
    .await;
    let absent = format!("arn:aws:lambda:{ACTIVATING}:{ACCOUNT}:function:not-created");
    let cases = [
        ("a function that raises", raised, "the switch could not be made"),
        // Lambda's own refusal of an invocation of a function it does not hold.
        ("an ARN naming no function at all", absent, "Function not found"),
    ];

    for (index, (case, arn, expected_detail)) in cases.into_iter().enumerate() {
        let plan_arn = plan_with(
            &emulator,
            case,
            &format!("failing-{index}"),
            vec![lambda_step(
                "Invoke",
                "activatingRegion",
                vec![json!({"arn": arn})],
            )],
        )
        .await;
        let execution_id = start_execution(&emulator, case, &plan_arn, ACTIVATING).await;

        let settled = poll_until_state(
            &emulator,
            case,
            &plan_arn,
            &execution_id,
            &["completed", "failed", "planExecutionTimedOut", "canceled"],
        )
        .await;
        assert_eq!(
            settled["executionState"],
            json!("failed"),
            "case {case:?}: expected the execution failed, got {}",
            settled["executionState"]
        );
        let statuses = step_statuses(&settled);
        assert_eq!(
            statuses,
            vec!["failed"],
            "case {case:?}: expected the step failed, got {statuses:?}"
        );

        // WHY it failed is told: the step's own failure event and the execution's both
        // carry the invocation's failure, naming the function it was made against.
        let events = events_of(&emulator, case, &plan_arn, &execution_id).await;
        for event_type in ["stepFailed", "executionFailed"] {
            let error = events
                .iter()
                .find(|event| event["type"] == json!(event_type))
                .map(|event| event["error"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            assert!(
                error.contains(&arn) && error.contains(expected_detail),
                "case {case:?}: expected the {event_type} error to name {arn} and \
                 {expected_detail:?}, got {error:?}"
            );
        }

        // The failed execution is reported as a failed one and NOT as a completed one.
        for (state, expected) in [("failed", vec![execution_id.as_str()]), ("completed", vec![])] {
            let reported: Vec<String> = listed(&emulator, case, &plan_arn, state)
                .await
                .iter()
                .map(|item| item["executionId"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the executions in {state} to be {expected:?}, got \
                 {reported:?}"
            );
        }
    }
}

/// RULE: the function invoked is the one the block's `regionToRun` names — a block naming
/// one function per Region runs that Region's function and no other.
#[tokio::test]
async fn the_step_invokes_the_function_of_the_region_the_block_runs_in() {
    let emulator = Emulator::with_lambda();
    // The rows differ in which Region the block runs in, so each row expects the OTHER
    // Region's function to be left alone: that one raises if it is invoked, so an execution
    // that completes is one that invoked only the function the value names.
    let cases = [
        ("the activating Region's function", "activatingRegion", ACTIVATING),
        ("the deactivating Region's function", "deactivatingRegion", DEACTIVATING),
        // `activeRegion` names the Region carrying customer traffic after the switch, which
        // for a recovery workflow is the Region being activated.
        ("the active Region's function", "activeRegion", ACTIVATING),
        ("the inactive Region's function", "inactiveRegion", DEACTIVATING),
    ];

    for (index, (case, region_to_run, expected_region)) in cases.into_iter().enumerate() {
        let mut lambdas: Vec<Value> = Vec::new();
        for region in REGIONS {
            let name = format!("per-region-{index}-{region}");
            let source = if region == expected_region {
                checking_handler("event[\"StepName\"] == \"Invoke\"")
            } else {
                raising_handler("this Region's function should not have been invoked")
            };
            let arn = given_function(&emulator, case, region, &name, &source).await;
            lambdas.push(json!({"arn": arn}));
        }
        let plan_arn = plan_with(
            &emulator,
            case,
            &format!("per-region-{index}"),
            vec![lambda_step("Invoke", region_to_run, lambdas)],
        )
        .await;
        let execution_id = start_execution(&emulator, case, &plan_arn, ACTIVATING).await;

        let settled = poll_until_state(
            &emulator,
            case,
            &plan_arn,
            &execution_id,
            &["completed", "failed", "planExecutionTimedOut", "canceled"],
        )
        .await;
        let events = events_of(&emulator, case, &plan_arn, &execution_id).await;
        assert_eq!(
            settled["executionState"],
            json!("completed"),
            "case {case:?}: expected only the {expected_region} function invoked, which \
             completes the execution; got {} with the events {events:?}",
            settled["executionState"]
        );
    }
}

/// RULE: a step is performed between the states a caller can observe — reached and running
/// before its functions answer, completed after — and the execution walks the REST of its
/// workflow afterwards.
#[tokio::test]
async fn a_performed_step_lets_the_clock_reach_the_steps_behind_it() {
    let emulator = Emulator::with_lambda();
    let case = "two custom actions in one workflow";
    let mut lambdas: Vec<Value> = Vec::new();
    for index in 0..2 {
        let arn = given_function(
            &emulator,
            case,
            ACTIVATING,
            &format!("chained-{index}"),
            &checking_handler(&format!("event[\"StepName\"] == \"Invoke-{index}\"")),
        )
        .await;
        lambdas.push(json!({"arn": arn}));
    }
    let steps: Vec<Value> = lambdas
        .into_iter()
        .enumerate()
        .map(|(index, lambda)| {
            lambda_step(&format!("Invoke-{index}"), "activatingRegion", vec![lambda])
        })
        .collect();
    let plan_arn = plan_with(&emulator, case, "chained", steps).await;
    let execution_id = start_execution(&emulator, case, &plan_arn, ACTIVATING).await;

    // The span the clock owes the first step is never zero: a caller reading straight
    // after the start still sees the execution before any step was reached.
    let immediately = get_execution(&emulator, case, &plan_arn, &execution_id).await;
    assert_eq!(
        immediately["executionState"],
        json!("pending"),
        "case {case:?}: straight after the start expected pending, got {}",
        immediately["executionState"]
    );
    assert_eq!(
        step_statuses(&immediately),
        vec!["notStarted", "notStarted"],
        "case {case:?}: straight after the start expected no step reached, got {:?}",
        step_statuses(&immediately)
    );

    let settled = poll_until_state(
        &emulator,
        case,
        &plan_arn,
        &execution_id,
        &["completed", "failed", "planExecutionTimedOut", "canceled"],
    )
    .await;
    let events = events_of(&emulator, case, &plan_arn, &execution_id).await;
    assert_eq!(
        settled["executionState"],
        json!("completed"),
        "case {case:?}: expected both steps performed and the execution completed, got {} \
         with the events {events:?}",
        settled["executionState"]
    );
    assert_eq!(
        step_statuses(&settled),
        vec!["completed", "completed"],
        "case {case:?}: expected both steps completed, got {:?}",
        step_statuses(&settled)
    );
    // Each step is recorded as started and then as succeeded, in the order it ran, with the
    // execution's own events around them.
    let timeline: Vec<(&str, &str)> = events
        .iter()
        .map(|event| {
            (
                event["type"].as_str().unwrap_or_default(),
                event["stepName"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        timeline,
        vec![
            ("executionPending", ""),
            ("executionStarted", ""),
            ("stepStarted", "Invoke-0"),
            ("stepSucceeded", "Invoke-0"),
            ("stepStarted", "Invoke-1"),
            ("stepSucceeded", "Invoke-1"),
            ("executionSucceeded", ""),
        ],
        "case {case:?}: expected one started/succeeded pair per step, got {timeline:?}"
    );

    // Completing released the plan: it is executable again, which a second start proves.
    let (status, restarted) = emulator
        .call(
            "StartPlanExecution",
            &json!({"planArn": plan_arn, "targetRegion": ACTIVATING, "action": "activate"}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a second execution after the first completed expected 200, got \
         {status} with {}",
        error_message(&restarted)
    );
}

/// RULE: a step's execution block has the window its own `timeoutMinutes` configures to
/// answer in — `CustomActionLambdaConfiguration$timeoutMinutes` is "The timeout value
/// specified for the configuration" (`@range(min: 1)`, `@default: 60`) — so a custom action
/// whose invocation never answers fails that step when the window closes, instead of
/// leaving the step running and the plan held for ever.
#[tokio::test]
async fn a_custom_action_that_never_answers_fails_when_its_configured_window_closes() {
    // The rows differ in the window the block configures: the low bound of the member's
    // range, its documented default, and a longer one.
    for (index, (case, timeout_minutes)) in [
        ("the low bound of the member's range", 1u64),
        ("the member's documented default", 60),
        ("a window of a working day", 480),
    ]
    .into_iter()
    .enumerate()
    {
        let emulator = Emulator::with_lambda();
        // The function EXISTS and would complete the step, so nothing but the window
        // closing can fail it.
        let arn = given_function(
            &emulator,
            case,
            ACTIVATING,
            &format!("abandoned-{index}"),
            &checking_handler("event[\"StepName\"] == \"Invoke\""),
        )
        .await;
        let mut step = lambda_step("Invoke", "activatingRegion", vec![json!({"arn": arn})]);
        step["executionBlockConfiguration"]["customActionLambdaConfig"]["timeoutMinutes"] =
            json!(timeout_minutes);
        let plan_arn = plan_with(&emulator, case, &format!("abandoned-{index}"), vec![step]).await;
        let execution_id = start_execution(&emulator, case, &plan_arn, ACTIVATING).await;

        // Drive the clock's own half: it reaches the step and starts performing it, queuing
        // the invocation — which is then ABANDONED, as a caller disconnecting mid-invocation
        // leaves it.
        emulator.settle_ahead(EXECUTION_START_SPAN);
        let abandoned = emulator.abandon_queued_work();
        assert_eq!(
            abandoned, 1,
            "case {case:?}: expected the clock to have queued the one invocation, it queued \
             {abandoned}"
        );

        let window = std::time::Duration::from_secs(timeout_minutes * 60);
        // Inside the window the step is still being performed: an outstanding invocation is
        // not a failure yet.
        emulator.settle_ahead(EXECUTION_START_SPAN + window / 2);
        let waiting = get_execution(&emulator, case, &plan_arn, &execution_id).await;
        assert_eq!(
            (
                waiting["executionState"].clone(),
                json!(step_statuses(&waiting))
            ),
            (json!("inProgress"), json!(["running"])),
            "case {case:?}: inside the window expected the step still running, got {waiting}"
        );

        // Once it closes, the step failed and so did the execution, with the reason told.
        emulator.settle_ahead(EXECUTION_START_SPAN + window + std::time::Duration::from_secs(60));
        let closed = get_execution(&emulator, case, &plan_arn, &execution_id).await;
        assert_eq!(
            (
                closed["executionState"].clone(),
                json!(step_statuses(&closed))
            ),
            (json!("failed"), json!(["failed"])),
            "case {case:?}: after the window closed expected the step failed, got {closed}"
        );
        let events = events_of(&emulator, case, &plan_arn, &execution_id).await;
        let reason = events
            .iter()
            .find(|event| event["type"] == json!("executionFailed"))
            .map(|event| event["error"].clone())
            .unwrap_or(Value::Null);
        assert_eq!(
            reason,
            json!(PERFORMANCE_TIMEOUT_REASON),
            "case {case:?}: expected the timed-out-block reason, got {reason}"
        );

        // Failing released the plan, which a second start proves.
        let (status, restarted) = emulator
            .call(
                "StartPlanExecution",
                &json!({"planArn": plan_arn, "targetRegion": ACTIVATING, "action": "activate"}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a second execution after the first failed expected 200, got \
             {status} with {}",
            error_message(&restarted)
        );
    }
}
