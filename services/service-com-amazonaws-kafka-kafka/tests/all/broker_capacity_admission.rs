// SPDX-License-Identifier: CC0-1.0

//! What Amazon MSK admits as the CAPACITY of a provisioned cluster: the broker
//! types it runs brokers on, the client-subnet lists it places them in, and which
//! direction a broker count may move in.
//!
//! Three rules, each asserted over a TABLE rather than over one recorded request:
//!
//! 1. a cluster's brokers run on a broker type the service OFFERS, so an
//!    `InstanceType` outside its catalogue is refused `BadRequestException` (400)
//!    with MSK's fixed sentence and the catalogue it does offer, and no cluster is
//!    registered;
//! 2. a client-subnet list places one broker interface per Availability Zone, so a
//!    list naming the same subnet twice is refused with MSK's fixed duplicate-list
//!    sentence, whichever member carries that list;
//! 3. a broker count only ever goes UP: `UpdateBrokerCount` with a target below
//!    the count the cluster holds is refused, and the cluster keeps its brokers,
//!    its state and its version, while a target at or above that count is
//!    accepted.
//!
//! The rows differ along what each rule generalises over: the operation that names
//! the value (the v1 create, the v2 provisioned create, the v2 serverless create),
//! the FORM of the rejected broker type (an unknown family, an unknown size, a type
//! of another service's catalogue, an Express-looking one), the broker types the
//! catalogue really holds, WHERE in a subnet list the repeat sits and how long the
//! list is, and the count a cluster starts at together with the size of the
//! decrease asked for.
//!
//! The Availability-Zone side of rule 3 — a count has to divide by the number of
//! zones the cluster's client subnets span — needs Amazon EC2 beside this service
//! to say which zones those are, so it lives in
//! `core/router/tests/all/kafka_broker_count_divides_the_subnet_zones.rs`; the
//! harness here mounts this service alone, so nothing resolves its subnets.
//!
//! The HTTP harness comes from [`crate::kafka_harness`] — the same router, the same
//! SigV4-scoped request builder — rather than a second one built here.

use serde_json::{json, Value};

use crate::kafka_harness::{
    pct, sleep_past_activation, sleep_past_update, ClusterRequest, Emulator, Reply,
};

/// Every broker type Amazon MSK runs brokers on, written out here rather than read
/// from the service's own catalogue, so the list is pinned by this suite instead of
/// by the code under test.
///
/// Source: the Amazon MSK Developer Guide, "Broker types"
/// (<https://docs.aws.amazon.com/msk/latest/developerguide/msk-brokers.html>) — the
/// Express category (`express.m7g.*`) and the standard families (`kafka.m5.*`,
/// `kafka.m7g.*`, `kafka.t3.small`), which is the same list the service renders in
/// the refusal below.
const BROKER_TYPES: &[&str] = &[
    "express.m7g.large",
    "express.m7g.xlarge",
    "express.m7g.2xlarge",
    "express.m7g.4xlarge",
    "express.m7g.8xlarge",
    "express.m7g.12xlarge",
    "express.m7g.16xlarge",
    "kafka.t3.small",
    "kafka.m5.large",
    "kafka.m5.xlarge",
    "kafka.m5.2xlarge",
    "kafka.m5.4xlarge",
    "kafka.m5.8xlarge",
    "kafka.m5.12xlarge",
    "kafka.m5.16xlarge",
    "kafka.m5.24xlarge",
    "kafka.m7g.large",
    "kafka.m7g.xlarge",
    "kafka.m7g.2xlarge",
    "kafka.m7g.4xlarge",
    "kafka.m7g.8xlarge",
    "kafka.m7g.12xlarge",
    "kafka.m7g.16xlarge",
];

/// The sentence MSK refuses a broker type it does not offer with: fixed text, then
/// the types it DOES offer, ascending by name — which is one sequence over both
/// categories.
fn unsupported_instance_type_message() -> String {
    let mut valid: Vec<&str> = BROKER_TYPES.to_vec();
    valid.sort_unstable();
    format!(
        "Unsupported InstanceType specified. Valid values: [{}]",
        valid.join(", ")
    )
}

/// What MSK answers a list that names the same item twice: entirely fixed text,
/// written out here so the wording is pinned by this suite.
const DUPLICATE_ITEMS_MESSAGE: &str = "The list provided contains duplicate items.";

/// What MSK answers a request that asks for fewer brokers than the cluster holds:
/// entirely fixed text, written out here for the same reason.
const BROKER_COUNT_DECREASE_MESSAGE: &str =
    "Amazon MSK cannot proceed with decreasing the cluster's broker count. Make sure that at \
     least one broker in each Availability Zone has no user partitions and try again. Allow five \
     minutes after partition reassignments.";

/// A `CreateClusterV2` body whose `Provisioned` block asks for `brokers` brokers of
/// `instance_type` in `subnets`.
fn v2_provisioned_body(name: &str, instance_type: &str, brokers: i64, subnets: &[&str]) -> Value {
    json!({
        "clusterName": name,
        "provisioned": {
            "kafkaVersion": "3.9.x",
            "numberOfBrokerNodes": brokers,
            "brokerNodeGroupInfo": {
                "instanceType": instance_type,
                "clientSubnets": subnets,
            },
        },
    })
}

/// A `CreateClusterV2` body whose `Serverless` block names ONE VPC configuration
/// over `subnets`, with the SASL/IAM client authentication a serverless cluster
/// must use — the one scheme MSK admits a serverless cluster's clients through, and
/// one a serverless create has to turn on.
fn v2_serverless_body(name: &str, subnets: &[&str]) -> Value {
    json!({
        "clusterName": name,
        "serverless": {
            "vpcConfigs": [{ "subnetIds": subnets }],
            "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
        },
    })
}

/// Every cluster name `ListClustersV2` reports — what a refused create must leave
/// empty.
async fn listed_names(emulator: &Emulator) -> Vec<String> {
    emulator
        .list_v2("")
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| entry["clusterName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Assert `reply` is the modelled 400 `BadRequestException` naming `parameter` and
/// carrying `message`.
fn assert_refusal(reply: &Reply, case: &str, parameter: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", message),
        "case {case}: expected the refusal {message:?}, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.body["invalidParameter"].as_str(),
        Some(parameter),
        "case {case}: expected the refusal to name {parameter:?}, got {}",
        reply.body
    );
}

/// RULE: a provisioned cluster's brokers run on one of the broker types Amazon MSK
/// offers, so a `BrokerNodeGroupInfo$InstanceType` outside that catalogue is
/// refused `BadRequestException` (400) with MSK's fixed sentence and the catalogue
/// it does offer, and no cluster is registered — while every type the catalogue
/// holds is accepted and read back off the cluster.
#[tokio::test]
async fn a_create_refuses_a_broker_type_the_service_does_not_offer() {
    let emulator = Emulator::new();
    let expected = unsupported_instance_type_message();

    // The rows differ along the FORM of the rejected value: a family MSK has no
    // brokers of, a size a family it does offer does not come in, a type of
    // another EC2 family altogether, an Express-looking type outside the category,
    // a type with the right shape and a bogus prefix, and an Express size the
    // category does not come in. The EMPTY value is not here: it is outside the
    // `length` the model puts on `BrokerNodeGroupInfo$InstanceType`, so the
    // request boundary refuses it before this rule is reached, which
    // `tests/all/broker_type_update.rs` is the suite for.
    let rejected = [
        "kafka.t1.nano",
        "kafka.m5.32xlarge",
        "kafka.r5.large",
        "express.m5.large",
        "msk.m5.large",
        "kafka.m7g.small",
        "express.m7g.24xlarge",
    ];
    for (index, instance_type) in rejected.iter().enumerate() {
        // The v1 create and the v2 provisioned one submit the same broker
        // configuration, so the same rule holds for both.
        let case = format!("CreateCluster instanceType={instance_type:?}");
        let reply = emulator
            .create(&ClusterRequest {
                instance_type,
                ..ClusterRequest::named(&format!("bad-type-v1-{index}"))
            })
            .await;
        assert_refusal(&reply, &case, "instanceType", &expected);

        let case = format!("CreateClusterV2 instanceType={instance_type:?}");
        let reply = emulator
            .create_cluster_v2(v2_provisioned_body(
                &format!("bad-type-v2-{index}"),
                instance_type,
                3,
                &["subnet-a", "subnet-b", "subnet-c"],
            ))
            .await;
        assert_refusal(&reply, &case, "instanceType", &expected);
    }

    // Not one of them registered anything.
    let listed = listed_names(&emulator).await;
    assert!(
        listed.is_empty(),
        "a refused create must register no cluster, got {listed:?}"
    );

    // The other side of the rule: EVERY type the catalogue holds is accepted —
    // including the Express sizes and the burstable one the issue that filed this
    // rule never named — and the cluster reports the type it was created with.
    for (index, instance_type) in BROKER_TYPES.iter().enumerate() {
        let case = format!("a create on the catalogued type {instance_type:?}");
        let arn = emulator
            .create_ok(&ClusterRequest {
                instance_type,
                ..ClusterRequest::named(&format!("good-type-{index}"))
            })
            .await;
        let described = emulator.describe(&arn).await;
        assert_eq!(
            described
                .at("/clusterInfo/brokerNodeGroupInfo/instanceType")
                .as_str(),
            Some(*instance_type),
            "case {case}: the cluster must report the broker type it was created with, got {}",
            described.body
        );
    }
}

/// RULE: a client-subnet list places ONE broker interface per Availability Zone, so
/// a list naming the same subnet twice names no second zone: it is refused
/// `BadRequestException` (400) with MSK's fixed duplicate-list sentence, whichever
/// member carries the list, and nothing is registered.
#[tokio::test]
async fn a_create_refuses_a_subnet_list_that_repeats_a_subnet() {
    let emulator = Emulator::new();

    // The rows differ along WHERE the repeat sits and how long the list is: a
    // two-entry list that is one subnet twice, a repeat at the front of a longer
    // list, one at the back, a list that repeats a subnet three times, and one
    // whose repeat is separated by another subnet.
    let lists: Vec<(&str, Vec<&str>)> = vec![
        ("the same subnet twice", vec!["subnet-a", "subnet-a"]),
        (
            "a repeat at the front",
            vec!["subnet-a", "subnet-a", "subnet-b"],
        ),
        (
            "a repeat at the back",
            vec!["subnet-a", "subnet-b", "subnet-b"],
        ),
        (
            "the same subnet three times",
            vec!["subnet-c", "subnet-c", "subnet-c"],
        ),
        (
            "a repeat separated by another subnet",
            vec!["subnet-a", "subnet-b", "subnet-a"],
        ),
    ];

    for (index, (case, subnets)) in lists.iter().enumerate() {
        let brokers = i64::try_from(subnets.len()).expect("a small list");

        let reply = emulator
            .create(&ClusterRequest {
                brokers,
                subnets: subnets.clone(),
                ..ClusterRequest::named(&format!("dup-v1-{index}"))
            })
            .await;
        assert_refusal(
            &reply,
            &format!("CreateCluster with {case}"),
            "brokerNodeGroupInfo",
            DUPLICATE_ITEMS_MESSAGE,
        );

        let reply = emulator
            .create_cluster_v2(v2_provisioned_body(
                &format!("dup-v2-{index}"),
                "kafka.m5.large",
                brokers,
                subnets,
            ))
            .await;
        assert_refusal(
            &reply,
            &format!("CreateClusterV2 provisioned with {case}"),
            "brokerNodeGroupInfo",
            DUPLICATE_ITEMS_MESSAGE,
        );

        // A serverless cluster is reached through the subnets of its VPC
        // configurations, which are one per zone too, so the same rule holds for
        // that member.
        let reply = emulator
            .create_cluster_v2(v2_serverless_body(
                &format!("dup-serverless-{index}"),
                subnets,
            ))
            .await;
        assert_refusal(
            &reply,
            &format!("CreateClusterV2 serverless with {case}"),
            "subnetIds",
            DUPLICATE_ITEMS_MESSAGE,
        );
    }

    let listed = listed_names(&emulator).await;
    assert!(
        listed.is_empty(),
        "a refused create must register no cluster, got {listed:?}"
    );

    // The control: the same lists with their repeat dropped are accepted, and the
    // cluster is placed in exactly the subnets the request named.
    for (index, subnets) in [
        vec!["subnet-a", "subnet-b"],
        vec!["subnet-a", "subnet-b", "subnet-c"],
    ]
    .iter()
    .enumerate()
    {
        let case = format!("a create over the distinct subnets {subnets:?}");
        let brokers = i64::try_from(subnets.len()).expect("a small list");
        let arn = emulator
            .create_ok(&ClusterRequest {
                brokers,
                subnets: subnets.clone(),
                ..ClusterRequest::named(&format!("distinct-{index}"))
            })
            .await;
        let described = emulator.describe(&arn).await;
        let placed: Vec<String> = described
            .at("/clusterInfo/brokerNodeGroupInfo/clientSubnets")
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            placed,
            subnets.iter().map(|id| (*id).to_string()).collect::<Vec<String>>(),
            "case {case}: the cluster must report the subnets it was placed in, got {}",
            described.body
        );
    }
}

/// `PUT /v1/clusters/{ClusterArn}/nodes/count`.
async fn update_broker_count(
    emulator: &Emulator,
    cluster_arn: &str,
    current_version: &str,
    target: i64,
) -> Reply {
    emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/nodes/count", pct(cluster_arn)),
            Some(json!({
                "currentVersion": current_version,
                "targetNumberOfBrokerNodes": target,
            })),
        )
        .await
}

/// A cluster holding `brokers` brokers, waited into `ACTIVE`, as
/// (ARN, `CurrentVersion`): the state an update is accepted from.
async fn active_cluster(emulator: &Emulator, name: &str, brokers: i64) -> (String, String) {
    let arn = emulator
        .create_ok(&ClusterRequest {
            brokers,
            ..ClusterRequest::named(name)
        })
        .await;
    sleep_past_activation();
    let described = emulator.describe(&arn).await;
    assert_eq!(
        described.at("/clusterInfo/state").as_str(),
        Some("ACTIVE"),
        "case {name}: expected the new cluster to settle ACTIVE, got {}",
        described.body
    );
    let version = described
        .at("/clusterInfo/currentVersion")
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: DescribeCluster reported no currentVersion"))
        .to_string();
    (arn, version)
}

/// RULE: Amazon MSK cannot take brokers out of a provisioned cluster, so
/// `UpdateBrokerCount` with a `TargetNumberOfBrokerNodes` BELOW the count the
/// cluster holds is refused `BadRequestException` (400) with MSK's fixed sentence —
/// the cluster stays `ACTIVE` at the same broker count and the same
/// `CurrentVersion`, and no operation is queued — while a target at or above that
/// count is accepted.
#[tokio::test]
async fn an_update_refuses_a_broker_count_below_the_one_the_cluster_holds() {
    // The rows differ along the count the cluster starts at and the size of the
    // decrease: one below, several below, down to the published minimum, and down
    // from the minimum's neighbour.
    let rows: &[(&str, i64, i64)] = &[
        ("one below", 4, 3),
        ("four below", 6, 2),
        ("down to the published minimum", 4, 1),
        ("down by one from two", 2, 1),
    ];

    let emulator = Emulator::new();
    for (index, (case, initial, target)) in rows.iter().enumerate() {
        let (arn, version) =
            active_cluster(&emulator, &format!("decrease-{index}"), *initial).await;

        let reply = update_broker_count(&emulator, &arn, &version, *target).await;
        assert_refusal(
            &reply,
            &format!("a decrease {case} (from {initial} to {target})"),
            "targetNumberOfBrokerNodes",
            BROKER_COUNT_DECREASE_MESSAGE,
        );
        assert!(
            reply.body["clusterOperationArn"].is_null(),
            "case {case}: a refused update must report no operation, got {}",
            reply.body
        );

        // The cluster is exactly as it was: same state, same brokers, same
        // version, and no operation in flight.
        let after = emulator.describe(&arn).await;
        assert_eq!(
            (
                after.at("/clusterInfo/state").as_str(),
                after.at("/clusterInfo/numberOfBrokerNodes").as_i64(),
                after.at("/clusterInfo/currentVersion").as_str(),
            ),
            (Some("ACTIVE"), Some(*initial), Some(version.as_str())),
            "case {case}: a refused decrease must leave the cluster untouched, got {}",
            after.body
        );
        assert_eq!(
            after.at("/clusterInfo/activeOperationArn"),
            &Value::Null,
            "case {case}: a refused decrease must start no operation, got {}",
            after.at("/clusterInfo/activeOperationArn")
        );
    }

    // The control: from the same count, the target the cluster already holds and a
    // target above it are both accepted, and the increase lands.
    let (arn, version) = active_cluster(&emulator, "increase", 2).await;
    let same = update_broker_count(&emulator, &arn, &version, 2).await;
    assert_eq!(
        same.status, 200,
        "case \"the count the cluster already holds\": expected 200, got {} {}",
        same.status, same.body
    );
    sleep_past_update();
    let version = emulator
        .describe(&arn)
        .await
        .at("/clusterInfo/currentVersion")
        .as_str()
        .expect("DescribeCluster reports a currentVersion")
        .to_string();
    let up = update_broker_count(&emulator, &arn, &version, 4).await;
    assert_eq!(
        up.status, 200,
        "case \"a target above the count the cluster holds\": expected 200, got {} {}",
        up.status, up.body
    );
    sleep_past_update();
    let after = emulator.describe(&arn).await;
    assert_eq!(
        after.at("/clusterInfo/numberOfBrokerNodes").as_i64(),
        Some(4),
        "case \"a target above the count the cluster holds\": expected the increase to land, \
         got {}",
        after.body
    );
}
