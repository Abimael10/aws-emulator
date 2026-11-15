// SPDX-License-Identifier: CC0-1.0

//! `GetEBSVolumeRecommendations` over the account's OWN Amazon EBS volumes, end to end.
//!
//! Compute Optimizer "generates recommendations for Amazon EBS volumes that meet a specific
//! set of requirements": a volume is one the account OWNS, and a finding is computed from the
//! utilization accumulated for it. So the rule this suite asserts is that a recommendation
//! exists for exactly the volumes the Amazon EC2 emulator holds for this account, in this
//! Region, for which utilization has been published — and that every member of one is computed
//! from that volume's own stored configuration and those observations. An account that owns no
//! volume reads the empty list, and a volume ARN that resolves to no volume of the account
//! selects nothing.
//!
//! Compute Optimizer, Amazon EC2 and CloudWatch are mounted on ONE engine, exactly as
//! `core/router` mounts them, and every call goes through the front door a client uses: the
//! Compute Optimizer calls as awsJson `POST /` with `X-Amz-Target`, the Amazon EC2 calls as
//! the `ec2Query` form posts an SDK sends. Each volume is read back through Amazon EC2's own
//! `DescribeVolumes` before it is analysed, so what the analysis reads is a resource the
//! owning service itself reports.
//!
//! The `AWS/EBS` utilization is seeded into the metric store the way Amazon EBS writes it: a
//! client cannot publish into the AWS-reserved namespace space, so a suite that needs observed
//! utilization records it there and then reads the recommendation computed from it over the
//! wire, exactly as the Amazon ECS and Amazon RDS suites of this crate do.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_ec2_in, call_in_region, element, emulator_with_volume_inventory, make_analysis_due,
    metrics, opt_in, seed_cloudwatch_series, BYTES_PER_MIB, REGION,
};

/// The CloudWatch namespace Amazon EBS publishes a volume's utilization in, and the dimension
/// that identifies one volume's series in it (the Amazon EBS metrics reference).
const EBS_NAMESPACE: &str = "AWS/EBS";
/// See [`EBS_NAMESPACE`].
const VOLUME_DIMENSION: &str = "VolumeId";

/// The period one published datapoint covers, in seconds: Amazon EBS publishes its ops and
/// bytes series as COUNTS over the datapoint's period, so a per-second rate is published as
/// that rate times this span.
const PERIOD_SECS: f64 = 60.0;

/// The Availability Zone a volume is created in, per Region — a volume belongs to the Region
/// its zone is in.
fn zone_in(region: &str) -> String {
    format!("{region}a")
}

/// Whether two figures agree to the hundredth every reported figure is rounded to.
fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.01
}

/// Create one Amazon EBS volume over Amazon EC2's own wire, answering the volume id EC2
/// minted, then read it back through `DescribeVolumes` — which proves the owning service holds
/// the volume the analysis is about.
async fn create_volume(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    volume_type: &str,
    size: i32,
    iops: Option<i32>,
    throughput: Option<i32>,
) -> String {
    let mut params = vec![
        ("AvailabilityZone", zone_in(region)),
        ("VolumeType", volume_type.to_string()),
        ("Size", size.to_string()),
    ];
    if let Some(iops) = iops {
        params.push(("Iops", iops.to_string()));
    }
    if let Some(throughput) = throughput {
        params.push(("Throughput", throughput.to_string()));
    }
    let (status, body) = call_ec2_in(router, region, "CreateVolume", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the volume must succeed, got {status}: {body}"
    );
    let volume_id = element(&body, "volumeId");
    assert!(
        volume_id.starts_with("vol-"),
        "case {case:?}: CreateVolume must report the volume it minted, got {body}"
    );

    let (status, described) = call_ec2_in(
        router,
        region,
        "DescribeVolumes",
        &[("VolumeId.1", volume_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: describing {volume_id} must succeed, got {status}: {described}"
    );
    assert_eq!(
        element(&described, "volumeId"),
        volume_id,
        "case {case:?}: Amazon EC2 must report the volume it created: {described}"
    );
    volume_id
}

/// The ARN Compute Optimizer reports a volume of `region` under.
fn volume_arn(region: &str, volume_id: &str) -> String {
    format!(
        "arn:aws:ec2:{region}:{}:volume/{volume_id}",
        ::engine::emulator_account_id()
    )
}

/// Publish one interval of demand on a volume, `days_ago` days before now, as the four
/// `AWS/EBS` series Amazon EBS publishes for a volume.
fn publish_demand(
    engine: &Arc<engine::InMemoryEngine>,
    volume_id: &str,
    days_ago: f64,
    read_ops: f64,
    write_ops: f64,
    read_mib: f64,
    write_mib: f64,
) {
    let at = ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state::now_secs()
        - days_ago * 86_400.0;
    for (metric_name, unit, rate) in [
        ("VolumeReadOps", "Count", read_ops),
        ("VolumeWriteOps", "Count", write_ops),
        ("VolumeReadBytes", "Bytes", read_mib * BYTES_PER_MIB),
        ("VolumeWriteBytes", "Bytes", write_mib * BYTES_PER_MIB),
    ] {
        seed_cloudwatch_series(
            engine,
            EBS_NAMESPACE,
            metric_name,
            &[(VOLUME_DIMENSION, volume_id)],
            Some(unit),
            at,
            &[rate * PERIOD_SECS],
        );
    }
}

/// Opt the account in and make the analysis its enrollment started DUE — the gate every
/// recommendation reader of this service applies, reached by moving the stored enrollment
/// instant rather than by waiting for the clock.
async fn analysed(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>) {
    opt_in(router).await;
    make_analysis_due(engine);
}

/// Read the volume recommendations at the endpoint of `region`, asserting the call was served.
async fn recommendations(router: &MultiServiceRouter, region: &str, request: Value) -> Value {
    let (status, body) =
        call_in_region(router, region, "GetEBSVolumeRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "reading {request} at {region}: expected 200, got {status}: {body}"
    );
    body
}

/// Every reported recommendation by the `volumeArn` it names.
fn reported_by_arn(body: &Value) -> BTreeMap<String, Value> {
    body["volumeRecommendations"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of volumeRecommendations, got {body}"))
        .iter()
        .map(|recommendation| {
            (
                recommendation["volumeArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                recommendation.clone(),
            )
        })
        .collect()
}

/// A number the wire carries, or `0` for one the protocol omitted because it equals the
/// member's modelled default.
fn number(value: &Value, member: &str) -> f64 {
    value[member].as_f64().unwrap_or(0.0)
}

/// The volumes a recommendation reader reports are the volumes the account OWNS: none before
/// it creates one, that one once Amazon EC2 holds it and utilization has been published for
/// it, and nothing for an ARN no volume of the account answers to.
#[tokio::test]
async fn the_volumes_analysed_are_the_volumes_amazon_ec2_holds() {
    let (router, engine) = emulator_with_volume_inventory();
    analysed(&router, &engine).await;

    // An enrolled account that owns no volume has no volume recommendation — not a failure,
    // and not a volume resident in this emulator.
    let empty = recommendations(&router, REGION, json!({})).await;
    assert_eq!(
        reported_by_arn(&empty).keys().cloned().collect::<Vec<_>>(),
        Vec::<String>::new(),
        "an account that owns no Amazon EBS volume reads no volume recommendation: {empty}"
    );
    assert_eq!(
        empty["errors"],
        json!([]),
        "owning no volume is no error of the request: {empty}"
    );

    // One over-provisioned gp3 volume of the account's own, created over Amazon EC2's wire.
    let volume_id = create_volume(
        &router,
        REGION,
        "an over-provisioned gp3 volume",
        "gp3",
        100,
        Some(8_000),
        Some(500),
    )
    .await;
    let arn = volume_arn(REGION, &volume_id);

    // Amazon EBS publishes a volume's utilization while it is attached to a running instance,
    // so a volume nothing has published for is one the analysis has no input for: it is absent
    // from the answer rather than carrying an invented finding, and absent from `errors` too,
    // which reports the volumes an analysis REJECTED.
    let unobserved = recommendations(&router, REGION, json!({"volumeArns": [arn.clone()]})).await;
    assert_eq!(
        reported_by_arn(&unobserved).keys().cloned().collect::<Vec<_>>(),
        Vec::<String>::new(),
        "a volume no utilization was published for has no recommendation yet: {unobserved}"
    );
    assert_eq!(
        unobserved["errors"],
        json!([]),
        "a volume the analysis has not reached is no error of the request: {unobserved}"
    );

    // The demand observed on it: 2,000 IOPS and 80 MiB/s at its peak, well inside what it is
    // provisioned for, plus a quieter interval the peak has to win over.
    publish_demand(&engine, &volume_id, 0.0, 1_200.0, 800.0, 50.0, 30.0);
    publish_demand(&engine, &volume_id, 2.0, 300.0, 200.0, 10.0, 5.0);

    let body = recommendations(&router, REGION, json!({})).await;
    let reported = reported_by_arn(&body);
    assert_eq!(
        reported.keys().cloned().collect::<Vec<_>>(),
        vec![arn.clone()],
        "the account's one volume is the one volume analysed: {body}"
    );
    let recommendation = &reported[&arn];

    // The configuration reported is the volume Amazon EC2 holds: its type, its size and the
    // performance provisioned on it.
    let current = &recommendation["currentConfiguration"];
    assert_eq!(
        current["volumeType"],
        json!("gp3"),
        "the reported configuration is the volume's own type: {recommendation}"
    );
    assert!(
        close(number(current, "volumeSize"), 100.0)
            && close(number(current, "volumeBaselineIOPS"), 8_000.0)
            && close(number(current, "volumeBaselineThroughput"), 500.0),
        "the reported configuration is what Amazon EC2 holds for the volume (100 GiB, 8,000 \
         IOPS, 500 MiB/s), got {current}"
    );

    // The utilization reported is the utilization published, as the per-second rates this API
    // reports: a count over the datapoint's period divided by that period.
    let observed = metrics(&recommendation["utilizationMetrics"]);
    for (name, expected) in [
        ("VolumeReadOpsPerSecond", 1_200.0),
        ("VolumeWriteOpsPerSecond", 800.0),
        ("VolumeReadBytesPerSecond", 50.0 * BYTES_PER_MIB),
        ("VolumeWriteBytesPerSecond", 30.0 * BYTES_PER_MIB),
        // The peak demand (2,000 IOPS, 80 MiB/s) stayed inside what the volume is
        // provisioned for, so neither limit was exceeded.
        ("VolumeIOPSExceeded", 0.0),
        ("VolumeThroughputExceeded", 0.0),
    ] {
        let actual = observed.get(name).copied().unwrap_or(f64::NAN);
        assert!(
            close(actual, expected),
            "case {name}: the analysis reports the peak of what was published ({expected}), \
             got {actual}: {recommendation}"
        );
    }

    // The option carries that peak with the service's headroom, and the volume is not
    // optimized because the option costs less than what it runs on today.
    assert_eq!(
        recommendation["finding"],
        json!("NotOptimized"),
        "a volume whose option is cheaper than its current configuration is not optimized: \
         {recommendation}"
    );
    let option = &recommendation["volumeRecommendationOptions"][0]["configuration"];
    assert!(
        number(option, "volumeBaselineIOPS") >= 2_000.0
            && number(option, "volumeBaselineIOPS") < 8_000.0,
        "the option carries the analysed peak of 2,000 IOPS with less than the 8,000 \
         provisioned today, got {option}"
    );
    let saved = number(
        &recommendation["volumeRecommendationOptions"][0]["savingsOpportunity"]
            ["estimatedMonthlySavings"],
        "value",
    );
    assert!(
        saved > 0.0,
        "dropping the provisioned performance the workload does not use saves money, got \
         {saved}: {recommendation}"
    );

    // An ARN of the same account and Region that no volume of the account answers to selects
    // nothing — and is no error.
    let unknown = volume_arn(REGION, "vol-00000000000000001");
    let missing = recommendations(&router, REGION, json!({"volumeArns": [unknown]})).await;
    assert_eq!(
        reported_by_arn(&missing).keys().cloned().collect::<Vec<_>>(),
        Vec::<String>::new(),
        "a volume Amazon EC2 does not hold has no recommendation: {missing}"
    );
    assert_eq!(
        missing["errors"],
        json!([]),
        "a volume Amazon EC2 does not hold is no error of the request: {missing}"
    );
}

/// One row of the volume-type table: a volume the account creates, and the baseline and burst
/// performance its type and size deliver.
struct Delivered {
    case: &'static str,
    volume_type: &'static str,
    size: i32,
    iops: Option<i32>,
    throughput: Option<i32>,
    baseline_iops: f64,
    burst_iops: f64,
    baseline_throughput: f64,
    burst_throughput: f64,
}

/// What each volume type delivers, from the Amazon EBS volume types reference — the figures
/// `VolumeConfiguration`'s baseline and burst members report, which Amazon EC2 does not store
/// (a volume stores only what was PROVISIONED on it).
///
/// Source, per type: `gp2` delivers 3 IOPS per GiB between 100 and 16,000 IOPS and bursts to
/// 3,000 while its baseline is below that, with 128 MiB/s of throughput under 170 GiB, a
/// 250 MiB/s burst from 170 GiB and 250 MiB/s sustained from 334 GiB; `gp3` delivers exactly
/// what is provisioned on it and does not burst; `io1` and `io2` deliver their provisioned
/// IOPS and reach their 1,000 MiB/s ceiling at 16,000 IOPS; `st1` accumulates 40 MiB/s per TiB
/// and bursts to 250 MiB/s per TiB up to 500 MiB/s and 500 IOPS, `sc1` 12 and 80 MiB/s per TiB
/// up to 250 MiB/s and 250 IOPS, both in 1 MiB operations; a `standard` magnetic volume
/// delivers about 100 IOPS and 40 to 90 MiB/s.
///
/// The rows span each published bound and one step past it, and cover the types the issue this
/// suite was written for never cited (`io2`, `st1`, `sc1`, `standard`).
fn delivered() -> Vec<Delivered> {
    vec![
        Delivered {
            case: "gp2 at its smallest size, under the 100 IOPS floor",
            volume_type: "gp2",
            size: 1,
            iops: None,
            throughput: None,
            baseline_iops: 100.0,
            burst_iops: 3_000.0,
            baseline_throughput: 128.0,
            burst_throughput: 128.0,
        },
        Delivered {
            case: "gp2 one GiB under the burst-throughput threshold",
            volume_type: "gp2",
            size: 169,
            iops: None,
            throughput: None,
            baseline_iops: 507.0,
            burst_iops: 3_000.0,
            baseline_throughput: 128.0,
            burst_throughput: 128.0,
        },
        Delivered {
            case: "gp2 at the burst-throughput threshold",
            volume_type: "gp2",
            size: 170,
            iops: None,
            throughput: None,
            baseline_iops: 510.0,
            burst_iops: 3_000.0,
            baseline_throughput: 128.0,
            burst_throughput: 250.0,
        },
        Delivered {
            case: "gp2 at the sustained-throughput threshold",
            volume_type: "gp2",
            size: 334,
            iops: None,
            throughput: None,
            baseline_iops: 1_002.0,
            burst_iops: 3_000.0,
            baseline_throughput: 250.0,
            burst_throughput: 250.0,
        },
        Delivered {
            case: "gp2 at its largest size, past the 16,000 IOPS ceiling",
            volume_type: "gp2",
            size: 16_384,
            iops: None,
            throughput: None,
            baseline_iops: 16_000.0,
            // A baseline at or above 3,000 IOPS has no burst above it.
            burst_iops: 0.0,
            baseline_throughput: 250.0,
            burst_throughput: 250.0,
        },
        Delivered {
            case: "gp3 at its included performance",
            volume_type: "gp3",
            size: 100,
            iops: None,
            throughput: None,
            baseline_iops: 3_000.0,
            burst_iops: 0.0,
            baseline_throughput: 125.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "gp3 provisioned past its included performance",
            volume_type: "gp3",
            size: 200,
            iops: Some(16_000),
            throughput: Some(1_000),
            baseline_iops: 16_000.0,
            burst_iops: 0.0,
            baseline_throughput: 1_000.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "io1 at its smallest size and fewest IOPS",
            volume_type: "io1",
            size: 4,
            iops: Some(100),
            throughput: None,
            baseline_iops: 100.0,
            burst_iops: 0.0,
            // 100 of the 16,000 IOPS that reach 1,000 MiB/s.
            baseline_throughput: 6.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "io1 below the IOPS that reach its throughput ceiling",
            volume_type: "io1",
            size: 200,
            iops: Some(8_000),
            throughput: None,
            baseline_iops: 8_000.0,
            burst_iops: 0.0,
            baseline_throughput: 500.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "io1 at the IOPS that reach its throughput ceiling",
            volume_type: "io1",
            size: 200,
            iops: Some(16_000),
            throughput: None,
            baseline_iops: 16_000.0,
            burst_iops: 0.0,
            baseline_throughput: 1_000.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "io1 past the IOPS that reach its throughput ceiling",
            volume_type: "io1",
            size: 200,
            iops: Some(64_000),
            throughput: None,
            baseline_iops: 64_000.0,
            burst_iops: 0.0,
            baseline_throughput: 1_000.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "io2 provisioned past io1's IOPS range",
            volume_type: "io2",
            size: 200,
            iops: Some(100_000),
            throughput: None,
            baseline_iops: 100_000.0,
            burst_iops: 0.0,
            baseline_throughput: 1_000.0,
            burst_throughput: 0.0,
        },
        Delivered {
            case: "st1 at one TiB",
            volume_type: "st1",
            size: 1_024,
            iops: None,
            throughput: None,
            baseline_iops: 40.0,
            burst_iops: 250.0,
            baseline_throughput: 40.0,
            burst_throughput: 250.0,
        },
        Delivered {
            case: "st1 past its per-volume throughput ceiling",
            volume_type: "st1",
            size: 16_384,
            iops: None,
            throughput: None,
            baseline_iops: 500.0,
            burst_iops: 500.0,
            baseline_throughput: 500.0,
            burst_throughput: 500.0,
        },
        Delivered {
            case: "sc1 at one TiB",
            volume_type: "sc1",
            size: 1_024,
            iops: None,
            throughput: None,
            baseline_iops: 12.0,
            burst_iops: 80.0,
            baseline_throughput: 12.0,
            burst_throughput: 80.0,
        },
        Delivered {
            case: "sc1 past its per-volume throughput ceiling",
            volume_type: "sc1",
            size: 16_384,
            iops: None,
            throughput: None,
            baseline_iops: 192.0,
            burst_iops: 250.0,
            baseline_throughput: 192.0,
            burst_throughput: 250.0,
        },
        Delivered {
            case: "a standard magnetic volume",
            volume_type: "standard",
            size: 100,
            iops: None,
            throughput: None,
            baseline_iops: 100.0,
            burst_iops: 0.0,
            baseline_throughput: 40.0,
            burst_throughput: 90.0,
        },
    ]
}

/// The configuration a recommendation reports for a volume is the volume Amazon EC2 holds: its
/// own type and size, with the baseline and burst performance that type and size deliver.
#[tokio::test]
async fn the_reported_configuration_is_what_the_volume_type_delivers() {
    let (router, engine) = emulator_with_volume_inventory();
    analysed(&router, &engine).await;

    let mut expected: Vec<(String, Delivered)> = Vec::new();
    for row in delivered() {
        let volume_id = create_volume(
            &router,
            REGION,
            row.case,
            row.volume_type,
            row.size,
            row.iops,
            row.throughput,
        )
        .await;
        // Every volume needs utilization for the analysis to have an input; what it is does
        // not matter to the configuration reported for the volume.
        publish_demand(&engine, &volume_id, 0.0, 1.0, 1.0, 1.0, 1.0);
        expected.push((volume_arn(REGION, &volume_id), row));
    }

    let body = recommendations(&router, REGION, json!({})).await;
    let reported = reported_by_arn(&body);
    assert_eq!(
        reported.len(),
        expected.len(),
        "every volume the account created is analysed: {body}"
    );

    for (arn, row) in &expected {
        let case = row.case;
        let recommendation = reported
            .get(arn)
            .unwrap_or_else(|| panic!("case {case:?}: no recommendation for {arn}: {body}"));
        let current = &recommendation["currentConfiguration"];
        assert_eq!(
            current["volumeType"],
            json!(row.volume_type),
            "case {case:?}: the volume's own type is reported, got {current}"
        );
        assert!(
            close(number(current, "volumeSize"), f64::from(row.size)),
            "case {case:?}: expected the volume's own size {}, got {current}",
            row.size
        );
        for (member, expected) in [
            ("volumeBaselineIOPS", row.baseline_iops),
            ("volumeBurstIOPS", row.burst_iops),
            ("volumeBaselineThroughput", row.baseline_throughput),
            ("volumeBurstThroughput", row.burst_throughput),
        ] {
            let actual = number(current, member);
            assert!(
                close(actual, expected),
                "case {case:?}/{member}: expected the {expected} its type and size deliver, \
                 got {actual}: {current}"
            );
        }
        // A volume attached to nothing is no instance's root volume.
        assert_eq!(
            current["rootVolume"],
            json!(false),
            "case {case:?}: a volume no instance boots from is not a root volume, got {current}"
        );
    }
}

/// A volume belongs to ONE Region — the Region of its Availability Zone — so it is analysed at
/// that Region's endpoint and at no other, whichever endpoint the request was signed for.
#[tokio::test]
async fn a_volume_is_analysed_at_the_endpoint_of_its_own_region() {
    let (router, engine) = emulator_with_volume_inventory();
    analysed(&router, &engine).await;

    // The same volume type and size in two Regions, so the only thing that differs between
    // the two volumes is where they are.
    let mut arns: Vec<(&str, String)> = Vec::new();
    for region in ["us-east-1", "eu-central-1"] {
        let volume_id = create_volume(&router, region, region, "gp3", 100, None, None).await;
        publish_demand(&engine, &volume_id, 0.0, 400.0, 100.0, 10.0, 5.0);
        arns.push((region, volume_arn(region, &volume_id)));
    }

    for (region, arn) in &arns {
        let body = recommendations(&router, region, json!({})).await;
        assert_eq!(
            reported_by_arn(&body).keys().cloned().collect::<Vec<_>>(),
            vec![arn.clone()],
            "case {region}: only the volumes of this Region are analysed here: {body}"
        );

        // Naming the OTHER Region's volume at this endpoint selects nothing, and is no error.
        for (other_region, other) in &arns {
            if other_region == region {
                continue;
            }
            let body = recommendations(&router, region, json!({"volumeArns": [other]})).await;
            assert_eq!(
                reported_by_arn(&body).keys().cloned().collect::<Vec<_>>(),
                Vec::<String>::new(),
                "case {region}: a volume of {other_region} is not analysed here: {body}"
            );
            assert_eq!(
                body["errors"],
                json!([]),
                "case {region}: a volume of another Region is no error: {body}"
            );
        }
    }
}

/// The account's EBS volume SUMMARY counts exactly the volumes the recommendation reader
/// reports, at the findings it reports for them: both read the same analysis of the same
/// inventory, so a volume the account creates appears in both answers or in neither.
#[tokio::test]
async fn the_summary_counts_the_volumes_the_account_owns() {
    let (router, engine) = emulator_with_volume_inventory();
    analysed(&router, &engine).await;

    let summary_count = |body: &Value| -> f64 {
        body["recommendationSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|summary| summary["recommendationResourceType"] == json!("EbsVolume"))
            .map(|summary| {
                summary["summaries"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|finding| number(finding, "value"))
                    .sum()
            })
            .unwrap_or_else(|| panic!("the EBS volume summary must be reported: {body}"))
    };

    let (status, before) = call(&router, "GetRecommendationSummaries", json!({})).await;
    assert_eq!(status, 200, "summaries: expected 200, got {status}: {before}");
    assert!(
        close(summary_count(&before), 0.0),
        "an account that owns no volume summarises none: {before}"
    );

    for volume_type in ["gp2", "gp3"] {
        let volume_id =
            create_volume(&router, REGION, volume_type, volume_type, 200, None, None).await;
        publish_demand(&engine, &volume_id, 0.0, 900.0, 600.0, 40.0, 20.0);
    }

    let read = recommendations(&router, REGION, json!({})).await;
    let reported = reported_by_arn(&read);
    let (status, after) = call(&router, "GetRecommendationSummaries", json!({})).await;
    assert_eq!(status, 200, "summaries: expected 200, got {status}: {after}");
    assert!(
        close(summary_count(&after), reported.len() as f64),
        "the summary must count exactly the volumes the reader reports ({}), got {after}",
        reported.len()
    );
}
