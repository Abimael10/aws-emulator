// SPDX-License-Identifier: CC0-1.0

//! How much of a request a stage's EXECUTION log carries about it: the header line is bounded
//! by the emulator, whatever the caller sends.
//!
//! The rule under test:
//!
//! > The names of the headers a request carries are the CALLER's choice, and a log event is
//! > itself bounded, so the `Method request headers` line of a stage's execution log carries at
//! > most the guard's worth of names and reports the rest by their COUNT — the same reading the
//! > trace already takes of a payload's size. The request is served exactly as it would be with
//! > fewer headers: the guard bounds what is WRITTEN, never what is answered, and the trace of
//! > the request is delivered either way.
//!
//! The guard is `MAX_TRACED_HEADER_NAMES` in
//! `src/service/invoke/stage_logs.rs`, which also words this line for the two `TestInvoke*`
//! operations, so one bound holds for a request a stage served and for a simulated one.
//!
//! The rows differ along what the rule generalises over: how many headers the request carries —
//! a handful, exactly as many as the guard carries, one past it, and far past it.

use serde_json::json;

use crate::execute_api_data_plane::{deploy, invoke, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, gateway_with_iam_and_logs, Gateway};
// The account's logging role is installed the one way every suite about what a stage delivers
// installs it ([`crate::stage_access_log_absent_destination`]): without it API Gateway has
// nothing to write the account's logs as, so the trace below would not be delivered at all.
use crate::stage_access_log_absent_destination::install_account_role;
use crate::stage_access_logging::delivered;

/// The stage the api is deployed to, and the one `MOCK` method it serves.
const STAGE: &str = "live";
const PATH_PART: &str = "ping";
const VERB: &str = "GET";
const STATUS: &str = "200";
const BODY: &str = r#"{"pong":true}"#;

/// The guard the rule is about, as `src/service/invoke/stage_logs.rs` declares it
/// (`MAX_TRACED_HEADER_NAMES`): the most header names one traced line carries.
const MAX_TRACED_HEADER_NAMES: usize = 128;

/// The header API Gateway reports the id of the request it processed in, which every line of
/// that request's trace is prefixed with.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// An api with one `MOCK` resource, deployed to the stage, with execution logging turned on
/// for every method of it. Answers the api id.
async fn traced_api(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "execution-log-bounded-header-line").await;
    mock_resource(
        gateway,
        &api,
        &root,
        PATH_PART,
        VERB,
        STATUS,
        json_template(BODY),
    )
    .await;
    deploy(gateway, &api, STAGE).await;
    // `loggingLevel` INFO on every method, which is what turns the execution log on
    // ("specifies the logging level for this method", `MethodSetting$loggingLevel`).
    let logging = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/*/*/logging/loglevel", "value": "INFO" },
                ],
            })),
        )
        .await;
    assert_eq!(
        logging.status, 200,
        "setup UpdateStage /*/*/logging/loglevel: expected 200, got {} with {:?}",
        logging.status, logging.body
    );
    api
}

/// The `Method request headers` line of `request_id`'s trace, out of the records the group
/// holds.
fn header_line(records: &[String], request_id: &str, case: &str) -> String {
    let prefix = format!("({request_id}) Method request headers: [");
    records
        .iter()
        .find(|record| record.starts_with(&prefix))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no header line for request {request_id:?} in {records:?}")
        })
}

/// What one header line reports: how many names it LISTS, and how many it says are past the
/// guard (`0` when it says nothing of the kind).
fn listed_and_past(line: &str, case: &str) -> (usize, usize) {
    let (_, rest) = line
        .split_once('[')
        .unwrap_or_else(|| panic!("case {case:?}: no name list in {line:?}"));
    let (names, tail) = rest
        .rsplit_once(']')
        .unwrap_or_else(|| panic!("case {case:?}: the name list is not closed in {line:?}"));
    let listed = if names.is_empty() {
        0
    } else {
        names.split(", ").count()
    };
    let past = match tail.trim() {
        "" => 0,
        reported => reported
            .strip_prefix("and ")
            .and_then(|rest| rest.strip_suffix(" more"))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the names past the guard to be counted, got {line:?}")
            }),
    };
    (listed, past)
}

/// RULE: the trace's header line carries at most the guard's worth of names and counts the
/// rest, and the request is served whatever it carries.
#[tokio::test]
async fn the_traced_header_line_is_bounded_however_many_headers_a_request_carries() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = traced_api(&gateway).await;
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");

    for (case, carried) in [
        ("a handful of headers", 3usize),
        ("one header short of the guard", MAX_TRACED_HEADER_NAMES - 1),
        ("as many headers as the guard carries", MAX_TRACED_HEADER_NAMES),
        ("one header past the guard", MAX_TRACED_HEADER_NAMES + 1),
        ("far more headers than the guard carries", MAX_TRACED_HEADER_NAMES * 4),
    ] {
        // Each name distinct, so the request carries exactly `carried` of its own beside the
        // `host` header every request addresses an endpoint by.
        let names: Vec<String> = (0..carried).map(|index| format!("x-probe-{index:04}")).collect();
        let headers: Vec<(&str, &str)> = names
            .iter()
            .map(|name| (name.as_str(), "probe"))
            .collect();
        let answered = invoke(
            &gateway,
            &api,
            VERB,
            &format!("/{STAGE}/{PATH_PART}"),
            &headers,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the stage to serve the request whatever it carries, got {} \
             with {:?}",
            answered.status, answered.body
        );
        let request_id = answered
            .header(REQUEST_ID_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the answer named no request id"))
            .to_owned();

        let records = delivered(&gateway.router, &group)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the group {group:?} to be there"));
        let line = header_line(&records, &request_id, case);
        let (listed, past) = listed_and_past(&line, case);
        // The request carries its own names and the `host` header it addressed the endpoint
        // by, each of which is a name the trace reports.
        let total = carried + 1;
        assert_eq!(
            listed,
            total.min(MAX_TRACED_HEADER_NAMES),
            "case {case:?}: expected at most {MAX_TRACED_HEADER_NAMES} names listed of {total}, \
             got {listed} in {line:?}"
        );
        assert_eq!(
            past,
            total.saturating_sub(MAX_TRACED_HEADER_NAMES),
            "case {case:?}: expected the names past the guard to be counted, got {past} in \
             {line:?}"
        );
        // And the line is one event of a trace that was delivered in full: the guard bounds the
        // line, not the request's record.
        assert!(
            records
                .iter()
                .any(|record| record == &format!("({request_id}) Method completed with status: 200")),
            "case {case:?}: expected the whole trace of {request_id:?} to be delivered, got \
             {records:?}"
        );
    }
}
