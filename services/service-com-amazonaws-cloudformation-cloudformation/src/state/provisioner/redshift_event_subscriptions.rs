// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of Amazon Redshift's EVENT NOTIFICATION SUBSCRIPTION: the
//! subscription an `AWS::Redshift::EventSubscription` declares. One file of [`super`]'s
//! per-type arms, dispatched to by its `Provisioner`.
//!
//! "Ref returns the name of the Amazon Redshift event notification subscription", and the type
//! states that name itself (`SubscriptionName`, read through
//! `super::template::declared_name_property`), so the resource's physical id is the name the
//! template asked for and the subscription is created in Amazon Redshift under exactly it. That
//! is the handle every call of that service on a subscription takes —
//! `DescribeEventSubscriptions`, `ModifyEventSubscription` and `DeleteEventSubscription` each
//! address one by its name — so the name a stack publishes resolves in the service that owns
//! the resource.
//!
//! The type's remaining properties are `CreateEventSubscription`'s own members
//! ([`CREATE_EVENT_SUBSCRIPTION_MEMBERS`]): the Amazon SNS topic the subscription publishes to,
//! the sources, categories and severity it is scoped to, whether it is active, and the tag list
//! this arm merges with the tags CloudFormation propagates. The topic is resolved by Amazon
//! Redshift against Amazon SNS exactly as it is for a client's own call, so an ARN that service
//! holds no topic under is that service's refusal on this resource rather than a subscription
//! publishing to nothing.
//!
//! Everything the subscription carries beside its name and its tags is a member of
//! `ModifyEventSubscription`, so a changed declaration reaches the subscription the stack
//! already has through that one call ([`update_event_subscription`]) rather than replacing it.
//!
//! What the owning service REPORTS for a subscription — the account it belongs to, the name it
//! holds it under, its status, the instant it was created and the resolved source and category
//! lists — are the type's published `Fn::GetAtt` attributes, so they are read off that
//! service's own answer ([`reported_attributes`]) rather than computed here: none of them is a
//! value these naming rules could produce.

use super::*;

/// `AWS::Redshift::EventSubscription`: the event notification subscription that publishes
/// Amazon Redshift's events to an Amazon SNS topic, which `CreateEventSubscription` creates
/// under the name the declaration states — the name this type's `Ref` returns —
/// `DescribeEventSubscriptions` reports, `ModifyEventSubscription` changes and
/// `DeleteEventSubscription` removes.
pub(crate) const REDSHIFT_EVENT_SUBSCRIPTION: &str = "AWS::Redshift::EventSubscription";

/// The error code Amazon Redshift answers for a subscription it does not hold:
/// `SubscriptionNotFoundFault`'s own `awsQueryError` code
/// (`smithy_query(query="operation", operation="DeleteEventSubscription")`), which is what an
/// awsQuery client branches on.
const SUBSCRIPTION_NOT_FOUND: &str = "SubscriptionNotFound";

/// The property the type carries its own tag list in, which travels as the merged set this arm
/// pushes rather than as a member of the walk.
const TAGS_PROPERTY: &str = "Tags";

/// The members `CreateEventSubscription` takes, from Amazon Redshift's own model
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
    ("Severity", QueryMember::Scalar),
    ("SnsTopicArn", QueryMember::Scalar),
    ("SourceIds", QueryMember::List("SourceId")),
    ("SourceType", QueryMember::Scalar),
    ("SubscriptionName", QueryMember::Scalar),
    (TAGS_PROPERTY, QueryMember::List("Tag")),
];

/// The members `ModifyEventSubscription` takes
/// (`smithy_query(query="operation", operation="ModifyEventSubscription")`): every member of the
/// create but the name that identifies the subscription and the tag list, which that call
/// carries no member for at all.
const MODIFY_EVENT_SUBSCRIPTION_MEMBERS: &[(&str, QueryMember)] = &[
    ("Enabled", QueryMember::Scalar),
    ("EventCategories", QueryMember::List("EventCategory")),
    ("Severity", QueryMember::Scalar),
    ("SnsTopicArn", QueryMember::Scalar),
    ("SourceIds", QueryMember::List("SourceId")),
    ("SourceType", QueryMember::Scalar),
];

/// The declared properties neither call's walk turns into a member: the name this arm pushes
/// (it is the resource's physical id) and the tag list this arm merges with the ones
/// CloudFormation propagates.
const EVENT_SUBSCRIPTION_PROPERTIES_NOT_SENT: &[&str] = &["SubscriptionName", TAGS_PROPERTY];

/// The SCALAR values Amazon Redshift reports for a subscription that the type publishes as
/// `Fn::GetAtt` attributes: the account the subscription belongs to, the name that service
/// holds it under, its status and the instant it was created.
///
/// Each is read off the record that service answered with, under the name the type publishes it
/// as, which is the name that service's own `EventSubscription` structure spells
/// (`smithy_query(query="shape", shape="EventSubscription")`).
const EVENT_SUBSCRIPTION_REPORTED_ATTRIBUTES: &[&str] = &[
    "CustSubscriptionId",
    "CustomerAwsId",
    "Status",
    "SubscriptionCreationTime",
];

/// The LIST values the same record carries, each with the element name its list shape's member
/// `@xmlName` gives it: the sources the subscription publishes events for and the categories it
/// is scoped to, as that service RESOLVED them.
///
/// A list reaches the resource's string-valued attribute map one element per indexed key
/// ([`list_attribute_element`]), the one carrier such a map has for one, so a template reads
/// the whole list through `Fn::GetAtt` and one element through the dotted notation.
const EVENT_SUBSCRIPTION_REPORTED_LISTS: &[(&str, &str)] = &[
    ("EventCategoriesList", "EventCategory"),
    ("SourceIdsList", "SourceId"),
];

/// The attributes one answered `<EventSubscription>` record gives the resource, read off the
/// owning service's own report of it.
///
/// A value that record does not carry gives no attribute at all, so the resource publishes what
/// that service reported rather than an empty value standing in for it.
fn reported_attributes(answered: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let reported = xml_scalar_children(answered, "EventSubscription");
    for attribute in EVENT_SUBSCRIPTION_REPORTED_ATTRIBUTES {
        if let Some(value) = reported.get(*attribute).and_then(scalar_text) {
            attributes.insert((*attribute).to_string(), value);
        }
    }
    for (attribute, element) in EVENT_SUBSCRIPTION_REPORTED_LISTS {
        for (index, value) in xml_elements(answered, element).into_iter().enumerate() {
            attributes.insert(list_attribute_element(attribute, index), value);
        }
    }
    attributes
}

/// Create the event notification subscription an `AWS::Redshift::EventSubscription` declares,
/// under the name the declaration states — this resource's physical id, which is what its `Ref`
/// returns.
///
/// The subscription is created in the service that OWNS it, so the `SnsTopicArn` the declaration
/// names is resolved by Amazon Redshift against Amazon SNS exactly as it is for a client's own
/// call: a value that names no topic is that service's refusal, which fails the resource and
/// rolls the stack back rather than publishing a subscription that notifies nothing. What that
/// service reports for the subscription it made is what the stack publishes for the resource
/// ([`reported_attributes`]).
pub(crate) async fn create_event_subscription(
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
        REDSHIFT_EVENT_SUBSCRIPTION,
    )?;
    parameters.push((
        "SubscriptionName".to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        TAGS_PROPERTY,
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    let answered = call_query(
        ctx,
        REDSHIFT_SIGNING_NAME,
        region,
        REDSHIFT_API_VERSION,
        "CreateEventSubscription",
        &parameters,
    )
    .await?;
    Ok(Created::attributes(reported_attributes(&answered)))
}

/// The first part of a CHANGED declaration `ModifyEventSubscription` cannot express, or `None`
/// where every change it states is one that call carries.
///
/// Two cases, both of which would otherwise be applied by nothing while the stack reported the
/// update as complete: a member the new declaration DROPS, which that call has no way to unset —
/// a member a request omits leaves the subscription's own value in place — and the tag list,
/// which no member of that call carries and which Amazon Redshift's tagging surface resolves no
/// event-subscription ARN for.
fn change_the_modify_cannot_carry(
    resource: &StoredResource,
    declared: &Value,
) -> Option<&'static str> {
    if resource.properties.get(TAGS_PROPERTY) != declared.get(TAGS_PROPERTY) {
        return Some(TAGS_PROPERTY);
    }
    MODIFY_EVENT_SUBSCRIPTION_MEMBERS
        .iter()
        .map(|(member, _)| *member)
        .find(|member| {
            resource.properties.get(*member).is_some() && declared.get(*member).is_none()
        })
}

/// Apply a CHANGED declaration of the type to the subscription the stack already has.
///
/// Everything the subscription carries beside its name and its tags is a member of
/// `ModifyEventSubscription` — the topic it publishes to, the sources, categories and severity
/// it is scoped to, and whether it is active — so a deployment that states one of them
/// differently reaches that service as that one call, on the subscription the stack created, and
/// the subscription keeps the name its `Ref` publishes.
///
/// A change that call cannot express is REFUSED rather than quietly dropped
/// ([`change_the_modify_cannot_carry`]): the stack never reports a subscription scoped
/// differently from its declaration. The attributes the modify's own answer carries replace the
/// ones the create reported, so a resource whose status or resolved lists changed publishes what
/// that service reports now.
pub(crate) async fn update_event_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if let Some(unchangeable) = change_the_modify_cannot_carry(resource, declared) {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply a change to the {unchangeable} property of an \
                 existing {REDSHIFT_EVENT_SUBSCRIPTION}, which Amazon Redshift's \
                 ModifyEventSubscription carries no member for; the resource was left as it is."
            ),
        });
    }
    let mut parameters = query_members(
        declared,
        MODIFY_EVENT_SUBSCRIPTION_MEMBERS,
        &[],
        EVENT_SUBSCRIPTION_PROPERTIES_NOT_SENT,
        REDSHIFT_EVENT_SUBSCRIPTION,
    )?;
    parameters.push((
        "SubscriptionName".to_string(),
        resource.physical_resource_id.clone(),
    ));
    let answered = call_query(
        ctx,
        REDSHIFT_SIGNING_NAME,
        region,
        REDSHIFT_API_VERSION,
        "ModifyEventSubscription",
        &parameters,
    )
    .await?;
    Ok(Some(Created::attributes(reported_attributes(&answered))))
}

/// Delete the subscription a stack created, addressed by its name — this resource's physical id.
/// A subscription Amazon Redshift no longer holds (`SubscriptionNotFound`) is already in the
/// state the delete asks for.
pub(crate) async fn delete_event_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let parameters = vec![(
        "SubscriptionName".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        REDSHIFT_SIGNING_NAME,
        region,
        REDSHIFT_API_VERSION,
        "DeleteEventSubscription",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == SUBSCRIPTION_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The subscription's configuration as Amazon Redshift reports it, for drift detection. A
/// subscription it no longer holds (`SubscriptionNotFound`) was deleted, as is an answer that
/// carries no record of one at all: either way that service holds nothing under the name.
///
/// Its scalar members are read the way every other awsQuery record's are, which covers the
/// properties a declaration can drift in under the same name (`SnsTopicArn`, `SourceType`,
/// `Severity`, `Enabled`); the sources and categories are reported as LISTS under names of
/// their own (`SourceIdsList`, `EventCategoriesList`), so they take no part in the comparison —
/// the position every other arm whose list members are reported under another name is in, and
/// what [`super::drift`] documents.
pub(crate) async fn describe_event_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "SubscriptionName".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        REDSHIFT_SIGNING_NAME,
        region,
        REDSHIFT_API_VERSION,
        "DescribeEventSubscriptions",
        &parameters,
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
