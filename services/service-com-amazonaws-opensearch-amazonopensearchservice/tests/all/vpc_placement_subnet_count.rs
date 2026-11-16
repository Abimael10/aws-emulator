// SPDX-License-Identifier: CC0-1.0

//! A VPC-access domain names ONE subnet per Availability Zone its cluster is placed in.
//!
//! RULE: `VPCOptions$SubnetIds` is "if your domain uses multiple Availability Zones, you
//! need to provide two subnet IDs, one per zone. Otherwise, provide only one", and
//! `ClusterConfig$ZoneAwarenessEnabled` is the switch that decides how many zones the
//! domain uses (`ZoneAwarenessConfig$AvailabilityZoneCount` how many when it is on) — so a
//! VPC-access domain names exactly as many subnets as it has zones: one where it asks for
//! no zone awareness, and the zone count where it does. A call naming any other number of
//! subnets describes a placement the service does not provision and is refused with the
//! modelled `ValidationException` (HTTP 400), with nothing created or changed. Both calls
//! that carry `VPCOptions` for a domain — `CreateDomain` and `UpdateDomainConfig` — are
//! judged by it.
//!
//! The rows differ along what the rule generalises over: whether zone awareness is asked
//! for, absent or switched off, every zone count the service places a domain in, how many
//! subnets the request names on each side of the required number (none, one, two, three),
//! and whether the request names a placement at all.

use serde_json::{json, Value};

use super::control_plane_refusal_wording::message;
use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS, PREFIX,
};

/// The engine release every domain below runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The instance type every cluster below runs on — one the catalog publishes for that
/// release.
const INSTANCE_TYPE: &str = "t3.small.search";

/// How the service words the number of subnets a placement takes, for each number of zones
/// it places a domain in: a single-zone cluster, and each value of
/// `ZoneAwarenessConfig$AvailabilityZoneCount` ("valid values are `2` and `3`").
const ONE_SUBNET: &str = "You must specify exactly one subnet.";
const TWO_SUBNETS: &str = "You must specify exactly two subnets.";
const THREE_SUBNETS: &str = "You must specify exactly three subnets.";

/// What a call is expected to answer.
#[derive(Debug)]
enum Expect {
    /// The call is served (HTTP 200).
    Served,
    /// The call is refused with a 400 `ValidationException` carrying this sentence.
    Refused(&'static str),
}

/// One `CreateDomain` to make.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The `ClusterConfig` the create carries, if any — a request naming none asks for the
    /// service's own defaults, zone awareness off among them.
    cluster: Option<Value>,
    /// The subnets the create names, or `None` for a domain reached on a public endpoint,
    /// which has no placement at all.
    subnets: Option<&'static [&'static str]>,
    expected: Expect,
}

/// Every subnet identifier the rows draw from. Nothing resolves them — the rule under test
/// is a cross-check between two members of the REQUEST, so what the account's network says
/// about a subnet is no part of it.
const SUBNETS: [&str; 3] = [
    "subnet-0a0a0000000000001",
    "subnet-0a0a0000000000002",
    "subnet-0a0a0000000000003",
];

/// The table: every zone topology the service places a domain in, against the subnet counts
/// on either side of the one it takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no ClusterConfig at all, one subnet",
            name: "subnets-default-one",
            cluster: None,
            subnets: Some(&SUBNETS[..1]),
            expected: Expect::Served,
        },
        Row {
            case: "no ClusterConfig at all, two subnets",
            name: "subnets-default-two",
            cluster: None,
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Refused(ONE_SUBNET),
        },
        Row {
            case: "zone awareness off, one subnet",
            name: "subnets-single-one",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": false,
            })),
            subnets: Some(&SUBNETS[..1]),
            expected: Expect::Served,
        },
        Row {
            case: "zone awareness off, two subnets",
            name: "subnets-single-two",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": false,
                "MultiAZWithStandbyEnabled": false,
            })),
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Refused(ONE_SUBNET),
        },
        Row {
            case: "zone awareness off, three subnets",
            name: "subnets-single-three",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": false,
            })),
            subnets: Some(&SUBNETS[..3]),
            expected: Expect::Refused(ONE_SUBNET),
        },
        Row {
            case: "zone awareness off, an empty subnet list",
            name: "subnets-single-none",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": false,
            })),
            subnets: Some(&[]),
            expected: Expect::Refused(ONE_SUBNET),
        },
        Row {
            case: "zone awareness on with no zone count named, two subnets",
            name: "subnets-aware-default",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
            })),
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Served,
        },
        Row {
            case: "zone awareness on with no zone count named, one subnet",
            name: "subnets-aware-default-one",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
            })),
            subnets: Some(&SUBNETS[..1]),
            expected: Expect::Refused(TWO_SUBNETS),
        },
        Row {
            case: "two zones, two subnets",
            name: "subnets-two-zones",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Served,
        },
        Row {
            case: "two zones, one subnet",
            name: "subnets-two-zones-one",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: Some(&SUBNETS[..1]),
            expected: Expect::Refused(TWO_SUBNETS),
        },
        Row {
            case: "two zones, three subnets",
            name: "subnets-two-zones-three",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: Some(&SUBNETS[..3]),
            expected: Expect::Refused(TWO_SUBNETS),
        },
        Row {
            case: "three zones, three subnets",
            name: "subnets-three-zones",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 3,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
            })),
            subnets: Some(&SUBNETS[..3]),
            expected: Expect::Served,
        },
        Row {
            case: "three zones, one subnet",
            name: "subnets-three-zones-one",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 3,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
            })),
            subnets: Some(&SUBNETS[..1]),
            expected: Expect::Refused(THREE_SUBNETS),
        },
        Row {
            case: "three zones, two subnets",
            name: "subnets-three-zones-two",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 3,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
            })),
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Refused(THREE_SUBNETS),
        },
        Row {
            case: "a public domain, which has no placement to judge",
            name: "subnets-public",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: None,
            expected: Expect::Served,
        },
    ]
}

/// A `CreateDomain` body for one row.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "DomainName": row.name,
        "EngineVersion": ENGINE_VERSION,
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    });
    if let Some(cluster) = &row.cluster {
        body["ClusterConfig"] = cluster.clone();
    }
    if let Some(subnets) = row.subnets {
        body["VPCOptions"] = json!({
            "SubnetIds": subnets,
            "SecurityGroupIds": ["sg-0a0a0000000000001"],
        });
    }
    body
}

/// Assert `answer` is the modelled refusal carrying `sentence`.
fn assert_refused(answer: &Answer, case: &str, sentence: &str) {
    assert_eq!(
        (answer.status, error_code(answer).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected a 400 ValidationException, got {} with {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        message(answer),
        sentence,
        "case {case:?}: expected the sentence the service words this condition with, got {:?}",
        answer.body
    );
}

/// The domain names `ListDomainNames` reports for this account.
async fn listed_domains(emulator: &Emulator) -> Vec<String> {
    let listed = emulator
        .call("GET", &format!("{PREFIX}/domain"), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "a listing of this account's domains must be served, got {} with {:?}",
        listed.status, listed.body
    );
    listed.body["DomainNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|info| info["DomainName"].as_str().map(str::to_owned))
        .collect()
}

/// The subnets a reported `DomainStatus` carries.
fn reported_subnets(status: &Value) -> Vec<String> {
    status["VPCOptions"]["SubnetIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_create_names_one_subnet_per_availability_zone() {
    let emulator = Emulator::new();

    for row in rows() {
        let case = row.case;
        let answer = emulator.create(create_body(&row)).await;

        match row.expected {
            Expect::Served => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the create to be served, got {} with {:?}",
                    answer.status, answer.body
                );
                // Read the domain back, so what is asserted is the placement the service
                // STORED rather than only what the create echoed.
                let read = emulator.describe(row.name).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: expected the domain to be readable, got {} with {:?}",
                    read.status, read.body
                );
                let expected: Vec<String> = row
                    .subnets
                    .unwrap_or(&[])
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect();
                for (source, status) in [
                    ("the create", &answer.body["DomainStatus"]),
                    ("DescribeDomain", &read.body["DomainStatus"]),
                ] {
                    assert_eq!(
                        reported_subnets(status),
                        expected,
                        "case {case:?}: {source} must report the subnets the request named, \
                         got {status:?}"
                    );
                }
            }
            Expect::Refused(sentence) => {
                assert_refused(&answer, case, sentence);

                // NOTHING is created: the name resolves nowhere and the account's listing
                // does not carry it.
                let read = emulator.describe(row.name).await;
                assert_eq!(
                    (read.status, error_code(&read).as_str()),
                    (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
                    "case {case:?}: a refused create must leave the name unresolvable, got \
                     {} with {:?}",
                    read.status,
                    read.body
                );
                assert_eq!(
                    message(&read),
                    format!("Domain not found: {}", row.name),
                    "case {case:?}: expected the sentence the service words an absent domain \
                     with, got {:?}",
                    read.body
                );
                assert!(
                    !listed_domains(&emulator)
                        .await
                        .contains(&row.name.to_string()),
                    "case {case:?}: a refused create must leave the account's domains as \
                     they were"
                );
            }
        }
    }
}

/// One `UpdateDomainConfig` to apply to a single-zone domain placed in one subnet.
struct ChangeRow {
    case: &'static str,
    /// The `ClusterConfig` the change names, if any.
    cluster: Option<Value>,
    /// The subnets the change names, if any — a supplied `VPCOptions` replaces the stored
    /// one whole.
    subnets: Option<&'static [&'static str]>,
    expected: Expect,
}

/// The change table: the same rule over the operation that CHANGES a placement, judged on
/// the configuration the change would leave the domain with.
fn change_rows() -> Vec<ChangeRow> {
    vec![
        ChangeRow {
            case: "a change naming two subnets for a cluster that keeps one zone",
            cluster: None,
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Refused(ONE_SUBNET),
        },
        ChangeRow {
            case: "a change switching zone awareness on for a domain that keeps one subnet",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: None,
            expected: Expect::Refused(TWO_SUBNETS),
        },
        ChangeRow {
            case: "a change naming three subnets for a cluster of two zones",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: Some(&SUBNETS[..3]),
            expected: Expect::Refused(TWO_SUBNETS),
        },
        ChangeRow {
            case: "a change moving both the topology and the subnets together",
            cluster: Some(json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 2,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            })),
            subnets: Some(&SUBNETS[..2]),
            expected: Expect::Served,
        },
    ]
}

#[tokio::test]
async fn a_change_leaves_one_subnet_per_availability_zone() {
    let emulator = Emulator::new();

    let name = "subnets-changed";
    let created = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": INSTANCE_TYPE, "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "VPCOptions": { "SubnetIds": [SUBNETS[0]] },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDomain must be served, got {} with {:?}",
        created.status, created.body
    );

    for row in change_rows() {
        let case = row.case;
        // The domain is idle before each change, which is the condition — not a stretch of
        // time — a client waits for.
        let before = settled_domain(&emulator, name).await;
        let mut body = json!({});
        if let Some(cluster) = &row.cluster {
            body["ClusterConfig"] = cluster.clone();
        }
        if let Some(subnets) = row.subnets {
            body["VPCOptions"] = json!({ "SubnetIds": subnets });
        }
        let answer = emulator.update_config(name, body).await;

        match row.expected {
            Expect::Served => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the change to be served, got {} with {:?}",
                    answer.status, answer.body
                );
                let read = emulator.describe(name).await;
                let expected: Vec<String> = row
                    .subnets
                    .unwrap_or(&[])
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect();
                assert_eq!(
                    reported_subnets(&read.body["DomainStatus"]),
                    expected,
                    "case {case:?}: the domain must carry the subnets the change named, got \
                     {:?}",
                    read.body
                );
            }
            Expect::Refused(sentence) => {
                assert_refused(&answer, case, sentence);

                // A refused change leaves the placement standing.
                let read = emulator.describe(name).await;
                assert_eq!(
                    reported_subnets(&read.body["DomainStatus"]),
                    reported_subnets(&before),
                    "case {case:?}: a refused change must leave the placement as it was, got \
                     {:?}",
                    read.body
                );
            }
        }
    }
}
