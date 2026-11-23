// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon VPC Lattice's own control plane: the service
//! network an `AWS::VpcLattice::ServiceNetwork` declares and the access-log subscription an
//! `AWS::VpcLattice::AccessLogSubscription` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! Both identities are that service's to mint, and both types' `Ref` returns the ARN it built:
//!
//! * `CreateServiceNetwork` takes the network's NAME and answers the `id` it assigned
//!   (`^sn-[0-9a-z]{17}$`) with the `arn` built around it. That ARN is the type's primary
//!   identifier and one of the two forms every call on a network takes, so it becomes this
//!   resource's physical id, replacing the stand-in the naming rules computed;
//! * `CreateAccessLogSubscription` takes the resource to monitor and the DESTINATION records
//!   go to, takes no identifier at all, and answers the `als-…` id with the
//!   `accesslogsubscription/als-…` ARN. That ARN is this resource's physical id for the same
//!   reason, and it is what `GetAccessLogSubscription` and `DeleteAccessLogSubscription`
//!   address one by.
//!
//! The subscription's destination is another service's resource — a CloudWatch Logs log
//! group, a Firehose delivery stream or an Amazon S3 bucket — and VPC Lattice itself refuses
//! a destination that service does not hold, with its own `ResourceNotFoundException` (HTTP
//! 404). Nothing is pre-checked here: the refusal belongs to the owning service, so what the
//! failed resource reports is that service's own sentence inside the provider envelope
//! ([`super::super::provisioning::handler_failure_reason`]), with the `NotFound` handler class
//! that code falls in.

use super::*;

/// Amazon VPC Lattice's signing name, from its model (`sigv4Name: vpc-lattice`), which speaks
/// `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares, naming the operation so a refusal is reported the way
/// that service's own SDK raises it.
pub(in crate::state) const VPC_LATTICE_SIGNING_NAME: &str = "vpc-lattice";

/// The collections that service binds these two resources to, as its own `@http` traits
/// declare: `POST /servicenetworks` creates a network and
/// `GET`/`PATCH`/`DELETE /servicenetworks/{serviceNetworkIdentifier}` read, change and remove
/// one; `POST /accesslogsubscriptions` creates a subscription and
/// `GET`/`PATCH`/`DELETE /accesslogsubscriptions/{accessLogSubscriptionIdentifier}` read,
/// change and remove one.
const SERVICE_NETWORKS_URI: &str = "/servicenetworks";
const ACCESS_LOG_SUBSCRIPTIONS_URI: &str = "/accesslogsubscriptions";

/// The collection its tagging surface is bound to: `GET /tags/{resourceArn}` reads a
/// resource's tags, `POST` adds them and `DELETE … ?tagKeys=` drops them, each addressing the
/// resource by the ARN that service minted.
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as Amazon VPC Lattice's own model names them.
const CREATE_SERVICE_NETWORK: &str = "CreateServiceNetwork";
const GET_SERVICE_NETWORK: &str = "GetServiceNetwork";
const UPDATE_SERVICE_NETWORK: &str = "UpdateServiceNetwork";
const DELETE_SERVICE_NETWORK: &str = "DeleteServiceNetwork";
const CREATE_ACCESS_LOG_SUBSCRIPTION: &str = "CreateAccessLogSubscription";
const GET_ACCESS_LOG_SUBSCRIPTION: &str = "GetAccessLogSubscription";
const UPDATE_ACCESS_LOG_SUBSCRIPTION: &str = "UpdateAccessLogSubscription";
const DELETE_ACCESS_LOG_SUBSCRIPTION: &str = "DeleteAccessLogSubscription";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code Amazon VPC Lattice answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, its updates and its deletes).
const VPC_LATTICE_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The members a create answers the identity it minted in, and the ones the further published
/// values come back in (`smithy_query(query="operation",operation="CreateServiceNetwork")`,
/// `…="CreateAccessLogSubscription"`).
const ID_MEMBER: &str = "id";
const ARN_MEMBER: &str = "arn";
const RESOURCE_ID_MEMBER: &str = "resourceId";
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The members `GetServiceNetwork` reports the two instants of that service's clock in.
const CREATED_AT_MEMBER: &str = "createdAt";
const LAST_UPDATED_AT_MEMBER: &str = "lastUpdatedAt";

/// The member the tag set travels as on either create, and on `TagResource`: a `TagMap`, so
/// the set is sent as the MAP of key to value that member takes ([`tags_as_map`]) rather than
/// as a list of pairs.
const TAGS_MEMBER: &str = "tags";

/// The property the service network's NAME is declared in, and the member
/// `CreateServiceNetwork` takes it as.
const SERVICE_NETWORK_NAME_PROPERTY: &str = "Name";
const NAME_MEMBER: &str = "name";

/// The property the network's authentication mode is declared in, and the member both the
/// create and `UpdateServiceNetwork` take it as — that update's ONE member beside the
/// identifier, which is why it is the only property a changed declaration applies in place.
const AUTH_TYPE_PROPERTY: &str = "AuthType";
const AUTH_TYPE_MEMBER: &str = "authType";

/// The property the network's sharing setting is declared in, its one inner member, and the
/// member and inner member `CreateServiceNetwork` takes them as
/// (`SharingConfig$enabled`).
const SHARING_CONFIG_PROPERTY: &str = "SharingConfig";
const SHARING_ENABLED_PROPERTY: &str = "Enabled";
const SHARING_CONFIG_MEMBER: &str = "sharingConfig";
const SHARING_ENABLED_MEMBER: &str = "enabled";

/// Every property of `AWS::VpcLattice::ServiceNetwork` this arm carries onto the owning API,
/// so a declaration stating one outside the set is REFUSED rather than reported as created
/// without it ([`refuse_unmapped_properties`]).
const SERVICE_NETWORK_MAPPED_PROPERTIES: &[&str] = &[
    AUTH_TYPE_PROPERTY,
    SERVICE_NETWORK_NAME_PROPERTY,
    SHARING_CONFIG_PROPERTY,
    "Tags",
];

/// The properties of `AWS::VpcLattice::AccessLogSubscription` that are
/// `CreateAccessLogSubscription`'s own members under the same name with a lower-cased initial,
/// with the member each is carried as.
const ACCESS_LOG_SUBSCRIPTION_MEMBERS: &[(&str, &str)] = &[
    ("DestinationArn", "destinationArn"),
    ("ResourceIdentifier", "resourceIdentifier"),
    ("ServiceNetworkLogType", "serviceNetworkLogType"),
];

/// The member `UpdateAccessLogSubscription` takes — the destination, which is the one thing
/// that call changes on a subscription that exists.
const DESTINATION_ARN_PROPERTY: &str = "DestinationArn";
const DESTINATION_ARN_MEMBER: &str = "destinationArn";

/// Every property of the subscription type this arm carries onto the owning API.
const ACCESS_LOG_SUBSCRIPTION_MAPPED_PROPERTIES: &[&str] = &[
    DESTINATION_ARN_PROPERTY,
    "ResourceIdentifier",
    "ServiceNetworkLogType",
    "Tags",
];

/// The prefix of the name a declaration that states none is created under, which the generated
/// name below opens with so it satisfies the modelled `ServiceNetworkName`
/// (`^(?![-])(?!.*[-]$)(?!.*[-]{2})[a-z0-9-]+$`, `@length 3..63`): lower case and digits, no
/// leading, trailing or doubled hyphen.
const GENERATED_SERVICE_NETWORK_NAME_PREFIX: &str = "service-network-";

/// Create the service network an `AWS::VpcLattice::ServiceNetwork` declares, in Amazon VPC
/// Lattice — so a client that deployed the stack reads the network back through that service's
/// own `GetServiceNetwork` and `ListServiceNetworks` for the identity the stack published, and
/// the tags the deployment gives it are what its `ListTagsForResource` answers.
///
/// The declared properties ARE the create's members once their initials are lower-cased, with
/// the declared tags beside the ones CloudFormation gives the resource ([`tags_of`]) as the
/// `TagMap` that call's own `tags` member takes.
///
/// The identity is that service's: the `arn` it answered becomes this resource's physical id —
/// the type's primary identifier, which its `Ref` returns — and the id inside it, beside the
/// two instants that service recorded, are the attributes the type publishes.
pub(super) async fn create_vpc_lattice_service_network(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SERVICE_NETWORK_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(service_network_name(&properties, resource)),
    );
    if let Some(auth_type) = properties.get(AUTH_TYPE_PROPERTY) {
        request.insert(AUTH_TYPE_MEMBER.to_string(), auth_type.clone());
    }
    if let Some(sharing) = sharing_config_member(&properties) {
        request.insert(SHARING_CONFIG_MEMBER.to_string(), sharing);
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        CREATE_SERVICE_NETWORK,
        "POST",
        SERVICE_NETWORKS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = answered_arn(&answered, "service network")?;
    // The instants the type publishes are that service's clock, so they are read off its own
    // report of the network the create just made rather than from any clock here.
    let reported = read_service_network(ctx, region, &arn).await?;
    let mut attributes = reported
        .as_ref()
        .map(reported_service_network_attributes)
        .unwrap_or_default();
    attributes.insert("Arn".to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the service network the stack created, through
/// `UpdateServiceNetwork` — the one call that reconfigures a network that exists.
///
/// It takes the network's `authType` and nothing else, addressed by the ARN the stack
/// publishes; every other property is fixed when the network is made, which is why a
/// declaration restating one replaces the resource
/// ([`super::super::template::replacement_properties`]) instead of reaching here. A
/// declaration that drops the property leaves the network's mode where it is, that member
/// being REQUIRED on the update: there is no value to send that means "no change".
pub(super) async fn update_vpc_lattice_service_network(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some(auth_type) = declared.get(AUTH_TYPE_PROPERTY).cloned() else {
        return Ok(None);
    };
    let request = Map::from_iter([(AUTH_TYPE_MEMBER.to_string(), auth_type)]);
    call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        UPDATE_SERVICE_NETWORK,
        "PATCH",
        &service_network_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // The update answers no instant, so what the resource now publishes is read off that
    // service's own report of the network.
    let reported = read_service_network(ctx, region, &resource.physical_resource_id).await?;
    let mut attributes = reported
        .as_ref()
        .map(reported_service_network_attributes)
        .unwrap_or_default();
    attributes
        .entry("Arn".to_string())
        .or_insert_with(|| resource.physical_resource_id.clone());
    Ok(Some(Created::attributes(attributes)))
}

/// Delete the service network a stack created, by the ARN it publishes — this resource's
/// physical id, which is one of the two forms `DeleteServiceNetwork` takes. One Amazon VPC
/// Lattice no longer holds (`ResourceNotFoundException`, judged on the CODE and never on a
/// message that carries the request's own values) is already in the state the delete asks for.
pub(super) async fn delete_vpc_lattice_service_network(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_record(
        ctx,
        region,
        DELETE_SERVICE_NETWORK,
        &service_network_uri(physical_resource_id),
    )
    .await
}

/// The network's configuration as Amazon VPC Lattice reports it, for drift detection:
/// `GetServiceNetwork`'s own record, read by the ARN the stack published. One that service no
/// longer holds was deleted.
///
/// The members it reports under the names this type declares are the network's name and its
/// authentication mode; a property the read does not report is not evidence of anything
/// ([`super::drift`]).
pub(super) async fn describe_vpc_lattice_service_network(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_service_network(ctx, region, physical_resource_id)
        .await?
        .map(|network| {
            reported_as_properties(
                &network,
                &[
                    (SERVICE_NETWORK_NAME_PROPERTY, NAME_MEMBER),
                    (AUTH_TYPE_PROPERTY, AUTH_TYPE_MEMBER),
                ],
            )
        }))
}

/// Create the access-log subscription an `AWS::VpcLattice::AccessLogSubscription` declares, so
/// a client that deployed the stack finds it in that service's own
/// `ListAccessLogSubscriptions` for the monitored resource and the records really go to the
/// destination the template asked for.
///
/// The declared properties ARE the create's members once their initials are lower-cased
/// ([`ACCESS_LOG_SUBSCRIPTION_MEMBERS`]), the declared tags travelling beside the ones
/// CloudFormation gives the resource. The identity is that service's: the `arn` it answered
/// becomes this resource's physical id, and the id inside it beside the monitored resource's
/// own id and ARN are the attributes the type publishes.
///
/// A destination the owning service does not hold is ITS refusal, not a pre-check here: the
/// call is made and that service's `ResourceNotFoundException` fails the resource.
pub(super) async fn create_vpc_lattice_access_log_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, ACCESS_LOG_SUBSCRIPTION_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, ACCESS_LOG_SUBSCRIPTION_MEMBERS, &mut request);
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        CREATE_ACCESS_LOG_SUBSCRIPTION,
        "POST",
        ACCESS_LOG_SUBSCRIPTIONS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = answered_arn(&answered, "access log subscription")?;
    let mut attributes = reported_subscription_attributes(&answered);
    attributes.insert("Arn".to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the subscription the stack created, through
/// `UpdateAccessLogSubscription` — the one call that changes a subscription that exists, and
/// the one member it takes is the DESTINATION.
///
/// Every other property is fixed when the subscription is made, so a declaration restating one
/// replaces the resource instead of reaching here; a declaration that drops the destination
/// leaves it where it is, that member being REQUIRED on the update.
pub(super) async fn update_vpc_lattice_access_log_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some(destination) = declared.get(DESTINATION_ARN_PROPERTY).cloned() else {
        return Ok(None);
    };
    let request = Map::from_iter([(DESTINATION_ARN_MEMBER.to_string(), destination)]);
    let answered = call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        UPDATE_ACCESS_LOG_SUBSCRIPTION,
        "PATCH",
        &access_log_subscription_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let mut attributes = reported_subscription_attributes(&answered);
    attributes
        .entry("Arn".to_string())
        .or_insert_with(|| resource.physical_resource_id.clone());
    Ok(Some(Created::attributes(attributes)))
}

/// Delete the subscription a stack created, by the ARN it publishes — this resource's physical
/// id, which is one of the two forms `DeleteAccessLogSubscription` takes. One that service no
/// longer holds is already in the state the delete asks for.
pub(super) async fn delete_vpc_lattice_access_log_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_record(
        ctx,
        region,
        DELETE_ACCESS_LOG_SUBSCRIPTION,
        &access_log_subscription_uri(physical_resource_id),
    )
    .await
}

/// The subscription's configuration as that service reports it, for drift detection:
/// `GetAccessLogSubscription`'s own record, read by the ARN the stack published.
///
/// The members it reports under the names this type declares are the destination and the
/// monitored resource's own ARN — which is the form `ResourceIdentifier` is reported in — and
/// the log type; a property the read does not report is not evidence of anything.
pub(super) async fn describe_vpc_lattice_access_log_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        read_access_log_subscription(ctx, region, physical_resource_id)
            .await?
            .map(|subscription| {
                reported_as_properties(
                    &subscription,
                    &[
                        (DESTINATION_ARN_PROPERTY, DESTINATION_ARN_MEMBER),
                        ("ServiceNetworkLogType", "serviceNetworkLogType"),
                    ],
                )
            }),
    )
}

/// Bring the tags of the network or the subscription a resource record names to the ones this
/// deployment gives it, through Amazon VPC Lattice's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment.
///
/// Those calls address a resource by its ARN, which is this resource's physical id for either
/// type, so no read is needed to find it.
pub(super) async fn retag_vpc_lattice_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let uri = format!(
        "{TAGS_URI}/{}",
        percent_encoded(&resource.physical_resource_id)
    );
    let answered = call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await?;
    let held = reported_tag_map(&answered, TAGS_MEMBER);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            VPC_LATTICE_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "POST",
            &uri,
            Some(&Value::Object(Map::from_iter([(
                TAGS_MEMBER.to_string(),
                Value::Object(tags_as_map(&added)),
            )]))),
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
            VPC_LATTICE_SIGNING_NAME,
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

/// The service-network record that service holds under `identifier`, or `None` when it holds
/// none.
async fn read_service_network(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_SERVICE_NETWORK,
        &service_network_uri(identifier),
    )
    .await
}

/// The subscription record that service holds under `identifier`, or `None` when it holds none.
async fn read_access_log_subscription(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_ACCESS_LOG_SUBSCRIPTION,
        &access_log_subscription_uri(identifier),
    )
    .await
}

/// The ONE read both resources' reads are: that service's own `GET` of the record, with the
/// not-found it models read as the record being gone rather than as a refused read.
async fn read_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    uri: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(ctx, VPC_LATTICE_SIGNING_NAME, region, operation, "GET", uri, None)
        .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == VPC_LATTICE_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The ONE delete both resources' deletes are: that service's own `DELETE` of the record, with
/// the not-found it models read as the resource already being in the state the delete asks for.
async fn delete_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    uri: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        operation,
        "DELETE",
        uri,
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == VPC_LATTICE_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The ARN a create answered, or the refusal for an answer that carried none: there would be no
/// handle a client could call Amazon VPC Lattice with, and nothing for the stack to publish, so
/// that is a failed create rather than a created resource. `subject` names what was being
/// created.
fn answered_arn(answered: &Value, subject: &str) -> Result<String, Refusal> {
    match reported_text(answered, ARN_MEMBER) {
        Some(arn) if !arn.is_empty() => Ok(arn),
        _ => Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the Amazon VPC Lattice control plane answered a created {subject} without an ARN"
            ),
        }),
    }
}

/// The attributes one reported service network gives the resource: the id that service minted
/// and the two instants it recorded, read the one way every arm reads a reported attribute
/// ([`reported_attributes`]). The ARN is set by the caller, which holds the one the create
/// answered.
fn reported_service_network_attributes(network: &Value) -> BTreeMap<String, String> {
    reported_attributes(
        network,
        &[
            ("Id", ID_MEMBER),
            ("CreatedAt", CREATED_AT_MEMBER),
            ("LastUpdatedAt", LAST_UPDATED_AT_MEMBER),
        ],
    )
}

/// The attributes one answered subscription gives the resource: the id that service minted and
/// the monitored resource's own id and ARN, which that service resolved rather than this
/// arm recomputing them.
fn reported_subscription_attributes(subscription: &Value) -> BTreeMap<String, String> {
    reported_attributes(
        subscription,
        &[
            ("Id", ID_MEMBER),
            ("ResourceId", RESOURCE_ID_MEMBER),
            ("ResourceArn", RESOURCE_ARN_MEMBER),
        ],
    )
}

/// The NAME the service network is created under: the `Name` the declaration states, or — for a
/// declaration that states none, which the type's own documentation allows ("If you don't
/// specify a name, CloudFormation generates one") — a name derived from the identity
/// CloudFormation computed for this resource, which is a function of the stack and the
/// resource's logical id and therefore unique to it.
///
/// Derived rather than taken as the generated `<stack>-<logical id>-<unique id>` name: that
/// name carries the stack's own characters, which the modelled `ServiceNetworkName` pattern
/// does not admit in general.
fn service_network_name(properties: &Map<String, Value>, resource: &StoredResource) -> String {
    if let Some(name) = properties
        .get(SERVICE_NETWORK_NAME_PROPERTY)
        .and_then(scalar_text)
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    let computed = resource
        .physical_resource_id
        .rsplit(['-', '/'])
        .next()
        .unwrap_or(&resource.physical_resource_id);
    format!("{GENERATED_SERVICE_NETWORK_NAME_PREFIX}{computed}")
}

/// The `sharingConfig` the declaration asks for, or `None` for a declaration that states none:
/// the type publishes `SharingConfig.Enabled`, which is that structure's own `enabled` member,
/// read in either spelling a template gives a boolean ([`declared_flag`]) and sent as it came
/// for a value that is no boolean at all, so that operation's own validation is what refuses
/// it.
fn sharing_config_member(properties: &Map<String, Value>) -> Option<Value> {
    let declared = properties.get(SHARING_CONFIG_PROPERTY)?.as_object()?;
    let enabled = declared.get(SHARING_ENABLED_PROPERTY)?;
    let carried = declared_flag(declared, SHARING_ENABLED_PROPERTY)
        .map(Value::Bool)
        .unwrap_or_else(|| enabled.clone());
    Some(Value::Object(Map::from_iter([(
        SHARING_ENABLED_MEMBER.to_string(),
        carried,
    )])))
}

/// The URI one service network is addressed at: the collection and the
/// `serviceNetworkIdentifier` `@httpLabel` escaped as a single path segment
/// ([`percent_encoded`]), so an ARN's own `:` and `/` address one network rather than another
/// path.
fn service_network_uri(identifier: &str) -> String {
    format!("{SERVICE_NETWORKS_URI}/{}", percent_encoded(identifier))
}

/// The URI one subscription is addressed at, built the same way from the
/// `accessLogSubscriptionIdentifier` label.
fn access_log_subscription_uri(identifier: &str) -> String {
    format!(
        "{ACCESS_LOG_SUBSCRIPTIONS_URI}/{}",
        percent_encoded(identifier)
    )
}
