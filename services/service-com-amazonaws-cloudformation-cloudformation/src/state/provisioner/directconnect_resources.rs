// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS Direct Connect's own control plane: the gateway an
//! `AWS::DirectConnect::DirectConnectGateway` declares and the group an
//! `AWS::DirectConnect::ResiliencyGroup` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! Each type's properties ARE its create's own members, named identically on both sides once
//! the initial is lower-cased ([`GATEWAY_MEMBERS`], [`RESILIENCY_GROUP_MEMBERS`]): that
//! service spells its members in lower camel case, as `directConnectGatewayName` and
//! `resiliencyGroupName`, and renames none of them.
//!
//! The IDENTITY of either resource is that service's. `CreateDirectConnectGateway` takes no
//! identifier and answers the `directConnectGatewayId` it drew; `CreateResiliencyGroup` takes
//! none either and answers a `resiliencyGroupId` of the form its own pattern states. Both
//! types publish the ARN built around that id as what `Ref` returns, and an ARN of this
//! service carries NO Region (`ResiliencyGroupArn`:
//! `^arn:(aws|aws-cn|aws-us-gov):directconnect::[0-9]{12}:dx-resiliency-group/\
//! dxrg-[0-9a-z]{17}$`), so the physical id each arm reports is that Region-less ARN
//! ([`gateway_arn`], [`resiliency_group_arn`]) and the id inside it is the attribute the type
//! publishes.
//!
//! Neither create leaves the resource usable: a gateway is answered `pending` ("the initial
//! state after calling `CreateDirectConnectGateway`") and a group `pending`, and in neither
//! state does the resource carry traffic — so each resource's creation is held open until that
//! service reports it `available` ([`direct_connect_gateway_readiness`],
//! [`resiliency_group_readiness`]). Their teardowns are asynchronous for the same reason:
//! either delete leaves the resource `deleting`, and the deletion is finished once that
//! service reports the terminal `deleted` ([`direct_connect_gateway_removal`],
//! [`resiliency_group_removal`]), which is also the state a torn-down resource stays readable
//! in.

use super::*;

/// AWS Direct Connect's signing name, from its model (`sigv4Name: directconnect`), which
/// speaks `awsJson1_1` — so its calls ride on [`call_json_rpc`], each under the
/// `X-Amz-Target` its service shape and operation name build.
const DIRECT_CONNECT_SIGNING_NAME: &str = "directconnect";

/// The service shape an `X-Amz-Target` of this service names before the operation, which is
/// that model's own service shape name (`com.amazonaws.directconnect#OvertureService`).
const DIRECT_CONNECT_TARGET_PREFIX: &str = "OvertureService";

/// The operations this module calls, as that service's own model names them.
const CREATE_DIRECT_CONNECT_GATEWAY: &str = "CreateDirectConnectGateway";
const DESCRIBE_DIRECT_CONNECT_GATEWAYS: &str = "DescribeDirectConnectGateways";
const UPDATE_DIRECT_CONNECT_GATEWAY: &str = "UpdateDirectConnectGateway";
const DELETE_DIRECT_CONNECT_GATEWAY: &str = "DeleteDirectConnectGateway";
const CREATE_RESILIENCY_GROUP: &str = "CreateResiliencyGroup";
const GET_RESILIENCY_GROUP: &str = "GetResiliencyGroup";
const UPDATE_RESILIENCY_GROUP: &str = "UpdateResiliencyGroup";
const DELETE_RESILIENCY_GROUP: &str = "DeleteResiliencyGroup";

/// The members and answer documents of the gateway half.
const GATEWAY_ID_MEMBER: &str = "directConnectGatewayId";
const GATEWAY_NAME_MEMBER: &str = "directConnectGatewayName";
const NEW_GATEWAY_NAME_MEMBER: &str = "newDirectConnectGatewayName";
const GATEWAY_MEMBER: &str = "directConnectGateway";
const GATEWAYS_MEMBER: &str = "directConnectGateways";
const GATEWAY_STATE_MEMBER: &str = "directConnectGatewayState";
const AMAZON_SIDE_ASN_MEMBER: &str = "amazonSideAsn";

/// The members and answer document of the resiliency-group half.
const RESILIENCY_GROUP_ID_MEMBER: &str = "resiliencyGroupId";
const RESILIENCY_GROUP_NAME_MEMBER: &str = "resiliencyGroupName";
const RESILIENCY_GROUP_MEMBER: &str = "resiliencyGroup";
const RESILIENCY_GROUP_STATE_MEMBER: &str = "state";
const INTENDED_RESILIENCY_MODEL_MEMBER: &str = "intendedResiliencyModel";

/// The member a tag set travels as on either create: a `TagList` of `{key, value}` pairs, so
/// the set is sent as the LIST that member takes ([`tags_as_list`] with this service's
/// lower-cased spelling) rather than as a map.
const TAGS_MEMBER: &str = "tags";

/// The `ResiliencyGroupState` and `DirectConnectGatewayState` a resource that is ready for use
/// is reported in: "available: The Direct Connect gateway is ready for use".
const DIRECT_CONNECT_AVAILABLE: &str = "available";

/// The states a resource of either type is still SETTLING in: `pending`, "the initial state
/// after calling `CreateDirectConnectGateway`", in which the resource carries no traffic yet.
const DIRECT_CONNECT_SETTLING: &[&str] = &["pending"];

/// The state a torn-down resource of either type is reported in while that service is still
/// removing it ("deleting: the initial state after calling `DeleteDirectConnectGateway`"), and
/// the TERMINAL state it reaches ("deleted: the Direct Connect gateway is deleted and cannot
/// pass traffic"), which is the answer that ends a deletion's wait.
const DIRECT_CONNECT_DELETING: &str = "deleting";
const DIRECT_CONNECT_DELETED: &str = "deleted";

/// The error code AWS Direct Connect answers every client fault under: its model declares one
/// client exception for the whole service (`DirectConnectClientException`, the modelled 400 of
/// each of these operations), so a resource that service no longer holds is reported under the
/// same code as any other bad parameter — which is why a DELETE here judges "already gone" on
/// its own READ of the resource rather than on that code.
const DIRECT_CONNECT_CLIENT_EXCEPTION: &str = "DirectConnectClientException";

/// Every property of `AWS::DirectConnect::DirectConnectGateway` this arm carries onto the
/// owning API, so a declaration stating one outside the set is REFUSED rather than reported as
/// created without it ([`refuse_unmapped_properties`]).
const GATEWAY_MAPPED_PROPERTIES: &[&str] = &["AmazonSideAsn", "DirectConnectGatewayName", "Tags"];

/// The one member `CreateDirectConnectGateway` takes beside the gateway's name and its tags:
/// "the autonomous system number (ASN) for Border Gateway Protocol (BGP) to be configured on
/// the Amazon side of the connection", a `LongAsn`, so a declaration stating it as text is
/// carried as the NUMBER that member takes ([`declared_number`]).
const GATEWAY_NUMBER_MEMBERS: &[(&str, &str)] = &[("AmazonSideAsn", AMAZON_SIDE_ASN_MEMBER)];

/// Every property of `AWS::DirectConnect::ResiliencyGroup` this arm carries onto the owning
/// API.
const RESILIENCY_GROUP_MAPPED_PROPERTIES: &[&str] =
    &["IntendedResiliencyModel", "ResiliencyGroupName", "Tags"];

/// The members `CreateResiliencyGroup` takes this type's two scalar properties as: each
/// property is the member of the same name with a lower-cased initial.
const RESILIENCY_GROUP_MEMBERS: &[(&str, &str)] = &[
    ("IntendedResiliencyModel", INTENDED_RESILIENCY_MODEL_MEMBER),
    ("ResiliencyGroupName", RESILIENCY_GROUP_NAME_MEMBER),
];

/// The members of the gateway's create that are not numbers: its NAME, which the declaration
/// states under the property of the same name.
const GATEWAY_MEMBERS: &[(&str, &str)] = &[("DirectConnectGatewayName", GATEWAY_NAME_MEMBER)];

/// The attributes each type publishes, and the member of that service's own report each is
/// read off: the identifier it minted for the resource.
const GATEWAY_REPORTED_ATTRIBUTES: &[(&str, &str)] =
    &[("DirectConnectGatewayId", GATEWAY_ID_MEMBER)];
const RESILIENCY_GROUP_REPORTED_ATTRIBUTES: &[(&str, &str)] =
    &[("ResiliencyGroupId", RESILIENCY_GROUP_ID_MEMBER)];

/// The members of each type's own report that carry the properties the declaration states,
/// for drift detection.
const GATEWAY_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("AmazonSideAsn", AMAZON_SIDE_ASN_MEMBER),
    ("DirectConnectGatewayName", GATEWAY_NAME_MEMBER),
];
const RESILIENCY_GROUP_REPORTED_PROPERTIES: &[(&str, &str)] =
    &[("ResiliencyGroupName", RESILIENCY_GROUP_NAME_MEMBER)];

/// Call one AWS Direct Connect operation on the `awsJson1_1` wire its model declares, naming
/// the operation so a refusal is reported the way that service's own SDK raises it.
async fn direct_connect(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        DIRECT_CONNECT_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{DIRECT_CONNECT_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// The ARN of the Direct Connect gateway `gateway_id`, which is what this type's `Ref`
/// returns: that service's ARN namespace, NO Region — a gateway "is global and visible in any
/// Amazon Web Services Region after it is created" — the account and the `dx-gateway/<id>`
/// resource path. Built from the same constants the naming rules compute the stand-in from
/// ([`super::template`]), so the published identity and the one the deployment re-points to it
/// are the same form.
fn gateway_arn(gateway_id: &str) -> String {
    ::emulator_server::emulator_arn(
        template::DIRECT_CONNECT_ARN_NAMESPACE,
        "",
        &format!("{}/{gateway_id}", template::DIRECT_CONNECT_GATEWAY_PATH),
    )
}

/// The ARN of the resiliency group `group_id`, built the same way from the resource path
/// `ResiliencyGroupArn`'s own pattern states.
fn resiliency_group_arn(group_id: &str) -> String {
    ::emulator_server::emulator_arn(
        template::DIRECT_CONNECT_ARN_NAMESPACE,
        "",
        &format!(
            "{}/{group_id}",
            template::DIRECT_CONNECT_RESILIENCY_GROUP_PATH
        ),
    )
}

/// The identifier inside the ARN that is a resource's physical id — the handle every AWS
/// Direct Connect call on the resource takes, which the ARN carries after its resource path.
fn identifier_of(physical_resource_id: &str) -> &str {
    physical_resource_id
        .rsplit('/')
        .next()
        .unwrap_or(physical_resource_id)
}

/// Create the gateway an `AWS::DirectConnect::DirectConnectGateway` declares, in AWS Direct
/// Connect — so a client that read the stack's outputs finds the gateway in that service's own
/// `DescribeDirectConnectGateways` and the virtual interfaces of the account can really be
/// attached to it.
///
/// The declared properties ARE the create's members ([`GATEWAY_MEMBERS`],
/// [`GATEWAY_NUMBER_MEMBERS`]), with the declared tags beside the ones CloudFormation gives
/// the resource ([`tags_of`]) as the `TagList` that call's own `tags` member takes.
///
/// The identity is that service's: the `directConnectGatewayId` it drew becomes the id inside
/// this resource's physical id, which is the ARN built around it, and the id is reported back
/// as the type's `DirectConnectGatewayId` attribute.
pub(super) async fn create_direct_connect_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, GATEWAY_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, GATEWAY_MEMBERS, &mut request);
    for (property, member) in GATEWAY_NUMBER_MEMBERS {
        if let Some(number) = properties.get(*property).and_then(declared_number) {
            request.insert((*member).to_string(), number);
        }
    }
    insert_tags(&mut request, &tags_of(&resource.properties, propagated));
    let answered = direct_connect(
        ctx,
        region,
        CREATE_DIRECT_CONNECT_GATEWAY,
        &Value::Object(request),
    )
    .await?;
    let reported = answered.get(GATEWAY_MEMBER).cloned().unwrap_or(Value::Null);
    let gateway_id = answered_identifier(&reported, GATEWAY_ID_MEMBER, "Direct Connect gateway")?;
    let mut attributes = reported_attributes(&reported, GATEWAY_REPORTED_ATTRIBUTES);
    attributes.insert("DirectConnectGatewayId".to_string(), gateway_id.clone());
    Ok(Created {
        physical_resource_id: Some(gateway_arn(&gateway_id)),
        attributes,
    })
}

/// Apply a changed declaration to the gateway the stack created, through
/// `UpdateDirectConnectGateway` — the one call that changes a gateway that exists, which
/// "updates the NAME of a current Direct Connect gateway" and takes nothing else. Every other
/// property of the type is fixed when the gateway is made, which is why a declaration
/// restating one replaces the resource ([`super::template::replacement_properties`]) instead
/// of reaching here.
pub(super) async fn update_direct_connect_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some(name) = declared
        .get("DirectConnectGatewayName")
        .and_then(scalar_text)
    else {
        return Ok(None);
    };
    let request = serde_json::json!({
        GATEWAY_ID_MEMBER: identifier_of(&resource.physical_resource_id),
        NEW_GATEWAY_NAME_MEMBER: name,
    });
    let answered = direct_connect(ctx, region, UPDATE_DIRECT_CONNECT_GATEWAY, &request).await?;
    let reported = answered.get(GATEWAY_MEMBER).cloned().unwrap_or(Value::Null);
    Ok(Some(Created::attributes(reported_attributes(
        &reported,
        GATEWAY_REPORTED_ATTRIBUTES,
    ))))
}

/// Delete the gateway a stack created, by the id inside the ARN that is this resource's
/// physical id. A gateway that service no longer holds is already in the state the delete asks
/// for, which is judged on its own READ of the gateway rather than on the one client error
/// code this service answers every bad parameter under.
pub(super) async fn delete_direct_connect_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let gateway_id = identifier_of(physical_resource_id);
    let request = serde_json::json!({ GATEWAY_ID_MEMBER: gateway_id });
    match direct_connect(ctx, region, DELETE_DIRECT_CONNECT_GATEWAY, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DIRECT_CONNECT_CLIENT_EXCEPTION => {
            match reported_gateway(ctx, region, gateway_id).await? {
                None => Ok(()),
                Some(_) => Err(refusal),
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// Whether AWS Direct Connect has finished bringing up the gateway a stack created.
///
/// A created gateway is `pending`, in which it passes no traffic at all, so
/// [`DIRECT_CONNECT_AVAILABLE`] is the state its creation is finished in; a gateway reported
/// being torn down, or one that service no longer holds, is in a state no wait will bring it
/// out of.
pub(super) async fn direct_connect_gateway_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let gateway_id = identifier_of(physical_resource_id);
    let Some(gateway) = reported_gateway(ctx, region, gateway_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AWS Direct Connect holds no Direct Connect gateway {gateway_id}"
        )));
    };
    Ok(settled_state(
        &gateway,
        GATEWAY_STATE_MEMBER,
        &format!("Direct Connect gateway {gateway_id}"),
    ))
}

/// Whether AWS Direct Connect has finished removing the gateway a stack deleted.
///
/// `DeleteDirectConnectGateway` leaves the gateway `deleting`, so it is still reported when
/// the delete answers; the deletion is finished once that service reports the terminal
/// `deleted`, which is also the state the torn-down gateway stays readable in.
pub(super) async fn direct_connect_gateway_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let gateway_id = identifier_of(physical_resource_id);
    Ok(removal_state(
        reported_gateway(ctx, region, gateway_id).await?,
        GATEWAY_STATE_MEMBER,
    ))
}

/// The gateway's configuration as AWS Direct Connect reports it, for drift detection: that
/// service's own record of it, read by the id the stack published. A gateway it no longer
/// reports was deleted.
///
/// The members it reports under the names this type declares are the gateway's name and the
/// Amazon-side ASN it was created with; a property the report does not carry is not evidence
/// of anything ([`super::drift`]).
pub(super) async fn describe_direct_connect_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        reported_gateway(ctx, region, identifier_of(physical_resource_id))
            .await?
            .map(|gateway| reported_as_properties(&gateway, GATEWAY_REPORTED_PROPERTIES)),
    )
}

/// That service's own record of ONE Direct Connect gateway, by the id it minted, or `None`
/// where its listing carries none under that id.
///
/// `DescribeDirectConnectGateways` is the one read of a gateway that service publishes, and it
/// is a LISTING narrowed by `directConnectGatewayId`, so the answer is searched for the id
/// asked about rather than read off its first element.
async fn reported_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gateway_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ GATEWAY_ID_MEMBER: gateway_id });
    let answered = direct_connect(ctx, region, DESCRIBE_DIRECT_CONNECT_GATEWAYS, &request).await?;
    let Some(Value::Array(listed)) = answered.get(GATEWAYS_MEMBER) else {
        return Ok(None);
    };
    Ok(listed
        .iter()
        .find(|gateway| reported_text(gateway, GATEWAY_ID_MEMBER).as_deref() == Some(gateway_id))
        .cloned())
}

/// Create the group an `AWS::DirectConnect::ResiliencyGroup` declares, in AWS Direct Connect —
/// so a client that read the stack's outputs finds the group in that service's own
/// `GetResiliencyGroup` and `ListResiliencyGroups` and connections can really be grouped under
/// it.
///
/// The declared properties ARE the create's members ([`RESILIENCY_GROUP_MEMBERS`]), with the
/// declared tags beside the ones CloudFormation gives the resource ([`tags_of`]).
///
/// The identity is that service's: the `resiliencyGroupId` it minted becomes the id inside
/// this resource's physical id, which is the ARN built around it — the same ARN that service
/// reports for the group — and the id is reported back as the type's `ResiliencyGroupId`
/// attribute.
pub(super) async fn create_resiliency_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, RESILIENCY_GROUP_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, RESILIENCY_GROUP_MEMBERS, &mut request);
    insert_tags(&mut request, &tags_of(&resource.properties, propagated));
    let answered = direct_connect(
        ctx,
        region,
        CREATE_RESILIENCY_GROUP,
        &Value::Object(request),
    )
    .await?;
    let reported = answered
        .get(RESILIENCY_GROUP_MEMBER)
        .cloned()
        .unwrap_or(Value::Null);
    let group_id =
        answered_identifier(&reported, RESILIENCY_GROUP_ID_MEMBER, "resiliency group")?;
    let mut attributes = reported_attributes(&reported, RESILIENCY_GROUP_REPORTED_ATTRIBUTES);
    attributes.insert("ResiliencyGroupId".to_string(), group_id.clone());
    Ok(Created {
        physical_resource_id: Some(resiliency_group_arn(&group_id)),
        attributes,
    })
}

/// Apply a changed declaration to the group the stack created, through
/// `UpdateResiliencyGroup` — the one call that changes a group that exists, which takes the
/// group's id and a new `resiliencyGroupName` and nothing else. The resiliency model the group
/// was created to meet is fixed, which is why a declaration restating it replaces the resource
/// instead of reaching here.
pub(super) async fn update_resiliency_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some(name) = declared.get("ResiliencyGroupName").and_then(scalar_text) else {
        return Ok(None);
    };
    let request = serde_json::json!({
        RESILIENCY_GROUP_ID_MEMBER: identifier_of(&resource.physical_resource_id),
        RESILIENCY_GROUP_NAME_MEMBER: name,
    });
    let answered = direct_connect(ctx, region, UPDATE_RESILIENCY_GROUP, &request).await?;
    let reported = answered
        .get(RESILIENCY_GROUP_MEMBER)
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Some(Created::attributes(reported_attributes(
        &reported,
        RESILIENCY_GROUP_REPORTED_ATTRIBUTES,
    ))))
}

/// Delete the group a stack created, by the id inside the ARN that is this resource's physical
/// id. A group that service no longer holds is already in the state the delete asks for,
/// judged on its own read for the reason the gateway's delete is.
pub(super) async fn delete_resiliency_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let group_id = identifier_of(physical_resource_id);
    let request = serde_json::json!({ RESILIENCY_GROUP_ID_MEMBER: group_id });
    match direct_connect(ctx, region, DELETE_RESILIENCY_GROUP, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DIRECT_CONNECT_CLIENT_EXCEPTION => {
            match reported_resiliency_group(ctx, region, group_id).await? {
                None => Ok(()),
                Some(_) => Err(refusal),
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// Whether AWS Direct Connect has finished bringing up the group a stack created — the group's
/// half of [`direct_connect_gateway_readiness`], which it reads the same way.
pub(super) async fn resiliency_group_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let group_id = identifier_of(physical_resource_id);
    let Some(group) = reported_resiliency_group(ctx, region, group_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AWS Direct Connect holds no resiliency group {group_id}"
        )));
    };
    Ok(settled_state(
        &group,
        RESILIENCY_GROUP_STATE_MEMBER,
        &format!("resiliency group {group_id}"),
    ))
}

/// Whether AWS Direct Connect has finished removing the group a stack deleted — the group's
/// half of [`direct_connect_gateway_removal`].
pub(super) async fn resiliency_group_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let group_id = identifier_of(physical_resource_id);
    Ok(removal_state(
        reported_resiliency_group(ctx, region, group_id).await?,
        RESILIENCY_GROUP_STATE_MEMBER,
    ))
}

/// The group's configuration as AWS Direct Connect reports it, for drift detection: that
/// service's own `GetResiliencyGroup`, by the id the stack published.
///
/// The one member it reports under a name this type declares is the group's own name; the
/// resiliency model the group was created to meet is not a member of the `ResiliencyGroup`
/// shape at all — that shape publishes the group's TYPE — so it is not evidence of anything
/// ([`super::drift`]).
pub(super) async fn describe_resiliency_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        reported_resiliency_group(ctx, region, identifier_of(physical_resource_id))
            .await?
            .map(|group| reported_as_properties(&group, RESILIENCY_GROUP_REPORTED_PROPERTIES)),
    )
}

/// That service's own record of ONE resiliency group, by the id it minted, or `None` where it
/// holds none under that id.
///
/// `GetResiliencyGroup` RESOLVES its group, so an id that service holds nothing under is its
/// one client exception; that is read as "no such group" rather than propagated, which is what
/// lets a deletion's wait and a drift read tell a group that is gone from one that is there.
async fn reported_resiliency_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ RESILIENCY_GROUP_ID_MEMBER: group_id });
    match direct_connect(ctx, region, GET_RESILIENCY_GROUP, &request).await {
        Ok(answered) => Ok(answered.get(RESILIENCY_GROUP_MEMBER).cloned()),
        Err(refusal) if refusal.code == DIRECT_CONNECT_CLIENT_EXCEPTION => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The identifier a create answered, or the refusal for an answer that carried none: there
/// would be no handle a client could call AWS Direct Connect with, and nothing for the stack
/// to publish, so that is a failed create rather than a created resource.
fn answered_identifier(
    reported: &Value,
    member: &str,
    subject: &str,
) -> Result<String, Refusal> {
    match reported_text(reported, member) {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the AWS Direct Connect control plane answered a created {subject} without an \
                 identifier"
            ),
        }),
    }
}

/// Put the tag set on a create's request as the `TagList` its own `tags` member takes, with
/// each pair's members spelled the way this service's `Tag` shape spells them (`key`/`value`).
///
/// A set with no tags in it contributes NO member: `TagList` is `@length 1..`, so an empty
/// list is a value that operation's own validation refuses.
fn insert_tags(request: &mut Map<String, Value>, tags: &[(String, String)]) {
    if tags.is_empty() {
        return;
    }
    request.insert(
        TAGS_MEMBER.to_string(),
        tags_as_list(tags, LOWER_CASED_TAG),
    );
}

/// What a reported resource's own state says about a creation that is still running: ready in
/// [`DIRECT_CONNECT_AVAILABLE`], still settling in one of [`DIRECT_CONNECT_SETTLING`], and
/// failed in every other state — one being torn down, one whose advance failed — which no wait
/// will bring the resource out of.
fn settled_state(reported: &Value, member: &str, subject: &str) -> Readiness {
    let state = reported_text(reported, member).unwrap_or_default();
    if state == DIRECT_CONNECT_AVAILABLE {
        return Readiness::Ready;
    }
    if DIRECT_CONNECT_SETTLING.contains(&state.as_str()) {
        return Readiness::Pending(state);
    }
    Readiness::Failed(format!(
        "AWS Direct Connect reports {subject} in state {state}"
    ))
}

/// What a reported resource says about a deletion that is still running: gone once that
/// service no longer reports the resource at all or reports it in the terminal
/// [`DIRECT_CONNECT_DELETED`], and still being removed in any other state.
fn removal_state(reported: Option<Value>, member: &str) -> Removal {
    let Some(reported) = reported else {
        return Removal::Gone;
    };
    let state = reported_text(&reported, member).unwrap_or_default();
    if state == DIRECT_CONNECT_DELETED {
        return Removal::Gone;
    }
    Removal::Pending(if state.is_empty() {
        DIRECT_CONNECT_DELETING.to_string()
    } else {
        state
    })
}
