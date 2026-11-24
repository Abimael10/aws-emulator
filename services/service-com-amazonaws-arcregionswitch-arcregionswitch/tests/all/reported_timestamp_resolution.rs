// SPDX-License-Identifier: CC0-1.0

//! The RESOLUTION every `Timestamp` member of this service carries on the wire.
//!
//! The rule under test, asserted over a SET of inputs rather than one recorded call: a
//! `Timestamp` member whose model declares no `@timestampFormat` travels in the protocol's
//! default epoch-seconds form, which carries no finer resolution than a MILLISECOND — a
//! bare integer when the instant has no sub-second part, and otherwise a number with no
//! more than three fractional digits
//! (`smithy_query(query="shape", shape="GetPlanExecutionResponse,StepState,AbbreviatedExecution", model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`
//! reports every one of these members targeting the plain `smithy.api#Timestamp` with no
//! format trait of its own).
//!
//! So the cases below read EVERY operation of this service that reports an instant, at
//! every depth a reported instant sits at (a top-level member, a nested structure's
//! member, a list item's member), and hold every timestamp member they find to that
//! resolution — not only the two read paths and five members one trace diverged on.
//!
//! Every case drives the emulator through the same `MultiServiceRouter::dispatch` the
//! router serves on a socket, so what these tests observe is what an SDK observes:
//! `awsJson1_0`, `POST /`, `X-Amz-Target: ArcRegionSwitch.<Operation>`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_arcregionswitch_arcregionswitch::state;
use smithy_server_runtime::multi_service_router::MultiServiceRouter;

/// The canonical account id every ARN a generated emulator mints carries
/// (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The most fractional digits an epoch-seconds timestamp may carry: three, one
/// millisecond being the finest resolution the form expresses.
const MAX_FRACTIONAL_DIGITS: usize = 3;

/// Every member name of this model that targets the plain `smithy.api#Timestamp` shape,
/// from
/// `smithy_query(query="shape", shape="GetPlanExecutionResponse,StepState,AbbreviatedExecution,Plan,ExecutionEvent,ResourceWarning,GetPlanEvaluationStatusResponse,ServiceQuotaWarning,GeneratedReportDetails", model_path="services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json")`.
/// A response is searched for all of them at every depth, so a member the cited trace
/// never reached is held to the same rule as one it did.
const TIMESTAMP_MEMBERS: &[&str] = &[
    "updatedAt",
    "startTime",
    "endTime",
    "timestamp",
    "lastEvaluationTime",
    "warningUpdatedTime",
    "reportGenerationTime",
    "lastCheckedAt",
    "warningCreatedAt",
];

/// This one service on a fresh shared engine, plus the engine itself — so a case can ask
/// the store to apply what the clock will owe, at an instant of its own choosing, instead
/// of sleeping for it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

/// One operation's answer: its HTTP status, its parsed body, and the body EXACTLY as it
/// travelled, so a case can hold the number's own text to the rule rather than a value a
/// parser already re-rendered.
struct Answer {
    status: u16,
    body: Value,
    raw: String,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // This suite exercises operation behaviour, not request signing.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_arcregionswitch_arcregionswitch::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Call one operation and return what it answered.
    async fn call(&self, operation: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("ArcRegionSwitch.{operation}"))
            // The router keys on the signing name in the SigV4 credential scope.
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDTEST/20260101/us-east-1/arc-region-switch/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = String::from_utf8_lossy(response.body()).to_string();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer {
            status,
            body: parsed,
            raw,
        }
    }

    /// Call one operation, expecting the 200 the case is about.
    async fn succeed(&self, case: &str, operation: &str, body: &Value) -> Answer {
        let answer = self.call(operation, body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: {operation} expected 200, got {} with {}",
            answer.status, answer.body
        );
        answer
    }

    /// Apply the ONE transition the clock owes each started execution, the same `settle`
    /// every handler runs first but at an instant past the span it is owed in — so a case
    /// that needs a reached step does not sleep for the test machine.
    fn settle_past_the_clock(&self) {
        let ahead = state::EXECUTION_START_SPAN + Duration::from_secs(1);
        state::service_state(&*self.engine)
            .expect("the shared engine holds this service's store")
            .lock()
            .settle(SystemTime::now() + ahead);
    }

    /// Decide the manual-approval step the execution is waiting on AT `now` — a chosen
    /// instant, so a case can drive an instant whose sub-millisecond part it picked rather
    /// than whatever the host clock happened to read.
    fn approve_at(&self, case: &str, key: &(String, String), step: &str, now: SystemTime) {
        let outcome = state::service_state(&*self.engine)
            .expect("the shared engine holds this service's store")
            .lock()
            .apply_approval(key, step, true, None, now);
        assert!(
            matches!(outcome, state::ApprovalOutcome::Applied),
            "case {case:?}: expected the approval of {step} to apply, got {outcome:?}"
        );
    }
}

/// One step carrying a manual-approval execution block — the one block type whose work is
/// inside this service, so the clock really does reach it.
fn approval_step(name: &str) -> Value {
    json!({
        "name": name,
        "executionBlockType": "ManualApproval",
        "executionBlockConfiguration": {
            "executionApprovalConfig": {
                "approvalRole": format!("arn:aws:iam::{ACCOUNT}:role/Approver"),
                "timeoutMinutes": 60,
            }
        }
    })
}

/// The `CreatePlan` request every case starts from: one `deactivate` workflow of a single
/// manual approval, so the clock stops at a step a case can decide at an instant of its
/// own.
fn plan_request(name: &str) -> Value {
    json!({
        "name": name,
        "regions": ["us-east-1", "us-west-2"],
        "recoveryApproach": "activePassive",
        "executionRole": format!("arn:aws:iam::{ACCOUNT}:role/ArcRegionSwitchRole"),
        "workflows": [
            {"workflowTargetAction": "deactivate", "steps": [approval_step("confirm-shift")]},
        ],
    })
}

/// Every timestamp member of `body`, as `(path, value)` pairs, at every depth.
fn timestamp_members(body: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    collect_timestamps(body, "$", &mut found);
    found
}

/// Append every timestamp member of `value` to `found`, naming each by its path.
fn collect_timestamps(value: &Value, path: &str, found: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(members) => {
            for (name, child) in members {
                let child_path = format!("{path}.{name}");
                if TIMESTAMP_MEMBERS.contains(&name.as_str()) && !child.is_null() {
                    found.push((child_path.clone(), child.clone()));
                }
                collect_timestamps(child, &child_path, found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_timestamps(child, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

/// The fractional digits `value` carries as it travels: the digits after the decimal
/// point of the epoch-seconds number, trailing zeros (which name no finer instant)
/// ignored.
///
/// A member that is not a plain decimal number at all fails here: the epoch-seconds form
/// is a JSON number, never a string and never an exponent form no epoch-seconds
/// deserializer reads.
fn fractional_digits(case: &str, path: &str, value: &Value) -> usize {
    assert!(
        value.as_f64().is_some(),
        "case {case:?}: {path} must travel as an epoch-seconds number, got {value:?}"
    );
    let text = value.to_string();
    assert!(
        !text.contains(['e', 'E']),
        "case {case:?}: {path} must travel as a plain epoch-seconds number, got {text}"
    );
    text.split_once('.')
        .map_or(0, |(_, fraction)| fraction.trim_end_matches('0').len())
}

/// Hold every timestamp member of one answer to the rule, and report how many were found
/// so a case can assert it read any at all.
fn assert_millisecond_resolution(case: &str, answer: &Answer) -> usize {
    let members = timestamp_members(&answer.body);
    for (path, value) in &members {
        let digits = fractional_digits(case, path, value);
        assert!(
            digits <= MAX_FRACTIONAL_DIGITS,
            "case {case:?}: {path} must carry at most {MAX_FRACTIONAL_DIGITS} fractional \
             digits of epoch seconds, got {value} with {digits}"
        );
    }
    // The same rule over the bytes that actually travelled, so a value no parser could
    // re-render at its original precision cannot slip through the check above.
    assert_no_over_precise_number(case, &answer.raw);
    members.len()
}

/// Refuse any epoch-seconds-shaped number in `raw` — one whose integer part is as long as
/// an epoch-seconds instant — that carries more than [`MAX_FRACTIONAL_DIGITS`] fractional
/// digits.
///
/// This holds the BYTES that travelled to the rule, which is where the resolution lives: a
/// number read back through a parser has already been re-rendered, and the defect this
/// suite pins was a number's text. It is a scan for runs of `<digits>.<digits>`, not a
/// reader of the body's structure — `serde_json` above is what reads that.
fn assert_no_over_precise_number(case: &str, raw: &str) {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let whole = index - start;
        if index >= bytes.len() || bytes[index] != b'.' {
            continue;
        }
        index += 1;
        let fraction_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let fraction = index - fraction_start;
        // Nine digits of whole seconds is 2001 onwards: the shape of an epoch-seconds
        // instant, as against a version or a count that happens to be fractional.
        assert!(
            whole < 9 || fraction <= MAX_FRACTIONAL_DIGITS,
            "case {case:?}: the body carries the epoch-seconds number {} with {fraction} \
             fractional digits, past the {MAX_FRACTIONAL_DIGITS} the form expresses: {raw}",
            &raw[start..index]
        );
    }
}

/// RULE: every operation of this service that reports an instant reports it as
/// epoch seconds at millisecond resolution — whatever the host clock read.
///
/// The rows span what the rule generalises over: the operation, the member, and the depth
/// the member sits at (top level, inside the nested `plan`, inside a `stepStates` or
/// `items` entry).
#[tokio::test]
async fn every_reported_instant_carries_at_most_millisecond_resolution() {
    let emulator = Emulator::new();

    // A plan written twice (so `updatedAt` is re-stamped), an execution started, the
    // clock's first transition applied, the approval decided, and the clock's last
    // transition applied: five separate clock readings, each with a sub-millisecond part
    // of the host's own choosing.
    let created = emulator
        .succeed("CreatePlan", "CreatePlan", &plan_request("resolution"))
        .await;
    let plan_arn = created.body["plan"]["arn"]
        .as_str()
        .expect("CreatePlan reports the plan it stored")
        .to_string();
    let updated = emulator
        .succeed(
            "UpdatePlan",
            "UpdatePlan",
            &json!({
                "arn": plan_arn,
                "description": "re-stamped",
                "workflows": [
                    {
                        "workflowTargetAction": "deactivate",
                        "steps": [approval_step("confirm-shift")],
                    },
                ],
                "executionRole": format!("arn:aws:iam::{ACCOUNT}:role/ArcRegionSwitchRole"),
            }),
        )
        .await;
    let started = emulator
        .succeed(
            "StartPlanExecution",
            "StartPlanExecution",
            &json!({
                "planArn": plan_arn,
                "targetRegion": "us-west-2",
                "action": "deactivate",
                "comment": "a switch whose instants are read off the host clock",
            }),
        )
        .await;
    let execution_id = started.body["executionId"]
        .as_str()
        .expect("StartPlanExecution reports the execution it stored")
        .to_string();
    // The clock reaches the approval step: the step records a `startTime`.
    emulator.settle_past_the_clock();
    let pending_approval = emulator
        .succeed(
            "GetPlanExecution while the approval is pending",
            "GetPlanExecution",
            &json!({"planArn": plan_arn, "executionId": execution_id}),
        )
        .await;
    let approved = emulator
        .succeed(
            "ApprovePlanExecutionStep",
            "ApprovePlanExecutionStep",
            &json!({
                "planArn": plan_arn,
                "executionId": execution_id,
                "stepName": "confirm-shift",
                "approval": "approve",
            }),
        )
        .await;
    // Nothing is left to reach, so the clock completes the execution: an `endTime`.
    emulator.settle_past_the_clock();

    let completed = emulator
        .succeed(
            "GetPlanExecution once the execution completed",
            "GetPlanExecution",
            &json!({"planArn": plan_arn, "executionId": execution_id}),
        )
        .await;
    let listed_executions = emulator
        .succeed(
            "ListPlanExecutions",
            "ListPlanExecutions",
            &json!({"planArn": plan_arn}),
        )
        .await;
    let events = emulator
        .succeed(
            "ListPlanExecutionEvents",
            "ListPlanExecutionEvents",
            &json!({"planArn": plan_arn, "executionId": execution_id, "maxResults": 100}),
        )
        .await;
    let plan = emulator
        .succeed("GetPlan", "GetPlan", &json!({"arn": plan_arn}))
        .await;
    let listed_plans = emulator.succeed("ListPlans", "ListPlans", &json!({})).await;
    let evaluation = emulator
        .succeed(
            "GetPlanEvaluationStatus",
            "GetPlanEvaluationStatus",
            &json!({"planArn": plan_arn}),
        )
        .await;

    // Each row: the answer, and the paths whose instants it has to report, so a case
    // cannot pass by reporting nothing at all.
    let rows: &[(&str, &Answer, &[&str])] = &[
        ("CreatePlan", &created, &["$.plan.updatedAt"]),
        ("UpdatePlan", &updated, &["$.plan.updatedAt"]),
        (
            // `StartPlanExecutionResponse` carries no `Timestamp` member of its own
            // (`smithy_query(query="operation", operation="StartPlanExecution", …)`): the
            // instants it recorded are read back through the two readers below.
            "StartPlanExecution",
            &started,
            &[],
        ),
        (
            "GetPlanExecution while the approval is pending",
            &pending_approval,
            &[
                "$.startTime",
                "$.updatedAt",
                "$.plan.updatedAt",
                "$.stepStates[0].startTime",
            ],
        ),
        ("ApprovePlanExecutionStep", &approved, &[]),
        (
            "GetPlanExecution once the execution completed",
            &completed,
            &[
                "$.startTime",
                "$.endTime",
                "$.updatedAt",
                "$.plan.updatedAt",
                "$.stepStates[0].startTime",
                "$.stepStates[0].endTime",
            ],
        ),
        (
            "ListPlanExecutions",
            &listed_executions,
            &[
                "$.items[0].startTime",
                "$.items[0].endTime",
                "$.items[0].updatedAt",
            ],
        ),
        ("ListPlanExecutionEvents", &events, &["$.items[0].timestamp"]),
        ("GetPlan", &plan, &["$.plan.updatedAt"]),
        ("ListPlans", &listed_plans, &["$.plans[0].updatedAt"]),
        (
            "GetPlanEvaluationStatus",
            &evaluation,
            &[
                "$.lastEvaluationTime",
                "$.warnings[0].warningUpdatedTime",
            ],
        ),
    ];
    for (case, answer, expected_paths) in rows {
        assert_millisecond_resolution(case, answer);
        let reported = timestamp_members(&answer.body);
        for path in *expected_paths {
            assert!(
                reported.iter().any(|(found, _)| found == path),
                "case {case:?}: expected an instant at {path}, got {:?} in {}",
                reported.iter().map(|(found, _)| found).collect::<Vec<_>>(),
                answer.body
            );
        }
    }
}

/// RULE: the millisecond a reported instant names is the instant's OWN — the conversion
/// truncates to the resolution the form expresses and changes nothing else.
///
/// The rows span the sub-second part the rule generalises over: none at all, the finest
/// tick a host clock offers, one just under and one just over a millisecond boundary, an
/// arbitrary nanosecond remainder, and the last nanosecond of a second.
#[tokio::test]
async fn a_reported_instant_names_the_millisecond_it_fell_in() {
    // Each row is the sub-second part of the instant its approval is decided at, with the
    // milliseconds that instant falls in.
    let cases: &[(&str, u64, u64)] = &[
        ("a whole second", 0, 0),
        ("one nanosecond past the second", 1, 0),
        ("a microsecond under a millisecond", 999_000, 0),
        ("a microsecond over a millisecond", 1_001_000, 1),
        ("an arbitrary nanosecond remainder", 123_456_789, 123),
        ("the last nanosecond of the second", 999_999_999, 999),
    ];
    // A second in the past, so the instant a case chooses is one no host clock reading
    // could be mistaken for. Any whole second does: the rule is about the sub-second part.
    const WHOLE_SECONDS: u64 = 1_700_000_000;

    for (case, subsec_nanos, expected_millis) in cases {
        let emulator = Emulator::new();
        let created = emulator
            .succeed(case, "CreatePlan", &plan_request("decided"))
            .await;
        let plan_arn = created.body["plan"]["arn"]
            .as_str()
            .expect("CreatePlan reports the plan it stored")
            .to_string();
        let started = emulator
            .succeed(
                case,
                "StartPlanExecution",
                &json!({
                    "planArn": plan_arn,
                    "targetRegion": "us-west-2",
                    "action": "deactivate",
                }),
            )
            .await;
        let execution_id = started.body["executionId"]
            .as_str()
            .expect("StartPlanExecution reports the execution it stored")
            .to_string();
        // The clock reaches the approval step, which is then decided AT the instant this
        // row chose.
        emulator.settle_past_the_clock();
        let decided_at =
            SystemTime::UNIX_EPOCH + Duration::new(WHOLE_SECONDS, *subsec_nanos as u32);
        emulator.approve_at(
            case,
            &(plan_arn.clone(), execution_id.clone()),
            "confirm-shift",
            decided_at,
        );

        let read = emulator
            .succeed(
                case,
                "GetPlanExecution",
                &json!({"planArn": plan_arn, "executionId": execution_id}),
            )
            .await;
        assert_millisecond_resolution(case, &read);
        let reported = read.body["stepStates"][0]["endTime"].clone();
        #[allow(clippy::cast_precision_loss)]
        let expected = (WHOLE_SECONDS * 1_000 + expected_millis) as f64 / 1_000.0;
        assert_eq!(
            reported.as_f64(),
            Some(expected),
            "case {case:?}: expected the decided step to report the millisecond \
             {WHOLE_SECONDS}.{expected_millis:03} it fell in, got {reported} in {}",
            read.body
        );
    }
}

/// RULE: a reported instant is a function of the stored record, so two reads of one
/// execution report the same instants — a client comparing a timestamp it read back
/// against one it already holds sees no drift.
#[tokio::test]
async fn two_reads_of_one_execution_report_the_same_instants() {
    let emulator = Emulator::new();
    let created = emulator
        .succeed("stability", "CreatePlan", &plan_request("stable"))
        .await;
    let plan_arn = created.body["plan"]["arn"]
        .as_str()
        .expect("CreatePlan reports the plan it stored")
        .to_string();
    let started = emulator
        .succeed(
            "stability",
            "StartPlanExecution",
            &json!({
                "planArn": plan_arn,
                "targetRegion": "us-east-1",
                "action": "deactivate",
            }),
        )
        .await;
    let execution_id = started.body["executionId"]
        .as_str()
        .expect("StartPlanExecution reports the execution it stored")
        .to_string();
    emulator.settle_past_the_clock();
    let request = json!({"planArn": plan_arn, "executionId": execution_id});

    let first = emulator
        .succeed("stability", "GetPlanExecution", &request)
        .await;
    let again = emulator
        .succeed("stability", "GetPlanExecution", &request)
        .await;
    let found = assert_millisecond_resolution("stability", &first);
    assert!(
        found > 0,
        "case {:?}: expected GetPlanExecution to report instants, got {}",
        "stability",
        first.body
    );
    assert_eq!(
        timestamp_members(&first.body),
        timestamp_members(&again.body),
        "case {:?}: expected two reads of one execution to report the same instants, got \
         {} then {}",
        "stability",
        first.body,
        again.body
    );
    // The abbreviated projection the list path reports is the same record, so it carries
    // the same instants the detailed read does.
    let listed = emulator
        .succeed(
            "stability",
            "ListPlanExecutions",
            &json!({"planArn": plan_arn}),
        )
        .await;
    assert_millisecond_resolution("stability", &listed);
    for member in ["startTime", "endTime", "updatedAt"] {
        assert_eq!(
            listed.body["items"][0][member],
            first.body[member],
            "case {member:?}: expected the listed execution to carry the instant \
             GetPlanExecution reports, got {} against {}",
            listed.body["items"][0][member],
            first.body[member]
        );
    }
}
