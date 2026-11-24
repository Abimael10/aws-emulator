// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the number of UltraWarm nodes a cluster may be provisioned
//! with, on both paths that configure one: `CreateDomain` and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a
//! client sees, not what a handler returns. The harness comes from
//! [`super::domain_lifecycle`]; this suite adds no second copy of it.
//!
//! The rule under test: a cluster that ENABLES the warm tier is provisioned in a number
//! of warm nodes inside the range the service publishes for the warm role, so a request
//! naming a count outside that range is refused with `ValidationException` (HTTP 400)
//! and the sentence that states the published bounds, and creates or changes nothing. A
//! cluster whose warm tier is off provisions no warm node, so its count is not judged.
//!
//! The bounds themselves are not written out here: each case reads them off the ANSWER
//! `DescribeInstanceTypeLimits` gives for the warm role
//! (`GET /2021-01-01/opensearch/instanceTypeLimits/{EngineVersion}/{InstanceType}`,
//! `LimitsByRole["ultrawarm"]`), which is where the service publishes them — so the
//! refusal and the published limit are asserted to be ONE fact rather than compared with
//! a second copy of the service's own table.
//!
//! The rows therefore span what the rule generalises over: each published bound, one
//! step outside it at either end, counts inside it, a count that is no count at all, and
//! each warm instance type the model declares.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// The warm instance types the model declares — the values
/// `ClusterConfig$WarmType` targets (source:
/// `smithy_query(query="shape",shape="OpenSearchWarmPartitionInstanceType")`). The rule
/// is the warm ROLE's, so it holds for every one of them.
const WARM_TYPES: [&str; 3] = [
    "ultrawarm1.medium.search",
    "ultrawarm1.large.search",
    "ultrawarm1.xlarge.search",
];

/// The engine release every case runs on, and one the warm tier is offered on.
const RELEASE: &str = "OpenSearch_2.11";

/// The key `DescribeInstanceTypeLimits` publishes the warm tier's limits under: the
/// `LimitsByRole` map's own documentation names its three roles `data`, `master` and
/// `ultra_warm` (source: `smithy_query(query="doc",shape="LimitsByRole")`).
const WARM_ROLE: &str = "ultra_warm";

/// This suite's OWN statement of the refusal text: the fixed sentence, whose variable
/// parts are the two bounds the service publishes for the warm role. Restated here
/// rather than imported from the service, so changing it in the service fails this suite
/// instead of being asserted against itself.
fn expected_message(minimum: i64, maximum: i64) -> String {
    format!("Warm storage node count must be between {minimum} and {maximum}.")
}

/// The warm-node count bounds the service PUBLISHES for `warm_type`: the
/// `MinimumInstanceCount` and `MaximumInstanceCount` `DescribeInstanceTypeLimits`
/// reports under the warm role.
async fn published_warm_bounds(emulator: &Emulator, warm_type: &str) -> (i64, i64) {
    let case = format!("DescribeInstanceTypeLimits({RELEASE}, {warm_type})");
    let answer = emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/instanceTypeLimits/{RELEASE}/{warm_type}"),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200, got {} {:?}",
        answer.status, answer.body
    );
    let limits = answer.body["LimitsByRole"][WARM_ROLE]["InstanceLimits"]
        ["InstanceCountLimits"]
        .clone();
    let minimum = limits["MinimumInstanceCount"].as_i64().unwrap_or_else(|| {
        panic!("case {case}: expected a published MinimumInstanceCount, got {limits:?}")
    });
    let maximum = limits["MaximumInstanceCount"].as_i64().unwrap_or_else(|| {
        panic!("case {case}: expected a published MaximumInstanceCount, got {limits:?}")
    });
    assert!(
        minimum < maximum,
        "case {case}: expected the published warm-role bounds to be a range, got \
         {minimum}..={maximum}"
    );
    (minimum, maximum)
}

/// A cluster that carries the warm tier on `warm_type` with `warm_count` warm nodes: a
/// non-burstable hot tier (warm storage is offered only on a family that publishes it)
/// with the dedicated master nodes the warm tier rests on.
fn warm_cluster(warm_type: &str, warm_count: Option<i64>) -> Value {
    let mut cluster = json!({
        "InstanceType": "r6g.large.search",
        "InstanceCount": 2,
        "DedicatedMasterEnabled": true,
        "DedicatedMasterType": "m6g.large.search",
        "DedicatedMasterCount": 3,
        "ZoneAwarenessEnabled": false,
        "WarmEnabled": true,
        "WarmType": warm_type,
    });
    if let Some(warm_count) = warm_count {
        cluster["WarmCount"] = json!(warm_count);
    }
    cluster
}

/// `CreateDomain` for one row: a domain of the modelled name shape
/// (`^[a-z][a-z0-9\-]+$`, 3..28 characters) carrying `cluster`.
async fn create(emulator: &Emulator, name: &str, cluster: Value) -> Answer {
    emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": RELEASE,
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "ClusterConfig": cluster,
        }))
        .await
}

/// Assert that `answer` is the refusal the rule prescribes, stating the published
/// bounds and nothing of the count the request carried.
fn assert_refused(answer: &Answer, minimum: i64, maximum: i64, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "ValidationException",
        "case {case}: expected the modelled validation refusal, got {:?}",
        answer.body
    );
    let actual = answer.body["message"]
        .as_str()
        .or_else(|| answer.body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case}: expected an error message, got {:?}", answer.body))
        .to_owned();
    assert_eq!(
        actual,
        expected_message(minimum, maximum),
        "case {case}: expected the sentence stating the published warm-node bounds, got \
         {actual:?}"
    );
}

/// Assert that nothing was stored under `name`.
async fn assert_no_domain(emulator: &Emulator, name: &str, case: &str) {
    let read = emulator.describe(name).await;
    assert_eq!(
        (read.status, error_code(&read).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case {case}: a refused create stores no domain, got {} {:?}",
        read.status,
        read.body
    );
}

/// The counts one set of published bounds is spanned by, each with whether the service
/// provisions the warm tier in it: outside at either end, at each bound, and inside.
fn counts(minimum: i64, maximum: i64) -> Vec<(String, i64, bool)> {
    vec![
        ("zero warm nodes".to_string(), 0, false),
        ("one below the published minimum".to_string(), minimum - 1, false),
        ("the published minimum".to_string(), minimum, true),
        ("one above the published minimum".to_string(), minimum + 1, true),
        ("one below the published maximum".to_string(), maximum - 1, true),
        ("the published maximum".to_string(), maximum, true),
        ("one above the published maximum".to_string(), maximum + 1, false),
        ("far above the published maximum".to_string(), maximum * 2, false),
    ]
}

#[tokio::test]
async fn a_create_provisions_the_warm_tier_only_in_a_published_node_count() {
    let emulator = Emulator::new();
    let warm_type = WARM_TYPES[0];
    let (minimum, maximum) = published_warm_bounds(&emulator, warm_type).await;

    for (row, (label, warm_count, provisioned)) in counts(minimum, maximum).into_iter().enumerate() {
        let name = format!("warm-count-{row}");
        let case = format!("CreateDomain({name}, {label}, WarmCount={warm_count})");
        let answer = create(&emulator, &name, warm_cluster(warm_type, Some(warm_count))).await;
        if !provisioned {
            assert_refused(&answer, minimum, maximum, &case);
            assert_no_domain(&emulator, &name, &case).await;
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case}: expected a count inside the published bounds to be provisioned, \
             got {} {:?}",
            answer.status, answer.body
        );
        // Round trip: the created domain reads back on the count it was created with.
        let stored = settled_domain(&emulator, &name).await;
        assert_eq!(
            stored["ClusterConfig"]["WarmCount"],
            json!(warm_count),
            "case {case}: expected the stored cluster to carry the created warm-node count, \
             got {:?}",
            stored["ClusterConfig"]
        );
    }
}

#[tokio::test]
async fn the_bound_is_the_warm_roles_and_holds_for_every_warm_instance_type() {
    let emulator = Emulator::new();

    for (row, warm_type) in WARM_TYPES.into_iter().enumerate() {
        let (minimum, maximum) = published_warm_bounds(&emulator, warm_type).await;

        let name = format!("warm-type-low-{row}");
        let case = format!("CreateDomain({name}, {warm_type}, WarmCount={})", minimum - 1);
        let answer = create(
            &emulator,
            &name,
            warm_cluster(warm_type, Some(minimum - 1)),
        )
        .await;
        assert_refused(&answer, minimum, maximum, &case);
        assert_no_domain(&emulator, &name, &case).await;

        let name = format!("warm-type-high-{row}");
        let case = format!("CreateDomain({name}, {warm_type}, WarmCount={})", maximum + 1);
        let answer = create(
            &emulator,
            &name,
            warm_cluster(warm_type, Some(maximum + 1)),
        )
        .await;
        assert_refused(&answer, minimum, maximum, &case);
        assert_no_domain(&emulator, &name, &case).await;

        let name = format!("warm-type-ok-{row}");
        let case = format!("CreateDomain({name}, {warm_type}, WarmCount={minimum})");
        let answer = create(&emulator, &name, warm_cluster(warm_type, Some(minimum))).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected a count inside the published bounds to be provisioned, \
             got {} {:?}",
            answer.status, answer.body
        );
    }
}

#[tokio::test]
async fn a_cluster_with_no_warm_tier_names_no_warm_node_count_to_judge() {
    let emulator = Emulator::new();
    let warm_type = WARM_TYPES[0];
    let (minimum, maximum) = published_warm_bounds(&emulator, warm_type).await;

    // A count outside the bounds beside a tier that provisions no node, and a tier that
    // provisions nodes while naming no count at all: neither describes a warm tier, so
    // neither is refused.
    let rows: Vec<(&str, Value)> = vec![
        (
            "warm storage off with a count below the minimum",
            json!({
                "InstanceType": "r6g.large.search",
                "InstanceCount": 2,
                "WarmEnabled": false,
                "WarmType": warm_type,
                "WarmCount": minimum - 1,
            }),
        ),
        (
            "warm storage off with a count above the maximum",
            json!({
                "InstanceType": "r6g.large.search",
                "InstanceCount": 2,
                "WarmEnabled": false,
                "WarmType": warm_type,
                "WarmCount": maximum + 1,
            }),
        ),
        (
            "no warm container at all",
            json!({ "InstanceType": "r6g.large.search", "InstanceCount": 2 }),
        ),
        (
            "warm storage on with no count",
            warm_cluster(warm_type, None),
        ),
    ];

    for (row, (label, cluster)) in rows.into_iter().enumerate() {
        let name = format!("warm-inert-{row}");
        let case = format!("CreateDomain({name}, {label})");
        let answer = create(&emulator, &name, cluster).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected a cluster that provisions no warm node to be served, \
             got {} {:?}",
            answer.status, answer.body
        );
    }
}

#[tokio::test]
async fn an_update_is_held_to_the_same_bound_and_keeps_the_stored_cluster() {
    let emulator = Emulator::new();
    let warm_type = WARM_TYPES[0];
    let (minimum, maximum) = published_warm_bounds(&emulator, warm_type).await;

    let name = "warm-count-update";
    let created = create(&emulator, name, warm_cluster(warm_type, Some(minimum))).await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({name}): expected 200, got {} {:?}",
        created.status, created.body
    );
    // The service applies one change of a domain at a time, so the change the create
    // started has to land before an update can start another.
    settled_domain(&emulator, name).await;

    for (label, warm_count, applied) in [
        ("one below the published minimum", minimum - 1, false),
        ("one above the published maximum", maximum + 1, false),
        ("the published maximum", maximum, true),
    ] {
        let case = format!("UpdateDomainConfig({name}, {label}, WarmCount={warm_count})");
        let answer = emulator
            .update_config(
                name,
                json!({ "ClusterConfig": warm_cluster(warm_type, Some(warm_count)) }),
            )
            .await;
        if !applied {
            assert_refused(&answer, minimum, maximum, &case);
            let stored = emulator.describe_config(name).await;
            assert_eq!(
                stored.body["DomainConfig"]["ClusterConfig"]["Options"]["WarmCount"],
                json!(minimum),
                "case {case}: a refused update leaves the stored cluster alone, got {:?}",
                stored.body["DomainConfig"]["ClusterConfig"]
            );
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case}: expected a count inside the published bounds to apply, got {} {:?}",
            answer.status, answer.body
        );
        let stored = emulator.describe_config(name).await;
        assert_eq!(
            stored.body["DomainConfig"]["ClusterConfig"]["Options"]["WarmCount"],
            json!(warm_count),
            "case {case}: expected the applied change to be stored, got {:?}",
            stored.body["DomainConfig"]["ClusterConfig"]
        );
    }
}
