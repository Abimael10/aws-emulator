// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the CAPACITY a created endpoint reports for each of its
//! variants, wherever a response carries `ProductionVariantSummary`.
//!
//! RULE: every variant summary of a created endpoint reports `CurrentInstanceCount` — "the
//! number of instances associated with the variant" (`TaskCount`, `@range min 0`) — as the
//! number of instances the variant's configuration provisions, which is zero for a variant
//! that provisions none (one declaring a `ServerlessConfig` instead of an instance type and
//! count). The member is reported on EVERY summary a read serves, for the production and the
//! shadow fleet alike, and by every read path that carries the shape: `DescribeEndpoint` and
//! `Search`'s `SearchRecord$Endpoint`.
//!
//! The rule is asserted over a TABLE of configurations whose rows differ along what it
//! generalises over: the variant's form (serverless or instance-based), the instance count it
//! asks for, the number of variants, and whether the fleet is the production or the shadow
//! one. Each row is driven over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router assembles.
//!
//! Nothing here is weighed against how fast this host ran it: the deployment is waited for by
//! reading the endpoint again until it reports the status asked of it, under a deadline far
//! longer than the span this state gives a deployment.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{primary_container, str_field, Emulator};

/// How long an endpoint is polled for before a case fails, and how often it is looked at.
/// What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a
/// case the clock would have served.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// The model every configuration below hosts.
const MODEL: &str = "capacity-model";

/// An instance-based production variant: it provisions `instances` instances of an ML
/// instance type, so that is the capacity associated with it.
fn instance_variant(name: &str, instances: i64) -> Value {
    json!({
        "VariantName": name,
        "ModelName": MODEL,
        "InitialInstanceCount": instances,
        "InstanceType": "ml.m5.large",
    })
}

/// A serverless production variant: it names no instance type and no count, so it holds no
/// instances at all. `MemorySizeInMB` is `@range 1024..6144` and `MaxConcurrency`
/// `@range 1..200` (`smithy_query(query="shape",shape="ProductionVariantServerlessConfig")`).
fn serverless_variant(name: &str, memory_mb: i64, max_concurrency: i64) -> Value {
    json!({
        "VariantName": name,
        "ModelName": MODEL,
        "ServerlessConfig": {
            "MemorySizeInMB": memory_mb,
            "MaxConcurrency": max_concurrency,
        },
    })
}

/// The summary of the variant named `name` in a reported summary list.
fn summary_of<'reported>(case: &str, reported: &'reported Value, name: &str) -> &'reported Value {
    let listed = reported
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary list, got {reported}"));
    listed
        .iter()
        .find(|summary| summary.get("VariantName").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary for {name}, got {reported}"))
}

/// Assert that `reported` carries one summary per variant `asked` declares, each reporting
/// `CurrentInstanceCount` as the number of instances that variant provisions — its
/// `InitialInstanceCount`, and zero for a variant that provisions none.
fn assert_capacity_reported(case: &str, label: &str, asked: &[Value], reported: Option<&Value>) {
    let reported = reported.unwrap_or_else(|| {
        panic!("case {case:?}: expected {label} to be reported for {asked:?}, got nothing")
    });
    assert_eq!(
        reported.as_array().map(Vec::len),
        Some(asked.len()),
        "case {case:?}: expected {label} to carry one summary per variant, got {reported}"
    );
    for want in asked {
        let name = want
            .get("VariantName")
            .and_then(Value::as_str)
            .expect("the row declares a variant name");
        let got = summary_of(case, reported, name);
        let provisioned = want
            .get("InitialInstanceCount")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        assert_eq!(
            got.get("CurrentInstanceCount").and_then(Value::as_i64),
            Some(provisioned),
            "case {case:?}: expected {label}/{name} CurrentInstanceCount {provisioned}, got {got}"
        );
    }
}

/// Read `endpoint` until it reports `wanted`, answering that reading. The wait is on the
/// CONDITION, never on a span, so a slow host cannot fail a case the clock would serve.
async fn poll_until(emulator: &Emulator, case: &str, endpoint: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "EndpointName": endpoint });
    loop {
        let read = emulator.ok("DescribeEndpoint", &body).await;
        if str_field(&read, "EndpointStatus") == wanted {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint never reached {wanted}; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The `SearchRecord$Endpoint` of the endpoint named `endpoint`, from a `Search` over every
/// stored endpoint.
async fn searched_endpoint(emulator: &Emulator, case: &str, endpoint: &str) -> Value {
    let answered = emulator.ok("Search", &json!({ "Resource": "Endpoint" })).await;
    let records = answered
        .get("Results")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected Search Results, got {answered}"));
    records
        .iter()
        .filter_map(|record| record.get("Endpoint"))
        .find(|record| record.get("EndpointName").and_then(Value::as_str) == Some(endpoint))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {endpoint} in {answered}"))
}

/// RULE: every variant summary of a created endpoint reports `CurrentInstanceCount` as the
/// number of instances the variant provisions — zero for a variant that provisions none —
/// on the production and the shadow fleet alike, and on every read path that carries the
/// shape.
#[tokio::test]
async fn every_variant_summary_reports_the_instances_its_configuration_provisions() {
    // Rows differ along the variant's form, the count it asks for, the number of variants,
    // and which fleet carries them.
    let cases: Vec<(&str, Vec<Value>, Vec<Value>)> = vec![
        (
            "serverless-at-the-minimum-memory",
            vec![serverless_variant("AllTraffic", 1024, 1)],
            Vec::new(),
        ),
        (
            "serverless-at-the-maximum-memory-and-concurrency",
            vec![serverless_variant("AllTraffic", 6144, 200)],
            Vec::new(),
        ),
        (
            "one-instance",
            // `InitialInstanceCount` is `@range min 1`, so one instance is the smallest
            // capacity an instance-based variant can ask for.
            vec![instance_variant("AllTraffic", 1)],
            Vec::new(),
        ),
        (
            "several-instances",
            vec![instance_variant("AllTraffic", 3)],
            Vec::new(),
        ),
        (
            "both-forms-behind-one-endpoint",
            vec![
                serverless_variant("Serverless", 2048, 1),
                instance_variant("Provisioned", 2),
            ],
            Vec::new(),
        ),
        (
            "a-shadow-fleet-of-the-other-form",
            vec![instance_variant("AllTraffic", 1)],
            vec![serverless_variant("Shadow", 3072, 5)],
        ),
    ];

    for (case, production, shadow) in cases {
        let emulator = Emulator::new();
        emulator
            .ok(
                "CreateModel",
                &json!({ "ModelName": MODEL, "PrimaryContainer": primary_container("demo:1") }),
            )
            .await;

        let config = "capacity-config";
        let endpoint = "capacity-endpoint";
        let mut config_body = json!({
            "EndpointConfigName": config,
            "ProductionVariants": production.clone(),
        });
        if !shadow.is_empty() {
            config_body["ShadowProductionVariants"] = json!(shadow.clone());
        }
        emulator.ok("CreateEndpointConfig", &config_body).await;
        emulator
            .ok(
                "CreateEndpoint",
                &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
            )
            .await;

        let described = poll_until(&emulator, case, endpoint, "InService").await;
        assert_capacity_reported(
            case,
            "DescribeEndpoint ProductionVariants",
            &production,
            described.get("ProductionVariants"),
        );
        if shadow.is_empty() {
            assert!(
                described.get("ShadowProductionVariants").is_none(),
                "case {case:?}: expected no ShadowProductionVariants, got {described}"
            );
        } else {
            assert_capacity_reported(
                case,
                "DescribeEndpoint ShadowProductionVariants",
                &shadow,
                described.get("ShadowProductionVariants"),
            );
        }

        // The same stored capacity, through the other read path that carries the shape.
        let searched = searched_endpoint(&emulator, case, endpoint).await;
        assert_capacity_reported(
            case,
            "Search Endpoint ProductionVariants",
            &production,
            searched.get("ProductionVariants"),
        );
        if !shadow.is_empty() {
            assert_capacity_reported(
                case,
                "Search Endpoint ShadowProductionVariants",
                &shadow,
                searched.get("ShadowProductionVariants"),
            );
        }
    }
}

/// RULE (the same one, read twice): the capacity a variant summary reports is a property of
/// the endpoint's stored fleet, so repeated reads of a settled endpoint report the same
/// number — reading an endpoint neither provisions nor releases capacity.
#[tokio::test]
async fn repeated_reads_report_the_same_variant_capacity() {
    let emulator = Emulator::new();
    let case = "serverless-and-provisioned-read-twice";
    let production = vec![
        serverless_variant("Serverless", 2048, 1),
        instance_variant("Provisioned", 2),
    ];

    emulator
        .ok(
            "CreateModel",
            &json!({ "ModelName": MODEL, "PrimaryContainer": primary_container("demo:1") }),
        )
        .await;
    emulator
        .ok(
            "CreateEndpointConfig",
            &json!({
                "EndpointConfigName": "stable-config",
                "ProductionVariants": production.clone(),
            }),
        )
        .await;
    emulator
        .ok(
            "CreateEndpoint",
            &json!({
                "EndpointName": "stable-endpoint",
                "EndpointConfigName": "stable-config",
            }),
        )
        .await;

    let first = poll_until(&emulator, case, "stable-endpoint", "InService").await;
    assert_capacity_reported(
        case,
        "first read",
        &production,
        first.get("ProductionVariants"),
    );

    let second = emulator
        .ok(
            "DescribeEndpoint",
            &json!({ "EndpointName": "stable-endpoint" }),
        )
        .await;
    assert_capacity_reported(
        case,
        "second read",
        &production,
        second.get("ProductionVariants"),
    );
    assert_eq!(
        second.get("ProductionVariants"),
        first.get("ProductionVariants"),
        "case {case:?}: expected the second read to report the same fleet, got {second}"
    );
}
