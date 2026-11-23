// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of Amazon RDS's EVENT NOTIFICATION SUBSCRIPTION: the
//! subscription an `AWS::RDS::EventSubscription` declares. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! "Ref returns the subscription name", and the type states that name itself
//! (`SubscriptionName`, read through `super::template::declared_name_property`), so the
//! resource's physical id is the name the template asked for and the subscription is created in
//! Amazon RDS under exactly it. That is the handle every call of that service on a subscription
//! takes — `DescribeEventSubscriptions`, `ModifyEventSubscription` and `DeleteEventSubscription`
//! each address one by its `SubscriptionName` — so the handle a stack publishes resolves in the
//! service that owns the resource, and a client that feeds an output back into Amazon RDS
//! reaches the subscription the stack made.
//!
//! The type's remaining properties are `CreateEventSubscription`'s own members
//! ([`CREATE_EVENT_SUBSCRIPTION_MEMBERS`]): the Amazon SNS topic the subscription publishes to,
//! the source type, sources and event categories it is scoped to, whether it is active, and the
//! tag list this arm merges with the tags CloudFormation propagates. Every one of them is
//! resolved by Amazon RDS exactly as it is for a client's own call — the topic against Amazon
//! SNS, the sources against that service's own records, each category against the ones
//! `DescribeEventCategories` publishes for the declared source type — so a declaration that
//! service refuses is a FAILED RESOURCE carrying its refusal rather than a subscription that
//! could never match an event.
//!
//! A CHANGED declaration of the type has no arm here, so a deployment that states one of its
//! properties differently reports the change as NOT applied ([`super::Provisioner::update`])
//! rather than as a success that dropped it; the subscription a stack created keeps the
//! configuration it was created with until that arm lands.

use super::*;

/// `AWS::RDS::EventSubscription`: the event notification subscription that publishes Amazon
/// RDS's events to an Amazon SNS topic, which `CreateEventSubscription` creates under the name
/// the declaration states — the name this type's `Ref` returns —
/// `DescribeEventSubscriptions` reports and `DeleteEventSubscription` removes.
pub(crate) const RDS_EVENT_SUBSCRIPTION: &str = "AWS::RDS::EventSubscription";

/// The member Amazon RDS takes a subscription's own name as, which is this resource's physical
/// id: "The name of the subscription" (`smithy_query(query="operation",
/// operation="CreateEventSubscription")`), and the handle every call on one takes.
pub(crate) const SUBSCRIPTION_NAME_MEMBER: &str = "SubscriptionName";

/// The error code Amazon RDS answers for a subscription it does not hold:
/// `SubscriptionNotFoundFault`'s own `awsQueryError` code (`smithy_query(query="operation",
/// operation="DeleteEventSubscription")`), which is what an awsQuery client branches on.
const SUBSCRIPTION_NOT_FOUND: &str = "SubscriptionNotFound";

/// The property the type carries its own tag list in, which travels as the merged set this arm
/// pushes rather than as a member of the walk.
const TAGS_PROPERTY: &str = "Tags";

/// The members `CreateEventSubscription` takes, from Amazon RDS's own model
/// (`smithy_query(query="operation", operation="CreateEventSubscription")`), with each list's
/// element name from its list shape's member `@xmlName`
/// (`smithy_query(query="shape", shape="SourceIdsList,EventCategoriesList,TagList")`).
///
/// This is the catalogue the declaration of the type is walked against: every property of the
/// type is one of these members' own names, so a property the catalogue does not carry is
/// refused rather than dropped ([`query_members`]).
const CREATE_EVENT_SUBSCRIPTION_MEMBERS: &[(&str, QueryMember)] = &[
    ("Enabled", QueryMember::Scalar),
    ("EventCategories", QueryMember::List("EventCategory")),
    ("SnsTopicArn", QueryMember::Scalar),
    ("SourceIds", QueryMember::List("SourceId")),
    ("SourceType", QueryMember::Scalar),
    (SUBSCRIPTION_NAME_MEMBER, QueryMember::Scalar),
    (TAGS_PROPERTY, QueryMember::List("Tag")),
];

/// The declared properties the walk does NOT turn into a member of the create: the name this arm
/// pushes (it is the resource's physical id, so the subscription is created under the id the
/// stack publishes) and the tag list this arm merges with the ones CloudFormation propagates.
const EVENT_SUBSCRIPTION_PROPERTIES_NOT_SENT: &[&str] = &[SUBSCRIPTION_NAME_MEMBER, TAGS_PROPERTY];

/// Create the event notification subscription an `AWS::RDS::EventSubscription` declares, under
/// the name the declaration states — this resource's physical id, which is what its `Ref`
/// returns.
///
/// The subscription is created in the service that OWNS it, which is the whole point of
/// provisioning it: the `SnsTopicArn`, `SourceIds` and `EventCategories` the declaration names
/// are resolved by Amazon RDS exactly as they are for a client's own call, so a value that
/// service refuses fails the resource and rolls the stack back rather than publishing a
/// subscription nothing holds.
///
/// The type publishes no `Fn::GetAtt` attribute of its own — `Ref` is its one return value — so
/// the create reports none: the record that service answered with is read back through its own
/// `DescribeEventSubscriptions`, under the name the stack published.
pub(crate) async fn create_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_EVENT_SUBSCRIPTION_MEMBERS,
        // Every property of this type is its member's own name.
        &[],
        EVENT_SUBSCRIPTION_PROPERTIES_NOT_SENT,
        RDS_EVENT_SUBSCRIPTION,
    )?;
    parameters.push((
        SUBSCRIPTION_NAME_MEMBER.to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        TAGS_PROPERTY,
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "CreateEventSubscription",
        &parameters,
    )
    .await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Delete the subscription a stack created, addressed by its name — this resource's physical id.
/// A subscription Amazon RDS no longer holds (`SubscriptionNotFound`) is already in the state
/// the delete asks for.
pub(crate) async fn delete_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DeleteEventSubscription",
        &addressed(physical_resource_id),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == SUBSCRIPTION_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The subscription's configuration as Amazon RDS reports it, for drift detection. A
/// subscription it no longer holds (`SubscriptionNotFound`) was deleted, as is an answer that
/// carries no record of one at all: either way that service holds nothing under the name.
///
/// Its scalar members are read the way every other awsQuery record's are, which covers the
/// properties a declaration can drift in under the same name (`SnsTopicArn`, `SourceType`,
/// `Enabled`); the sources and categories are reported as LISTS under names of their own
/// (`SourceIdsList`, `EventCategoriesList`), so they take no part in the comparison — the
/// position every other arm whose list members are reported under another name is in, and what
/// [`super::drift`] documents.
pub(crate) async fn read_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeEventSubscriptions",
        &addressed(physical_resource_id),
    )
    .await
    {
        Ok(answered) => {
            let reported = xml_scalar_children(&answered, "EventSubscription");
            // A record of a subscription always carries the name that service holds it under,
            // so an answer with no members is an answer about no subscription — reported as a
            // DELETED resource rather than as a live one that drifted in every property.
            Ok((!reported.is_empty()).then(|| Value::Object(reported)))
        }
        Err(refusal) if refusal.code == SUBSCRIPTION_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The one parameter a call about an EXISTING subscription takes: the name Amazon RDS holds it
/// under, which is this resource's physical id.
fn addressed(physical_resource_id: &str) -> Vec<(String, String)> {
    vec![(
        SUBSCRIPTION_NAME_MEMBER.to_string(),
        physical_resource_id.to_string(),
    )]
}
