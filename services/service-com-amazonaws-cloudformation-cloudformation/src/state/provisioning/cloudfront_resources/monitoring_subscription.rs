// SPDX-License-Identifier: CC0-1.0

//! `AWS::CloudFront::MonitoringSubscription`: the additional CloudWatch metrics a stack turns
//! on for one distribution, created in Amazon CloudFront itself.
//!
//! ## Why it is really created
//!
//! This type has no identity of its own: a monitoring subscription IS a property of the
//! distribution it is made on — `CreateMonitoringSubscription`, `GetMonitoringSubscription`
//! and `DeleteMonitoringSubscription` each address one by the `DistributionId` alone, and
//! there is at most one per distribution. So the resource a stack reports is the subscription
//! CloudFront holds for that distribution, and a stack that reported one created while nothing
//! created it publishes a distribution whose `GetMonitoringSubscription` answers
//! `NoSuchMonitoringSubscription`: the metrics the template asked for are not being collected.
//!
//! ## The declaration, and the identity
//!
//! The type publishes two properties: the `DistributionId` the subscription belongs to and the
//! `MonitoringSubscription` structure that IS the create's payload, so the declaration is
//! rendered as that payload with nothing renamed. The resource's physical id is the
//! distribution's id, which is both what `Ref` returns and what every call on the subscription
//! takes ([`crate::state::template::declared_name_property`] is where that is recorded).
//!
//! A subscription CloudFront already holds for the distribution is refused
//! ([`MONITORING_SUBSCRIPTION_ALREADY_EXISTS`]) rather than replaced, so an UPDATE of a
//! changed declaration removes the one in force and makes the declared one — the only way that
//! API has of changing a subscription.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Created, Refusal, Removal, StoredResource};

/// The resource type this module provisions.
pub(crate) const CLOUDFRONT_MONITORING_SUBSCRIPTION: &str =
    crate::state::template::CLOUDFRONT_MONITORING_SUBSCRIPTION;

/// Every property the type publishes. A declared property this list does not carry is one no
/// call here applies.
const PROPERTIES: &[&str] = &["DistributionId", "MonitoringSubscription"];

/// The members `MonitoringSubscription` publishes, which a template spells identically.
const SUBSCRIPTION_MEMBERS: &[&str] = &["RealtimeMetricsSubscriptionConfig"];

/// The code CloudFront answers a create for a distribution that already has a subscription.
const MONITORING_SUBSCRIPTION_ALREADY_EXISTS: &str = "MonitoringSubscriptionAlreadyExists";

/// The codes that say CloudFront holds no subscription to report or remove: the distribution
/// has none, or there is no such distribution at all.
const NO_SUCH_MONITORING_SUBSCRIPTION: &str = "NoSuchMonitoringSubscription";
const NO_SUCH_DISTRIBUTION: &str = "NoSuchDistribution";

/// Create the monitoring subscription the declaration describes, on the distribution it names.
///
/// `Err` is CloudFront's own refusal: a distribution id nothing resolves to, a subscription
/// that distribution already has, a metrics status it will not take.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let body = subscription_document(&resource.properties, resource)?;
    let distribution = distribution_id(&resource.properties, resource)?;
    super::call_cloudfront(
        ctx,
        region,
        "POST",
        &subscription_path(&distribution),
        Some(&body),
        None,
    )
    .await?;
    // The subscription carries no identifier of its own: the resource IS the one CloudFront
    // holds for that distribution, so the identity the stack publishes is the distribution's
    // own id, taken from the declaration now that the distribution exists.
    Ok(Created {
        physical_resource_id: Some(distribution),
        attributes: BTreeMap::new(),
    })
}

/// Apply a CHANGED declaration: the API takes a subscription only where the distribution has
/// none, so the one in force is removed and the declared one is made.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let body = subscription_document(declared, resource)?;
    let distribution = distribution_id(declared, resource)?;
    let path = subscription_path(&distribution);
    match super::call_cloudfront(ctx, region, "POST", &path, Some(&body), None).await {
        Ok(_) => Ok(Some(Created::default())),
        Err(refusal) if refusal.code == MONITORING_SUBSCRIPTION_ALREADY_EXISTS => {
            super::call_cloudfront(ctx, region, "DELETE", &path, None, None).await?;
            super::call_cloudfront(ctx, region, "POST", &path, Some(&body), None).await?;
            Ok(Some(Created::default()))
        }
        Err(refusal) => Err(refusal),
    }
}

/// Delete the subscription a stack made, addressed by the distribution it was made on.
///
/// A distribution CloudFront no longer holds, and one it holds no subscription for, are both
/// already in the state the delete asks for — judged on the CODE and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let distribution = distribution_id(&resource.properties, resource)?;
    match super::call_cloudfront(
        ctx,
        region,
        "DELETE",
        &subscription_path(&distribution),
        None,
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal)
            if refusal.code == NO_SUCH_MONITORING_SUBSCRIPTION
                || refusal.code == NO_SUCH_DISTRIBUTION =>
        {
            Ok(())
        }
        Err(refusal) => Err(refusal),
    }
}

/// A subscription is in force as its create answers: nothing is asked, and nothing waits.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// The distribution the subscription belongs to.
///
/// Read from the DECLARATION rather than off the record's physical id: a declaration naming
/// the distribution the same stack creates (`DistributionId: !Ref Distribution`) was resolved
/// against the stand-in that resource had before CloudFront minted its id, and the deployment
/// re-points the properties at the minted one before this is reached
/// ([`crate::state::provisioning::repoint_references`]) — so the properties are where the id
/// CloudFront knows the distribution by lives. It is also what the create reports back as the
/// resource's identity, which is what `Ref` resolves to from then on.
fn distribution_id(properties: &Value, resource: &StoredResource) -> Result<String, Refusal> {
    properties
        .get("DistributionId")
        .filter(|value| !value.is_null())
        .and_then(crate::state::scalar_text)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: super::MISSING_PROPERTY.to_string(),
            message: format!(
                "Property DistributionId cannot be empty for resource of type {}",
                resource.resource_type
            ),
        })
}

/// The `MonitoringSubscription` document a create sends: the declared structure under the root
/// element the operation's payload is bound to.
fn subscription_document(
    properties: &Value,
    resource: &StoredResource,
) -> Result<String, Refusal> {
    super::refuse_unapplied_properties(properties, &resource.resource_type, PROPERTIES)?;
    super::refuse_unrenderable_members(properties, &resource.resource_type, 0)?;
    let declared = properties
        .get("MonitoringSubscription")
        .and_then(Value::as_object)
        .ok_or_else(|| Refusal {
            code: super::MISSING_PROPERTY.to_string(),
            message: format!(
                "Property MonitoringSubscription cannot be empty for resource of type {}",
                resource.resource_type
            ),
        })?;
    super::refuse_unapplied_members(declared, "MonitoringSubscription", SUBSCRIPTION_MEMBERS)?;
    let mut rendered = String::new();
    for member in SUBSCRIPTION_MEMBERS {
        if let Some(value) = declared.get(*member).filter(|value| !value.is_null()) {
            rendered.push_str(&super::rendered_member(
                member,
                value,
                declared,
                "MonitoringSubscription",
            ));
        }
    }
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    Ok(format!(
        "<MonitoringSubscription xmlns=\"{namespace}\">{rendered}</MonitoringSubscription>"
    ))
}

/// The URI the subscription operations are bound to:
/// `/2020-05-31/distributions/{DistributionId}/monitoring-subscription`, with the id written
/// as ONE path segment.
fn subscription_path(distribution: &str) -> String {
    format!(
        "/distributions/{}/monitoring-subscription",
        super::percent_encoded(distribution)
    )
}
