// SPDX-License-Identifier: CC0-1.0

//! What an enrichment UPDATE is answered with while the account's enrichment is not enabled.
//!
//! The rule under test: **`UpdateOTelEnrichment` acts on the enrichment the account already
//! has, so a call made while that enrichment is not enabled is refused with the
//! not-found class its model declares
//! (`smithy_query(query="operation",operation="UpdateOTelEnrichment")`:
//! `ResourceNotFoundException`, `awsQueryError` code `ResourceNotFoundException`) and the fixed
//! sentence of that refusal — `OTel enrichment is not enabled for this account` — whatever put
//! the account in that state, and the same call made while it IS enabled is served.**
//!
//! Not enabled is ONE condition, which is why the one sentence answers for all of it: an
//! account no enrichment call was ever made for, one whose enrichment has been stopped, and one
//! stopped after an update are the same state, and the read of the enrichment reports it as the
//! one stopped status either way (`GetOTelEnrichmentOutput$Status`).
//!
//! The rows differ along what the rule generalises over: HOW the account came to be without
//! enabled enrichment (never started, started then stopped, started-updated-stopped, stopped
//! twice), and what the refused update asked for (both filter lists, one of them, neither).

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, ok};

/// The error SHAPE the not-found class names in the body of an answer.
const NOT_FOUND_SHAPE: &str = "ResourceNotFoundException";

/// The whole sentence that refusal states: fixed text, with no part of the request in it.
const NOT_ENABLED_MESSAGE: &str = "OTel enrichment is not enabled for this account";

/// The two modelled statuses the read of the enrichment reports
/// (`smithy_query(query="shape",shape="OTelEnrichmentStatus")`).
const RUNNING: &str = "Running";
const STOPPED: &str = "Stopped";

/// One filter selecting a namespace the pattern of its member admits.
fn filter(namespace: &str) -> Value {
    json!({"Namespace": namespace})
}

/// The status the account's enrichment reports.
async fn status(router: &smithy_server_runtime::MultiServiceRouter) -> String {
    let read = ok(router, "GetOTelEnrichment", &json!({})).await;
    read["Status"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn an_update_while_enrichment_is_not_enabled_is_refused() {
    // (case, the calls that put the account in that state)
    let rows: Vec<(&str, Vec<(&str, Value)>)> = vec![
        ("enrichment never started", vec![]),
        (
            "enrichment started and then stopped",
            vec![
                ("StartOTelEnrichment", json!({})),
                ("StopOTelEnrichment", json!({})),
            ],
        ),
        (
            "enrichment started, updated and then stopped",
            vec![
                (
                    "StartOTelEnrichment",
                    json!({"IncludeFilters": [filter("Svc/One")]}),
                ),
                (
                    "UpdateOTelEnrichment",
                    json!({"IncludeFilters": [filter("Svc/Two")]}),
                ),
                ("StopOTelEnrichment", json!({})),
            ],
        ),
        (
            "enrichment stopped twice",
            vec![
                ("StartOTelEnrichment", json!({})),
                ("StopOTelEnrichment", json!({})),
                ("StopOTelEnrichment", json!({})),
            ],
        ),
    ];

    // What the refused update asks for: the filters are not what decides the refusal, so each
    // row is asserted for every one of them.
    let asked: Vec<(&str, Value)> = vec![
        ("both filter lists", json!({
            "IncludeFilters": [filter("Svc/Include")],
            "ExcludeFilters": [filter("Svc/Exclude")],
        })),
        (
            "the include filters alone",
            json!({"IncludeFilters": [filter("Svc/Include")]}),
        ),
        ("neither list", json!({})),
    ];

    for (case, setup) in rows {
        for (asking, request) in &asked {
            let router = build_router();
            for (operation, body) in &setup {
                ok(&router, operation, body).await;
            }
            // The state the rule is about, as the service's own read reports it.
            let reported = status(&router).await;
            assert_eq!(
                reported, STOPPED,
                "case {case:?}: expected the account's enrichment to report {STOPPED:?}, got \
                 {reported:?}"
            );

            let (status_code, body) = call(&router, "UpdateOTelEnrichment", request).await;
            assert_eq!(
                (status_code, error_code(&body).as_str()),
                (404, NOT_FOUND_SHAPE),
                "case {case:?} asking for {asking:?}: expected HTTP 404 {NOT_FOUND_SHAPE}, got \
                 {status_code} with {body}"
            );
            let stated = error_message(&body);
            assert_eq!(
                stated, NOT_ENABLED_MESSAGE,
                "case {case:?} asking for {asking:?}: expected {NOT_ENABLED_MESSAGE:?}, got \
                 {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_same_update_is_served_while_enrichment_is_enabled() {
    // The positive control: the refused call differs from this one in the account's state
    // alone, and what the served update stored is readable back.
    let router = build_router();
    ok(&router, "StartOTelEnrichment", &json!({})).await;
    let reported = status(&router).await;
    assert_eq!(
        reported, RUNNING,
        "expected the started enrichment to report {RUNNING:?}, got {reported:?}"
    );

    let updated = ok(
        &router,
        "UpdateOTelEnrichment",
        &json!({"IncludeFilters": [filter("Svc/Served")]}),
    )
    .await;
    assert_eq!(
        updated["IncludeFilters"],
        json!([filter("Svc/Served")]),
        "expected the update to report the filters it stored, got {updated}"
    );
    let read = ok(&router, "GetOTelEnrichment", &json!({})).await;
    assert_eq!(
        read["IncludeFilters"],
        json!([filter("Svc/Served")]),
        "expected the read to report the stored filters, got {read}"
    );
}
