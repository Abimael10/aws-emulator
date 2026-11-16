// SPDX-License-Identifier: CC0-1.0

//! The ports this service does not accept, end to end over the wire.
//!
//! `port` carries a constraint the model DOCUMENTS without declaring it — "Constraints:
//! The value can't be 2375-2376, 7788-7799, 8090, or 51678-51680"
//! (`smithy_query(query="doc", shape="CreateDbClusterInput", member="port")`, the same
//! sentence on `CreateDbInstanceInput$port` and `UpdateDbInstanceInput$port`) — so the
//! `@range` 1024..65535 the shape declares admits values the service itself does not.
//!
//! The RULE asserted here: a request asking InfluxDB to accept connections on one of
//! those values is refused with the modelled `ValidationException` (HTTP 400) and leaves
//! the store exactly as it was, while every other port the declared range admits is
//! served. The rows span the class rather than replaying one call: every documented span,
//! at both of its bounds and inside it, against every operation that takes a port, and
//! the accepted rows take the value one past each bound beside the two engine defaults
//! and the bounds of the modelled `Port` range.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_timestreaminfluxdb_amazontimestreaminfluxdb as influxdb;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_lifecycle::{call, ok, router_and_engine};

/// The fixed tail of the refusal's sentence: the documented constraint, worded as the
/// documentation words it. Written out here rather than read from the crate, so the words
/// a caller is shown are pinned by this suite and not by the code it tests.
const REFUSAL_TAIL: &str =
    "is not available. The value can't be 2375-2376, 7788-7799, 8090, or 51678-51680";

/// The bounds of the modelled `Port` range (`smithy_query(query="shape", shape="Port")` —
/// range 1024..65535) and the two defaults the `port` member documents (8086 for InfluxDB
/// v2, 8181 for v3): ports the service accepts, none of them in a documented span.
const ALWAYS_AVAILABLE_PORTS: &[i32] = &[1024, 8086, 8181, 65535];

/// Every port this suite expects the service to refuse: both bounds of each documented
/// span and, for a span wider than one value, a port inside it.
fn unavailable_ports() -> Vec<i32> {
    let mut ports: Vec<i32> = Vec::new();
    for (first, last) in influxdb::state::UNAVAILABLE_PORT_SPANS.iter().copied() {
        ports.push(first);
        ports.push(last);
        let inside = first + (last - first) / 2;
        if inside != first && inside != last {
            ports.push(inside);
        }
    }
    ports.dedup();
    ports
}

/// Every port this suite expects the service to accept: the value one PAST each bound of
/// every documented span, plus [`ALWAYS_AVAILABLE_PORTS`]. A neighbour that falls in
/// another span (the spans are listed in order, but nothing says they cannot touch) is
/// dropped, so this list stays the complement of the one above.
fn available_ports() -> Vec<i32> {
    let spans = influxdb::state::UNAVAILABLE_PORT_SPANS;
    let mut ports: Vec<i32> = Vec::new();
    for (first, last) in spans.iter().copied() {
        ports.push(first - 1);
        ports.push(last + 1);
    }
    ports.extend_from_slice(ALWAYS_AVAILABLE_PORTS);
    ports.retain(|port| {
        (1024..=65535).contains(port)
            && !spans
                .iter()
                .any(|(first, last)| (*first..=*last).contains(port))
    });
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// A router whose account holds the AWS Marketplace subscription every cluster deployment
/// type is gated behind, so a cluster create fails only on what this suite is about.
fn router_with_subscription() -> MultiServiceRouter {
    let (router, engine) = router_and_engine();
    influxdb::state::subscribe_account_to_marketplace_offering(
        &*engine,
        influxdb::state::READ_REPLICAS_OFFERING,
    )
    .expect("the shared engine holds this service's store");
    router
}

/// The minimum a `CreateDbInstance` request must carry, per the model's `@required`
/// members, asking for `port`.
fn instance_create(name: &str, port: i32) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "allocatedStorage": 20,
        "vpcSubnetIds": ["subnet-aaa"],
        "vpcSecurityGroupIds": ["sg-aaa"],
        "port": port,
    })
}

/// The minimum a `CreateDbCluster` request must carry, per the model's `@required`
/// members, asking for `port`.
fn cluster_create(name: &str, port: i32) -> Value {
    json!({
        "name": name,
        "password": "supersecret1",
        "dbInstanceType": "db.influx.medium",
        "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
        "vpcSecurityGroupIds": ["sg-aaa"],
        "port": port,
    })
}

/// A resource name carrying `port`, inside the modelled `DbInstanceName` /
/// `DbClusterName` length (3..40) and pattern.
fn named_for(prefix: &str, port: i32) -> String {
    format!("{prefix}-p{port}")
}

/// Assert `answer` is the refusal this service states for an unavailable `port`.
fn assert_unavailable_port_refusal(case: &str, port: i32, answer: (u16, Value)) {
    let (status, body) = answer;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert!(
        body["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("ValidationException"),
        "case {case:?}: expected a ValidationException, got {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(format!("The port [{port}] {REFUSAL_TAIL}").as_str()),
        "case {case:?}: unexpected message {:?}",
        body["message"]
    );
    assert_eq!(
        body["reason"].as_str(),
        Some("FIELD_VALIDATION_FAILED"),
        "case {case:?}: a field of the request failed, got reason {:?}",
        body["reason"]
    );
}

#[tokio::test]
async fn creating_a_db_instance_on_an_unavailable_port_is_refused_and_stores_nothing() {
    let router = router_with_subscription();
    for port in unavailable_ports() {
        let case = format!("CreateDbInstance on port {port}");
        assert_unavailable_port_refusal(
            &case,
            port,
            call(
                &router,
                "CreateDbInstance",
                instance_create(&named_for("inst", port), port),
            )
            .await,
        );
    }

    let listed = ok(
        "the refusals stored nothing",
        call(&router, "ListDbInstances", json!({})).await,
    );
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(0),
        "case \"the refusals stored nothing\": a refused create must persist no instance, \
         got {listed}"
    );
}

#[tokio::test]
async fn creating_a_db_cluster_on_an_unavailable_port_is_refused_and_stores_nothing() {
    let router = router_with_subscription();
    for port in unavailable_ports() {
        let case = format!("CreateDbCluster on port {port}");
        assert_unavailable_port_refusal(
            &case,
            port,
            call(
                &router,
                "CreateDbCluster",
                cluster_create(&named_for("clus", port), port),
            )
            .await,
        );
    }

    // Neither the cluster nor the DB instances its nodes would have been.
    for (operation, resource) in [
        ("ListDbClusters", "cluster"),
        ("ListDbInstances", "node"),
    ] {
        let listed = ok(
            "the refusals stored nothing",
            call(&router, operation, json!({})).await,
        );
        assert_eq!(
            listed["items"].as_array().map(Vec::len),
            Some(0),
            "case \"the refusals stored nothing\": a refused create must persist no \
             {resource}, got {listed}"
        );
    }
}

#[tokio::test]
async fn updating_a_db_instance_onto_an_unavailable_port_is_refused_and_changes_nothing() {
    let router = router_with_subscription();
    let case = "an update onto an unavailable port";
    let created = ok(
        case,
        call(&router, "CreateDbInstance", instance_create("movable", 8086)).await,
    );
    let id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string `id`, got {created}"))
        .to_string();

    // Wait for the CONDITION the record settles into, so the comparison below is between
    // two reads of a record the clock owes nothing: a generous deadline, polled.
    let read = || call(&router, "GetDbInstance", json!({ "identifier": &id }));
    let started = Instant::now();
    let deadline = Duration::from_secs(30);
    let mut before = ok(case, read().await);
    while before["status"].as_str() != Some("AVAILABLE") && started.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(2)).await;
        before = ok(case, read().await);
    }
    assert_eq!(
        before["status"].as_str(),
        Some("AVAILABLE"),
        "case {case:?}: the instance never settled within {deadline:?}, got {before}"
    );

    for port in unavailable_ports() {
        let case = format!("UpdateDbInstance onto port {port}");
        assert_unavailable_port_refusal(
            &case,
            port,
            call(
                &router,
                "UpdateDbInstance",
                json!({ "identifier": &id, "port": port }),
            )
            .await,
        );
        let after = ok(&case, read().await);
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must leave the record as it was, got {after}"
        );
    }
}

#[tokio::test]
async fn a_port_the_documented_constraint_admits_is_served() {
    let router = router_with_subscription();
    for port in available_ports() {
        let case = format!("port {port}");

        // A cluster reports the port its create asked for.
        let created = ok(
            &case,
            call(
                &router,
                "CreateDbCluster",
                cluster_create(&named_for("clus", port), port),
            )
            .await,
        );
        let cluster_id = created["dbClusterId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a `dbClusterId`, got {created}"))
            .to_string();
        let cluster = ok(
            &case,
            call(
                &router,
                "GetDbCluster",
                json!({ "dbClusterId": &cluster_id }),
            )
            .await,
        );
        assert_eq!(
            cluster["port"].as_i64(),
            Some(i64::from(port)),
            "case {case:?}: expected the cluster to report the port it was created on, \
             got {:?}",
            cluster["port"]
        );

        // A standalone DB instance is created on it too, and reads back.
        let created = ok(
            &case,
            call(
                &router,
                "CreateDbInstance",
                instance_create(&named_for("inst", port), port),
            )
            .await,
        );
        let instance_id = created["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a string `id`, got {created}"))
            .to_string();
        let instance = ok(
            &case,
            call(
                &router,
                "GetDbInstance",
                json!({ "identifier": &instance_id }),
            )
            .await,
        );
        assert_eq!(
            instance["id"].as_str(),
            Some(instance_id.as_str()),
            "case {case:?}: the created instance must read back, got {instance}"
        );

        // And the instance accepts an update onto that port.
        let updated = ok(
            &case,
            call(
                &router,
                "UpdateDbInstance",
                json!({ "identifier": &instance_id, "port": port }),
            )
            .await,
        );
        assert_eq!(
            updated["id"].as_str(),
            Some(instance_id.as_str()),
            "case {case:?}: expected the updated instance, got {updated}"
        );
    }
}
