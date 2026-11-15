// SPDX-License-Identifier: CC0-1.0

//! What a cluster reports while the capacity providers a request ASSOCIATED with it are
//! being attached, over a table of the ways that association is made.
//!
//! The rule under test: associating capacity providers with a cluster updates the cluster's
//! ATTACHMENTS — `Cluster$attachments`: "When using a capacity provider with a cluster, the
//! capacity provider and associated resources are returned as cluster attachments" — so
//! whichever call makes that association leaves `Cluster$attachmentsStatus`
//! `UPDATE_IN_PROGRESS` ("the available capacity providers for the cluster are updating")
//! and the CLOCK takes it to `UPDATE_COMPLETE` ("the capacity providers have successfully
//! updated"), which is the documented way a client waits for an attachment to finish. A
//! cluster that associates no capacity provider updates no attachment and reports no status
//! at all.
//!
//! The rows span HOW the association is made — a create naming the providers, a create that
//! also names a default strategy over them, and a later `PutClusterCapacityProviders` —
//! plus the control that associates none.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, and every wait polls for the CONDITION rather than for a time, so
//! nothing here depends on how fast the host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{create_auto_scaling_group, list, ok, service, string};

/// `Cluster$attachmentsStatus` while the association is being applied.
const UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";

/// `Cluster$attachmentsStatus` once it has been.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// How long a wait for the attachments update keeps re-reading the cluster. Generous — the
/// wait stops the instant its condition holds — so a loaded test machine cannot fail it.
const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// How often that wait re-reads the cluster.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: how a cluster comes to have its capacity providers associated.
struct Row {
    /// The case name.
    case: &'static str,
    /// The cluster the row works in.
    cluster: &'static str,
    /// The `capacityProviders` the CREATE carries.
    created_with: &'static [&'static str],
    /// Whether the create also names a default strategy over them.
    with_default_strategy: bool,
    /// The `capacityProviders` a later `PutClusterCapacityProviders` carries, or `None`
    /// when the row makes no later association.
    put_later: Option<&'static [&'static str]>,
    /// Whether the row expects the cluster's attachments to be updating at all.
    expects_update: bool,
}

/// The capacity providers these rows associate.
const PROVIDERS: [&str; 2] = ["attach-one", "attach-two"];

/// The cluster a describe reports, with `ATTACHMENTS` asked for.
async fn described(router: &MultiServiceRouter, cluster: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeClusters",
        json!({ "clusters": [cluster], "include": ["ATTACHMENTS"] }),
        case,
    )
    .await;
    let clusters = list(&body, "clusters", case);
    assert_eq!(
        clusters.len(),
        1,
        "case {case:?}: expected exactly one cluster back, got {body}"
    );
    clusters[0].clone()
}

/// Re-read `cluster` until its `attachmentsStatus` is `UPDATE_COMPLETE`, and answer how it
/// read.
async fn settled(router: &MultiServiceRouter, cluster: &str, case: &str) -> Value {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = described(router, cluster, case).await;
        if read["attachmentsStatus"] == json!(UPDATE_COMPLETE) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected attachmentsStatus to reach {UPDATE_COMPLETE}, got {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn an_association_leaves_the_clusters_attachments_updating_until_the_clock_completes_it() {
    let rows: [Row; 4] = [
        Row {
            case: "a create that associates one provider",
            cluster: "attachcreateone",
            created_with: &["attach-one"],
            with_default_strategy: false,
            put_later: None,
            expects_update: true,
        },
        Row {
            case: "a create that associates two and defaults over them",
            cluster: "attachcreatetwo",
            created_with: &["attach-one", "attach-two"],
            with_default_strategy: true,
            put_later: None,
            expects_update: true,
        },
        Row {
            case: "a later association on a cluster created without providers",
            cluster: "attachlater",
            created_with: &[],
            with_default_strategy: false,
            put_later: Some(&["attach-two"]),
            expects_update: true,
        },
        Row {
            case: "a cluster that associates no provider at all",
            cluster: "attachnone",
            created_with: &[],
            with_default_strategy: false,
            put_later: None,
            expects_update: false,
        },
    ];

    let router = service();
    for provider in PROVIDERS {
        create_auto_scaling_group(&router, &format!("asg-{provider}"), "setup").await;
        ok(
            &router,
            "CreateCapacityProvider",
            json!({
                "name": provider,
                "autoScalingGroupProvider": { "autoScalingGroupArn": format!("asg-{provider}") },
            }),
            "setup",
        )
        .await;
    }

    for row in rows {
        let case = row.case;
        let mut request = json!({ "clusterName": row.cluster });
        if !row.created_with.is_empty() {
            request["capacityProviders"] = json!(row.created_with);
            if row.with_default_strategy {
                request["defaultCapacityProviderStrategy"] = json!(row
                    .created_with
                    .iter()
                    .map(|provider| json!({ "capacityProvider": provider, "weight": 1 }))
                    .collect::<Vec<Value>>());
            }
        }
        let created = ok(&router, "CreateCluster", request, case).await;
        let created = &created["cluster"];

        if row.created_with.is_empty() {
            // Nothing was associated, so no attachment is updating and the member is not
            // reported at all — "not updating" and "updating" stay different answers.
            assert_eq!(
                created["attachmentsStatus"],
                Value::Null,
                "case {case:?}: a create that associates no provider reports no \
                 attachmentsStatus, got {created}"
            );
            let read = described(&router, row.cluster, case).await;
            assert_eq!(
                read["attachmentsStatus"],
                Value::Null,
                "case {case:?}: and neither does a read of it, got {read}"
            );
        } else {
            assert_eq!(
                string(created, "attachmentsStatus", case),
                UPDATE_IN_PROGRESS,
                "case {case:?}: the create that associated the providers reports the \
                 attachments updating, got {created}"
            );
        }

        if let Some(providers) = row.put_later {
            let put = ok(
                &router,
                "PutClusterCapacityProviders",
                json!({
                    "cluster": row.cluster,
                    "capacityProviders": providers,
                    "defaultCapacityProviderStrategy": providers
                        .iter()
                        .map(|provider| json!({ "capacityProvider": provider, "weight": 1 }))
                        .collect::<Vec<Value>>(),
                }),
                case,
            )
            .await;
            assert_eq!(
                string(&put["cluster"], "attachmentsStatus", case),
                UPDATE_IN_PROGRESS,
                "case {case:?}: the association reports the attachments updating, got {put}"
            );
        }

        if !row.expects_update {
            continue;
        }

        // The documented wait: the status the association started reaches
        // `UPDATE_COMPLETE` on the clock, whoever reads and whether or not anybody does.
        let read = settled(&router, row.cluster, case).await;
        // The association itself is what the cluster reports beside the status, so the
        // status belongs to the providers this row attached.
        let expected: Vec<Value> = row
            .put_later
            .unwrap_or(row.created_with)
            .iter()
            .map(|provider| json!(provider))
            .collect();
        assert_eq!(
            list(&read, "capacityProviders", case),
            expected,
            "case {case:?}: the completed update is the association it applied, got {read}"
        );
        // And it STAYS complete: the status is a property of the cluster, not of the read.
        let again = described(&router, row.cluster, case).await;
        assert_eq!(
            string(&again, "attachmentsStatus", case),
            UPDATE_COMPLETE,
            "case {case:?}: a second read reports the same completed update, got {again}"
        );
    }
}
