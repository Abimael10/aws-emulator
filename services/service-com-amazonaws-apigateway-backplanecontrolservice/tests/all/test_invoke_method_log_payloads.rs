// SPDX-License-Identifier: CC0-1.0

//! WHAT the execution log a simulated invocation hands back states about the call it is a
//! log of.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateResource` / `PutMethod` / `PutIntegration` /
//! `PutMethodResponse` / `PutIntegrationResponse` / `TestInvokeMethod` helpers are this
//! crate's existing harness, reused from `rest_api_lifecycle`, `method_integration`,
//! `method_response`, `integration_response` and `test_invoke_method`.
//!
//! THE RULE UNDER TEST: `TestInvokeMethodResponse$log` is "the API Gateway execution log for
//! the test invoke request", so it is a record of THIS call rather than of the operation: at
//! each transformation point the call crossed it states the payload that crossed there — the
//! method request before transformations, and the method response after them — so a log read
//! by a developer debugging an integration varies with the method's own configuration (the
//! templates it renders, the branch its result selects) and with the request simulated. The
//! payload is the caller's own and the response is rendered from the API's records, and both
//! travel back to that caller in THIS response beside the log, so the log states no value the
//! caller does not already hold.
//!
//! The rows span what the rule generalises over: the template the integration renders (a
//! distinct per-row marker, a body of another JSON shape, an empty one), the branch the
//! integration's result selects (a 200 and a 404 of the same method), the media type the
//! simulated `Accept` header selects a response template by, the body the simulated request
//! carries (none, short, multi-kilobyte) and the verb the method is addressed by.

use serde_json::{json, Value};

use crate::integration_response::put_integration_response;
use crate::method_integration::{put_integration, resource_with_a_method};
use crate::method_response::put_method_response;
use crate::rest_api_lifecycle::{create_api, Gateway};
use crate::test_invoke_method::test_invoke;

/// The two transformation points a simulated call always crosses, as the log labels them.
const REQUEST_POINT: &str = "Method request body before transformations";
const RESPONSE_POINT: &str = "Method response body after transformations";

/// The execution log of a simulated invocation — the `log` member of the response.
fn log_of(body: &Value, case: &str) -> String {
    body["log"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a log member, got {body:?}"))
        .to_string()
}

/// One row: its case, the verb its method is put under, the `Accept` header the simulated
/// request carries, the body it carries, the status the integration's result selects, and the
/// body that branch's response template renders.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    String,
    &'static str,
    String,
);

#[tokio::test]
async fn the_execution_log_states_the_payload_that_crossed_each_transformation_point() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "test-invoke-log-payloads").await;

    // Each row's own marker, so a log that did not come from this row's configuration and
    // this row's request cannot carry it.
    let marker = |row: usize| format!("marker-{row}");
    // A body past one kilobyte: how large a payload is is the caller's choice, and the line
    // states the payload the call carried whatever its size. Row 0's and row 5's requests
    // carry no body at all, the other bound of the same axis.
    let long_body = format!("{{\"padding\":\"{}\"}}", "p".repeat(2048));

    let rows: Vec<Row> = vec![
        (
            "a mock answering 200 from its own response template",
            "GET",
            "application/json",
            String::new(),
            "200",
            format!("{{\"marker\":\"{}\"}}", marker(0)),
        ),
        (
            "another method whose template renders a JSON array",
            "POST",
            "application/json",
            json!({ "sent": marker(1) }).to_string(),
            "200",
            format!("[\"{}\",\"second\"]", marker(1)),
        ),
        (
            "a branch the integration's result selects by status",
            "PUT",
            "application/json",
            json!({ "sent": marker(2) }).to_string(),
            "404",
            format!("{{\"error\":\"{}\"}}", marker(2)),
        ),
        (
            "a template selected by another media type",
            "DELETE",
            "text/plain",
            json!({ "sent": marker(3) }).to_string(),
            "200",
            format!("plain {}", marker(3)),
        ),
        (
            "a multi-kilobyte simulated request body",
            "PATCH",
            "application/json",
            long_body.clone(),
            "200",
            format!("{{\"marker\":\"{}\"}}", marker(4)),
        ),
        (
            "a template rendering a nested object",
            "HEAD",
            "application/json",
            String::new(),
            "200",
            format!("{{\"nested\":{{\"marker\":\"{}\"}}}}", marker(5)),
        ),
    ];

    for (row, (case, verb, accepted, sent, status, rendered)) in rows.iter().enumerate() {
        let resource =
            resource_with_a_method(&gateway, &api, &root, &format!("row{row}"), verb, json!({}))
                .await;
        // A `MOCK` has no backend: its rendered request template IS the result the
        // integration produced, so the row's `status` is what its branch is selected by.
        let created = put_integration(
            &gateway,
            &api,
            &resource,
            verb,
            json!({
                "type": "MOCK",
                "requestTemplates": {
                    "application/json": format!("{{\"statusCode\": {status}}}")
                },
            }),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected 201 from PutIntegration, got {} with {:?}",
            created.status, created.body
        );
        let response = put_method_response(&gateway, &api, &resource, verb, status, json!({})).await;
        assert_eq!(
            response.status, 201,
            "case {case}: expected 201 from PutMethodResponse, got {} with {:?}",
            response.status, response.body
        );
        let integration_response = put_integration_response(
            &gateway,
            &api,
            &resource,
            verb,
            status,
            json!({ "responseTemplates": { (*accepted): rendered } }),
        )
        .await;
        assert_eq!(
            integration_response.status, 201,
            "case {case}: expected 201 from PutIntegrationResponse, got {} with {:?}",
            integration_response.status, integration_response.body
        );

        let simulated = test_invoke(
            &gateway,
            &api,
            &resource,
            verb,
            json!({
                "headers": { "Content-Type": "application/json", "Accept": accepted },
                "body": sent,
            }),
        )
        .await;
        assert_eq!(
            simulated.http_status, 200,
            "case {case}: expected TestInvokeMethod itself to answer 200, got {} with {:?}",
            simulated.http_status, simulated.body
        );
        let served = simulated.simulated_body();
        assert_eq!(
            &served, rendered,
            "case {case}: expected the configured template's body {rendered:?}, got {served:?}"
        );

        let log = log_of(&simulated.body, case);
        // The two payload points of the call: what the caller sent, and what the API's own
        // records rendered for it.
        for (point, payload) in [(REQUEST_POINT, sent), (RESPONSE_POINT, &served)] {
            let line = format!("{point}: {payload}");
            assert!(
                log.contains(&line),
                "case {case}: expected the log to state {line:?}, got {log:?}"
            );
        }
        // So the log is about THIS configuration and THIS request: it carries the row's own
        // marker and no other row's.
        assert!(
            log.contains(&marker(row)),
            "case {case}: expected the log to carry this row's marker {:?}, got {log:?}",
            marker(row)
        );
        for other in 0..rows.len() {
            if other == row {
                continue;
            }
            assert!(
                !log.contains(&marker(other)),
                "case {case}: expected no other row's marker, got {:?} in {log:?}",
                marker(other)
            );
        }
        // And the log still reports the steps around those points: the status the call
        // completed with, and the integration the result came from.
        for step in [
            format!("Method completed with status: {status}"),
            "Integration type: MOCK".to_string(),
            "Successfully completed execution".to_string(),
        ] {
            assert!(
                log.contains(&step),
                "case {case}: expected the log to report {step:?}, got {log:?}"
            );
        }
    }
}

#[tokio::test]
async fn reconfiguring_the_integration_changes_the_log_of_the_next_call() {
    // The log is a record of the call, so the same request simulated after a change of the
    // method's records states what the records render NOW. Rows are the successive
    // configurations of ONE method, each with the body its template renders.
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "test-invoke-log-reconfigured").await;
    let resource = resource_with_a_method(&gateway, &api, &root, "pets", "GET", json!({})).await;
    let created = put_integration(
        &gateway,
        &api,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case PutIntegration: expected 201, got {} with {:?}",
        created.status, created.body
    );
    let response = put_method_response(&gateway, &api, &resource, "GET", "200", json!({})).await;
    assert_eq!(
        response.status, 201,
        "case PutMethodResponse: expected 201, got {} with {:?}",
        response.status, response.body
    );

    let configurations = [
        ("the first template", "{\"state\":\"first\"}"),
        ("a replacement template", "{\"state\":\"second\"}"),
        ("a third, of another shape", "[\"third\"]"),
    ];
    for (case, rendered) in configurations {
        let written = put_integration_response(
            &gateway,
            &api,
            &resource,
            "GET",
            "200",
            json!({ "responseTemplates": { "application/json": rendered } }),
        )
        .await;
        assert_eq!(
            written.status, 201,
            "case {case}: expected 201 from PutIntegrationResponse, got {} with {:?}",
            written.status, written.body
        );
        let simulated = test_invoke(
            &gateway,
            &api,
            &resource,
            "GET",
            json!({ "headers": { "Content-Type": "application/json" } }),
        )
        .await;
        let log = log_of(&simulated.body, case);
        let line = format!("{RESPONSE_POINT}: {rendered}");
        assert!(
            log.contains(&line),
            "case {case}: expected the log to state {line:?}, got {log:?}"
        );
    }
}
