// SPDX-License-Identifier: CC0-1.0

//! WHICH stage's method settings gate a request's execution log, and WHICH group that log is
//! delivered into.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource"
//! > and each `MethodSetting` carries the `loggingLevel` ("valid values are `OFF`, `ERROR`,
//! > and `INFO`") and `dataTraceEnabled` that turn a method's execution log on
//! > (`smithy_query(query="shape", shape="MethodSetting")`). The map belongs to ONE stage, so
//! > the pair in force for a request is the one carried by the stage the request reached, and
//! > the trace it writes is delivered into the group API Gateway publishes for that stage of
//! > that api — `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}` — every record named
//! > by the id the answer reported. Two stages of one api, and two apis carrying a stage of
//! > the same name, therefore trace by their own pair into their own group: a stage at `OFF`
//! > leaves nothing anywhere however its siblings are gated, a stage at `ERROR` leaves only
//! > the failure of a request it answered with an error, a stage at `INFO` leaves the
//! > informational steps beside it, and no record of one stage's request appears in another
//! > stage's group.
//!
//! The rows differ along what the rule generalises over: the stage a request is addressed to
//! (three stages of one api, released from one deployment, plus a fourth stage of a SECOND api
//! carrying the same name as one of them), the `loggingLevel` that stage carries, the
//! `dataTraceEnabled` beside it, and the status the stage answered the request with — which is
//! what separates `ERROR` from `INFO`. The recorded case is one row of it: one stage flipped
//! through the levels.
//!
//! What the sibling suites pin is the gate of ONE stage: the product of the level and the data
//! trace ([`crate::stage_execution_log_data_trace_gate`]), which `methodSettings` ENTRY of a
//! stage is in force for the method a request reached
//! ([`crate::stage_method_level_logging_level`]), and the same gate over an `AWS` service
//! integration ([`crate::stage_execution_log_service_integration_levels`]). What this suite
//! adds is the SCOPE of that gate: the stage it is read off and the group it writes into.
//!
//! Only a cross-service suite can pin this: the api, its stages' method settings and the
//! emission of the trace are API Gateway's, the proxied function is AWS Lambda's, and the
//! groups the traces land in are CloudWatch Logs' — all three on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every assertion reads the outcome back through
//! CloudWatch Logs' own `FilterLogEvents`, so a record that did not reach that store cannot
//! pass.
//!
//! Nothing is stood up here: the router, the invoke, the records of one request and the patch
//! that turns the two settings on are [`crate::stage_access_logging`]'s, and the function and
//! the deployed api are [`crate::stage_method_level_logging_level`]'s. What this suite adds is
//! the second and third stage, the second api, and the table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{apigw, member};
use crate::stage_access_logging::{
    emulator, execution_log_group, gate_execution_logging, install_account_role, invoke,
    records_opening_with, trace_of, traced_line, PAYLOAD_PREFIXES,
};
use crate::stage_method_level_logging_level::{deployed, given_function, RESOURCE_PART, STAGE};

/// The verb every request below is served by: WHICH method of a stage carries the entry in
/// force is a sibling suite's rule, and this one is about which STAGE the entry is read off.
const VERB: &str = "GET";

/// The sentence API Gateway opens a request's trace with, whichever level wrote it: the one
/// line of the trace that is about the request rather than about a step of serving it.
const OPENS: &str = "Execution log for request";

/// One stage of the api under test: the two settings it carries.
struct Row {
    /// The stage the row's requests are addressed to.
    stage: &'static str,
    /// The `loggingLevel` that stage's stage-wide entry carries.
    level: &'static str,
    /// The `dataTraceEnabled` patched beside it.
    data_trace: bool,
}

/// The stages of the one api: every documented level of `MethodSetting$loggingLevel`, one per
/// stage, with the data trace both ways over the two that write a trace at all.
///
/// The first row is the stage the deployment was released to ([`STAGE`]); the others are stages
/// created from that same deployment, so what differs between them is the settings they carry
/// and nothing else.
const ROWS: &[Row] = &[
    Row {
        stage: STAGE,
        level: "INFO",
        data_trace: true,
    },
    Row {
        stage: "errors",
        level: "ERROR",
        data_trace: true,
    },
    Row {
        stage: "quiet",
        level: "OFF",
        data_trace: true,
    },
];

/// The statuses each stage is driven over: one request the stage served and one it answered
/// with an error, which is the axis that separates `ERROR` from `INFO`.
const STATUSES: [u16; 2] = [200, 500];

/// The id of the one deployment of `api`, which the stages below are released from.
async fn deployment_of(router: &MultiServiceRouter, api: &str) -> String {
    let read = apigw(
        router,
        "GetDeployments",
        "GET",
        &format!("/restapis/{api}/deployments"),
        None,
        200,
    )
    .await;
    read.get("item")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|item| member("GetDeployments", item, "id"))
        .expect("setup GetDeployments: the api's own deployment")
}

/// Release `deployment` to another stage of the api, named `stage`.
async fn release_to(router: &MultiServiceRouter, api: &str, stage: &str, deployment: &str) {
    apigw(
        router,
        "CreateStage",
        "POST",
        &format!("/restapis/{api}/stages"),
        Some(json!({ "stageName": stage, "deploymentId": deployment })),
        201,
    )
    .await;
}

/// Assert `stage` of `api` REPORTS the pair `(level, data_trace)` under its stage-wide entry:
/// the entry the data plane resolves is the entry the patch stored on THAT stage, so a stage
/// reporting another pair is gating by another configuration than the caller named.
async fn assert_reported(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    level: &str,
    data_trace: bool,
) {
    let read = apigw(
        router,
        "GetStage",
        "GET",
        &format!("/restapis/{api}/stages/{stage}"),
        None,
        200,
    )
    .await;
    let settings = &read["methodSettings"]["*/*"];
    assert_eq!(
        (&settings["loggingLevel"], &settings["dataTraceEnabled"]),
        (&json!(level), &json!(data_trace)),
        "case {stage:?}: expected the stage to report loggingLevel {level:?} with \
         dataTraceEnabled {data_trace:?}, got {read:?}"
    );
}

/// What the rule says one request's trace holds under the pair in force for the stage it
/// reached: computed from that pair and the status, never written out per row.
struct Expected {
    /// Whether anything about the request is written at all.
    traced: bool,
    /// Whether the INFORMATIONAL steps of serving it are among what was written.
    informational: bool,
    /// Whether the PAYLOAD lines of the call are among what was written.
    payloads: bool,
}

impl Expected {
    /// The trace the rule owes a request answered with `status` under `level`/`data_trace`.
    fn of(level: &str, data_trace: bool, status: u16) -> Self {
        let traced = match level {
            "INFO" => true,
            "ERROR" => status >= 400,
            _ => false,
        };
        let informational = traced && level == "INFO";
        // The payload lines are informational, so a level writing no informational step writes
        // no payload line whatever the data trace says.
        Self {
            traced,
            informational,
            payloads: informational && data_trace,
        }
    }
}

/// Assert the records `trace` holds about one request against `expected`.
fn assert_trace(case: &str, request_id: &str, trace: &[String], status: u16, expected: &Expected) {
    if !expected.traced {
        assert!(
            trace.is_empty(),
            "case {case:?}: expected nothing about this request to be written, got {trace:?}"
        );
        return;
    }
    let opens = traced_line(request_id, &format!("{OPENS} {request_id}"));
    assert!(
        trace.contains(&opens),
        "case {case:?}: expected the record {opens:?} in this request's trace, got {trace:?}"
    );
    for line in [
        traced_line(
            request_id,
            &format!("HTTP Method: {VERB}, Resource Path: /{RESOURCE_PART}"),
        ),
        traced_line(request_id, &format!("Method completed with status: {status}")),
    ] {
        assert_eq!(
            trace.contains(&line),
            expected.informational,
            "case {case:?}: expected the record {line:?} in this request's trace={}, got \
             {trace:?}",
            expected.informational
        );
    }
    for prefix in PAYLOAD_PREFIXES {
        let prefix = traced_line(request_id, prefix);
        let wanted = usize::from(expected.payloads);
        assert_eq!(
            records_opening_with(trace, &prefix),
            wanted,
            "case {case:?}: expected the payload line {prefix:?} exactly {wanted} time(s), got \
             {trace:?}"
        );
    }
}

/// Drive one request at `stage` of `api`, assert what that stage's own group then holds about
/// it, and assert no OTHER group among `groups` holds a record of it at all.
async fn one_leg(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    case: &str,
    status: u16,
    expected: &Expected,
    groups: &[(String, String)],
) {
    let answered = invoke(
        router,
        api,
        stage,
        VERB,
        &format!("/{RESOURCE_PART}?status={status}"),
    )
    .await;
    assert_eq!(
        (answered.status, answered.json()),
        (status, json!({ "verb": VERB, "status": status })),
        "case {case:?}: expected the function's own answer under HTTP {status}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let request_id = answered
        .request_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the answer named no request id"));

    let trace = trace_of(router, &execution_log_group(api, stage), &request_id).await;
    assert_trace(case, &request_id, &trace, status, expected);

    // And nothing of this request anywhere else: a stage's trace is delivered into the group
    // named by ITS api and ITS name, so a group of another stage — or of another api carrying a
    // stage of the same name — holds no record of it.
    for (elsewhere_api, elsewhere_stage) in groups {
        if elsewhere_api == api && elsewhere_stage == stage {
            continue;
        }
        let group = execution_log_group(elsewhere_api, elsewhere_stage);
        let crossed = trace_of(router, &group, &request_id).await;
        assert!(
            crossed.is_empty(),
            "case {case:?}: expected no record of this request in {group:?}, got {crossed:?}"
        );
    }
}

/// RULE: the `loggingLevel`/`dataTraceEnabled` in force for a request are the ones carried by
/// the STAGE it reached, and the trace they write is delivered into the group API Gateway
/// publishes for that stage of that api — so three stages of one api gated differently trace
/// by their own pair into their own group, and an api carrying a stage of the same name as
/// another's traces into its own group too.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits an invocation this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_stage_traces_by_its_own_settings_into_its_own_group() {
    let router = emulator();
    let function_arn = given_function(&router).await;
    install_account_role(&router).await;

    // One api, one deployment, three stages of it: what differs between the stages is the pair
    // of settings each carries.
    let api = deployed(&router, &function_arn, "per-stage-scope").await;
    let deployment = deployment_of(&router, &api).await;
    for row in ROWS {
        if row.stage != STAGE {
            release_to(&router, &api, row.stage, &deployment).await;
        }
    }
    for row in ROWS {
        gate_execution_logging(&router, &api, row.stage, row.level, row.data_trace).await;
    }
    // Every stage reports the pair IT was patched with, after every other stage has been
    // patched: the map belongs to one stage, so patching a sibling leaves it untouched.
    for row in ROWS {
        assert_reported(&router, &api, row.stage, row.level, row.data_trace).await;
    }

    // A SECOND api, with a stage of the same name as the first api's, gated at `INFO` with the
    // data trace off: the group is named by the api as well as the stage.
    let sibling = deployed(&router, &function_arn, "per-stage-scope-sibling").await;
    gate_execution_logging(&router, &sibling, STAGE, "INFO", false).await;
    assert_reported(&router, &sibling, STAGE, "INFO", false).await;

    let mut groups: Vec<(String, String)> = ROWS
        .iter()
        .map(|row| (api.clone(), row.stage.to_string()))
        .collect();
    groups.push((sibling.clone(), STAGE.to_string()));

    for row in ROWS {
        for status in STATUSES {
            let case = format!(
                "stage {:?} at {} with the data trace {} answering {status}",
                row.stage, row.level, row.data_trace
            );
            let expected = Expected::of(row.level, row.data_trace, status);
            one_leg(&router, &api, row.stage, &case, status, &expected, &groups).await;
        }
    }

    // The sibling api's stage of the SAME name: traced by its own pair — the informational steps
    // without a payload line, the data trace being off there — into its own group.
    let case = "the sibling api's stage of the same name, at INFO with the data trace off";
    let expected = Expected::of("INFO", false, 200);
    one_leg(&router, &sibling, STAGE, case, 200, &expected, &groups).await;
}
