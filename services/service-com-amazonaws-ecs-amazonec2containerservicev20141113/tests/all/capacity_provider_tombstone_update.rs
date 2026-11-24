// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateCapacityProvider` naming the TOMBSTONE a delete left behind is refused
//! with, as against one naming an identifier the account answers to with nothing at all.
//!
//! The rule under test: a capacity provider a delete retired is KEPT — `CapacityProvider$status`
//! `INACTIVE` is "when a capacity provider is successfully deleted", and the provider stays
//! describable from there on — so an update naming it resolves to a provider that HAS BEEN
//! deleted, which the refusal states, while only a reference the account holds no record
//! under at all is refused for naming no provider. Both refusals are HTTP 400 and
//! `ClientException` (the class Amazon ECS answers an identifier it cannot act on in), and
//! neither writes anything: the tombstone keeps the status and the configuration the last
//! ACCEPTED call left on it.
//!
//! That distinction is what a client reconciling a delete of its own against an update still
//! in flight keys on — a provider it deleted is not to be retried under the same name, while
//! an identifier that names nothing is a name to correct and send again.
//!
//! The rows differ along what the rule generalises over: HOW the update names the provider
//! (its short name, its full ARN), WHICH class of configuration the update carries (the Auto
//! Scaling group binding's own members, a `managedScaling` block, the managed-instances
//! configuration), what BACKS the retired provider (an Auto Scaling group of the caller's,
//! capacity Amazon ECS stands up for one cluster), whether the request names a `cluster`, and
//! whether the name resolves to a tombstone or to nothing — plus, outside the table, whether
//! the release the delete left in flight has finished yet, since the same refusal is owed
//! either way.
//!
//! Every value asserted here is read back through `DescribeCapacityProviders` over the shared
//! wire harness in [`crate::ecs_wire`], so nothing reaches into the state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::{group_arn, managed_instances};
use crate::ecs_wire::{
    create_auto_scaling_group, create_cluster, list, ok, poll_until, refusal, service, string,
    ACCOUNT, SIGNED_REGION,
};

/// The class both refusals are answered in: an identifier Amazon ECS cannot act on is a
/// client action refused, not a parameter value it could not read
/// (`InvalidParameterException`).
const CLIENT_EXCEPTION: &str = "ClientException";

/// The whole sentence an update of a provider this account DELETED earns. Fixed text for the
/// class: it names neither the provider nor the cluster the request carried.
const DELETED_PROVIDER: &str = "The capacity provider could not be updated because it has \
                                been deleted.";

/// The whole sentence an update whose reference names NO record of the account earns, which
/// is the other half of the rule — the refusal a deleted provider must no longer collapse
/// into.
const MISSING_PROVIDER: &str = "The specified capacity provider does not exist. Specify a \
                                valid name or ARN and try again.";

/// The cluster the managed-instances provider of the setup is scoped to.
const CLUSTER: &str = "tombstones";

/// The ARN form of a capacity provider of this account, which a reference to one accepts
/// beside the short name.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{name}")
}

/// Describe one capacity provider by name and return it, with its tags.
async fn describe_provider(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": [name], "include": ["TAGS"] }),
        case,
    )
    .await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: a retired provider is still described, not failed, got {body}"
    );
    let providers = list(&body, "capacityProviders", case);
    assert_eq!(
        providers.len(),
        1,
        "case {case:?}: expected exactly one described provider, got {body}"
    );
    providers[0].clone()
}

/// The same provider once the release its delete left in flight has finished: the end of a
/// delete is owed by the CLOCK, so this waits for the condition rather than for a span.
async fn settled_tombstone(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    poll_until(
        case,
        &format!("the delete of {name} to finish"),
        || describe_provider(router, name, case),
        |provider| {
            provider["status"] == json!("INACTIVE")
                && provider["updateStatus"] == json!("DELETE_COMPLETE")
        },
    )
    .await
}

/// Create a capacity provider over an Auto Scaling group of the caller's, creating that group
/// through Auto Scaling's own API first.
async fn create_group_provider(
    router: &MultiServiceRouter,
    name: &str,
    group: &str,
    managed_scaling: Option<Value>,
) {
    create_auto_scaling_group(router, group, "setup").await;
    let mut binding = json!({ "autoScalingGroupArn": group_arn(group) });
    if let Some(managed_scaling) = managed_scaling {
        binding["managedScaling"] = managed_scaling;
    }
    ok(
        router,
        "CreateCapacityProvider",
        json!({ "name": name, "autoScalingGroupProvider": binding }),
        "setup",
    )
    .await;
}

/// Retire a provider, naming the cluster it is scoped to when it has one.
async fn delete_provider(router: &MultiServiceRouter, name: &str, cluster: Option<&str>) {
    let mut request = json!({ "capacityProvider": name });
    if let Some(cluster) = cluster {
        request["cluster"] = json!(cluster);
    }
    ok(router, "DeleteCapacityProvider", request, "setup").await;
}

/// One row of the refusal table: the update request as the row sends it, the sentence it must
/// earn, and the provider whose stored record the refusal must leave exactly as it stands.
struct StaleUpdateCase {
    case: &'static str,
    request: Value,
    message: &'static str,
    stored: Option<&'static str>,
}

#[tokio::test]
async fn an_update_naming_a_deleted_capacity_provider_reports_it_as_deleted() {
    let router = service();
    create_cluster(&router, CLUSTER, "setup").await;
    create_group_provider(
        &router,
        "byname",
        "asg-byname",
        Some(json!({ "status": "ENABLED", "targetCapacity": 70 })),
    )
    .await;
    create_group_provider(&router, "byarn", "asg-byarn", None).await;
    // A provider whose own capacity Amazon ECS stands up is scoped to one cluster, so it is
    // retired — and would be updated — through that cluster.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scoped",
            "cluster": CLUSTER,
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;
    for (provider, cluster) in [
        ("byname", None),
        ("byarn", None),
        ("scoped", Some(CLUSTER)),
    ] {
        delete_provider(&router, provider, cluster).await;
        // Each row reads its provider before and after the refusal, so the suite lets the
        // release the delete left in flight finish first: what the rows are about is what the
        // REFUSED call wrote, not what the clock owed anyway.
        settled_tombstone(&router, provider, "setup").await;
    }

    let rows = vec![
        StaleUpdateCase {
            case: "a tombstone named by its short name, updated with a managedScaling block",
            request: json!({
                "name": "byname",
                "autoScalingGroupProvider": {
                    "managedScaling": { "status": "DISABLED" },
                },
            }),
            message: DELETED_PROVIDER,
            stored: Some("byname"),
        },
        StaleUpdateCase {
            case: "the same tombstone named by its full ARN, updated with the binding's own \
                   members",
            request: json!({
                "name": provider_arn("byarn"),
                "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
            }),
            message: DELETED_PROVIDER,
            stored: Some("byarn"),
        },
        StaleUpdateCase {
            case: "a cluster-scoped tombstone updated through its own cluster",
            request: json!({
                "name": "scoped",
                "cluster": CLUSTER,
                "managedInstancesProvider": managed_instances(),
            }),
            message: DELETED_PROVIDER,
            stored: Some("scoped"),
        },
        StaleUpdateCase {
            case: "a short name the account holds no record under",
            request: json!({
                "name": "ghost",
                "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
            }),
            message: MISSING_PROVIDER,
            stored: None,
        },
        StaleUpdateCase {
            case: "an ARN the account holds no record under",
            request: json!({
                "name": provider_arn("ghostarn"),
                "autoScalingGroupProvider": {
                    "managedScaling": { "status": "DISABLED" },
                },
            }),
            message: MISSING_PROVIDER,
            stored: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let before = match row.stored {
            Some(provider) => Some(describe_provider(&router, provider, case).await),
            None => None,
        };

        let (code, message) = refusal(
            &router,
            "UpdateCapacityProvider",
            row.request.clone(),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (CLIENT_EXCEPTION, row.message),
            "case {case:?}: expected {:?}, got {code:?} with {message:?}",
            (CLIENT_EXCEPTION, row.message)
        );

        if let (Some(provider), Some(before)) = (row.stored, before) {
            let after = describe_provider(&router, provider, case).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused update writes nothing, got {after}"
            );
            assert_eq!(
                string(&after, "status", case),
                "INACTIVE",
                "case {case:?}: the tombstone stays INACTIVE, got {after}"
            );
        }
    }

    // A name that resolves to NO record is also one no read reports, which is the other side
    // of what tells the two refusals apart: the tombstone above is still described.
    let case = "a name the account holds no record under is reported by no read either";
    let body = ok(
        &router,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": ["ghost"] }),
        case,
    )
    .await;
    assert!(
        list(&body, "capacityProviders", case).is_empty(),
        "case {case:?}: expected no provider described, got {body}"
    );
    assert_eq!(
        list(&body, "failures", case).len(),
        1,
        "case {case:?}: expected the name reported as one failure, got {body}"
    );
}

#[tokio::test]
async fn the_last_accepted_update_is_what_a_deleted_capacity_provider_keeps() {
    // The same rule from the WRITE side: the refusal a stale update earns leaves the provider
    // carrying the configuration the last ACCEPTED update stored, so a client that reads the
    // tombstone after the refusal sees its own last successful write and nothing of the
    // refused one.
    let case = "a provider updated, deleted, then updated again";
    let router = service();
    create_group_provider(
        &router,
        "reconciled",
        "asg-reconciled",
        Some(json!({ "status": "DISABLED" })),
    )
    .await;

    // The positive control: while the provider is live the identical shape of request is
    // ANSWERED, and the configuration it carried is what the provider then reports.
    let updated = ok(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "reconciled",
            "autoScalingGroupProvider": {
                "managedScaling": {
                    "status": "ENABLED",
                    "targetCapacity": 100,
                    "minimumScalingStepSize": 1,
                    "maximumScalingStepSize": 1,
                },
            },
        }),
        case,
    )
    .await;
    let scaling = updated["capacityProvider"]["autoScalingGroupProvider"]["managedScaling"].clone();
    assert_eq!(
        scaling["status"],
        json!("ENABLED"),
        "case {case:?}: expected the accepted update to store ENABLED managed scaling, got \
         {updated}"
    );

    delete_provider(&router, "reconciled", None).await;
    let retired = settled_tombstone(&router, "reconciled", case).await;

    let (code, message) = refusal(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "reconciled",
            "autoScalingGroupProvider": { "managedScaling": { "status": "DISABLED" } },
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (CLIENT_EXCEPTION, DELETED_PROVIDER),
        "case {case:?}: expected {:?}, got {code:?} with {message:?}",
        (CLIENT_EXCEPTION, DELETED_PROVIDER)
    );

    let after = describe_provider(&router, "reconciled", case).await;
    assert_eq!(
        after, retired,
        "case {case:?}: the refused update changes nothing about the tombstone, got {after}"
    );
    assert_eq!(
        after["autoScalingGroupProvider"]["managedScaling"]["status"],
        json!("ENABLED"),
        "case {case:?}: expected the last accepted update's managed scaling to stand, got \
         {after}"
    );
    assert_eq!(
        after["autoScalingGroupProvider"]["managedScaling"]["targetCapacity"],
        json!(100),
        "case {case:?}: expected the last accepted update's target capacity to stand, got \
         {after}"
    );
}

#[tokio::test]
async fn a_release_still_in_flight_is_already_a_deleted_capacity_provider() {
    // The refusal is owed from the instant the delete is ACCEPTED, not from the instant the
    // release it left in flight finishes: the provider is `INACTIVE` straight away ("when a
    // capacity provider is successfully deleted, it has an INACTIVE status"), so an update
    // racing the release is refused for the same reason one arriving later is.
    let case = "an update racing the release a delete left in flight";
    let router = service();
    create_group_provider(&router, "racing", "asg-racing", None).await;
    delete_provider(&router, "racing", None).await;

    let (code, message) = refusal(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "racing",
            "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (CLIENT_EXCEPTION, DELETED_PROVIDER),
        "case {case:?}: expected {:?}, got {code:?} with {message:?}",
        (CLIENT_EXCEPTION, DELETED_PROVIDER)
    );

    // And once the clock has finished that release the very same request earns the very same
    // refusal.
    settled_tombstone(&router, "racing", case).await;
    let (code, message) = refusal(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "racing",
            "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (CLIENT_EXCEPTION, DELETED_PROVIDER),
        "case {case:?}: expected the settled tombstone to earn the same refusal, got {code:?} \
         with {message:?}"
    );
}
