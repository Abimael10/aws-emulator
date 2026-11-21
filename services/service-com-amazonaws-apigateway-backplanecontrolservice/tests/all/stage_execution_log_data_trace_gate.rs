// SPDX-License-Identifier: CC0-1.0

//! What the `loggingLevel` and the `dataTrace` in force for a method TOGETHER let a request
//! leave in the group API Gateway publishes for the stage's execution log.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `MethodSetting$loggingLevel` "specifies the logging level for this method, which affects
//! > the log entries pushed to Amazon CloudWatch Logs", with "valid values are `OFF`, `ERROR`,
//! > and `INFO`", and `MethodSetting$dataTrace` "specifies whether data trace logging is
//! > enabled for this method, which affects the log entries pushed to Amazon CloudWatch Logs"
//! > (`smithy_query(query="shape", shape="MethodSetting")`). The two gate the stage's execution
//! > log on two independent axes, and both have to admit a line for it to be written: the level
//! > decides WHICH of a request's lines are written — `INFO` the whole trace, `ERROR` only the
//! > entries that report a failure (so only a request the stage answered WITH an error leaves
//! > anything at all), `OFF` or no entry in force none — and the data trace decides whether the
//! > PAYLOAD lines of the call exist at all. The payload lines are informational, so a request's
//! > bodies reach `API-Gateway-Execution-Logs_{api-id}/{stage}` only where the level writes the
//! > informational steps and the data trace is on, and turning either off stops them.
//!
//! The rows span the product of what that generalises over: every documented level and the
//! absence of an entry, the data trace on and off on each, and a request the stage answered
//! `200` beside one it answered with an error — which is the axis that separates `ERROR` from
//! `INFO`. The recorded case is one row of it (`INFO` with the data trace on, then off); the
//! pair no sibling suite drives — `ERROR` with the data trace ON — is the one that shows the
//! two settings are nested rather than alternatives: the error trace is written and carries no
//! payload line, the payload lines being among the informational ones that level leaves out.
//!
//! Every request below is served THROUGH the method's integration, its status the backend's own
//! answer. What an `ERROR` trace of a request the stage could NOT serve through its integration
//! carries — the configuration error, the other thing that level writes — is
//! [`crate::stage_execution_log_failure_lines`]'s rule, and WHICH method's entry is in force is
//! [`crate::stage_method_level_logging_level`]'s.
//!
//! Only a cross-service suite can pin this: the api, its stage's method settings and the
//! emission of the trace are API Gateway's, the proxied function is AWS Lambda's, and the group
//! the trace lands in is CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs`
//! mounts them. Every assertion reads the outcome back through CloudWatch Logs' own reader
//! (`FilterLogEvents`), so a record that did not reach that store cannot pass.
//!
//! Nothing is stood up here: the api, its function and the gating patch are the ones the
//! sibling suites already drive — the router, the invoke, the records of one request
//! ([`trace_of`]), the payload lines a data trace writes ([`PAYLOAD_PREFIXES`]) and the patch
//! that turns the two settings on ([`gate_execution_logging`]) are
//! [`crate::stage_access_logging`]'s, and the function and the deployed api are
//! [`crate::stage_method_level_logging_level`]'s. What this suite adds is the TABLE.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::apigw;
use crate::stage_access_logging::{
    emulator, gate_execution_logging, install_account_role, invoke, trace_of, traced_line,
    PAYLOAD_PREFIXES,
};
use crate::stage_method_level_logging_level::{deployed, given_function, RESOURCE_PART, STAGE};

/// The verb the row's request is served by: one of the verbs the shared api carries a method
/// of, which is enough — WHICH method carries the entry in force is the sibling suite's rule,
/// and this one is about the two settings of the entry.
const VERB: &str = "GET";

/// One row of the gating table: the two settings in force, the status the stage answers the
/// request with, and what the stage's execution log then holds about that request.
struct Row {
    case: &'static str,
    /// The stage-wide `loggingLevel`, or `None` for the stage as it is deployed — carrying no
    /// method setting at all.
    level: Option<&'static str>,
    /// The `dataTrace` patched beside it.
    data_trace: bool,
    /// The status the function answers with, which is the status the stage answers the client.
    status: u16,
    /// Whether anything about the request is written at all.
    traced: bool,
    /// Whether the INFORMATIONAL steps of serving it are among what was written.
    informational: bool,
    /// Whether the PAYLOAD lines of the call are among what was written.
    payloads: bool,
}

/// The rows: the product of every level (and no entry at all) with the data trace both ways,
/// over a request the stage served and one it answered with an error.
///
/// The row carrying NO entry comes first, because it is the only one that asserts against a
/// stage nothing has patched yet.
const ROWS: &[Row] = &[
    Row {
        case: "no method setting at all",
        level: None,
        data_trace: false,
        status: 200,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "logging off, data trace off, a served request",
        level: Some("OFF"),
        data_trace: false,
        status: 200,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "logging off with the data trace on, a served request",
        level: Some("OFF"),
        data_trace: true,
        status: 200,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "logging off with the data trace on, a request answered with an error",
        level: Some("OFF"),
        data_trace: true,
        status: 400,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "errors only, data trace off, a served request",
        level: Some("ERROR"),
        data_trace: false,
        status: 200,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "errors only with the data trace on, a served request",
        level: Some("ERROR"),
        data_trace: true,
        status: 200,
        traced: false,
        informational: false,
        payloads: false,
    },
    Row {
        case: "errors only, data trace off, a request answered with an error",
        level: Some("ERROR"),
        data_trace: false,
        status: 500,
        traced: true,
        informational: false,
        payloads: false,
    },
    Row {
        case: "errors only with the data trace on, a request answered with an error",
        level: Some("ERROR"),
        data_trace: true,
        status: 503,
        traced: true,
        informational: false,
        payloads: false,
    },
    Row {
        case: "the whole trace, data trace off, a served request",
        level: Some("INFO"),
        data_trace: false,
        status: 200,
        traced: true,
        informational: true,
        payloads: false,
    },
    Row {
        case: "the whole trace with the data trace on, a served request",
        level: Some("INFO"),
        data_trace: true,
        status: 200,
        traced: true,
        informational: true,
        payloads: true,
    },
    Row {
        case: "the whole trace, data trace off, a request answered with an error",
        level: Some("INFO"),
        data_trace: false,
        status: 404,
        traced: true,
        informational: true,
        payloads: false,
    },
    Row {
        case: "the whole trace with the data trace on, a request answered with an error",
        level: Some("INFO"),
        data_trace: true,
        status: 500,
        traced: true,
        informational: true,
        payloads: true,
    },
];

/// Gate every method of the stage at `level` with `data_trace` beside it, and assert the stage
/// REPORTS both settings back: the entry the data plane resolves is the entry the patch stored,
/// so a stage reporting another pair is gating by another configuration than the caller named.
async fn gate(router: &MultiServiceRouter, api: &str, case: &str, level: &str, data_trace: bool) {
    gate_execution_logging(router, api, STAGE, level, data_trace).await;
    let read = apigw(
        router,
        "GetStage",
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    let settings = &read["methodSettings"]["*/*"];
    assert_eq!(
        (&settings["loggingLevel"], &settings["dataTraceEnabled"]),
        (&json!(level), &json!(data_trace)),
        "case {case:?}: expected the stage to report loggingLevel {level:?} with \
         dataTraceEnabled {data_trace:?}, got {read:?}"
    );
}

/// RULE: the level in force decides which of a request's lines are written, the data trace
/// decides whether the payload lines exist at all, and both have to admit a line — so the
/// bodies of a call reach the stage's execution log group only at `INFO` with the data trace
/// on, and turning either off stops them.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits an invocation this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_level_and_the_data_trace_in_force_both_gate_what_a_request_leaves() {
    let router = emulator();
    let function_arn = given_function(&router).await;
    install_account_role(&router).await;
    let api = deployed(&router, &function_arn, "level-against-data-trace").await;
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");

    for row in ROWS {
        let case = row.case;
        if let Some(level) = row.level {
            gate(&router, &api, case, level, row.data_trace).await;
        }

        let answered = invoke(
            &router,
            &api,
            STAGE,
            VERB,
            &format!("/{RESOURCE_PART}?status={}", row.status),
        )
        .await;
        assert_eq!(
            (answered.status, answered.json()),
            (row.status, json!({ "verb": VERB, "status": row.status })),
            "case {case:?}: expected the function's own answer under HTTP {}, got {} with {:?}",
            row.status,
            answered.status,
            answered.text()
        );
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));

        // The records of THIS request: every record of a stage's execution log names the
        // request it is about, which is what separates this request's trace from the traces of
        // the requests before it. A stage that has traced nothing yet has no group of its own
        // either, which is as much "nothing was written" as an empty trace in a group is.
        let trace = trace_of(&router, &group, &request_id).await;
        if !row.traced {
            assert!(
                trace.is_empty(),
                "case {case:?}: expected no trace of this request in {group:?}, got {trace:?}"
            );
            continue;
        }
        // The line that OPENS a request's log is in every trace, whichever level wrote it: it
        // is what names the request the records are about.
        let opens = traced_line(&request_id, &format!("Execution log for request {request_id}"));
        assert!(
            trace.contains(&opens),
            "case {case:?}: expected the record {opens:?} in this request's trace, got {trace:?}"
        );
        // The INFORMATIONAL steps of serving the request, which only a trace written at `INFO`
        // carries: the levels are nested, so a trace written at `ERROR` reports the failure
        // and leaves out every step of serving the request — the verb and path, the header
        // names, the integration that answered and the status the method completed with.
        for line in [
            traced_line(
                &request_id,
                &format!("HTTP Method: {VERB}, Resource Path: /{RESOURCE_PART}"),
            ),
            traced_line(&request_id, "Integration type: AWS_PROXY"),
            traced_line(
                &request_id,
                &format!("Method completed with status: {}", row.status),
            ),
        ] {
            assert_eq!(
                trace.contains(&line),
                row.informational,
                "case {case:?}: expected the record {line:?} in this request's trace={}, got \
                 {trace:?}",
                row.informational
            );
        }
        // One line per transformation point the request crossed where both settings admit it,
        // and NO payload line at all otherwise — which is the control `dataTrace` exists for.
        for prefix in PAYLOAD_PREFIXES {
            let prefix = traced_line(&request_id, prefix);
            let found = trace
                .iter()
                .filter(|record| record.starts_with(&prefix))
                .count();
            assert_eq!(
                found,
                usize::from(row.payloads),
                "case {case:?}: expected the payload line {prefix:?} exactly {} time(s), got \
                 {trace:?}",
                usize::from(row.payloads)
            );
        }
        // And the bodies themselves: the answer the caller holds is in the group exactly where
        // a payload line was written, so a trace without those lines logged no body.
        let body = answered.text();
        let carrying: Vec<&String> = trace
            .iter()
            .filter(|record| record.contains(&body))
            .collect();
        assert_eq!(
            !carrying.is_empty(),
            row.payloads,
            "case {case:?}: expected the bodies of the call ({body:?}) in this request's \
             trace={}, got {carrying:?} in {trace:?}",
            row.payloads
        );
    }
}
