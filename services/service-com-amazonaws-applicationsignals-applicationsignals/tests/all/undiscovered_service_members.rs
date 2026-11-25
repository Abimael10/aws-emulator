// SPDX-License-Identifier: CC0-1.0

//! What `GetService` reports for an identity nothing was observed for.
//!
//! The rule under test: Application Signals DISCOVERS a service from the telemetry an
//! instrumented application sends rather than creating one, so every member of the `Service`
//! object a read reports is a member it observed for the requested identity over the
//! requested window. A read of an identity it observed nothing for therefore carries no
//! `KeyAttributes` and no `MetricReferences` at all — not the identity the request named back
//! (an echo of the request is no evidence that anything discovered the service) and not an
//! empty metric list (which would state that the service was discovered and collects no
//! metrics).
//!
//! Every test drives the real `restJson1` surface through the router that production mounts,
//! and the rule is asserted over a TABLE: the rows differ along WHY there is nothing to report
//! (an identity no record monitors, an identity of another entity type, a window that ends
//! before the evidence existed, a window that has not begun) and are set beside the
//! discovered rows, where the same two members are present and carry what the store holds.
//!
//! The entity types and the attributes each makes applicable come from
//! `smithy_query(query="doc",shape="GetServiceInput",member="KeyAttributes")`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::slo_lifecycle::{call, create, router};

/// Seconds since the epoch, so a window can be placed around the instant a stored record is
/// created at.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as i64
}

/// A service identity, the three attributes a `Service` entity is named by.
fn service_entity(name: &str) -> Value {
    json!({ "Type": "Service", "Name": name, "Environment": "prod" })
}

/// A resource identity, the attributes a `Resource` entity is named by.
fn resource_entity(identifier: &str) -> Value {
    json!({
        "Type": "AWS::Resource",
        "ResourceType": "AWS::DynamoDB::Table",
        "Identifier": identifier
    })
}

/// Create one SLO whose indicator monitors `key_attributes`, which is this emulator's only
/// evidence that Application Signals discovered the entity.
async fn monitor(router: &MultiServiceRouter, case: &str, name: &str, key_attributes: &Value) {
    let (status, answer) = create(
        router,
        json!({
            "Name": name,
            "SliConfig": {
                "SliMetricConfig": { "KeyAttributes": key_attributes, "MetricType": "LATENCY" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the SLO create to answer 200, got {status} with {answer}"
    );
}

/// One `GetService` read over the window `start..end`.
async fn get_service(
    router: &MultiServiceRouter,
    key_attributes: &Value,
    start: i64,
    end: i64,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/service?StartTime={start}&EndTime={end}"),
        Some(json!({ "KeyAttributes": key_attributes })),
    )
    .await
}

/// One row: the identity a record monitors, the identity the read names, the window it asks
/// about and whether the store is evidence of that identity over it.
struct Row {
    label: &'static str,
    monitored: Option<Value>,
    request: Value,
    /// The window, in seconds from the instant the test runs at.
    window: (i64, i64),
    /// The `KeyAttributes` the answer must carry, or `None` when the answer must carry none.
    reported: Option<Value>,
}

fn rows() -> Vec<Row> {
    let covering = (-3_600, 3_600);
    vec![
        Row {
            label: "an identity no record monitors",
            monitored: None,
            request: service_entity("ghost"),
            window: covering,
            reported: None,
        },
        Row {
            label: "a record monitors another service entirely",
            monitored: Some(service_entity("checkout")),
            request: service_entity("ghost"),
            window: covering,
            reported: None,
        },
        Row {
            label: "a record monitors a service, the read names a resource",
            monitored: Some(service_entity("checkout")),
            request: resource_entity("orders-table"),
            window: covering,
            reported: None,
        },
        Row {
            label: "a window that ends before the evidence existed",
            monitored: Some(service_entity("checkout")),
            request: service_entity("checkout"),
            window: (-7_200, -3_600),
            reported: None,
        },
        Row {
            label: "a window that has not begun",
            monitored: Some(service_entity("checkout")),
            request: service_entity("checkout"),
            window: (3_600, 7_200),
            reported: None,
        },
        Row {
            label: "a monitored service over a covering window",
            monitored: Some(service_entity("checkout")),
            request: service_entity("checkout"),
            window: covering,
            reported: Some(service_entity("checkout")),
        },
        Row {
            label: "a monitored resource named by a subset of its identity",
            monitored: Some(json!({
                "Type": "AWS::Resource",
                "ResourceType": "AWS::DynamoDB::Table",
                "Identifier": "orders-table",
                "Environment": "prod"
            })),
            request: resource_entity("orders-table"),
            window: covering,
            reported: Some(json!({
                "Type": "AWS::Resource",
                "ResourceType": "AWS::DynamoDB::Table",
                "Identifier": "orders-table",
                "Environment": "prod"
            })),
        },
    ]
}

/// The members of the reported `Service` object, as the names a client testing for member
/// PRESENCE sees.
fn members(service: &Value) -> BTreeSet<String> {
    service
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

/// A read of an identity nothing was observed for carries no `KeyAttributes` and no
/// `MetricReferences`; a read of a discovered one carries both.
#[tokio::test]
async fn an_undiscovered_service_carries_no_observed_member() {
    let now = now_seconds();

    for row in rows() {
        let router = router();
        if let Some(monitored) = row.monitored.as_ref() {
            monitor(&router, row.label, "checkout-latency", monitored).await;
        }

        let (status, body) = get_service(
            &router,
            &row.request,
            now + row.window.0,
            now + row.window.1,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status} with {body}",
            row.label
        );
        // The `Service` object itself is always answered: it is the read's subject, and an
        // undiscovered one is the object with no member in it.
        assert!(
            body["Service"].is_object(),
            "case {:?}: expected a Service object, got {body}",
            row.label
        );

        match row.reported.as_ref() {
            None => {
                assert_eq!(
                    members(&body["Service"]),
                    BTreeSet::new(),
                    "case {:?}: expected a Service carrying no observed member, got {}",
                    row.label,
                    body["Service"]
                );
            }
            Some(reported) => {
                assert_eq!(
                    &body["Service"]["KeyAttributes"],
                    reported,
                    "case {:?}: expected the stored identity, got {}",
                    row.label,
                    body["Service"]["KeyAttributes"]
                );
                let references = body["Service"]["MetricReferences"].as_array();
                assert!(
                    references.is_some_and(|references| !references.is_empty()),
                    "case {:?}: expected the metric references of a discovered service, got {}",
                    row.label,
                    body["Service"]["MetricReferences"]
                );
            }
        }
    }
}

/// The same answer however often it is asked for: a read of an undiscovered identity does not
/// register the identity it was asked about, so a later read of it still reports nothing — and
/// a read of the discovered identity beside it is unaffected.
#[tokio::test]
async fn reading_an_undiscovered_service_discovers_nothing() {
    let case = "a read registers no entity";
    let now = now_seconds();
    let router = router();
    monitor(&router, case, "checkout-latency", &service_entity("checkout")).await;

    let (first_status, first) =
        get_service(&router, &service_entity("ghost"), now - 3_600, now + 3_600).await;
    let (second_status, second) =
        get_service(&router, &service_entity("ghost"), now - 3_600, now + 3_600).await;
    assert_eq!(
        (first_status, members(&first["Service"])),
        (second_status, members(&second["Service"])),
        "case {case:?}: two reads of one identity answered differently: {first} then {second}"
    );
    assert_eq!(
        members(&first["Service"]),
        BTreeSet::new(),
        "case {case:?}: expected a Service carrying no observed member, got {first}"
    );

    let (status, discovered) =
        get_service(&router, &service_entity("checkout"), now - 3_600, now + 3_600).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for the discovered service, got {status} with {discovered}"
    );
    assert_eq!(
        &discovered["Service"]["KeyAttributes"],
        &service_entity("checkout"),
        "case {case:?}: expected the discovered identity, got {}",
        discovered["Service"]["KeyAttributes"]
    );
}
