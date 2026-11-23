// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal an EBS volume whose per-volume PERFORMANCE figures
//! sit outside the range the service publishes for the pairing is answered with, on both
//! paths that provision a domain's storage: `CreateDomain` and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the published ranges are read through `DescribeInstanceTypeLimits` itself; this
//! suite adds no second copy of either, and [`super::ebs_volume_size_limit`] holds the
//! same rule for a volume's SIZE.
//!
//! The rule under test: `EBSOptions$Iops` is "the baseline input/output (I/O) performance
//! of EBS volumes attached to data nodes" and `EBSOptions$Throughput` "the throughput (in
//! MiB/s)" of the same volumes (`smithy_query(query="shape",shape="EBSOptions")`); the
//! service publishes a range for each per instance type and volume sub-type — the
//! `MinimumIops`/`MaximumIops` and `MinimumThroughput`/`MaximumThroughput` limits
//! `DescribeInstanceTypeLimits` reports — so a figure outside it asks for performance the
//! service does not provision for that volume. Like a volume SIZE outside its published
//! range that is a published LIMIT of the pairing rather than a malformed request, so it
//! is refused with the modelled `LimitExceededException` (`errorKind` client, `httpError`
//! 409, from `smithy_query(query="shape",shape="LimitExceededException")`) and the
//! sentence names the two published bounds, the instance type the volume attaches to and
//! the engine release the domain runs.
//!
//! So the assertions span SETS of inputs: a table of pairings — the burstable class,
//! whose own attachment narrows both ceilings, and current-generation sizes that reach
//! the sub-type's own — each probed below the published floor and above the published
//! ceiling for each figure, with every row's bounds read from the service's own catalog;
//! the bounds themselves, which are accepted and read back; and the update path, where
//! the merged record is what the bounds are read for.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};
use super::instance_type_limits::{describe_limits, limits_by_role};

/// The pairings the table spans: one row per instance class the published ceilings differ
/// for, across releases of both engine lines — the burstable class, whose attachment
/// narrows what the volume alone would provision, and two current-generation sizes that
/// reach the sub-type's own ceilings.
const ROWS: [(&str, &str); 3] = [
    ("OpenSearch_2.11", "t3.small.search"),
    ("OpenSearch_2.19", "m5.large.search"),
    ("Elasticsearch_7.10", "r5.4xlarge.search"),
];

/// The volume sub-type both figures are provisioned per volume for: `Throughput` is
/// "applicable only for the `gp3` volume type" and `Iops` "only for the `gp3` and
/// provisioned IOPS EBS volume types", so `gp3` is the one sub-type that publishes a
/// range for each.
const SUB_TYPE: &str = "gp3";

/// Each figure with the limit names its published range is reported under.
const FIGURES: [(&str, &str, &str); 2] = [
    ("Iops", "MinimumIops", "MaximumIops"),
    ("Throughput", "MinimumThroughput", "MaximumThroughput"),
];

/// One named limit of the `gp3` storage type the data role publishes for `instance_type`,
/// read through `DescribeInstanceTypeLimits` — so each row's bounds are the service's own
/// published content rather than a second copy of its table.
async fn published_limit(
    emulator: &Emulator,
    engine_version: &str,
    instance_type: &str,
    limit: &str,
) -> i64 {
    let case = format!("{engine_version} {instance_type} {SUB_TYPE} {limit}");
    let map = limits_by_role(
        &describe_limits(emulator, engine_version, instance_type, "").await,
        &case,
    );
    let storage = map["data"]["StorageTypes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let entry = storage
        .iter()
        .find(|entry| entry["StorageSubTypeName"] == json!(SUB_TYPE))
        .unwrap_or_else(|| {
            panic!("case {case}: expected a {SUB_TYPE} storage type, got {storage:?}")
        });
    entry["StorageTypeLimits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|published| published["LimitName"] == json!(limit))
        .and_then(|published| published["LimitValues"][0].as_str()?.parse::<i64>().ok())
        .unwrap_or_else(|| panic!("case {case}: expected a {limit} limit, got {entry:?}"))
}

/// This suite's OWN statement of the refusal text: the fixed sentence, whose variable
/// parts are the figure, the two bounds published for the pairing, the instance type the
/// volume attaches to and the engine release the domain runs. Restated here rather than
/// imported from the service, so changing it in the service fails this suite instead of
/// being asserted against itself.
fn expected_message(
    figure: &str,
    minimum: i64,
    maximum: i64,
    instance_type: &str,
    engine_version: &str,
) -> String {
    format!(
        "{figure} must be between {minimum} and {maximum} for {instance_type} instance \
         type and version {engine_version}"
    )
}

/// Assert one answer IS the service-limit refusal the rule prescribes.
fn assert_refused(answer: &Answer, expected: &str, case: &str) {
    assert_eq!(
        answer.status, 409,
        "case {case}: expected HTTP 409, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "LimitExceededException",
        "case {case}: expected the modelled service-limit refusal, got {:?}",
        answer.body
    );
    let actual = answer.body["message"].as_str().unwrap_or_default();
    assert_eq!(
        actual, expected,
        "case {case}: expected the published bounds and the pairing to be named, got \
         {actual:?}"
    );
}

/// An `EBSOptions` carrying one figure, with a volume size every pairing admits.
fn ebs_options(figure: &str, value: i64) -> Value {
    let mut options = json!({ "EBSEnabled": true, "VolumeType": SUB_TYPE, "VolumeSize": 20 });
    options[figure] = json!(value);
    options
}

/// A domain name of the modelled shape (`^[a-z][a-z0-9\-]+$`, 3..28) for one row.
fn domain_name(row: usize, figure: &str, end: &str) -> String {
    format!("ebs-perf-{row}-{}-{end}", figure.to_lowercase())
}

#[tokio::test]
async fn a_figure_outside_the_published_range_is_a_service_limit_refusal_on_create() {
    let emulator = Emulator::new();

    for (row, (engine_version, instance_type)) in ROWS.into_iter().enumerate() {
        for (figure, floor_name, ceiling_name) in FIGURES {
            let minimum =
                published_limit(&emulator, engine_version, instance_type, floor_name).await;
            let maximum =
                published_limit(&emulator, engine_version, instance_type, ceiling_name).await;
            let expected =
                expected_message(figure, minimum, maximum, instance_type, engine_version);

            for (end, value) in [("below", minimum - 1), ("above", maximum + 1)] {
                let name = domain_name(row, figure, end);
                let case = format!(
                    "CreateDomain({name}, {instance_type}, {figure}={value}, {engine_version})"
                );
                let answer = emulator
                    .create(json!({
                        "DomainName": name,
                        "EngineVersion": engine_version,
                        "ClusterConfig": { "InstanceType": instance_type, "InstanceCount": 1 },
                        "EBSOptions": ebs_options(figure, value),
                    }))
                    .await;
                assert_refused(&answer, &expected, &case);

                // A refused create mints nothing: the name is still free of any record.
                let read = emulator.describe(&name).await;
                assert_eq!(
                    read.status, RECORDED_NOT_FOUND_STATUS,
                    "case {case}: a refused create stores no domain, got {} {:?}",
                    read.status, read.body
                );
            }
        }
    }
}

#[tokio::test]
async fn the_published_bounds_themselves_are_accepted_and_read_back() {
    let emulator = Emulator::new();

    for (row, (engine_version, instance_type)) in ROWS.into_iter().enumerate() {
        let iops_floor =
            published_limit(&emulator, engine_version, instance_type, "MinimumIops").await;
        let iops_ceiling =
            published_limit(&emulator, engine_version, instance_type, "MaximumIops").await;
        let throughput_floor =
            published_limit(&emulator, engine_version, instance_type, "MinimumThroughput").await;
        let throughput_ceiling =
            published_limit(&emulator, engine_version, instance_type, "MaximumThroughput").await;

        for (end, iops, throughput) in [
            ("floor", iops_floor, throughput_floor),
            ("ceiling", iops_ceiling, throughput_ceiling),
        ] {
            let name = format!("ebs-bound-{row}-{end}");
            let case = format!(
                "CreateDomain({name}, {instance_type}, Iops={iops}, Throughput={throughput})"
            );
            let answer = emulator
                .create(json!({
                    "DomainName": name,
                    "EngineVersion": engine_version,
                    "ClusterConfig": { "InstanceType": instance_type, "InstanceCount": 1 },
                    "EBSOptions": {
                        "EBSEnabled": true,
                        "VolumeType": SUB_TYPE,
                        "VolumeSize": 20,
                        "Iops": iops,
                        "Throughput": throughput,
                    },
                }))
                .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the published bound to be accepted, got {} {:?}",
                answer.status, answer.body
            );

            // Round trip: both figures read back from the record.
            let read = emulator.describe(&name).await;
            let stored = &read.body["DomainStatus"]["EBSOptions"];
            assert_eq!(
                (&stored["Iops"], &stored["Throughput"]),
                (&json!(iops), &json!(throughput)),
                "case {case}: expected the stored volume to carry both figures, got {stored:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_is_judged_on_the_merged_pairing_and_keeps_the_stored_volume() {
    let emulator = Emulator::new();
    let engine_version = "OpenSearch_2.19";

    // The domain is created on an instance class whose attachment reaches the sub-type's
    // own ceilings, with a figure that class provisions; the change then moves the
    // instance type to the BURSTABLE class, whose attachment narrows the ceiling below
    // that stored figure — so the update is judged on the merged pairing, which is the
    // stored figure against the requested instance type.
    let name = "ebs-perf-merged";
    let narrow = "t3.small.search";
    let wide = "m5.large.search";
    let ceiling = published_limit(&emulator, engine_version, narrow, "MaximumIops").await;
    let stored_iops = published_limit(&emulator, engine_version, wide, "MaximumIops").await;
    assert!(
        stored_iops > ceiling,
        "case the merged pairing: expected the burstable class to publish the narrower \
         ceiling, got {ceiling} and {stored_iops}"
    );
    let floor = published_limit(&emulator, engine_version, narrow, "MinimumIops").await;

    let created = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": engine_version,
            "ClusterConfig": { "InstanceType": wide, "InstanceCount": 1 },
            "EBSOptions": {
                "EBSEnabled": true,
                "VolumeType": SUB_TYPE,
                "VolumeSize": 20,
                "Iops": stored_iops,
            },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({name}): expected 200, got {} {:?}",
        created.status, created.body
    );
    // The service applies one change of a domain at a time, so the change the create
    // started has to have landed before an update can start another: the case waits for
    // THAT condition rather than for a fixed stretch of time.
    settled_domain(&emulator, name).await;

    let case = format!("UpdateDomainConfig({name}, {narrow})");
    let answer = emulator
        .update_config(
            name,
            json!({ "ClusterConfig": { "InstanceType": narrow, "InstanceCount": 1 } }),
        )
        .await;
    assert_refused(
        &answer,
        &expected_message("Iops", floor, ceiling, narrow, engine_version),
        &case,
    );

    let stored = emulator.describe_config(name).await;
    assert_eq!(
        stored.body["DomainConfig"]["ClusterConfig"]["Options"]["InstanceType"],
        json!(wide),
        "case {case}: a refused update leaves the stored cluster alone, got {:?}",
        stored.body["DomainConfig"]["ClusterConfig"]
    );
    assert_eq!(
        stored.body["DomainConfig"]["EBSOptions"]["Options"]["Iops"],
        json!(stored_iops),
        "case {case}: a refused update leaves the stored volume alone, got {:?}",
        stored.body["DomainConfig"]["EBSOptions"]
    );
}
