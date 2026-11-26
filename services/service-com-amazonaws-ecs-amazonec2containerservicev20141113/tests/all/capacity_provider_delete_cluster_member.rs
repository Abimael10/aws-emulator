// SPDX-License-Identifier: CC0-1.0

//! What `DeleteCapacityProvider`'s optional `cluster` member does — and does NOT — decide,
//! driven end to end over the shared wire harness in [`crate::ecs_wire`] with
//! `DescribeCapacityProviders` as the reader.
//!
//! The rule: a capacity provider is identified account-wide by its own name or ARN —
//! `DeleteCapacityProvider$capacityProvider` is the operation's one required member and its
//! `resourceIdentifier` (`CapacityProviderName`; `smithy_query` query="operation"
//! operation="DeleteCapacityProvider"), and the ARN form a provider is also named by,
//! `capacity-provider/<name>`, carries no cluster segment — so the delete resolves its
//! target from that identifier alone and the optional `cluster`, which only says where a
//! cluster-scoped provider lives, refuses nothing: the delete is answered whether the
//! request names no cluster, a name this account holds no cluster under, a cluster it holds
//! only as a tombstone, another live cluster, or the provider's own.
//!
//! The boundary of that rule is pinned here too: the capacity-provider operations whose
//! SUBJECT is a cluster — creating a provider in one, scoping a read or an update to one,
//! associating providers with one — still resolve their `cluster` and still refuse a
//! reference that names no live cluster of the account.
//!
//! The rows differ along what the rule generalises over: what BACKS the provider and
//! therefore whether it is scoped to a cluster at all (an Auto Scaling group of the
//! caller's, which belongs to the account; capacity Amazon ECS stands up for one cluster),
//! how the delete NAMES the provider (short name, full ARN), and what the `cluster` member
//! carries (nothing, a name no cluster answers to, that name in its ARN form, a tombstone,
//! another live cluster, the provider's own cluster).
//!
//! Nothing here reaches into the state struct: every value asserted is read back through a
//! describe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::{group_arn, managed_instances};
use crate::ecs_wire::{
    create_auto_scaling_group, create_cluster, list, ok, poll_until, refusal, service, string,
    ACCOUNT, SIGNED_REGION,
};

/// The cluster the account holds no record of at all — what the `cluster` member carries in
/// the rows that name a cluster nothing created.
const GHOST_CLUSTER: &str = "ghostcluster";

/// The fixed sentence the operations that DO resolve their `cluster` answer when it names no
/// cluster of the account: the whole message, naming nothing from the request.
const CLUSTER_NOT_FOUND: &str = "Cluster not found.";

/// The ARN form of a capacity provider of this account, which `capacityProvider` accepts
/// beside the short name.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{name}")
}

/// The ARN form of a cluster of this account, which `cluster` accepts beside the short name.
fn cluster_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{name}")
}

/// The `(status, updateStatus)` pair a reported provider carries.
fn lifecycle(provider: &Value, case: &str) -> (String, Option<String>) {
    (
        string(provider, "status", case),
        provider["updateStatus"].as_str().map(str::to_string),
    )
}

/// Describe one capacity provider by name and answer it.
async fn describe_provider(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": [name] }),
        case,
    )
    .await;
    let providers = list(&body, "capacityProviders", case);
    assert_eq!(
        providers.len(),
        1,
        "case {case:?}: expected exactly one described provider, got {body}"
    );
    providers[0].clone()
}

/// One row: the provider the delete names, what backs it, and the `cluster` the request
/// carries.
struct Row {
    case: &'static str,
    provider: &'static str,
    /// The cluster the provider is SCOPED to, for one whose capacity Amazon ECS stands up —
    /// `None` for a provider over an Auto Scaling group, which belongs to the account.
    scope: Option<&'static str>,
    /// What the request's `capacityProvider` carries.
    named: fn(&'static str) -> String,
    /// What the request's `cluster` carries, if anything — a short name or an ARN.
    cluster: Option<fn() -> String>,
}

#[tokio::test]
async fn a_delete_resolves_its_provider_whatever_cluster_the_request_names() {
    let router = service();
    for cluster in ["home", "otherlive", "tombstone"] {
        create_cluster(&router, cluster, "setup").await;
    }
    // A cluster the account holds only as a tombstone: still a record, and no cluster any
    // operation may act IN.
    ok(
        &router,
        "DeleteCluster",
        json!({ "cluster": "tombstone" }),
        "setup",
    )
    .await;

    let rows = [
        Row {
            case: "an account-scoped provider deleted with no cluster named",
            provider: "accountnone",
            scope: None,
            named: |name| name.to_string(),
            cluster: None,
        },
        Row {
            case: "an account-scoped provider deleted through a cluster nothing created",
            provider: "accountghost",
            scope: None,
            named: |name| name.to_string(),
            cluster: Some(|| GHOST_CLUSTER.to_string()),
        },
        Row {
            case: "the same, with that cluster named by its ARN and the provider by its ARN",
            provider: "accountghostarn",
            scope: None,
            named: provider_arn,
            cluster: Some(|| cluster_arn(GHOST_CLUSTER)),
        },
        Row {
            case: "an account-scoped provider deleted through a deleted cluster",
            provider: "accounttombstone",
            scope: None,
            named: |name| name.to_string(),
            cluster: Some(|| "tombstone".to_string()),
        },
        Row {
            case: "an account-scoped provider deleted through another live cluster",
            provider: "accountother",
            scope: None,
            named: |name| name.to_string(),
            cluster: Some(|| "otherlive".to_string()),
        },
        Row {
            case: "a cluster-scoped provider deleted through its own cluster",
            provider: "scopedown",
            scope: Some("home"),
            named: |name| name.to_string(),
            cluster: Some(|| "home".to_string()),
        },
        Row {
            case: "a cluster-scoped provider deleted with no cluster named",
            provider: "scopednone",
            scope: Some("home"),
            named: |name| name.to_string(),
            cluster: None,
        },
        Row {
            case: "a cluster-scoped provider deleted through a cluster nothing created",
            provider: "scopedghost",
            scope: Some("home"),
            named: |name| name.to_string(),
            cluster: Some(|| GHOST_CLUSTER.to_string()),
        },
        Row {
            case: "a cluster-scoped provider deleted through another live cluster",
            provider: "scopedother",
            scope: Some("home"),
            named: provider_arn,
            cluster: Some(|| cluster_arn("otherlive")),
        },
        Row {
            case: "a cluster-scoped provider deleted through a deleted cluster",
            provider: "scopedtombstone",
            scope: Some("home"),
            named: |name| name.to_string(),
            cluster: Some(|| "tombstone".to_string()),
        },
    ];

    // Every provider is created FIRST, so the creates Amazon ECS has to stand capacity up
    // for run against the same clock rather than one after another.
    for row in &rows {
        match row.scope {
            None => {
                let group = format!("asg-{}", row.provider);
                create_auto_scaling_group(&router, &group, "setup").await;
                ok(
                    &router,
                    "CreateCapacityProvider",
                    json!({
                        "name": row.provider,
                        "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn(&group) },
                    }),
                    "setup",
                )
                .await;
            }
            Some(scope) => {
                ok(
                    &router,
                    "CreateCapacityProvider",
                    json!({
                        "name": row.provider,
                        "cluster": scope,
                        "managedInstancesProvider": managed_instances(),
                    }),
                    "setup",
                )
                .await;
            }
        }
    }
    // A provider whose own capacity Amazon ECS stands up is created in flight, so the
    // deletes below are asked of providers that have SETTLED — the condition, not a span.
    for row in &rows {
        let case = row.case;
        poll_until(
            case,
            "the create of the provider to settle",
            || describe_provider(&router, row.provider, case),
            |reported| reported["status"] == json!("ACTIVE"),
        )
        .await;
    }

    for row in &rows {
        let (case, provider) = (row.case, row.provider);
        let before = describe_provider(&router, provider, case).await;
        let mut request = json!({ "capacityProvider": (row.named)(provider) });
        if let Some(cluster) = row.cluster {
            request["cluster"] = json!(cluster());
        }

        let answer = ok(&router, "DeleteCapacityProvider", request.clone(), case).await;
        let deleted = answer["capacityProvider"].clone();
        assert_eq!(
            lifecycle(&deleted, case),
            (
                "INACTIVE".to_string(),
                Some("DELETE_IN_PROGRESS".to_string())
            ),
            "case {case:?}: expected the provider retired with its release in flight, got \
             {deleted}"
        );
        // The delete reported the STORED provider: what identifies it is what the describe
        // answered before the call.
        for member in ["capacityProviderArn", "name", "type"] {
            assert_eq!(
                deleted[member], before[member],
                "case {case:?}: a delete must not change {member}, got {deleted}"
            );
        }
        // And the retirement is state the delete WROTE, not a status it invented: the
        // describe answers the same pair.
        let read = describe_provider(&router, provider, case).await;
        assert_eq!(
            lifecycle(&read, case),
            lifecycle(&deleted, case),
            "case {case:?}: the describe must report what the delete stored, got {read}"
        );
    }
}

/// One row of the boundary table: the operation whose subject IS a cluster, and the request
/// that names a cluster nothing created.
struct SiblingRow {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
}

#[tokio::test]
async fn the_capacity_provider_operations_scoped_to_a_cluster_still_refuse_a_missing_one() {
    // The counter-case to the rule above: an unresolvable `cluster` is inert only for the
    // delete, whose subject is the provider. Every capacity-provider operation that acts IN
    // or ON a cluster resolves it, and a reference naming no live cluster of the account is
    // refused with HTTP 400 ClusterNotFoundException and the fixed not-found sentence.
    let router = service();
    create_auto_scaling_group(&router, "asg-boundary", "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "boundary",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-boundary") },
        }),
        "setup",
    )
    .await;

    let rows = [
        SiblingRow {
            case: "a create of a provider in a cluster nothing created",
            operation: "CreateCapacityProvider",
            request: || {
                json!({
                    "name": "wouldbecreated",
                    "cluster": GHOST_CLUSTER,
                    "managedInstancesProvider": managed_instances(),
                })
            },
        },
        SiblingRow {
            case: "a read scoped to a cluster nothing created",
            operation: "DescribeCapacityProviders",
            request: || json!({ "capacityProviders": ["boundary"], "cluster": GHOST_CLUSTER }),
        },
        SiblingRow {
            case: "an update scoped to a cluster nothing created",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": "boundary",
                    "cluster": GHOST_CLUSTER,
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
        },
        SiblingRow {
            case: "an association onto a cluster nothing created",
            operation: "PutClusterCapacityProviders",
            request: || {
                json!({
                    "cluster": GHOST_CLUSTER,
                    "capacityProviders": ["boundary"],
                    "defaultCapacityProviderStrategy": [],
                })
            },
        },
    ];

    for row in &rows {
        let case = row.case;
        let (code, message) = refusal(&router, row.operation, (row.request)(), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ClusterNotFoundException", CLUSTER_NOT_FOUND),
            "case {case:?}: expected the cluster-not-found refusal, got {code:?} / {message:?}"
        );
    }

    // And the provider those refusals named is exactly where its create left it, so each
    // refusal was the cluster being resolved and nothing being written.
    let case = "a refused call wrote nothing onto the provider";
    let read = describe_provider(&router, "boundary", case).await;
    assert_eq!(
        lifecycle(&read, case),
        ("ACTIVE".to_string(), None),
        "case {case:?}: expected the provider ACTIVE with no update against it, got {read}"
    );
}
