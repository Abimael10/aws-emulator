// SPDX-License-Identifier: CC0-1.0

//! The capacity providers a `PutClusterCapacityProviders` call ASSOCIATES with a cluster,
//! driven end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > `capacityProviders` is "The name of one or more capacity providers to associate with the
//! > cluster", and one provider is one entry of that association: each entry has to resolve
//! > to a capacity provider the account holds that no earlier entry already resolved to, so
//! > an entry that resolves to nothing — a name nothing holds, or a provider the list has
//! > already named — is refused with `InvalidParameterException` and the sentence Amazon ECS
//! > states for a provider it could not resolve, leaving the cluster's association exactly as
//! > it was.
//!
//! The table spans the providers Amazon ECS manages for every account and one a caller
//! created, lists of one and of several, the position of the repeat, and a name nothing holds
//! — rather than the one request the defect was found on. Nothing here reaches into the state
//! struct: what each call left is read back through `DescribeClusters`.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    associate_capacity_providers, create_auto_scaling_group, create_cluster,
    describe_cluster_attachments, list, ok, refusal, service,
};

/// The refusal class an unresolvable provider is reported under.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence it carries (`state`'s `CAPACITY_PROVIDER_NOT_FOUND_MESSAGE`).
const PROVIDER_NOT_FOUND: &str =
    "The specified capacity provider was not found. Specify a valid capacity provider and \
     try again.";

/// The capacity providers Amazon ECS manages for every account, which a cluster may be
/// associated with without creating anything (`PutClusterCapacityProviders$capacityProviders`:
/// "To use a Fargate capacity provider, specify either the FARGATE or FARGATE_SPOT capacity
/// providers. The Fargate capacity providers are available to all accounts").
const FARGATE: &str = "FARGATE";
const FARGATE_SPOT: &str = "FARGATE_SPOT";

/// A capacity provider of the caller's own, created over an Auto Scaling group of theirs.
const OWN_PROVIDER: &str = "pccpown";

/// The providers a cluster reports as associated, in the order it reports them.
async fn associated(router: &MultiServiceRouter, cluster: &str, case: &str) -> Vec<String> {
    let reported = describe_cluster_attachments(router, cluster, case).await;
    list(&reported, "capacityProviders", case)
        .into_iter()
        .map(|provider| provider.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Create one capacity provider of the caller's own, over a group this account holds.
async fn given_own_provider(router: &MultiServiceRouter, case: &str) {
    create_auto_scaling_group(router, "pccpgroup", case).await;
    ok(
        router,
        "CreateCapacityProvider",
        json!({
            "name": OWN_PROVIDER,
            "autoScalingGroupProvider": { "autoScalingGroupArn": "pccpgroup" },
        }),
        case,
    )
    .await;
}

#[tokio::test]
async fn a_capacity_provider_named_twice_resolves_to_no_further_provider() {
    let router = service();
    given_own_provider(&router, "setup").await;

    // (case, the list the call carries)
    let refused: &[(&str, &[&str])] = &[
        ("one managed provider named twice", &[FARGATE, FARGATE]),
        (
            "a managed provider repeated after another",
            &[FARGATE, FARGATE_SPOT, FARGATE],
        ),
        (
            "a managed provider repeated before another",
            &[FARGATE_SPOT, FARGATE_SPOT, FARGATE],
        ),
        ("the caller's own provider named twice", &[OWN_PROVIDER, OWN_PROVIDER]),
        (
            "the caller's own provider repeated beside a managed one",
            &[OWN_PROVIDER, FARGATE, OWN_PROVIDER],
        ),
        ("a name the account holds nothing under", &["pccpnosuch"]),
        (
            "a name nothing holds beside one that resolves",
            &[FARGATE, "pccpnosuch"],
        ),
    ];
    for (index, (case, providers)) in refused.iter().enumerate() {
        let cluster = format!("pccpdup{index}");
        create_cluster(&router, &cluster, case).await;
        // The cluster starts out associated with ONE provider, so the refusal can be shown
        // to leave the association it already had.
        associate_capacity_providers(&router, &cluster, &[FARGATE_SPOT], case).await;

        let (code, message) = refusal(
            &router,
            "PutClusterCapacityProviders",
            json!({
                "cluster": &cluster,
                "capacityProviders": providers,
                "defaultCapacityProviderStrategy": [],
            }),
            case,
        )
        .await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected an {INVALID_PARAMETER}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, PROVIDER_NOT_FOUND,
            "case {case:?}: expected the fixed sentence {PROVIDER_NOT_FOUND:?}, got \
             {message:?}"
        );
        assert_eq!(
            associated(&router, &cluster, case).await,
            vec![FARGATE_SPOT.to_string()],
            "case {case:?}: a refused association leaves the cluster as it was"
        );
    }
}

#[tokio::test]
async fn a_list_naming_each_provider_once_is_associated() {
    let router = service();
    given_own_provider(&router, "setup").await;

    // Every row names each provider exactly once, so every row is associated as given —
    // including the one-entry list a repeat cannot be built from.
    let accepted: &[(&str, &[&str])] = &[
        ("one managed provider", &[FARGATE]),
        ("both managed providers", &[FARGATE, FARGATE_SPOT]),
        ("the caller's own provider", &[OWN_PROVIDER]),
        (
            "the caller's own beside both managed ones",
            &[OWN_PROVIDER, FARGATE, FARGATE_SPOT],
        ),
    ];
    for (index, (case, providers)) in accepted.iter().enumerate() {
        let cluster = format!("pccpok{index}");
        create_cluster(&router, &cluster, case).await;
        associate_capacity_providers(&router, &cluster, providers, case).await;
        let held = associated(&router, &cluster, case).await;
        assert_eq!(
            held,
            providers
                .iter()
                .map(|provider| (*provider).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the association the call asked for, got {held:?}"
        );
    }
}
