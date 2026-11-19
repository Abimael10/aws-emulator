// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the ASSOCIATION an
//! `AWS::ECS::ClusterCapacityProviderAssociations` declares between one Amazon ECS cluster and
//! the capacity providers that run its tasks. One file of [`super`]'s per-type arms, beside the
//! cluster arms the same owning service has in [`super::application_services`], the provider
//! arms in [`super::ecs_capacity_providers`] and the service arms in [`super::ecs_services`].
//!
//! ## Why the association is really applied
//!
//! The resource IS the cluster's capacity-provider configuration: "A capacity provider must be
//! associated with the cluster to be used in a capacity provider strategy. The
//! `PutClusterCapacityProviders` API is used to associate a capacity provider with a cluster"
//! (`CreateService$capacityProviderStrategy`). A stack that reported the resource created while
//! nothing called that API leaves the cluster with no providers and no default strategy, so
//! `DescribeClusters` reports neither and every placement that names one of the declared
//! providers is refused — the template wired the cluster to its capacity and nothing happened.
//!
//! ## The identity
//!
//! The resource's physical id is the CLUSTER
//! ([`ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS_IDENTITY`], read by
//! [`crate::state::template::composite_identity`]): there is one such configuration per cluster,
//! `PutClusterCapacityProviders` addresses it by the cluster alone, and that is what this type's
//! `Ref` returns. So a `Ref` or an `Fn::Sub` that depends on the association resolves to a name
//! that IS a cluster, which is what an ECS call that takes one admits.
//!
//! ## The properties
//!
//! The type's three properties are `PutClusterCapacityProviders`' own members in lower camel
//! case ([`ECS_CLUSTER_CAPACITY_PROVIDERS_MEMBERS`] names each pair, and [`lower_camel_members`]
//! re-cases every member name inside a value the same way, so a declared
//! `DefaultCapacityProviderStrategy[].CapacityProvider` reaches that call as
//! `defaultCapacityProviderStrategy[].capacityProvider`). Both lists are REQUIRED members of
//! that call, so a declaration stating neither sends the empty lists rather than a request that
//! service would refuse — which is also what the resource's own DELETE asks for
//! ([`disassociate_cluster_capacity_providers`]): "any existing capacity providers that are
//! associated with a cluster that are omitted from a `PutClusterCapacityProviders` API call will
//! be disassociated with the cluster".
//!
//! A declared property none of those members carries is REFUSED rather than dropped: an
//! association reported applied with less than its declaration asked for is the shallow success
//! this arm exists to avoid.

use super::*;

/// The resource type these arms provision, which is also the name the rest of this service's
/// state keys its entries for the type by.
pub(in crate::state) const ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS: &str =
    "AWS::ECS::ClusterCapacityProviderAssociations";

/// The property whose value IS this resource's physical id: the cluster the association
/// configures, which is the only value `PutClusterCapacityProviders` addresses one by and what
/// "Ref returns" for the type.
pub(in crate::state) const ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS_IDENTITY: &[&str] =
    &[CLUSTER_PROPERTY];

/// The members `PutClusterCapacityProviders` takes this type's properties as, as
/// (property, member) pairs.
///
/// Source: AWS's CloudFormation resource reference for the type, member by member against the
/// `PutClusterCapacityProvidersRequest` shape
/// (`smithy_query(query="operation",operation="PutClusterCapacityProviders")`). The cluster is
/// not among them because it ADDRESSES the call rather than travelling beside the lists, and it
/// is this resource's own identity.
const ECS_CLUSTER_CAPACITY_PROVIDERS_MEMBERS: &[(&str, &str)] = &[
    (CAPACITY_PROVIDERS_PROPERTY, CAPACITY_PROVIDERS_MEMBER),
    (DEFAULT_STRATEGY_PROPERTY, DEFAULT_STRATEGY_MEMBER),
];

/// The property naming the cluster the association configures, and the member every call about
/// that cluster addresses it by.
const CLUSTER_PROPERTY: &str = "Cluster";
const CLUSTER_MEMBER: &str = "cluster";

/// The two LIST properties and the members that call takes them as.
const CAPACITY_PROVIDERS_PROPERTY: &str = "CapacityProviders";
const CAPACITY_PROVIDERS_MEMBER: &str = "capacityProviders";
const DEFAULT_STRATEGY_PROPERTY: &str = "DefaultCapacityProviderStrategy";
const DEFAULT_STRATEGY_MEMBER: &str = "defaultCapacityProviderStrategy";

/// The field a `DescribeClusters` read has to ASK for to be told how the association is going
/// (`ClusterField$ATTACHMENTS`), and the member it is reported under.
const ATTACHMENTS_FIELD: &str = "ATTACHMENTS";
const CLUSTERS_MEMBER: &str = "clusters";
const ATTACHMENTS_STATUS_MEMBER: &str = "attachmentsStatus";

/// `Cluster$attachmentsStatus` while the association is still being applied — "the available
/// capacity or capacity providers are updating" — and the status it reports for one that failed.
/// Every other status is one the association is settled in.
const ATTACHMENTS_UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
const ATTACHMENTS_UPDATE_FAILED: &str = "UPDATE_FAILED";

/// Apply the association an `AWS::ECS::ClusterCapacityProviderAssociations` declares, in Amazon
/// ECS itself — so a client that deployed the stack reads the declared providers in
/// `DescribeClusters`' `capacityProviders`, reads the declared
/// `defaultCapacityProviderStrategy` back, and can place tasks with a
/// `capacityProviderStrategy` naming one of them.
///
/// This is the type's CREATE and its UPDATE alike: `PutClusterCapacityProviders` sets the whole
/// configuration of the cluster it addresses, so a changed declaration is the same call with the
/// lists the template now states.
///
/// `declared` is the property document in force — the resource's own on a create, and the
/// deployment's new one on an update.
///
/// `Err` is Amazon ECS's own refusal, which is what the resource fails with: a cluster it does
/// not hold, a provider name that resolves to nothing, a default strategy naming a provider the
/// same request does not associate, or a provider still running the cluster's tasks being
/// dropped.
pub(super) async fn put_cluster_capacity_providers(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type)?;
    let mut request = Map::new();
    request.insert(
        CLUSTER_MEMBER.to_string(),
        Value::String(addressed_cluster(resource, declared)),
    );
    for (property, member) in ECS_CLUSTER_CAPACITY_PROVIDERS_MEMBERS {
        let declared = declared
            .get(*property)
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        request.insert((*member).to_string(), lower_camel_members(&declared, 0)?);
    }
    call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.PutClusterCapacityProviders"),
        &Value::Object(request),
    )
    .await?;
    // The resource's identity is the cluster it configures, which the declaration states, and
    // the type publishes no attribute of its own: `Ref` returns that cluster, and there is no
    // `Fn::GetAtt` on it.
    Ok(Created::default())
}

/// Remove the association a stack applied, which is the same call with NOTHING associated:
/// "any existing capacity providers that are associated with a cluster that are omitted from a
/// `PutClusterCapacityProviders` API call will be disassociated with the cluster".
///
/// A cluster Amazon ECS no longer holds is already in the state the delete asks for — the
/// configuration went with it — and is judged on that service's own code, never on a message
/// that carries the request's own values. Every other refusal is the resource's: a provider the
/// cluster's tasks still run on cannot be dropped, and the stack reports that rather than
/// recording an association removed while the cluster still has it.
pub(super) async fn disassociate_cluster_capacity_providers(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        CLUSTER_MEMBER.to_string(),
        Value::String(addressed_cluster(resource, &resource.properties)),
    );
    for (_, member) in ECS_CLUSTER_CAPACITY_PROVIDERS_MEMBERS {
        request.insert((*member).to_string(), Value::Array(Vec::new()));
    }
    match call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.PutClusterCapacityProviders"),
        &Value::Object(request),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == CLUSTER_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon ECS has finished applying the association.
///
/// `Cluster$attachmentsStatus` is [`ATTACHMENTS_UPDATE_IN_PROGRESS`] while "the available
/// capacity or capacity providers are updating", and that service brings the cluster out of it
/// by itself — so the resource's creation is held open until it does, which is also what keeps a
/// second association of the same cluster from being refused as an update already in flight
/// ("cluster attachments must be in UPDATE_COMPLETE or UPDATE_FAILED state before they can be
/// updated"). [`ATTACHMENTS_UPDATE_FAILED`] is an update nothing will finish, and a cluster that
/// read no longer holds is one nothing is going to associate anything with.
pub(super) async fn cluster_capacity_providers_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(cluster) = described_cluster(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon ECS holds no cluster {physical_resource_id}"
        )));
    };
    match reported_text(&cluster, ATTACHMENTS_STATUS_MEMBER).as_deref() {
        Some(ATTACHMENTS_UPDATE_IN_PROGRESS) => {
            Ok(Readiness::Pending(ATTACHMENTS_UPDATE_IN_PROGRESS.to_string()))
        }
        Some(ATTACHMENTS_UPDATE_FAILED) => Ok(Readiness::Failed(format!(
            "Amazon ECS reports the capacity providers of cluster {physical_resource_id} as \
             {ATTACHMENTS_UPDATE_FAILED}"
        ))),
        _ => Ok(Readiness::Ready),
    }
}

/// The association's configuration as Amazon ECS reports it, for drift detection: the cluster it
/// is the configuration OF, projected onto the property name the type declares it under.
///
/// The type's other two properties are LISTS on the wire, which is the position every arm whose
/// property is not a scalar is in and what [`drift`] documents. A cluster that read no longer
/// holds says the resource was deleted, which is what `None` reports.
pub(super) async fn describe_cluster_capacity_providers(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(_) = described_cluster(ctx, region, physical_resource_id).await? else {
        return Ok(None);
    };
    let mut reported = Map::new();
    reported.insert(
        CLUSTER_PROPERTY.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    Ok(Some(Value::Object(reported)))
}

/// Amazon ECS's own `DescribeClusters` entry for the cluster the association configures, with
/// the attachments asked for, and `None` where that read reports no such cluster — which it
/// answers as a `failures` entry beside an empty list rather than as a refusal of the call.
async fn described_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        CLUSTERS_MEMBER.to_string(),
        Value::Array(vec![Value::String(cluster.to_string())]),
    );
    request.insert(
        "include".to_string(),
        Value::Array(vec![Value::String(ATTACHMENTS_FIELD.to_string())]),
    );
    let answered = match call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.DescribeClusters"),
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == CLUSTER_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(first_listed(&answered, CLUSTERS_MEMBER))
}

/// The cluster a call about the association is addressed by: the one `declared` names — as a
/// short name or a full ARN, both of which `PutClusterCapacityProviders$cluster` admits — and
/// otherwise this resource's own physical id, which IS that cluster
/// ([`ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS_IDENTITY`]).
fn addressed_cluster(resource: &StoredResource, declared: &Value) -> String {
    declared
        .get(CLUSTER_PROPERTY)
        .and_then(scalar_text)
        .filter(|cluster| !cluster.is_empty())
        .unwrap_or_else(|| resource.physical_resource_id.clone())
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps an association from being reported applied with less than its
/// declaration asked for.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    let applied: Vec<&str> = ECS_CLUSTER_CAPACITY_PROVIDERS_MEMBERS
        .iter()
        .map(|(property, _)| *property)
        .chain([CLUSTER_PROPERTY])
        .collect();
    match unmapped_property(members, &applied) {
        Some(property) => Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the association is addressed by the cluster its declaration names, whichever form
    /// that names it in, and by the resource's own identity — which IS that cluster — for a
    /// document that states none.
    #[test]
    fn the_call_is_addressed_by_the_declared_cluster_or_the_resources_own_identity() {
        // (case, the declared properties, the identity the resource carries, the cluster)
        let rows: &[(&str, Value, &str, &str)] = &[
            (
                "a cluster named by its short name",
                serde_json::json!({ "Cluster": "orders" }),
                "orders",
                "orders",
            ),
            (
                "a cluster named by its full ARN",
                serde_json::json!({
                    "Cluster": "arn:aws:ecs:us-east-1:000000000000:cluster/orders",
                }),
                "arn:aws:ecs:us-east-1:000000000000:cluster/orders",
                "arn:aws:ecs:us-east-1:000000000000:cluster/orders",
            ),
            (
                "a document stating no cluster: the resource's own identity",
                serde_json::json!({}),
                "orders",
                "orders",
            ),
            (
                "a document stating an empty cluster: the resource's own identity",
                serde_json::json!({ "Cluster": "" }),
                "orders",
                "orders",
            ),
        ];
        for (case, properties, identity, expected) in rows {
            let resource = StoredResource {
                logical_id: "CPAssoc".to_string(),
                resource_type: ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS.to_string(),
                physical_resource_id: (*identity).to_string(),
                attributes: BTreeMap::new(),
                metadata: None,
                properties: properties.clone(),
                materialised: false,
                deletion_policy: None,
                update_replace_policy: None,
            };
            let addressed = addressed_cluster(&resource, properties);
            assert_eq!(
                addressed, *expected,
                "case {case:?}: expected the call to be addressed to {expected:?}, got \
                 {addressed:?}"
            );
        }
    }

    /// RULE: every property of the type reaches `PutClusterCapacityProviders` under that call's
    /// own member name, and a property no member of the call takes is refused rather than
    /// dropped.
    #[test]
    fn a_property_the_call_does_not_carry_is_refused() {
        let accepted = serde_json::json!({
            "Cluster": "orders",
            "CapacityProviders": ["a-provider"],
            "DefaultCapacityProviderStrategy": [
                { "CapacityProvider": "a-provider", "Weight": 1, "Base": 0 },
            ],
        });
        assert!(
            refuse_unapplied_properties(&accepted, ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS)
                .is_ok(),
            "case \"the properties this arm applies\": expected them to be admitted, got a \
             refusal"
        );
        let refused = serde_json::json!({ "Cluster": "orders", "NoSuchProperty": true });
        let answer =
            refuse_unapplied_properties(&refused, ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS);
        let message = answer
            .expect_err("a property no member of the call takes is refused")
            .message;
        assert!(
            message.contains("NoSuchProperty"),
            "case {refused:?}: the refusal names the property, got {message:?}"
        );
    }

    /// RULE: the member names inside a declared property are the API's own, which for this type
    /// is each property with its initial letter lower-cased, all the way down.
    #[test]
    fn the_declared_strategy_reaches_the_api_under_its_own_names() {
        let declared = serde_json::json!([
            { "CapacityProvider": "a-provider", "Base": 2, "Weight": 3 },
            { "CapacityProvider": "FARGATE_SPOT", "Weight": 1 },
        ]);
        let converted = lower_camel_members(&declared, 0).expect("the document is readable");
        let expected = serde_json::json!([
            { "capacityProvider": "a-provider", "base": 2, "weight": 3 },
            { "capacityProvider": "FARGATE_SPOT", "weight": 1 },
        ]);
        assert_eq!(
            converted, expected,
            "case {declared:?}: expected {expected:?}, got {converted:?}"
        );
    }
}
