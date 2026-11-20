// SPDX-License-Identifier: CC0-1.0

//! RETIRING a capacity provider: `DeleteCapacityProvider` driven end to end over the
//! shared wire harness in [`crate::ecs_wire`], against `DescribeCapacityProviders` and
//! `DescribeClusters` as the readers.
//!
//! The rule under test, as the operation's own contract states it — input
//! `{capacityProvider (required), cluster}`, output `{capacityProvider}`, `@idempotent`,
//! and "Only capacity providers that aren't associated with a cluster can be deleted. To
//! remove a capacity provider from a cluster, you can either use
//! PutClusterCapacityProviders or delete the cluster":
//!
//! * a provider NO live cluster is associated with is retired — answered 200 with
//!   `status` `INACTIVE` ("When a capacity provider is successfully deleted, it has an
//!   `INACTIVE` status") and its delete in flight, which the clock completes;
//! * a provider a live cluster STILL lists is not deleted and the call is answered all the
//!   same: 200, the provider under the status it already had, with `updateStatus`
//!   `DELETE_FAILED` — the one signal a caller gets that an association holds the provider
//!   — and the cluster keeps listing it. Once the association is removed the identical
//!   delete goes through;
//! * the delete is `@idempotent`, so repeating it reports the stored provider and writes
//!   nothing;
//! * the reserved providers Amazon ECS manages for every account cannot be deleted, a name
//!   that resolves to no provider and a `cluster` that resolves to no live cluster are
//!   refused under the errors the model declares, and a cluster-scoped provider is deleted
//!   only through the cluster it is scoped to — and a refused delete writes nothing.
//!
//! The rows differ along what the rule generalises over: how the delete NAMES the provider
//! (short name or full ARN), what BACKS it (an Auto Scaling group of the caller's, one
//! whose scaling Amazon ECS manages, capacity Amazon ECS stands up for a cluster), WHICH
//! cluster holds the association (the one the request names, another one, a deleted one,
//! none at all), and whether the request names a `cluster` at all.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through a
//! describe.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::{group_arn, managed_instances};
use crate::ecs_wire::{
    create_auto_scaling_group, create_cluster, list, ok, refusal, register_definition, service,
    string, ACCOUNT, SIGNED_REGION,
};

/// How often the polls below look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long they keep looking before giving up — generous, since each stops the instant the
/// condition it waits on holds.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// The whole of the sentence a delete of one of the providers Amazon ECS RESERVES earns:
/// the one sentence every operation asked to modify such a provider reports, spanned over
/// both of those operations by the `capacity_providers` suite.
const RESERVED_REFUSAL: &str =
    "The specified capacity provider is reserved and cannot be modified by this operation. \
     Specify a valid name or ARN and try again.";

/// The sentence a name that resolves to no capacity provider of this account earns, in
/// the class Amazon ECS answers an identifier it cannot RESOLVE in: `ClientException`
/// ("it might be specifying an identifier that isn't valid"), as against the
/// `InvalidParameterException` a parameter value it will not accept earns.
const UNRESOLVED_PROVIDER_REFUSAL: &str = "Failed to get info for provided CapacityProvider";

/// The error class that refusal carries.
const UNRESOLVED_PROVIDER_CODE: &str = "ClientException";

/// The sentence a delete of a CLUSTER-SCOPED provider that does not name its own cluster
/// earns, which is the whole of the `cluster` member's own rule.
const CLUSTER_SCOPED_REFUSAL: &str =
    "Managed instances capacity providers are cluster-scoped and can only be deleted from \
     their associated cluster.";

/// The ARN form of a capacity provider of this account, which the `capacityProvider` member
/// accepts beside the short name.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{name}")
}

/// The `(status, updateStatus)` pair a reported provider carries — the pair every outcome
/// of a delete is read off.
fn lifecycle(provider: &Value, case: &str) -> (String, Option<String>) {
    (
        string(provider, "status", case),
        provider["updateStatus"].as_str().map(str::to_string),
    )
}

/// Describe one capacity provider by name and return it.
async fn describe_provider(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": [name], "include": ["TAGS"] }),
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

/// Describe one cluster by name and return it, with the attachments the association left
/// on it (`DescribeClusters$include`: the `ATTACHMENTS` field).
async fn describe_cluster(router: &MultiServiceRouter, cluster: &str, case: &str) -> Value {
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
        "case {case:?}: expected exactly one described cluster, got {body}"
    );
    clusters[0].clone()
}

/// Create one capacity provider over the Auto Scaling group `group`, with managed scaling
/// when `managed_scaling` asks for it.
async fn create_provider(
    router: &MultiServiceRouter,
    name: &str,
    group: &str,
    managed_scaling: bool,
    case: &str,
) {
    create_auto_scaling_group(router, group, case).await;
    let mut provider = json!({ "autoScalingGroupArn": group_arn(group) });
    if managed_scaling {
        provider["managedScaling"] = json!({ "status": "ENABLED", "targetCapacity": 100 });
    }
    ok(
        router,
        "CreateCapacityProvider",
        json!({ "name": name, "autoScalingGroupProvider": provider }),
        case,
    )
    .await;
}

/// Associate exactly `providers` with `cluster`, then wait for the attachments update the
/// association owes to land — a cluster mid-update refuses the next cluster mutation, so a
/// suite that associates and then disassociates waits for the condition rather than for a
/// fixed time.
async fn associate(
    router: &MultiServiceRouter,
    cluster: &str,
    providers: &[&str],
    case: &str,
) {
    ok(
        router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": providers,
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let read = describe_cluster(router, cluster, case).await;
        if read["attachmentsStatus"].as_str() == Some("UPDATE_COMPLETE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the capacity-provider attachments update to land, got \
             {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the delete of `name` reports `DELETE_COMPLETE`, and answer the provider as it
/// then stands. The release of the capacity a retired provider bound onto is owed by the
/// CLOCK, so the wait is on the condition and nothing sleeps in the emulator.
async fn wait_for_delete_complete(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let read = describe_provider(router, name, case).await;
        let (status, update_status) = lifecycle(&read, case);
        // The status a retired provider reports does not move while its release finishes:
        // it is INACTIVE from the delete on, whatever the release is still doing.
        assert_eq!(
            status, "INACTIVE",
            "case {case:?}: a retired provider stays INACTIVE while its delete finishes, \
             got {read}"
        );
        if update_status.as_deref() == Some("DELETE_COMPLETE") {
            return read;
        }
        assert_eq!(
            update_status.as_deref(),
            Some("DELETE_IN_PROGRESS"),
            "case {case:?}: a delete in flight reports itself in progress, got {read}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the delete to finish, got {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the retire table: the provider, how the delete names it, and the `cluster`
/// the request carries.
struct RetireCase {
    case: &'static str,
    provider: &'static str,
    /// What the request's `capacityProvider` carries — a short name or a full ARN.
    named: fn(&'static str) -> String,
    /// The `cluster` the request carries, if any.
    cluster: Option<&'static str>,
}

#[tokio::test]
async fn a_delete_no_cluster_association_holds_retires_the_provider() {
    let router = service();
    create_cluster(&router, "retiring", "setup").await;
    create_cluster(&router, "shortlived", "setup").await;
    create_provider(&router, "retireshort", "asg-retireshort", false, "setup").await;
    create_provider(&router, "retirearn", "asg-retirearn", false, "setup").await;
    create_provider(&router, "retirescaled", "asg-retirescaled", true, "setup").await;
    create_provider(&router, "retiretombstone", "asg-tombstone", false, "setup").await;
    // A provider whose own capacity Amazon ECS stands up, scoped to the cluster it may be
    // used in: its delete is asked through that cluster.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "retirescoped",
            "cluster": "retiring",
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;
    // The association a DELETED cluster leaves behind holds nothing open: "To remove a
    // capacity provider from a cluster, you can either use PutClusterCapacityProviders or
    // delete the cluster."
    associate(&router, "shortlived", &["retiretombstone"], "setup").await;
    ok(
        &router,
        "DeleteCluster",
        json!({ "cluster": "shortlived" }),
        "setup",
    )
    .await;

    // The rows differ along how the delete names the provider, what backs the provider, and
    // whether a cluster is named — and the deleted cluster's row along where the association
    // that no longer holds came from.
    let rows = [
        RetireCase {
            case: "an Auto Scaling group provider named by its short name",
            provider: "retireshort",
            named: |name| name.to_string(),
            cluster: None,
        },
        RetireCase {
            case: "the same provider named by its full ARN",
            provider: "retirearn",
            named: provider_arn,
            cluster: None,
        },
        RetireCase {
            case: "a provider whose Auto Scaling group Amazon ECS scales",
            provider: "retirescaled",
            named: |name| name.to_string(),
            cluster: None,
        },
        RetireCase {
            case: "a provider only a DELETED cluster still lists",
            provider: "retiretombstone",
            named: |name| name.to_string(),
            cluster: None,
        },
        RetireCase {
            case: "a cluster-scoped provider deleted through its own cluster",
            provider: "retirescoped",
            named: |name| name.to_string(),
            cluster: Some("retiring"),
        },
    ];

    for row in &rows {
        let (case, provider) = (row.case, row.provider);
        let before = describe_provider(&router, provider, case).await;
        let mut request = json!({ "capacityProvider": (row.named)(provider) });
        if let Some(cluster) = row.cluster {
            request["cluster"] = json!(cluster);
        }

        let answer = ok(&router, "DeleteCapacityProvider", request.clone(), case).await;
        let deleted = answer["capacityProvider"].clone();
        assert_eq!(
            lifecycle(&deleted, case),
            (
                "INACTIVE".to_string(),
                Some("DELETE_IN_PROGRESS".to_string())
            ),
            "case {case:?}: an accepted delete retires the provider and leaves its release \
             in flight, got {deleted}"
        );
        // The delete reports the STORED record: everything that identifies the provider is
        // what the describe reported before the call.
        for member in ["capacityProviderArn", "name", "type"] {
            assert_eq!(
                deleted[member], before[member],
                "case {case:?}: a delete must not change {member}, got {deleted}"
            );
        }
        assert_eq!(
            deleted["autoScalingGroupProvider"], before["autoScalingGroupProvider"],
            "case {case:?}: a delete must not change what the provider binds onto, got \
             {deleted}"
        );
        // And the describe answers the same pair, so the delete reported state it wrote
        // rather than a status of its own.
        let read = describe_provider(&router, provider, case).await;
        assert_eq!(
            lifecycle(&read, case),
            lifecycle(&deleted, case),
            "case {case:?}: the describe must report what the delete stored, got {read}"
        );

        // The release the clock owes finishes on its own, and the provider stays INACTIVE.
        let settled = wait_for_delete_complete(&router, provider, case).await;

        // `@idempotent`: the identical delete reports the provider as it stands and changes
        // nothing.
        let again = ok(&router, "DeleteCapacityProvider", request, case).await;
        assert_eq!(
            again["capacityProvider"], settled,
            "case {case:?}: a repeated delete reports the stored provider unchanged, got \
             {again}"
        );
    }

    // A retired provider holds neither its name nor its Auto Scaling group against a new
    // provider: the create that was refused while it was live goes through now.
    let case = "the name and the group a retired provider no longer holds";
    let created = ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "retireshort",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-retireshort") },
        }),
        case,
    )
    .await;
    assert_eq!(
        lifecycle(&created["capacityProvider"], case),
        ("ACTIVE".to_string(), None),
        "case {case:?}: a provider created over a retired one's name and group is live, got \
         {created}"
    );
}

/// One row of the association table: the provider, the cluster that lists it, and how the
/// delete names the provider and the cluster.
struct HeldCase {
    case: &'static str,
    provider: &'static str,
    /// The cluster whose `capacityProviders` lists the provider.
    holder: &'static str,
    /// What the request's `capacityProvider` carries.
    named: fn(&'static str) -> String,
    /// The `cluster` the request carries, if any.
    cluster: Option<&'static str>,
}

#[tokio::test]
async fn a_delete_a_cluster_association_holds_is_reported_on_the_provider() {
    let router = service();
    for cluster in ["holder", "holderarn", "elsewhere"] {
        create_cluster(&router, cluster, "setup").await;
    }
    for (provider, group) in [
        ("heldshort", "asg-heldshort"),
        ("heldarn", "asg-heldarn"),
        ("heldelsewhere", "asg-heldelsewhere"),
    ] {
        create_provider(&router, provider, group, false, "setup").await;
    }
    associate(&router, "holder", &["heldshort"], "setup").await;
    associate(&router, "holderarn", &["heldarn"], "setup").await;
    associate(&router, "elsewhere", &["heldelsewhere"], "setup").await;

    // The rows differ along how the delete names the provider and whether the `cluster` the
    // request carries is the one holding the association — the hold is the ASSOCIATION,
    // wherever it is, and not the cluster the request happens to name.
    let rows = [
        HeldCase {
            case: "the delete names the provider by its short name",
            provider: "heldshort",
            holder: "holder",
            named: |name| name.to_string(),
            cluster: None,
        },
        HeldCase {
            case: "the delete names the provider by its full ARN, and names its cluster",
            provider: "heldarn",
            holder: "holderarn",
            named: provider_arn,
            cluster: Some("holderarn"),
        },
        HeldCase {
            case: "another cluster holds the association than the one the request names",
            provider: "heldelsewhere",
            holder: "elsewhere",
            named: |name| name.to_string(),
            cluster: Some("holder"),
        },
    ];

    for row in &rows {
        let (case, provider, holder) = (row.case, row.provider, row.holder);
        let mut request = json!({ "capacityProvider": (row.named)(provider) });
        if let Some(cluster) = row.cluster {
            request["cluster"] = json!(cluster);
        }

        // The call is ANSWERED: the provider keeps the status it had and its update status
        // is what says the delete did not happen. Asked twice, so the report is a property
        // of the state and not of the first call.
        for attempt in ["the first delete", "the identical delete again"] {
            let answer = ok(&router, "DeleteCapacityProvider", request.clone(), case).await;
            let held = answer["capacityProvider"].clone();
            assert_eq!(
                lifecycle(&held, case),
                ("ACTIVE".to_string(), Some("DELETE_FAILED".to_string())),
                "case {case:?}: {attempt} must report the provider ACTIVE with its delete \
                 failed, got {held}"
            );
            let read = describe_provider(&router, provider, case).await;
            assert_eq!(
                lifecycle(&read, case),
                ("ACTIVE".to_string(), Some("DELETE_FAILED".to_string())),
                "case {case:?}: after {attempt} the describe reports the same pair, got {read}"
            );
            // And the cluster keeps the provider it was not deleted out of.
            let cluster = describe_cluster(&router, holder, case).await;
            assert!(
                list(&cluster, "capacityProviders", case).contains(&json!(provider)),
                "case {case:?}: after {attempt} the cluster still lists the provider, got \
                 {cluster}"
            );
        }

        // "To remove a capacity provider from a cluster, you can either use
        // PutClusterCapacityProviders or delete the cluster": once the association is gone
        // the identical delete goes through.
        associate(&router, holder, &[], case).await;
        let answer = ok(&router, "DeleteCapacityProvider", request, case).await;
        let deleted = answer["capacityProvider"].clone();
        assert_eq!(
            lifecycle(&deleted, case),
            (
                "INACTIVE".to_string(),
                Some("DELETE_IN_PROGRESS".to_string())
            ),
            "case {case:?}: the delete of a provider nothing holds goes through, got {deleted}"
        );
        wait_for_delete_complete(&router, provider, case).await;
    }
}

/// One row of the refusal table: the request, and the `(code, message)` it earns.
struct RefusalCase {
    case: &'static str,
    request: fn() -> Value,
    code: &'static str,
    message: &'static str,
}

#[tokio::test]
async fn a_delete_outside_the_operations_rules_is_refused_and_writes_nothing() {
    let router = service();
    create_cluster(&router, "live", "setup").await;
    create_cluster(&router, "otherlive", "setup").await;
    create_cluster(&router, "deleted", "setup").await;
    ok(
        &router,
        "DeleteCluster",
        json!({ "cluster": "deleted" }),
        "setup",
    )
    .await;
    create_provider(&router, "survivor", "asg-survivor", false, "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scopedsurvivor",
            "cluster": "live",
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;

    // The rows differ along WHICH rule the request breaks: a provider Amazon ECS reserves
    // (both of them), a name that resolves to nothing, a `cluster` that resolves to no live
    // cluster (never created, and a tombstone — one error code, two sentences), an
    // identifier of another account, and a cluster-scoped provider named without its own
    // cluster or with another one.
    let rows = [
        RefusalCase {
            case: "the FARGATE provider Amazon ECS manages",
            request: || json!({ "capacityProvider": "FARGATE" }),
            code: "ClientException",
            message: RESERVED_REFUSAL,
        },
        RefusalCase {
            case: "the FARGATE_SPOT provider Amazon ECS manages",
            request: || json!({ "capacityProvider": "FARGATE_SPOT" }),
            code: "ClientException",
            message: RESERVED_REFUSAL,
        },
        RefusalCase {
            case: "a name that resolves to no provider",
            request: || json!({ "capacityProvider": "nosuchprovider" }),
            code: UNRESOLVED_PROVIDER_CODE,
            message: UNRESOLVED_PROVIDER_REFUSAL,
        },
        RefusalCase {
            case: "an ARN that resolves to no provider",
            request: || json!({ "capacityProvider": provider_arn("alsonosuchprovider") }),
            code: UNRESOLVED_PROVIDER_CODE,
            message: UNRESOLVED_PROVIDER_REFUSAL,
        },
        RefusalCase {
            case: "a cluster this account never held",
            request: || json!({ "capacityProvider": "survivor", "cluster": "nosuchcluster" }),
            code: "ClusterNotFoundException",
            message: "Cluster not found.",
        },
        RefusalCase {
            case: "a cluster this account holds as a tombstone",
            request: || json!({ "capacityProvider": "survivor", "cluster": "deleted" }),
            code: "ClusterNotFoundException",
            message: "The referenced cluster was inactive.",
        },
        RefusalCase {
            case: "a provider ARN of another account",
            request: || {
                json!({
                    "capacityProvider":
                        format!(
                            "arn:aws:ecs:{SIGNED_REGION}:999999999999:capacity-provider/survivor"
                        ),
                })
            },
            code: "InvalidParameterException",
            message: "AccountIDs mismatch",
        },
        RefusalCase {
            case: "a cluster-scoped provider named without its cluster",
            request: || json!({ "capacityProvider": "scopedsurvivor" }),
            code: "InvalidParameterException",
            message: CLUSTER_SCOPED_REFUSAL,
        },
        RefusalCase {
            case: "a cluster-scoped provider named with another live cluster",
            request: || {
                json!({ "capacityProvider": "scopedsurvivor", "cluster": "otherlive" })
            },
            code: "InvalidParameterException",
            message: CLUSTER_SCOPED_REFUSAL,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (code, message) =
            refusal(&router, "DeleteCapacityProvider", (row.request)(), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (row.code, row.message),
            "case {case:?}: unexpected refusal, got {code:?} / {message:?}"
        );
    }

    // A refused delete wrote nothing: both providers are still exactly where their own
    // create left them, with no delete reported against either. The managed-instances
    // provider's create is a transition the CLOCK owes, so it is read once it has settled
    // into the terminal pair of a create rather than mid-flight.
    let case = "a refused delete changes nothing";
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let settling = describe_provider(&router, "scopedsurvivor", case).await;
        if lifecycle(&settling, case)
            == ("ACTIVE".to_string(), Some("CREATE_COMPLETE".to_string()))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the create of a cluster-scoped provider to settle, got \
             {settling}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    for (provider, expected) in [
        ("survivor", ("ACTIVE", None)),
        ("scopedsurvivor", ("ACTIVE", Some("CREATE_COMPLETE"))),
    ] {
        let read = describe_provider(&router, provider, case).await;
        let (status, update_status) = lifecycle(&read, case);
        assert_eq!(
            (status.as_str(), update_status.as_deref()),
            expected,
            "case {case:?}: {provider} must be left under the pair its create answered, got \
             {read}"
        );
    }
}

#[tokio::test]
async fn a_provider_a_services_strategy_names_cannot_be_freed_for_deletion() {
    // The delete's other documented precondition — "Prior to a capacity provider being
    // deleted, the capacity provider must be removed from the capacity provider strategy
    // from all services" — is SUBSUMED by the cluster association: while a service's
    // strategy names the provider, the association cannot be removed either, so the
    // provider is still associated and the delete is still answered on the provider.
    let router = service();
    create_cluster(&router, "withservice", "setup").await;
    create_provider(&router, "heldbyservice", "asg-heldbyservice", false, "setup").await;
    associate(&router, "withservice", &["heldbyservice"], "setup").await;
    register_definition(&router, "strategyapp", "setup").await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": "withservice",
            "serviceName": "strategyservice",
            "taskDefinition": "strategyapp",
            "desiredCount": 0,
            "capacityProviderStrategy": [
                { "capacityProvider": "heldbyservice", "weight": 1 },
            ],
        }),
        "setup",
    )
    .await;

    // The disassociation the delete would need is refused for as long as the service's
    // strategy names the provider.
    let case = "the association a service's strategy depends on cannot be removed";
    let (code, message) = refusal(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "withservice",
            "capacityProviders": [],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "ResourceInUseException",
            "The specified capacity provider is in use and cannot be removed.",
        ),
        "case {case:?}: unexpected refusal, got {code:?} / {message:?}"
    );

    // So the delete still finds the association, and is answered on the provider.
    let case = "a provider a service's strategy names is not deleted";
    let answer = ok(
        &router,
        "DeleteCapacityProvider",
        json!({ "capacityProvider": "heldbyservice" }),
        case,
    )
    .await;
    let held = answer["capacityProvider"].clone();
    assert_eq!(
        lifecycle(&held, case),
        ("ACTIVE".to_string(), Some("DELETE_FAILED".to_string())),
        "case {case:?}: expected the provider ACTIVE with its delete failed, got {held}"
    );
    let read = describe_provider(&router, "heldbyservice", case).await;
    assert_eq!(
        lifecycle(&read, case),
        ("ACTIVE".to_string(), Some("DELETE_FAILED".to_string())),
        "case {case:?}: the describe reports the same pair, got {read}"
    );
    let cluster = describe_cluster(&router, "withservice", case).await;
    assert!(
        list(&cluster, "capacityProviders", case).contains(&json!("heldbyservice")),
        "case {case:?}: the cluster still lists the provider, got {cluster}"
    );
}

#[tokio::test]
async fn a_provider_the_delete_cannot_resolve_is_a_client_exception() {
    // The rule: the delete's TARGET is the provider its `capacityProvider` member names,
    // so a value that resolves to no provider of this account is an identifier Amazon ECS
    // could not resolve — `ClientException` — and never an `InvalidParameterException`,
    // which is what an unacceptable parameter value earns (the reserved names, a provider
    // scoped to another cluster). The rows walk the reference's FORM and SHAPE: a short
    // name and the ARN form the member documents, names differing in alphabet and length
    // (including the longest a provider name may be), and a provider of another cluster's
    // scope named through the cluster that does hold a provider — so the class is a
    // property of the resolution failing and not of the value's spelling.
    let router = service();
    create_cluster(&router, "resolving", "setup").await;
    create_provider(&router, "resolvable", "asg-resolvable", false, "setup").await;

    let longest = "p".repeat(255);
    let rows: [(&str, Value, Option<&str>); 7] = [
        (
            "a short name nothing was created under",
            json!("ghostprovider"),
            None,
        ),
        (
            "the same name in the ARN form",
            json!(provider_arn("ghostprovider")),
            None,
        ),
        ("a name with digits", json!("ghost42provider"), None),
        (
            "a name with the separators a provider name takes",
            json!("ghost_provider-1"),
            None,
        ),
        (
            "the longest name a provider may have",
            json!(longest.clone()),
            None,
        ),
        ("one character", json!("g"), None),
        (
            "an unknown name sent with a live cluster",
            json!("ghostprovider"),
            Some("resolving"),
        ),
    ];

    for (case, reference, cluster) in rows {
        let mut request = json!({ "capacityProvider": reference });
        if let Some(cluster) = cluster {
            request["cluster"] = json!(cluster);
        }
        let (code, message) = refusal(&router, "DeleteCapacityProvider", request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (UNRESOLVED_PROVIDER_CODE, UNRESOLVED_PROVIDER_REFUSAL),
            "case {case:?}: expected the unresolved-provider refusal, got {code:?} / \
             {message:?}"
        );
    }

    // The SAME resolution path serves the happy case, so the refusals above are the
    // provider not being found and not the operation refusing every name: the one provider
    // this account does hold is retired, by short name and by ARN alike.
    for (case, reference) in [
        ("the stored provider by short name", json!("resolvable")),
        ("the stored provider by ARN", json!(provider_arn("resolvable"))),
    ] {
        let answer = ok(
            &router,
            "DeleteCapacityProvider",
            json!({ "capacityProvider": reference }),
            case,
        )
        .await;
        let deleted = answer["capacityProvider"].clone();
        assert_eq!(
            string(&deleted, "status", case),
            "INACTIVE",
            "case {case:?}: expected the retired provider back, got {deleted}"
        );
    }
}
