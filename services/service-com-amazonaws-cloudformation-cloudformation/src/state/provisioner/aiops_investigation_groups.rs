// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon CloudWatch investigations' own control plane: the
//! investigation group an `AWS::AIOps::InvestigationGroup` declares. One file of [`super`]'s
//! per-type arms, dispatched to by its `Provisioner`.
//!
//! ## Why the resource is really created
//!
//! `CreateInvestigationGroup` takes the group's name, the IAM role it gathers investigation
//! data as, how long that data is kept, how it is encrypted, the chat channels it notifies, the
//! tag keys it narrows discovery by, whether it may read CloudTrail event history, the
//! cross-account roles it investigates through and its tags — it takes no identifier at all,
//! and answers the `arn` that service MINTED, whose resource path carries sixteen characters it
//! drew. That ARN is the value this type's `Ref` returns and the `identifier`
//! `GetInvestigationGroup`, `UpdateInvestigationGroup`, `DeleteInvestigationGroup`, the policy
//! calls and the tag plane each address a group by, so it becomes the resource's physical id,
//! replacing the stand-in the naming rules computed. A stack that reported the resource created
//! while nothing created it would publish an ARN resolving to nothing: no group to read, no
//! role resolved, no policy attached and no tag on it.
//!
//! ## The properties
//!
//! Most of the type's properties ARE that create's own members with a lower-cased initial
//! ([`CREATE_MEMBERS`], [`CREATE_NUMBERS`], [`CREATE_FLAGS`]). Three are spelled differently on
//! the two sides and are rewritten here:
//!
//! * `EncryptionConfiguration` publishes `EncryptionConfigurationType` and `KmsKeyId` while
//!   `EncryptionConfiguration`'s own members are `type` and `kmsKeyId`
//!   (`smithy_query(query="shape",shape="EncryptionConfiguration")`);
//! * `ChatbotNotificationChannels` is a LIST of `{SNSTopicArn, ChatConfigurationArns}` while
//!   `ChatbotNotificationChannel` is a MAP from the topic ARN to the configuration ARNs
//!   ([`chatbot_notification_channel`]);
//! * `Tags` is the usual `[{Key, Value}]` list while that call takes a tag MAP, so the tags
//!   CloudFormation gives the resource travel beside the declaration's own in that shape
//!   ([`tags_of`], [`tags_as_map`]) and the set is reconciled afterwards through that service's
//!   own `TagResource`/`UntagResource` ([`retag_investigation_group`]).
//!
//! `InvestigationGroupPolicy` is no member of any of those calls: the group's resource policy
//! is a resource of its own in that service, written by `PutInvestigationGroupPolicy` and taken
//! away by `DeleteInvestigationGroupPolicy`, so it is reconciled by its own call beside the
//! create and the update ([`reconcile_policy`]).
//!
//! A declared property no arm carries is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a group reported created without the role or the channels
//! its declaration asked for is the shallow success this arm exists to avoid.
//!
//! ## What the resource publishes
//!
//! The ARN beside the caller that service recorded for the group and the two instants of its
//! clock, all read off its own report of the group the create just made
//! ([`reported_attributes_of`]) rather than computed here — exactly as an
//! `AWS::Braket::SpendingLimit`'s `CreatedAt` is ([`super::braket_spending_limits`]).
//!
//! ## What a declaration that service refuses does
//!
//! Nothing here checks a declared value: every constraint is that service's own — an
//! investigation group gathers its data AS the role `RoleArn` names, so a role this account's
//! IAM does not show it able to assume is refused BY that service, as is a customer managed key
//! it cannot encrypt with, a configuration whose two encryption members disagree, a
//! cross-account source role outside this account and a second group in a Region that already
//! holds one. Each of those refusals is what fails the resource and rolls the stack back, which
//! is what a client calling that service directly would see.

use super::*;

/// Amazon CloudWatch investigations' signing name, from its model (`sigv4Name: aiops`, which is
/// also its `awsService.arnNamespace`), and it speaks `restJson1` — so its calls ride on
/// [`call_rest_json_named`], each on the URI its own operation's `@http` binding declares,
/// naming the operation so a refusal is reported the way that service's own SDK raises it.
pub(super) const AIOPS_SIGNING_NAME: &str = "aiops";

/// The collection `CreateInvestigationGroup` is bound to (`POST /investigationGroups`). ONE
/// group is addressed under it by the ARN that service minted
/// (`GET`/`PATCH`/`DELETE /investigationGroups/{identifier}`), its resource policy under that
/// group's own `policy` sub-resource, and the tag plane addresses one by the same ARN under its
/// own collection (`/tags/{resourceArn}`).
const INVESTIGATION_GROUPS_URI: &str = "/investigationGroups";
const POLICY_SEGMENT: &str = "policy";
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as that service's own model names them.
const CREATE_INVESTIGATION_GROUP: &str = "CreateInvestigationGroup";
const GET_INVESTIGATION_GROUP: &str = "GetInvestigationGroup";
const UPDATE_INVESTIGATION_GROUP: &str = "UpdateInvestigationGroup";
const DELETE_INVESTIGATION_GROUP: &str = "DeleteInvestigationGroup";
const GET_INVESTIGATION_GROUP_POLICY: &str = "GetInvestigationGroupPolicy";
const PUT_INVESTIGATION_GROUP_POLICY: &str = "PutInvestigationGroupPolicy";
const DELETE_INVESTIGATION_GROUP_POLICY: &str = "DeleteInvestigationGroupPolicy";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code that service answers for a group — or a group policy — it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read, its update, its delete, its
/// policy calls and its tag plane).
const AIOPS_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it.
const MAPPED_PROPERTIES: &[&str] = &[
    "ChatbotNotificationChannels",
    "CrossAccountConfigurations",
    "EncryptionConfiguration",
    "InvestigationGroupPolicy",
    "IsCloudTrailEventHistoryEnabled",
    "Name",
    "RetentionInDays",
    "RoleArn",
    "TagKeyBoundaries",
    "Tags",
];

/// The properties the create takes as the member of the same name with a lower-cased initial:
/// the group's name, the role it gathers data as and the tag keys it narrows discovery by.
const CREATE_MEMBERS: &[(&str, &str)] = &[
    ("Name", "name"),
    ("RoleArn", "roleArn"),
    ("TagKeyBoundaries", "tagKeyBoundaries"),
];

/// The one NUMERIC property, carried in the spelling a number travels in: `retentionInDays` is
/// a `Retention` (`@range 7..90`), so a template stating it as text reaches that member as a
/// number rather than as a string it would refuse.
const CREATE_NUMBERS: &[(&str, &str)] = &[("RetentionInDays", "retentionInDays")];

/// The one BOOLEAN property, carried in either spelling a template gives one, and the member it
/// travels as.
const CLOUD_TRAIL_PROPERTY: &str = "IsCloudTrailEventHistoryEnabled";
const CLOUD_TRAIL_MEMBER: &str = "isCloudTrailEventHistoryEnabled";
const CREATE_FLAGS: &[(&str, &str)] = &[(CLOUD_TRAIL_PROPERTY, CLOUD_TRAIL_MEMBER)];

/// The properties `UpdateInvestigationGroup` takes beside the `identifier` it addresses a group
/// by — everything that call can change on a group that exists. Its name and its retention are
/// not members of it at all, which is why a declaration restating either replaces the resource
/// ([`template::replacement_properties`], [`template::name_is_create_only`]) instead of
/// reaching here.
const UPDATE_MEMBERS: &[(&str, &str)] = &[("RoleArn", "roleArn")];
const TAG_KEY_BOUNDARIES_PROPERTY: &str = "TagKeyBoundaries";
const TAG_KEY_BOUNDARIES_MEMBER: &str = "tagKeyBoundaries";
const UPDATE_LISTS: &[(&str, &str)] =
    &[(TAG_KEY_BOUNDARIES_PROPERTY, TAG_KEY_BOUNDARIES_MEMBER)];

/// What `isCloudTrailEventHistoryEnabled` does when a request omits it: "The default is true"
/// (that member's own documentation), which is the value a group whose declaration states
/// nothing carries — so it is what an update asks for when the declaration stops stating it
/// ([`cleared_collections`]).
const DEFAULT_CLOUD_TRAIL_EVENT_HISTORY_ENABLED: bool = true;

/// The member the created group's ARN is answered in, which is also the member every read of
/// one reports it in and the handle every later call addresses a group by.
const ARN_MEMBER: &str = "arn";

/// The attribute the type publishes that ARN as, which is also the value its `Ref` returns.
const ARN_ATTRIBUTE: &str = "Arn";

/// The property the group's resource policy is declared under, and the member
/// `PutInvestigationGroupPolicy` and `GetInvestigationGroupPolicy` carry the document in.
const POLICY_PROPERTY: &str = "InvestigationGroupPolicy";
const POLICY_MEMBER: &str = "policy";

/// The property the encryption configuration is declared under, the member the create and the
/// update take it as, and the two members of the structure itself — which the type publishes
/// under names of its own.
const ENCRYPTION_PROPERTY: &str = "EncryptionConfiguration";
const ENCRYPTION_MEMBER: &str = "encryptionConfiguration";
const ENCRYPTION_MEMBERS: &[(&str, &str)] = &[
    ("EncryptionConfigurationType", "type"),
    ("KmsKeyId", "kmsKeyId"),
];

/// The property the cross-account configurations are declared under and the member they travel
/// as: a list whose elements' members are the same names with lower-cased initials, which is
/// what [`camel_cased`] does to them.
const CROSS_ACCOUNT_PROPERTY: &str = "CrossAccountConfigurations";
const CROSS_ACCOUNT_MEMBER: &str = "crossAccountConfigurations";

/// The property the chat channels are declared under and the member they travel as, with the
/// two members of one declared channel: the type publishes a LIST of structures while the
/// member is a MAP from the topic ARN to the configuration ARNs
/// ([`chatbot_notification_channel`]).
const CHANNELS_PROPERTY: &str = "ChatbotNotificationChannels";
const CHANNELS_MEMBER: &str = "chatbotNotificationChannel";
const CHANNEL_TOPIC_MEMBER: &str = "SNSTopicArn";
const CHANNEL_CONFIGURATIONS_MEMBER: &str = "ChatConfigurationArns";

/// The member the create and `TagResource` take their tag map in, which is also the member
/// `ListTagsForResource` answers a group's tags in — as a MAP, which is the shape
/// [`reported_tag_map`] reads.
const TAGS_MEMBER: &str = "tags";

/// The read-only values a read of a group reports, under the attributes the type publishes them
/// as: the principal that service recorded as having created the group and as having last
/// changed it, and the two instants of its own clock.
///
/// Each is that service's to report and none is computed here. The two instants are `Long`s on
/// that wire — the seconds its own `createdAt`/`lastModifiedAt` members carry — so each is
/// published as the text of the value it reported ([`attribute_text`]) rather than re-rendered
/// as something else.
const REPORTED_ATTRIBUTES: &[(&str, &str)] = &[
    ("CreatedBy", "createdBy"),
    ("CreatedAt", "createdAt"),
    ("LastModifiedBy", "lastModifiedBy"),
    ("LastModifiedAt", "lastModifiedAt"),
];

/// The members a read of a group reports under the properties the type declares them as, for
/// drift detection.
///
/// The three REWRITTEN properties are not among them: that service reports the encryption
/// configuration and each cross-account configuration under its own member names, and the chat
/// channels as a map rather than as the list a declaration states — so comparing either
/// document against the declaration would report a difference the template cannot express, and
/// a property the actual document does not carry is not evidence of anything
/// ([`super::drift`]). Its `Tags` are reconciled by [`retag_investigation_group`] instead.
const REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("IsCloudTrailEventHistoryEnabled", "isCloudTrailEventHistoryEnabled"),
    ("Name", "name"),
    ("RetentionInDays", "retentionInDays"),
    ("RoleArn", "roleArn"),
    ("TagKeyBoundaries", "tagKeyBoundaries"),
];

/// Create the investigation group an `AWS::AIOps::InvestigationGroup` declares, in Amazon
/// CloudWatch investigations itself — so a client that deployed the stack reads the group back
/// through that service's own `GetInvestigationGroup`, finds it in `ListInvestigationGroups` for
/// the ARN the stack published, and can start an investigation in the Region it belongs to.
///
/// The declaration IS the request, with the three properties the two sides spell differently
/// rewritten and the declared tags beside the ones CloudFormation gives the resource as the tag
/// MAP that call takes. A declared resource policy is attached by that service's own
/// `PutInvestigationGroupPolicy` once the group exists, which is the only call that writes one.
///
/// The identity is that service's: the `arn` it minted becomes this resource's physical id —
/// the type's primary identifier, which its `Ref` returns — and the read-only values the type
/// publishes are read off its own report of the group it has just stored.
pub(super) async fn create_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, CREATE_MEMBERS, &mut request);
    copy_numeric_members(&properties, CREATE_NUMBERS, &mut request);
    copy_flag_members(&properties, CREATE_FLAGS, &mut request);
    carry_rewritten_members(&properties, &mut request)?;
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        CREATE_INVESTIGATION_GROUP,
        "POST",
        INVESTIGATION_GROUPS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = reported_text(&answered, ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call that service with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Amazon CloudWatch investigations control plane answered a created \
                      investigation group without an ARN"
                .to_string(),
        });
    }
    reconcile_policy(ctx, region, &arn, declared_policy(&resource.properties)?).await?;
    let reported = read_investigation_group(ctx, region, &arn).await?;
    let mut attributes = reported_attributes_of(reported.as_ref());
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the group the stack created, through
/// `UpdateInvestigationGroup` — the one call that changes a group that exists — and through the
/// policy calls for the one property that is not a member of it.
///
/// That call takes the role the group gathers data as, its encryption configuration, its tag key
/// boundaries, its chat channels, its CloudTrail flag and its cross-account configurations,
/// addressed by the ARN that service minted. A declaration naming a role that service cannot
/// assume is ITS refusal, reported on the resource that failed, rather than a change applied
/// silently or a template-format error.
///
/// The request states the DECLARED state of every member that call takes, not only the members
/// the new declaration happens to carry: omitting one leaves the stored value in force
/// ("a member the request omits keeps the stored value"), so a property a deployment no longer
/// declares would otherwise stay applied while the stack reported the change carried through.
/// A collection the declaration no longer states is therefore sent EMPTY
/// ([`cleared_collections`]) and the CloudTrail flag as the value its own member documents for
/// an omission — which is what the create stored for a declaration stating neither.
pub(super) async fn update_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    // The same check the create makes, against the NEW declaration: a property no arm carries
    // onto the owning API would otherwise be reported as applied while being dropped.
    if let Some(property) = declared
        .as_object()
        .and_then(|properties| unmapped_property(properties, MAPPED_PROPERTIES))
    {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    let mut request = cleared_collections();
    if let Some(properties) = declared.as_object() {
        copy_members(properties, UPDATE_MEMBERS, &mut request);
        copy_members(properties, UPDATE_LISTS, &mut request);
        copy_flag_members(properties, CREATE_FLAGS, &mut request);
        carry_rewritten_members(properties, &mut request)?;
    }
    let identifier = &resource.physical_resource_id;
    call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        UPDATE_INVESTIGATION_GROUP,
        "PATCH",
        &investigation_group_uri(identifier),
        Some(&Value::Object(request)),
    )
    .await?;
    reconcile_policy(ctx, region, identifier, declared_policy(declared)?).await?;
    // The update answers no document, so what the resource now reports is read off that
    // service's own report of the group — the one reading the create uses.
    let reported = read_investigation_group(ctx, region, identifier).await?;
    let mut attributes = reported_attributes_of(reported.as_ref());
    attributes.insert(ARN_ATTRIBUTE.to_string(), identifier.clone());
    Ok(Created::attributes(attributes))
}

/// The members an update starts from, each stating the EMPTY collection or the documented
/// default: what the group is to carry where the new declaration states none of them.
///
/// `UpdateInvestigationGroup` keeps the stored value of a member its request omits, so what
/// clears a property a deployment no longer declares is sending that member empty. The
/// declaration's own values are copied over these afterwards, so a property it does state wins.
///
/// The CloudTrail flag is not a collection and is here for the same reason: its own
/// documentation states what the service does when the parameter is omitted ("The default is
/// true"), which is the value the create stored for a declaration stating nothing — so a
/// deployment that stops declaring it asks for that default back rather than for whatever was
/// last set. (`encryptionConfiguration` needs no entry: that service already stores the
/// documented Amazon Web Services owned key for a request omitting it.)
fn cleared_collections() -> Map<String, Value> {
    [
        (
            TAG_KEY_BOUNDARIES_MEMBER.to_string(),
            Value::Array(Vec::new()),
        ),
        (CROSS_ACCOUNT_MEMBER.to_string(), Value::Array(Vec::new())),
        (CHANNELS_MEMBER.to_string(), Value::Object(Map::new())),
        (
            CLOUD_TRAIL_MEMBER.to_string(),
            Value::Bool(DEFAULT_CLOUD_TRAIL_EVENT_HISTORY_ENABLED),
        ),
    ]
    .into_iter()
    .collect()
}

/// Bring the tags of the group a resource record names to the ones this deployment gives it,
/// through that service's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment; a group that service no longer holds has no tags to reconcile.
pub(super) async fn retag_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let uri = tags_uri(&resource.physical_resource_id);
    let answered = match call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == AIOPS_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // That read answers the tags as a MAP of key to value, which is the shape its own
    // `TagResource` takes them in too.
    let held = reported_tag_map(&answered, TAGS_MEMBER);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            AIOPS_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "POST",
            &uri,
            Some(&Value::Object(
                [(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&added)))]
                    .into_iter()
                    .collect(),
            )),
        )
        .await?;
    }
    if !removed.is_empty() {
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per
        // key rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            AIOPS_SIGNING_NAME,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{uri}?{keys}"),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the investigation group a stack created, by the ARN it was created under — this
/// resource's physical id. One that service no longer holds (`ResourceNotFoundException`, judged
/// on the CODE and never on a message that carries the request's own values) is already in the
/// state the delete asks for.
pub(super) async fn delete_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        DELETE_INVESTIGATION_GROUP,
        "DELETE",
        &investigation_group_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == AIOPS_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The group's configuration as that service reports it, for drift detection:
/// `GetInvestigationGroup`'s own record, read by the ARN the stack published, projected onto the
/// property names the type declares those members as ([`REPORTED_PROPERTIES`]). One it no longer
/// holds was deleted, which is what `None` reports.
pub(super) async fn describe_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_investigation_group(ctx, region, physical_resource_id)
        .await?
        .map(|group| reported_as_properties(&group, REPORTED_PROPERTIES)))
}

/// The group record that service holds under `identifier`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_investigation_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        GET_INVESTIGATION_GROUP,
        "GET",
        &investigation_group_uri(identifier),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == AIOPS_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Carry the three properties the type and the owning API spell differently onto `request`: the
/// encryption configuration, the cross-account configurations and the chat channels.
///
/// Shared by the create and the update because both calls take all three under the same member
/// names, so the one rewriting serves both and the two cannot disagree about it.
fn carry_rewritten_members(
    properties: &Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), Refusal> {
    if let Some(declared) = properties.get(ENCRYPTION_PROPERTY) {
        request.insert(
            ENCRYPTION_MEMBER.to_string(),
            encryption_configuration(declared),
        );
    }
    if let Some(declared) = properties.get(CROSS_ACCOUNT_PROPERTY) {
        request.insert(CROSS_ACCOUNT_MEMBER.to_string(), camel_cased(declared, 0)?);
    }
    if let Some(declared) = properties.get(CHANNELS_PROPERTY) {
        request.insert(
            CHANNELS_MEMBER.to_string(),
            chatbot_notification_channel(declared),
        );
    }
    Ok(())
}

/// One declared `EncryptionConfiguration` as the member both calls take it as: its two published
/// members under the names `EncryptionConfiguration` itself spells them ([`ENCRYPTION_MEMBERS`]).
///
/// A declaration that is not an object states no member of the structure and is passed through
/// as it came, so the owning operation's own validation is what refuses it rather than this
/// dropping it.
fn encryption_configuration(declared: &Value) -> Value {
    let Some(properties) = declared.as_object() else {
        return declared.clone();
    };
    let mut configuration = Map::new();
    copy_members(properties, ENCRYPTION_MEMBERS, &mut configuration);
    Value::Object(configuration)
}

/// The declared `ChatbotNotificationChannels` as the `ChatbotNotificationChannel` MAP both calls
/// take: one entry per declared channel, keyed by the Amazon SNS topic it notifies through and
/// valued with the chat application configurations associated with that topic.
///
/// The type publishes a LIST of structures and the member is a map ("For the first string,
/// specify the ARN of an Amazon SNS topic. For the array of strings, specify the ARNs of one or
/// more chat applications configurations"), so each element contributes one entry. An element
/// that states no topic contributes an entry under the empty key and one that states no
/// configurations an entry with no value, both of which the owning operation's own validation
/// refuses — rather than being dropped here, which would report the resource created with fewer
/// channels than its declaration asked for. A declaration that is not a list states no channel
/// at all and is passed through as it came.
fn chatbot_notification_channel(declared: &Value) -> Value {
    let Some(channels) = declared.as_array() else {
        return declared.clone();
    };
    let mut map = Map::new();
    for channel in channels {
        let topic = channel
            .get(CHANNEL_TOPIC_MEMBER)
            .and_then(scalar_text)
            .unwrap_or_default();
        let configurations = channel
            .get(CHANNEL_CONFIGURATIONS_MEMBER)
            .cloned()
            .unwrap_or(Value::Null);
        map.insert(topic, configurations);
    }
    Value::Object(map)
}

/// The resource policy this deployment declares for the group, as the JSON text
/// `PutInvestigationGroupPolicy` takes it in — `None` where the declaration states none.
///
/// A template states a policy either inline as an object (JSON or YAML) or as a string, and that
/// member takes it as text ([`document_text`]), so an object is rendered and a string passes
/// through. A declaration stating the property as anything else is REFUSED rather than attached
/// as something it is not.
fn declared_policy(properties: &Value) -> Result<Option<String>, Refusal> {
    let Some(declared) = properties.get(POLICY_PROPERTY) else {
        return Ok(None);
    };
    match document_text(declared) {
        Some(policy) => Ok(Some(policy)),
        None => Err(Refusal {
            code: "ValidationException".to_string(),
            message: format!(
                "the {POLICY_PROPERTY} property states no policy document a resource policy \
                 could be written from"
            ),
        }),
    }
}

/// Bring the resource policy of the group at `identifier` to the one this deployment declares,
/// through that service's own policy calls: `PutInvestigationGroupPolicy` where the declaration
/// states a document the group does not already carry, and `DeleteInvestigationGroupPolicy`
/// where it states none and the group carries one.
///
/// The group's policy is a resource of its own in that service — `GetInvestigationGroupPolicy`
/// answers its own not-found for a group that carries none — so what is reconciled is read from
/// there rather than assumed, which is what makes this safe to ask on every deployment: a group
/// whose policy already agrees with the declaration has nothing called about it.
async fn reconcile_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
    declared: Option<String>,
) -> Result<(), Refusal> {
    let held = read_investigation_group_policy(ctx, region, identifier).await?;
    match declared {
        Some(policy) if held.as_deref() != Some(policy.as_str()) => {
            call_rest_json_named(
                ctx,
                AIOPS_SIGNING_NAME,
                region,
                PUT_INVESTIGATION_GROUP_POLICY,
                "POST",
                &policy_uri(identifier),
                Some(&Value::Object(
                    [(POLICY_MEMBER.to_string(), Value::String(policy))]
                        .into_iter()
                        .collect(),
                )),
            )
            .await?;
        }
        Some(_) => {}
        None if held.is_some() => {
            match call_rest_json_named(
                ctx,
                AIOPS_SIGNING_NAME,
                region,
                DELETE_INVESTIGATION_GROUP_POLICY,
                "DELETE",
                &policy_uri(identifier),
                None,
            )
            .await
            {
                Ok(_) => {}
                // A policy that service no longer holds is already in the state this asks for.
                Err(refusal) if refusal.code == AIOPS_RESOURCE_NOT_FOUND => {}
                Err(refusal) => return Err(refusal),
            }
        }
        None => {}
    }
    Ok(())
}

/// The resource policy that service holds for the group at `identifier`, or `None` where it
/// holds none — which is the `ResourceNotFoundException` its own read answers for a group
/// carrying no policy, judged on the CODE and never on a message.
async fn read_investigation_group_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<Option<String>, Refusal> {
    match call_rest_json_named(
        ctx,
        AIOPS_SIGNING_NAME,
        region,
        GET_INVESTIGATION_GROUP_POLICY,
        "GET",
        &policy_uri(identifier),
        None,
    )
    .await
    {
        Ok(answered) => Ok(reported_text(&answered, POLICY_MEMBER)),
        Err(refusal) if refusal.code == AIOPS_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes one reported group gives the resource: the caller that service recorded for it
/// and the two instants of its clock ([`REPORTED_ATTRIBUTES`]). A group that service reports
/// nothing about gives none, so the resource keeps what the naming rules computed rather than
/// taking an empty value.
fn reported_attributes_of(reported: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(reported) = reported else {
        return attributes;
    };
    for (attribute, member) in REPORTED_ATTRIBUTES {
        if let Some(value) = reported.get(*member).filter(|value| !value.is_null()) {
            attributes.insert((*attribute).to_string(), attribute_text(value));
        }
    }
    attributes
}

/// The URI ONE investigation group is addressed at: the collection, then the `identifier`
/// `@httpLabel` escaped as a single path segment ([`percent_encoded`]) so an ARN's own `:` and
/// `/` address one group rather than another path.
fn investigation_group_uri(identifier: &str) -> String {
    format!(
        "{INVESTIGATION_GROUPS_URI}/{}",
        percent_encoded(identifier)
    )
}

/// The URI ONE group's resource POLICY is addressed at: that group's own URI and the segment the
/// policy calls bind under it.
fn policy_uri(identifier: &str) -> String {
    format!("{}/{POLICY_SEGMENT}", investigation_group_uri(identifier))
}

/// The URI the tags of ONE resource of that service are addressed at: its own collection and the
/// `resourceArn` label escaped the same way.
fn tags_uri(resource_arn: &str) -> String {
    format!("{TAGS_URI}/{}", percent_encoded(resource_arn))
}
