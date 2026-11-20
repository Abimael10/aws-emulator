// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the AWS Client VPN family of Amazon EC2: the Client VPN
//! endpoint an `AWS::EC2::ClientVpnEndpoint` declares, and the ingress authorization rule an
//! `AWS::EC2::ClientVpnAuthorizationRule` grants on one. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! ## The two resources
//!
//! A Client VPN ENDPOINT is "the resource you create and configure to enable and manage
//! client VPN sessions" (`CreateClientVpnEndpoint`): that one call makes it, out of the
//! client address range it hands connecting clients, the ACM certificates it presents and
//! the authentication options it checks a client against, and Amazon EC2 MINTS the
//! `cvpn-endpoint-…` id every later call on one takes — which is what this type's `Ref`
//! returns, so that id is the resource's physical id and
//! [`super::template::identity_is_service_assigned`] names the type.
//!
//! An AUTHORIZATION RULE is no resource of EC2's own: "ingress authorization rules act as
//! firewall rules that grant access to networks" (`AuthorizeClientVpnIngress`), and that
//! service mints no identifier for one — every call on a rule names it by the endpoint it is
//! on, the network it grants and the group it grants that network to. So the physical id of
//! one is the pair a declaration states ([`super::template::composite_identity_properties`]
//! carries [`EC2_CLIENT_VPN_AUTHORIZATION_RULE_IDENTITY`]), the same reading an
//! `AWS::EC2::Route` is given.
//!
//! ## How the declarations reach EC2
//!
//! Both types' properties ARE their create's own members, so each is carried onto the call
//! under the member of the same name — with the three places the two sides spell a member
//! differently taken from the wire's own form: Amazon EC2's query protocol carries a list
//! FLATTENED under the member's own name (`DnsServers.1`), and three of the endpoint's lists
//! are named in the singular on the wire (`Authentication.N`, `SecurityGroupId.N`,
//! `TagSpecification.N`). A property outside those tables is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]), so a stack never reports an endpoint configured
//! differently from its declaration.
//!
//! The tags CloudFormation gives the endpoint ride on the create, as that call's own
//! `TagSpecifications` member: this type states its tags there rather than in a `Tags`
//! property, so the declared set is read out of it ([`declared_endpoint_tag_set`]) and merged
//! with the propagated one the way every other arm merges them ([`tags_of`]). An
//! authorization rule is not a taggable resource — the type publishes no tag property at all
//! — so nothing is propagated to one.

use super::*;

/// The `AWS::EC2::ClientVpnEndpoint` resource type: the Client VPN endpoint
/// `CreateClientVpnEndpoint` makes. "Ref returns the Client VPN endpoint ID", which is the
/// `cvpn-endpoint-…` id that service mints, and the type publishes no `Fn::GetAtt` attribute
/// of its own.
pub(crate) const EC2_CLIENT_VPN_ENDPOINT: &str = "AWS::EC2::ClientVpnEndpoint";

/// The `AWS::EC2::ClientVpnAuthorizationRule` resource type: ONE ingress authorization rule
/// of such an endpoint, which `AuthorizeClientVpnIngress` adds and `RevokeClientVpnIngress`
/// takes off again.
pub(crate) const EC2_CLIENT_VPN_AUTHORIZATION_RULE: &str =
    "AWS::EC2::ClientVpnAuthorizationRule";

/// The properties an authorization rule's identity is the join of: the endpoint the rule is
/// on and the network it grants access to, which are the two members
/// `RevokeClientVpnIngress` addresses one by beside the group. Both are `@required` on this
/// type, so a declaration always carries them.
pub(crate) const EC2_CLIENT_VPN_AUTHORIZATION_RULE_IDENTITY: &[&str] =
    &["ClientVpnEndpointId", "TargetNetworkCidr"];

/// Every property of `AWS::EC2::ClientVpnEndpoint`, which is every property this arm carries
/// onto `CreateClientVpnEndpoint` — the scalar, structured and list members below plus the
/// tag specifications. A declaration stating one outside the set is refused rather than
/// reported as created without it.
const CLIENT_VPN_ENDPOINT_PROPERTIES: &[&str] = &[
    "AuthenticationOptions",
    "ClientCidrBlock",
    "ClientConnectOptions",
    "ClientLoginBannerOptions",
    "ClientRouteEnforcementOptions",
    "ConnectionLogOptions",
    "Description",
    "DisconnectOnSessionTimeout",
    "DnsServers",
    "SecurityGroupIds",
    "SelfServicePortal",
    "ServerCertificateArn",
    "SessionTimeoutHours",
    "SplitTunnel",
    "TagSpecifications",
    "TransportProtocol",
    "VpcId",
    "VpnPort",
];

/// The properties that are SCALAR members of `CreateClientVpnEndpoint`, each under the
/// member of the same name.
const CLIENT_VPN_ENDPOINT_SCALARS: &[&str] = &[
    "ClientCidrBlock",
    "Description",
    "DisconnectOnSessionTimeout",
    "SelfServicePortal",
    "ServerCertificateArn",
    "SessionTimeoutHours",
    "SplitTunnel",
    "TransportProtocol",
    "VpcId",
    "VpnPort",
];

/// The properties that are STRUCTURE members of that call, whose own members follow the
/// member's name (`ConnectionLogOptions.Enabled`) — nested the same way however deep the
/// structure goes.
const CLIENT_VPN_ENDPOINT_STRUCTURES: &[&str] = &[
    "ClientConnectOptions",
    "ClientLoginBannerOptions",
    "ClientRouteEnforcementOptions",
    "ConnectionLogOptions",
];

/// The properties that are LIST members of that call, as `(property, wire member)`: Amazon
/// EC2's query protocol indexes a list's entries from 1 directly under the member's name, and
/// two of these members are named in the singular there — `AuthenticationOptions` travels as
/// `Authentication.N` and `SecurityGroupIds` as `SecurityGroupId.N`, which is how the model's
/// own `@xmlName` for each member spells it.
const CLIENT_VPN_ENDPOINT_LISTS: &[(&str, &str)] = &[
    ("AuthenticationOptions", "Authentication"),
    ("DnsServers", "DnsServers"),
    ("SecurityGroupIds", "SecurityGroupId"),
];

/// The `ResourceType` a Client VPN endpoint is tagged under, which is the only type that
/// create's own `TagSpecification` may name
/// (`smithy_query(query="shape",shape="ResourceType")` lists `client-vpn-endpoint`).
const CLIENT_VPN_ENDPOINT_RESOURCE_TYPE: &str = "client-vpn-endpoint";

/// Every property of `AWS::EC2::ClientVpnAuthorizationRule`, which is also every member
/// `AuthorizeClientVpnIngress` takes beside the idempotency token and the dry-run flag — so
/// each is carried onto that call under the member of the same name.
const CLIENT_VPN_AUTHORIZATION_RULE_PROPERTIES: &[&str] = &[
    "AccessGroupId",
    "AuthorizeAllGroups",
    "ClientVpnEndpointId",
    "Description",
    "TargetNetworkCidr",
];

/// The error code Amazon EC2 answers for a Client VPN endpoint this account does not hold,
/// which is what a delete of one judges "already gone" on — the CODE, never a message, which
/// carries the request's own id.
const INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND: &str = "InvalidClientVpnEndpointId.NotFound";

/// The code it answers for an authorization rule an endpoint does not have, which the revoke
/// on the way out judges the same way.
const INVALID_CLIENT_VPN_AUTHORIZATION_RULE_NOT_FOUND: &str =
    "InvalidClientVpnEndpointAuthorizationRuleNotFound";

/// The `ClientVpnAuthorizationRuleStatusCode` a rule that grants the access it describes
/// carries, and the one a rule that will never grant it carries: "authorizing | active |
/// failed | revoking", of which `active` is the state the rule is in force in and `failed`
/// the one nothing is going to bring it out of.
const CLIENT_VPN_AUTHORIZATION_RULE_ACTIVE: &str = "active";
const CLIENT_VPN_AUTHORIZATION_RULE_FAILED: &str = "failed";

/// The state a rule the authorize has just answered for sits in, which is what this arm
/// reports while EC2 has not yet reported one.
const CLIENT_VPN_AUTHORIZATION_RULE_AUTHORIZING: &str = "authorizing";

/// The filters `DescribeClientVpnAuthorizationRules` narrows a report to ONE rule by:
/// "destination-cidr - The CIDR of the network to which the authorization rule applies" and
/// "group-id - The ID of the Active Directory group to which the authorization rule grants
/// access" (that operation's own `Filters` documentation). One endpoint can hold several
/// rules for one network — one per group, beside the rule that grants every client access —
/// so a read of THIS resource's rule narrows by both.
const CLIENT_VPN_AUTHORIZATION_RULE_CIDR_FILTER: &str = "destination-cidr";
const CLIENT_VPN_AUTHORIZATION_RULE_GROUP_FILTER: &str = "group-id";

/// The request that reports the ONE authorization rule a declaration granted: the endpoint it
/// is on, narrowed by the network it grants and — where the declaration named one — by the
/// group it grants it to. `None` when the declaration names neither the endpoint nor the
/// network, which leaves nothing to read a rule by.
///
/// Answered beside whether the declaration granted the network to EVERY CLIENT, which is what
/// picks the rule out of the report where the filters cannot: a rule granted to all clients
/// names no group, and no filter selects the ABSENCE of one, so the entry whose `accessAll`
/// matches the declaration is this resource's ([`reported_rule_status`],
/// [`reported_rule_members`]).
fn authorization_rule_report(properties: &Value) -> Option<(Vec<(String, String)>, bool)> {
    let endpoint_id = declared_scalar(properties, "ClientVpnEndpointId");
    let destination = declared_scalar(properties, "TargetNetworkCidr");
    if endpoint_id.is_empty() || destination.is_empty() {
        return None;
    }
    let group_id = declared_scalar(properties, "AccessGroupId");
    let mut parameters = vec![
        ("ClientVpnEndpointId".to_string(), endpoint_id),
        (
            "Filter.1.Name".to_string(),
            CLIENT_VPN_AUTHORIZATION_RULE_CIDR_FILTER.to_string(),
        ),
        ("Filter.1.Value.1".to_string(), destination),
    ];
    if !group_id.is_empty() {
        parameters.push((
            "Filter.2.Name".to_string(),
            CLIENT_VPN_AUTHORIZATION_RULE_GROUP_FILTER.to_string(),
        ));
        parameters.push(("Filter.2.Value.1".to_string(), group_id.clone()));
    }
    Some((parameters, group_id.is_empty()))
}

/// The ONE reported rule this declaration's is, out of the report
/// [`authorization_rule_report`] asked for: its POSITION among the rules the answer carries
/// and its own scalar members. `None` when the report holds none for this declaration.
///
/// The entry is the one whose `accessAll` matches the declaration — the filters narrow the
/// report to the network, and to the group where the declaration named one, but no filter can
/// express the ABSENCE of a group, so the all-groups rule is told from a group's rule for the
/// same network here.
fn reported_rule(answered: &str, grants_all_groups: bool) -> Option<(usize, Map<String, Value>)> {
    xml_entry_structures(answered, "authorizationRule", "item")
        .into_iter()
        .enumerate()
        .find(|(_, entry)| {
            let grants_all = entry.get("accessAll").and_then(Value::as_str) == Some("true");
            grants_all == grants_all_groups
        })
}

/// The `ClientVpnAuthorizationRuleStatusCode` that rule carries, or `None` when the report
/// holds none for this declaration.
///
/// A rule's status is a STRUCTURE on the wire, so it is not one of the scalar members
/// [`reported_rule`] reads; it is taken at that rule's own position among the statuses the
/// report carries, which is sound because the answering service reports exactly one status per
/// rule. The rule itself is chosen by [`reported_rule`], so this read and the one below cannot
/// disagree about which rule they are of.
fn reported_rule_status(answered: &str, grants_all_groups: bool) -> Option<String> {
    let (position, _) = reported_rule(answered, grants_all_groups)?;
    xml_elements(answered, "code").into_iter().nth(position)
}

/// The scalar members of that rule, for the drift comparison.
fn reported_rule_members(answered: &str, grants_all_groups: bool) -> Option<Map<String, Value>> {
    reported_rule(answered, grants_all_groups).map(|(_, entry)| entry)
}

/// Create the Client VPN endpoint an `AWS::EC2::ClientVpnEndpoint` declares, in Amazon EC2 —
/// so a client that deployed the stack reads the endpoint back through that service's own
/// `DescribeClientVpnEndpoints` for the identity the stack published, and the authorization
/// rules and routes of the same template really hang off an endpoint EC2 holds.
///
/// The declaration IS the request: each property reaches `CreateClientVpnEndpoint` under the
/// member of the same name, in the spelling that member's own wire form takes
/// ([`CLIENT_VPN_ENDPOINT_SCALARS`], [`CLIENT_VPN_ENDPOINT_STRUCTURES`],
/// [`CLIENT_VPN_ENDPOINT_LISTS`]), with the tags CloudFormation gives the resource beside the
/// declaration's own on that call's `TagSpecifications` member.
///
/// The identity is EC2's: the `clientVpnEndpointId` it minted becomes this resource's
/// physical id — the id this type's `Ref` returns — so the stack publishes a value every
/// Client VPN call really resolves.
pub(super) async fn create_client_vpn_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, CLIENT_VPN_ENDPOINT_PROPERTIES)?;
    let properties = &resource.properties;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in CLIENT_VPN_ENDPOINT_SCALARS {
        if let Some(value) = declared_member(properties, member) {
            parameters.push(((*member).to_string(), attribute_text(value)));
        }
    }
    for member in CLIENT_VPN_ENDPOINT_STRUCTURES {
        if let Some(value) = declared_member(properties, member) {
            push_structure(&mut parameters, member, value, 0)?;
        }
    }
    for (property, member) in CLIENT_VPN_ENDPOINT_LISTS {
        if let Some(value) = declared_member(properties, property) {
            push_flattened_list(&mut parameters, member, value)?;
        }
    }
    let tags = tags_of(&declared_endpoint_tag_set(properties), propagated);
    if !tags.is_empty() {
        parameters.push((
            "TagSpecification.1.ResourceType".to_string(),
            CLIENT_VPN_ENDPOINT_RESOURCE_TYPE.to_string(),
        ));
        for (index, (key, value)) in tags.iter().enumerate() {
            let position = index + 1;
            parameters.push((format!("TagSpecification.1.Tag.{position}.Key"), key.clone()));
            parameters.push((
                format!("TagSpecification.1.Tag.{position}.Value"),
                value.clone(),
            ));
        }
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateClientVpnEndpoint",
        &parameters,
    )
    .await?;
    let endpoint_id = xml_element(&answered, "clientVpnEndpointId")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "CreateClientVpnEndpoint answered no clientVpnEndpointId".to_string(),
        })?;
    Ok(Created {
        physical_resource_id: Some(endpoint_id),
        attributes: BTreeMap::new(),
    })
}

/// Delete the Client VPN endpoint a stack created, addressed by the `cvpn-endpoint-…` id
/// Amazon EC2 minted for it — this resource's physical id.
///
/// An endpoint EC2 no longer holds ([`INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND`], judged on
/// the CODE) is already in the state the delete asks for. An endpoint that still has a target
/// network associated is that service's own `DependencyViolation`, which the stack reports as
/// the failure it is rather than swallowing.
pub(super) async fn delete_client_vpn_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DeleteClientVpnEndpoint",
        &[(
            "ClientVpnEndpointId".to_string(),
            physical_resource_id.to_string(),
        )],
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Grant the ingress an `AWS::EC2::ClientVpnAuthorizationRule` declares, through
/// `AuthorizeClientVpnIngress` — the one call that adds a rule to a Client VPN endpoint.
///
/// The declaration IS the request: all five properties are members of that call under the
/// same names ([`CLIENT_VPN_AUTHORIZATION_RULE_PROPERTIES`]), the endpoint among them being
/// the id EC2 minted for the endpoint this stack created.
///
/// The rule keeps the identity the naming rules already gave it — the endpoint and the
/// network joined ([`EC2_CLIENT_VPN_AUTHORIZATION_RULE_IDENTITY`]), because EC2 mints no
/// identifier for one — so nothing is answered as a physical id and the type publishes no
/// attribute to report.
pub(super) async fn authorize_client_vpn_ingress(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, CLIENT_VPN_AUTHORIZATION_RULE_PROPERTIES)?;
    let properties = &resource.properties;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in CLIENT_VPN_AUTHORIZATION_RULE_PROPERTIES {
        if let Some(value) = declared_member(properties, member) {
            parameters.push(((*member).to_string(), attribute_text(value)));
        }
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "AuthorizeClientVpnIngress",
        &parameters,
    )
    .await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Take the rule a stack granted back off its endpoint, through `RevokeClientVpnIngress`.
///
/// That call names the rule the only way EC2 lets one be named: the endpoint, the network,
/// and either the group whose rule it is or `RevokeAllGroups` for the rule that grants every
/// client access — so which of the two a declaration asked for decides which member this
/// carries. A rule that endpoint no longer has, and an endpoint EC2 no longer holds at all,
/// are each already in the state this delete asks for (judged on their own CODES).
pub(super) async fn revoke_client_vpn_ingress(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let properties = &resource.properties;
    let endpoint_id = required_property(resource, "ClientVpnEndpointId")?;
    let destination = required_property(resource, "TargetNetworkCidr")?;
    let mut parameters = vec![
        ("ClientVpnEndpointId".to_string(), endpoint_id),
        ("TargetNetworkCidr".to_string(), destination),
    ];
    match declared_member(properties, "AccessGroupId").map(attribute_text) {
        Some(group_id) if !group_id.is_empty() => {
            parameters.push(("AccessGroupId".to_string(), group_id));
        }
        _ => parameters.push(("RevokeAllGroups".to_string(), "true".to_string())),
    }
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "RevokeClientVpnIngress",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal)
            if refusal.code == INVALID_CLIENT_VPN_AUTHORIZATION_RULE_NOT_FOUND
                || refusal.code == INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND =>
        {
            Ok(())
        }
        Err(refusal) => Err(refusal),
    }
}

/// Whether the authorization rule the stack granted is in force yet, read off Amazon EC2's
/// own `DescribeClientVpnAuthorizationRules`.
///
/// `AuthorizeClientVpnIngress` answers the rule `authorizing`, in which it grants nothing
/// yet, and EC2 reports it `active` once it does — so the resource's creation is finished
/// when that service reports it there, and FAILED when that service reports it `failed`,
/// which nothing is going to bring it out of.
///
/// The rule is read by the endpoint it is on, the network it grants and whoever it grants
/// that network to ([`authorization_rule_report`]), which is the only handle EC2 publishes for
/// one; a declaration the caller does not hold, or one naming no endpoint or network, leaves
/// nothing to read the rule by and establishes nothing, which is reported as the resource
/// being ready rather than as a wait nothing can end.
pub(super) async fn client_vpn_authorization_rule_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some((parameters, grants_all_groups)) = declared.and_then(authorization_rule_report)
    else {
        return Ok(Readiness::Ready);
    };
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeClientVpnAuthorizationRules",
        &parameters,
    )
    .await?;
    let Some(code) =
        reported_rule_status(&answered, grants_all_groups).filter(|code| !code.is_empty())
    else {
        // That service reports no rule for this grant yet, which is the state the authorize
        // left it in rather than an answer about a rule that will never come up.
        return Ok(Readiness::Pending(
            CLIENT_VPN_AUTHORIZATION_RULE_AUTHORIZING.to_string(),
        ));
    };
    match code.as_str() {
        CLIENT_VPN_AUTHORIZATION_RULE_ACTIVE => Ok(Readiness::Ready),
        CLIENT_VPN_AUTHORIZATION_RULE_FAILED => Ok(Readiness::Failed(format!(
            "Amazon EC2 reports the Client VPN authorization rule {code}"
        ))),
        pending => Ok(Readiness::Pending(pending.to_string())),
    }
}

/// The Client VPN endpoint's configuration as Amazon EC2 reports it, for drift detection:
/// `DescribeClientVpnEndpoints`' own record of the endpoint the stack's physical id names,
/// under that type's property names where the two sides spell a member differently. An
/// endpoint that service no longer holds ([`INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND`],
/// judged on the CODE) was deleted.
///
/// Only the SCALAR members are read back, which is the position every arm whose properties
/// are not all scalars is in: the endpoint's authentication options, its logging options and
/// its lists are structures and lists on the wire, so they are left out of the comparison
/// rather than compared against a flattened reading of themselves.
pub(super) async fn describe_client_vpn_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeClientVpnEndpoints",
        &[(
            "ClientVpnEndpointId.1".to_string(),
            physical_resource_id.to_string(),
        )],
    )
    .await
    {
        Ok(answered) => {
            let reported = Value::Object(ec2_reported_entry(&answered, "clientVpnEndpoint"));
            Ok(Some(reported_as_properties(
                &reported,
                CLIENT_VPN_ENDPOINT_REPORTED_PROPERTIES,
            )))
        }
        Err(refusal) if refusal.code == INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The scalar properties `DescribeClientVpnEndpoints` reports back under the names the type
/// declares, as `(property, member)`: every member the answer spells with an initial
/// lower-case letter is reported under its own name once [`ec2_reported_entry`] has
/// capitalised it, so the two sides differ only where the type renames a member.
const CLIENT_VPN_ENDPOINT_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("ClientCidrBlock", "ClientCidrBlock"),
    ("Description", "Description"),
    ("DisconnectOnSessionTimeout", "DisconnectOnSessionTimeout"),
    ("ServerCertificateArn", "ServerCertificateArn"),
    ("SessionTimeoutHours", "SessionTimeoutHours"),
    ("SplitTunnel", "SplitTunnel"),
    ("TransportProtocol", "TransportProtocol"),
    ("VpcId", "VpcId"),
    ("VpnPort", "VpnPort"),
];

/// The authorization rule's configuration as Amazon EC2 reports it, for drift detection: the
/// rule it reports for the endpoint, the network and the grant the declaration names
/// ([`authorization_rule_report`]), under that type's own property names
/// ([`CLIENT_VPN_AUTHORIZATION_RULE_REPORTED_PROPERTIES`]).
///
/// An endpoint that service no longer holds, and an endpoint that holds no such rule, both
/// report the rule GONE: a rule exists only as a grant on an endpoint, so either answer says
/// the grant is not there.
pub(super) async fn describe_client_vpn_authorization_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    // The two members the read is addressed by, refused as absent here rather than read as an
    // empty filter — a report of every rule of every endpoint says nothing about this one.
    required_property(resource, "ClientVpnEndpointId")?;
    required_property(resource, "TargetNetworkCidr")?;
    let Some((parameters, grants_all_groups)) = authorization_rule_report(&resource.properties)
    else {
        return Ok(None);
    };
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeClientVpnAuthorizationRules",
        &parameters,
    )
    .await
    {
        Ok(answered) => Ok(reported_rule_members(&answered, grants_all_groups).map(|reported| {
            reported_as_properties(
                &Value::Object(reported),
                CLIENT_VPN_AUTHORIZATION_RULE_REPORTED_PROPERTIES,
            )
        })),
        Err(refusal) if refusal.code == INVALID_CLIENT_VPN_ENDPOINT_ID_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The scalar members `DescribeClientVpnAuthorizationRules` reports back under the names the
/// type declares, as `(property, member)`: a rule's reported `destinationCidr` is the type's
/// `TargetNetworkCidr`, its `groupId` the type's `AccessGroupId` and its `accessAll` the
/// type's `AuthorizeAllGroups`, those being the same facts on either side of the call. The
/// members are spelled as the answer spells them, which is the lower-camel form
/// [`reported_rule_members`] reads them in.
const CLIENT_VPN_AUTHORIZATION_RULE_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("AccessGroupId", "groupId"),
    ("AuthorizeAllGroups", "accessAll"),
    ("ClientVpnEndpointId", "clientVpnEndpointId"),
    ("Description", "description"),
    ("TargetNetworkCidr", "destinationCidr"),
];

/// The declared property `property`, or `None` where the declaration states it as nothing.
///
/// A property stated as JSON `null` is one the declaration does not state, so nothing is sent
/// for it: the owning service's own default is what a member nobody set carries.
fn declared_member<'a>(properties: &'a Value, property: &str) -> Option<&'a Value> {
    properties.get(property).filter(|value| !value.is_null())
}

/// Write one LIST member into an Amazon EC2 query form body: each entry indexed from 1
/// directly under the member's own name (`DnsServers.1`), with a STRUCTURED entry's own
/// members following the index (`Authentication.1.MutualAuthentication.…`).
///
/// That is EC2's flattened list form, which is what tells it apart from the awsQuery lists
/// [`push_list`] writes under an element name of their own. A value the template did not
/// write as a list is the one-entry list of itself, so a single value a template spells
/// without the sequence syntax is not dropped; each entry is walked to its leaves by
/// [`push_structure`], which is also what bounds how deeply nested a declaration may be.
fn push_flattened_list(
    parameters: &mut Vec<(String, String)>,
    member: &str,
    declared: &Value,
) -> Result<(), Refusal> {
    let entries: Vec<&Value> = match declared {
        Value::Array(values) => values.iter().collect(),
        single => vec![single],
    };
    for (index, value) in entries.iter().enumerate() {
        let position = index + 1;
        push_structure(parameters, &format!("{member}.{position}"), value, 1)?;
    }
    Ok(())
}

/// The tags an `AWS::EC2::ClientVpnEndpoint` declaration asks for, as the `Tags` property
/// every other arm's tag reader takes.
///
/// This type states its tags in `TagSpecifications` — a list of `{ResourceType, Tags}`, the
/// shape `CreateClientVpnEndpoint`'s own member takes — rather than in a `Tags` property, so
/// the tags of the specifications that name the ENDPOINT's own resource type are collected
/// here and handed to [`tags_of`] under the name that reader looks them up by. A
/// specification naming no resource type at all names this one: the endpoint is the only type
/// that call's `TagSpecification` may name.
fn declared_endpoint_tag_set(properties: &Value) -> Value {
    let mut tags: Vec<Value> = Vec::new();
    for specification in properties
        .get("TagSpecifications")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let names_the_endpoint = specification
            .get("ResourceType")
            .and_then(Value::as_str)
            .is_none_or(|resource_type| resource_type == CLIENT_VPN_ENDPOINT_RESOURCE_TYPE);
        if !names_the_endpoint {
            continue;
        }
        for tag in specification
            .get("Tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            tags.push(tag.clone());
        }
    }
    Value::Object(
        [("Tags".to_string(), Value::Array(tags))]
            .into_iter()
            .collect(),
    )
}
