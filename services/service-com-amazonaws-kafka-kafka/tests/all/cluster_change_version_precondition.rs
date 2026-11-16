// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a call that changes a cluster's configuration answers when both
//! of its preconditions could refuse it, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared harness in
//! `tests/all/kafka_harness.rs`).
//!
//! RULE under test: a cluster's `CurrentVersion` is the optimistic-concurrency
//! token a change is accepted FROM, and accepting one SPENDS it — "A successful
//! operation will then generate a new version" is what every update request's
//! `CurrentVersion` documents, and the operation succeeds when the service accepts
//! it, so the version it generates is the cluster's from that instant. The version
//! pre-condition is therefore settled FIRST: a caller quoting a version the
//! cluster is no longer at is told the version is not current, whatever state the
//! cluster is in, and only a call quoting the version the cluster IS at is answered
//! on the state it is in. The two answers tell a client opposite things — read the
//! version again and retry with that, against retry this same request once the
//! cluster has settled — which is what makes the order part of the contract.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the
//! cluster-update family quotes the version (eight of them, over three different
//! kinds of change), WHY the cluster would otherwise refuse (still being
//! provisioned, or already applying a change it accepted), and — for the race the
//! rule exists for — the pair of targets two concurrent callers ask for.
//!
//! The two refusal sentences are spelled out here as Amazon MSK words them rather
//! than built from the service's own constants, so this suite pins the wording
//! instead of agreeing with whatever the code says.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, poll_until_active, ClusterRequest, Emulator, Reply};

/// How Amazon MSK refuses a call that changes a cluster which is not in a state
/// that call is accepted from: fixed text end to end, including the space before
/// the colon, with only the list of states the call IS accepted from determined by
/// the operation.
const STATE_REFUSAL: &str =
    "This operation is only valid for resources that are in one of the following states :[ACTIVE]";

/// How it refuses a call quoting a version the cluster will not accept its next
/// change from: fixed text end to end, carrying neither the quoted version nor the
/// cluster's own, with the typographic apostrophe (U+2019) the service sends.
const VERSION_REFUSAL: &str =
    "The version of the cluster isn\u{2019}t current. Check the current version and try again.";

/// The member a stale-version refusal names as the parameter at fault, as the wire
/// spells it on every request of the family.
const VERSION_MEMBER: &str = "currentVersion";

/// The member a state refusal names, as the cluster requests spell it.
const CLUSTER_MEMBER: &str = "ClusterArn";

/// One operation of the cluster-update family: the name the model gives it, the
/// route the model binds it to under the cluster, and the body it submits beside
/// `currentVersion`.
struct Update {
    operation: &'static str,
    /// The HTTP method the model binds the route to — `PATCH` for the one
    /// operation of the family the model binds that way, `PUT` for the rest.
    method: &'static str,
    suffix: &'static str,
    body: fn() -> Value,
}

/// Every operation of this service that CHANGES a provisioned cluster's
/// configuration by quoting its `CurrentVersion` in a body
/// (`smithy_query(query="operation", operation="UpdateBrokerCount,UpdateStorage,
/// UpdateBrokerStorage,UpdateBrokerType,UpdateMonitoring,UpdateRebalancing,
/// UpdateConnectivity,UpdateSecurity,UpdateClusterConfiguration")`), each with a
/// body whose every OTHER member is one the operation accepts — so the only thing
/// that can decide a row's answer is the version it quotes and the cluster it
/// addresses.
///
/// `UpdateClusterConfiguration` names a configuration revision, which the caller
/// supplies, so its row is built separately below; `UpdateClusterKafkaVersion`
/// names a target version off the cluster's own compatibility ladder and is left
/// to the suite that owns that ladder.
fn updates() -> Vec<Update> {
    vec![
        Update {
            operation: "UpdateBrokerCount",
            method: "PUT",
            suffix: "/nodes/count",
            body: || json!({ "targetNumberOfBrokerNodes": 6 }),
        },
        Update {
            operation: "UpdateStorage",
            method: "PUT",
            suffix: "/storage",
            body: || json!({ "volumeSizeGB": 1100 }),
        },
        Update {
            operation: "UpdateBrokerStorage",
            method: "PUT",
            suffix: "/nodes/storage",
            body: || {
                json!({
                    "targetBrokerEBSVolumeInfo": [
                        { "kafkaBrokerNodeId": "ALL", "volumeSizeGB": 1200 }
                    ]
                })
            },
        },
        Update {
            operation: "UpdateBrokerType",
            method: "PUT",
            suffix: "/nodes/type",
            body: || json!({ "targetInstanceType": "kafka.m5.xlarge" }),
        },
        Update {
            operation: "UpdateMonitoring",
            method: "PUT",
            suffix: "/monitoring",
            body: || json!({ "enhancedMonitoring": "PER_BROKER" }),
        },
        Update {
            operation: "UpdateRebalancing",
            method: "PUT",
            suffix: "/rebalancing",
            body: || json!({ "rebalancing": { "status": "PAUSED" } }),
        },
        Update {
            operation: "UpdateConnectivity",
            method: "PUT",
            suffix: "/connectivity",
            body: || json!({ "connectivityInfo": { "publicAccess": { "type": "DISABLED" } } }),
        },
        Update {
            operation: "UpdateSecurity",
            method: "PATCH",
            suffix: "/security",
            body: || json!({ "clientAuthentication": { "sasl": { "scram": { "enabled": true } } } }),
        },
    ]
}

impl Update {
    /// This operation's request, quoting `version`.
    fn request(&self, cluster_arn: &str, version: &str) -> (String, Value) {
        let mut body = (self.body)();
        body[VERSION_MEMBER] = json!(version);
        (
            format!("/v1/clusters/{}{}", pct(cluster_arn), self.suffix),
            body,
        )
    }

    async fn call(&self, emulator: &Emulator, cluster_arn: &str, version: &str) -> Reply {
        let (uri, body) = self.request(cluster_arn, version);
        emulator.call(self.method, &uri, Some(body)).await
    }
}

/// Assert `reply` is one of the two refusals, named by `message`, with the status,
/// the code and the parameter that refusal names — and that nothing of the quoted
/// version reached the caller, since both sentences are fixed text.
fn assert_refusal(reply: &Reply, case: &str, message: &str, parameter: &str, quoted: &str) {
    assert_eq!(
        (
            reply.status,
            reply.error_code(),
            reply.error_message(),
            reply.at("/invalidParameter").as_str(),
        ),
        (400, "BadRequestException", message, Some(parameter)),
        "case {case}: expected the refusal {message:?} naming {parameter}, got {} {}",
        reply.status,
        reply.body
    );
    assert!(
        !reply.error_message().contains(quoted),
        "case {case}: the refusal must not echo the quoted version {quoted:?}, got {:?}",
        reply.error_message()
    );
}

/// The cluster's `State` and `CurrentVersion` as `DescribeCluster` reports them.
async fn reported(emulator: &Emulator, arn: &str, case: &str) -> (String, String) {
    let reply = emulator.describe(arn).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: DescribeCluster must answer, got {} {}",
        reply.status, reply.body
    );
    let member = |pointer: &str| {
        reply
            .at(pointer)
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: DescribeCluster reported no {pointer}"))
            .to_string()
    };
    (
        member("/clusterInfo/state"),
        member("/clusterInfo/currentVersion"),
    )
}

/// RULE, first half: a cluster that is still being PROVISIONED has not spent its
/// version, so a call quoting that version is answered on the state the cluster is
/// in — every operation of the family, in the one sentence the service words that
/// condition in, and with nothing about the cluster changed.
#[tokio::test]
async fn a_change_quoting_the_version_a_creating_cluster_holds_is_refused_on_its_state() {
    let emulator = Emulator::new();
    for update in updates() {
        let case = format!("{}/still provisioning", update.operation);
        // A cluster of its own per row, read and then called straight away: the
        // state this half of the rule is about is the one a cluster is in before
        // the clock owes it ACTIVE.
        let arn = emulator
            .create_ok(&ClusterRequest::named(&format!(
                "creating-{}",
                update.operation.to_lowercase()
            )))
            .await;
        let (state, version) = reported(&emulator, &arn, &case).await;
        assert_eq!(
            state, "CREATING",
            "case {case}: a just-created cluster is reported CREATING, got {state}"
        );
        let refused = update.call(&emulator, &arn, &version).await;
        assert_refusal(&refused, &case, STATE_REFUSAL, CLUSTER_MEMBER, &version);

        // The refusal started nothing and spent nothing: the cluster settles on
        // its own, at the version it was already at.
        let settled = poll_until_active(&emulator, &arn, &case).await;
        assert_eq!(
            settled, version,
            "case {case}: a refused change must leave the cluster's version alone"
        );
        let after = emulator.describe(&arn).await;
        assert_eq!(
            after.at("/clusterInfo/activeOperationArn"),
            &Value::Null,
            "case {case}: a refused change must start no cluster operation, got {}",
            after.body
        );
    }
}

/// RULE, second half: accepting a change SPENDS the version it was accepted from,
/// so a second call quoting that same version is answered on the VERSION — by
/// every operation of the family, whether the accepted change is still rolling out
/// or has already landed the cluster at its next version, which is what makes this
/// half depend on no clock at all.
#[tokio::test]
async fn a_change_quoting_a_version_an_accepted_change_spent_is_refused_on_the_version() {
    let emulator = Emulator::new();
    let arn = emulator
        .create_ok(&ClusterRequest::named("spent-version"))
        .await;
    let spent = poll_until_active(&emulator, &arn, "spent version").await;

    // One accepted change, which spends the version every row below quotes.
    let accepted = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/monitoring", pct(&arn)),
            Some(json!({ "currentVersion": spent, "enhancedMonitoring": "PER_TOPIC_PER_BROKER" })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "expected the update quoting the cluster's own version to be accepted, got {} {}",
        accepted.status, accepted.body
    );

    for update in updates() {
        let case = format!("{}/a version an accepted change spent", update.operation);
        let refused = update.call(&emulator, &arn, &spent).await;
        assert_refusal(&refused, &case, VERSION_REFUSAL, VERSION_MEMBER, &spent);
    }

    // The change that WAS accepted still lands, and the version it generated is
    // one the next change is accepted from.
    let landed = poll_until_active(&emulator, &arn, "spent version").await;
    assert_ne!(
        landed, spent,
        "a landed change must give the cluster a new version, got {landed:?} twice"
    );
    let next = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/rebalancing", pct(&arn)),
            Some(json!({ "currentVersion": landed, "rebalancing": { "status": "PAUSED" } })),
        )
        .await;
    assert_eq!(
        next.status, 200,
        "expected the version the landed change generated to be accepted, got {} {}",
        next.status, next.body
    );
}

/// RULE, for the race it exists for: two callers that quote ONE version admit
/// exactly one change — the winner answers a `ClusterOperationArn` every
/// cluster-operation reader resolves and the cluster reaches its next version,
/// while the loser is told its version is not current rather than that the cluster
/// the winner just moved is no longer accepting changes.
///
/// The rows differ along the pair of targets the two callers ask for, across the
/// published range of `UpdateBrokerCount`'s `TargetNumberOfBrokerNodes`
/// (`__integerMin1Max15`): either side of the cluster's own count, both above it,
/// and the two published bounds.
#[tokio::test]
async fn two_concurrent_changes_from_one_version_admit_exactly_one() {
    let emulator = Emulator::new();
    // `(case, the two targets)`. The cluster each row races against starts with
    // three brokers, so the pairs span targets below, above and at both bounds.
    let rows: Vec<(&str, [i64; 2])> = vec![
        ("either side of the cluster's own count", [1, 6]),
        ("both above it", [4, 6]),
        ("the published bounds", [15, 1]),
    ];

    for (case, targets) in rows {
        let arn = emulator
            .create_ok(&ClusterRequest::named(&format!(
                "race-{}",
                targets[0] * 100 + targets[1]
            )))
            .await;
        let version = poll_until_active(&emulator, &arn, case).await;
        let uri = format!("/v1/clusters/{}/nodes/count", pct(&arn));
        let scale = |target: i64| {
            emulator.call(
                "PUT",
                &uri,
                Some(json!({
                    "currentVersion": version,
                    "targetNumberOfBrokerNodes": target,
                })),
            )
        };
        let (first, second) = ::tokio::join!(scale(targets[0]), scale(targets[1]));

        let replies = [(targets[0], &first), (targets[1], &second)];
        let admitted: Vec<usize> = (0..replies.len())
            .filter(|index| replies[*index].1.status == 200)
            .collect();
        assert_eq!(
            admitted.len(),
            1,
            "case {case}: exactly one of two changes from one version is admitted, got {} and {}",
            first.body,
            second.body
        );
        let (winning_target, winner) = replies[admitted[0]];
        let (losing_target, loser) = replies[1 - admitted[0]];
        let operation = winner
            .at("/clusterOperationArn")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case}: the admitted change reported no clusterOperationArn")
            })
            .to_string();
        // The operation the winner answered is a record every reader resolves.
        let described = emulator
            .call("GET", &format!("/v1/operations/{}", pct(&operation)), None)
            .await;
        assert_eq!(
            (
                described.status,
                described.at("/clusterOperationInfo/clusterArn").as_str(),
                described
                    .at("/clusterOperationInfo/targetClusterInfo/numberOfBrokerNodes")
                    .as_i64(),
            ),
            (200, Some(arn.as_str()), Some(winning_target)),
            "case {case}: expected the winner's operation to read back for this cluster, got {} {}",
            described.status,
            described.body
        );

        // The loser is told about its VERSION, not about the state the winner's
        // change left the cluster in.
        assert_refusal(
            loser,
            &format!("{case}/the losing caller (target {losing_target})"),
            VERSION_REFUSAL,
            VERSION_MEMBER,
            &version,
        );

        // And the change that was admitted is the one that lands: the cluster
        // reaches a NEW version with the winner's broker count.
        let landed = poll_until_active(&emulator, &arn, case).await;
        assert_ne!(
            landed, version,
            "case {case}: the admitted change must advance the cluster's version"
        );
        let after = emulator.describe(&arn).await;
        assert_eq!(
            after.at("/clusterInfo/numberOfBrokerNodes").as_i64(),
            Some(winning_target),
            "case {case}: expected the admitted change's broker count, got {}",
            after.body
        );
    }
}
