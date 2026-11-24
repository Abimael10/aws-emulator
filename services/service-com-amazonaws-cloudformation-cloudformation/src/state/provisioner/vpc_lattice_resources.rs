// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon VPC Lattice's RESOURCE-ACCESS estate: the
//! resource gateway, the resource configuration and the two associations a template declares,
//! beside the service-network and access-log-subscription arms of [`super::vpc_lattice`]. One
//! file of [`super`]'s per-type arms, dispatched to by its `Provisioner::VpcLatticeResource`
//! variant, which carries which of the four the declaration is ([`Resource`]).
//!
//! Each of the four is created by that service's own `Create…` call and removed by its own
//! `Delete…`, over the one in-process transport every arm here rides on
//! ([`call_rest_json_named`]), so a resource a stack creates is the same resource a client's
//! own call would have created and VPC Lattice's own reads report it.
//!
//! Every one of those creates MINTS the resource's identity: it answers the `rgw-`/`rcfg-`/
//! `snra-`/`snva-` id it drew together with the ARN built around it, and that ARN is the
//! resource's physical id — the value each type's `Ref` returns and the handle every later VPC
//! Lattice call takes — which is why
//! [`super::super::template::identity_is_service_assigned`] names all four types and why the
//! values this emulator's naming rules computed for one are only stand-ins until the create
//! has answered.
//!
//! Most declared properties ARE the create's own members with a lower-cased initial. Two are
//! not, and those are mapped member by member:
//!
//! * the identifier members: a type declares the handle of the resource it hangs off as
//!   `ResourceGatewayId`, `ResourceConfigurationId`, `ServiceNetworkId` or
//!   `ServiceNetworkIdentifier`, while the API takes each as an `…Identifier`, and the value
//!   a declaration states for one is a `Ref`/`Fn::GetAtt` of the declared resource — so what
//!   reaches VPC Lattice is the identifier that service minted for it;
//! * a resource configuration's DEFINITION, which the type declares as a union of flat
//!   values (`IpResource: <ip address>`, `ArnResource: <arn>`, `DnsResource: {DomainName,
//!   IpAddressType}`) while `ResourceConfigurationDefinition`'s own members are structures
//!   (`ipResource: {ipAddress}`, `arnResource: {arn}`, `dnsResource: {domainName,
//!   ipAddressType}`) — so [`configuration_definition`] lifts each flat value into the
//!   member that carries it.

use super::*;

/// Which of the four Amazon VPC Lattice resource-access resources a declaration is, carried
/// by the one `Provisioner` variant these arms are reached through so the four share one set
/// of arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resource {
    /// `AWS::VpcLattice::ResourceGateway`.
    Gateway,
    /// `AWS::VpcLattice::ResourceConfiguration`.
    Configuration,
    /// `AWS::VpcLattice::ServiceNetworkResourceAssociation`.
    ConfigurationAssociation,
    /// `AWS::VpcLattice::ServiceNetworkVpcAssociation`.
    VpcAssociation,
}

/// The resource-type names of the four, which the registry of recognised types, the
/// return-value catalogue and the primary-identifier table each key an entry by
/// ([`super::super::template`] re-exports them), re-exported here so an arm names the type
/// it serves by the same const.
pub(in crate::state) use super::super::template::{
    VPCLATTICE_RESOURCE_CONFIGURATION, VPCLATTICE_RESOURCE_GATEWAY,
    VPCLATTICE_SERVICE_NETWORK_RESOURCE_ASSOCIATION, VPCLATTICE_SERVICE_NETWORK_VPC_ASSOCIATION,
};

/// The collection each resource is created under and addressed by one id beneath, from each
/// operation's own `@http` binding (`POST /resourcegateways`,
/// `DELETE /resourcegateways/{resourceGatewayIdentifier}`, and so on).
const RESOURCE_GATEWAYS_URI: &str = "/resourcegateways";
const RESOURCE_CONFIGURATIONS_URI: &str = "/resourceconfigurations";
const RESOURCE_ASSOCIATIONS_URI: &str = "/servicenetworkresourceassociations";
const VPC_ASSOCIATIONS_URI: &str = "/servicenetworkvpcassociations";

/// The operations these arms call, as Amazon VPC Lattice's own model names them.
const CREATE_RESOURCE_GATEWAY: &str = "CreateResourceGateway";
const DELETE_RESOURCE_GATEWAY: &str = "DeleteResourceGateway";
const CREATE_RESOURCE_CONFIGURATION: &str = "CreateResourceConfiguration";
const DELETE_RESOURCE_CONFIGURATION: &str = "DeleteResourceConfiguration";
const CREATE_RESOURCE_ASSOCIATION: &str = "CreateServiceNetworkResourceAssociation";
const DELETE_RESOURCE_ASSOCIATION: &str = "DeleteServiceNetworkResourceAssociation";
const CREATE_VPC_ASSOCIATION: &str = "CreateServiceNetworkVpcAssociation";
const DELETE_VPC_ASSOCIATION: &str = "DeleteServiceNetworkVpcAssociation";

/// The error code Amazon VPC Lattice answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of every one of its deletes).
const VPC_LATTICE_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The members each create takes the declared properties as, where the property is the
/// member of the same name with a lower-cased initial.
const GATEWAY_MEMBERS: &[(&str, &str)] = &[
    ("VpcIdentifier", "vpcIdentifier"),
    ("SubnetIds", "subnetIds"),
    ("SecurityGroupIds", "securityGroupIds"),
    ("IpAddressType", "ipAddressType"),
];
const CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("PortRanges", "portRanges"),
    ("Protocol", "protocol"),
    ("ResourceConfigurationType", "type"),
];
const VPC_ASSOCIATION_MEMBERS: &[(&str, &str)] = &[("SecurityGroupIds", "securityGroupIds")];

/// The property each type states the handle of the resource it hangs off in, and the
/// `…Identifier` member the API takes it as.
const GATEWAY_IDENTIFIER: (&str, &str) = ("ResourceGatewayId", "resourceGatewayIdentifier");
const CONFIGURATION_GROUP_IDENTIFIER: (&str, &str) = (
    "ResourceConfigurationGroupId",
    "resourceConfigurationGroupIdentifier",
);
const CONFIGURATION_IDENTIFIER: (&str, &str) = (
    "ResourceConfigurationId",
    "resourceConfigurationIdentifier",
);

/// The `tags` MAP every one of those creates takes, and the `name` member the three named
/// resources take.
const TAGS_MEMBER: &str = "tags";
const NAME_MEMBER: &str = "name";
const NAME_PROPERTY: &str = "Name";

/// The members a created resource's identity is answered in, which every one of those
/// creates reports.
const ID_MEMBER: &str = "id";
const ARN_MEMBER: &str = "arn";

/// How a name is derived for a resource whose declaration states none, and how long that
/// name is.
///
/// Both named types of this family — the gateway and the configuration — declare `Name` as a
/// required property, so this is the fallback for a declaration that states none anyway: the
/// name is derived from the resource's own computed identity (a function of the stack and the
/// resource's logical id), which keeps the same declaration of the same stack asking for the
/// same resource. The form satisfies both `@pattern`s
/// (`^(?!rgw-)(?![-])(?!.*[-]$)(?!.*[-]{2})[a-z0-9-]+$`): a lower-case prefix, one hyphen and
/// lower-case hexadecimal digits.
const DERIVED_NAME_PREFIX: &str = "cfn-";
const DERIVED_NAME_DIGITS: usize = 16;

/// Create the Amazon VPC Lattice resource a declaration of one of the four types asks for,
/// so a client that read the ARN out of the stack's outputs finds it in that service's own
/// reads.
///
/// The identity is that service's: the id it minted and the ARN built around it are reported
/// back as this resource's `Id` and `Arn` attributes, and the ARN becomes the resource's
/// physical id — the handle every later call on the resource takes.
pub(super) async fn create_vpc_lattice_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    match resource_kind {
        Resource::Gateway => {
            request.insert(
                NAME_MEMBER.to_string(),
                Value::String(configured_name(resource, &properties)),
            );
            copy_members(&properties, GATEWAY_MEMBERS, &mut request);
        }
        Resource::Configuration => {
            request.insert(
                NAME_MEMBER.to_string(),
                Value::String(configured_name(resource, &properties)),
            );
            copy_members(&properties, CONFIGURATION_MEMBERS, &mut request);
            copy_identifier(&properties, GATEWAY_IDENTIFIER, &mut request);
            copy_identifier(&properties, CONFIGURATION_GROUP_IDENTIFIER, &mut request);
            if let Some(definition) = properties.get("ResourceConfigurationDefinition") {
                request.insert(
                    "resourceConfigurationDefinition".to_string(),
                    configuration_definition(definition),
                );
            }
            // The type spells the sharing setting with `Sharable` and the API's member with
            // `Shareable`, so both spellings of the property reach that one member.
            for property in [
                "AllowAssociationToSharableServiceNetwork",
                "AllowAssociationToShareableServiceNetwork",
            ] {
                if let Some(allowed) = properties.get(property) {
                    request.insert(
                        "allowAssociationToShareableServiceNetwork".to_string(),
                        allowed.clone(),
                    );
                }
            }
        }
        Resource::ConfigurationAssociation => {
            copy_identifier(&properties, CONFIGURATION_IDENTIFIER, &mut request);
            copy_service_network_identifier(&properties, &mut request);
        }
        Resource::VpcAssociation => {
            copy_service_network_identifier(&properties, &mut request);
            copy_members(&properties, VPC_ASSOCIATION_MEMBERS, &mut request);
            if let Some(vpc) = properties.get("VpcIdentifier") {
                request.insert("vpcIdentifier".to_string(), vpc.clone());
            }
        }
    }
    // Every one of those creates takes a `tags` MAP of its own, so a resource a stack
    // creates carries the tags CloudFormation gives it beside the ones its declaration asks
    // for.
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }

    let answered = call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        create_operation(resource_kind),
        "POST",
        collection_uri(resource_kind),
        Some(&Value::Object(request)),
    )
    .await?;

    let arn = reported_text(&answered, ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call Amazon VPC Lattice with, and nothing
        // for the stack to publish, so this is a failed create rather than a created
        // resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Amazon VPC Lattice control plane answered a created resource \
                      without an ARN"
                .to_string(),
        });
    }
    let mut attributes = BTreeMap::new();
    attributes.insert(
        super::super::template::VPCLATTICE_ARN_ATTRIBUTE.to_string(),
        arn.clone(),
    );
    if let Some(id) = reported_text(&answered, ID_MEMBER) {
        attributes.insert(
            super::super::template::VPCLATTICE_ID_ATTRIBUTE.to_string(),
            id,
        );
    }
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Delete the Amazon VPC Lattice resource a stack created, by the ARN it was created under —
/// this resource's physical id, which every one of those deletes takes as its identifier.
///
/// One the service no longer holds (`ResourceNotFoundException`, judged on the CODE and never
/// on a message that carries the request's own values) is already in the state the delete
/// asks for.
pub(super) async fn delete_vpc_lattice_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let uri = format!(
        "{}/{}",
        collection_uri(resource_kind),
        percent_encoded(physical_resource_id)
    );
    match call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        delete_operation(resource_kind),
        "DELETE",
        &uri,
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == VPC_LATTICE_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The collection one of the four lives under, which is also the path one resource of that
/// kind is addressed beneath.
fn collection_uri(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Gateway => RESOURCE_GATEWAYS_URI,
        Resource::Configuration => RESOURCE_CONFIGURATIONS_URI,
        Resource::ConfigurationAssociation => RESOURCE_ASSOCIATIONS_URI,
        Resource::VpcAssociation => VPC_ASSOCIATIONS_URI,
    }
}

/// The operation that creates one of the four, as that service's own model names it.
fn create_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Gateway => CREATE_RESOURCE_GATEWAY,
        Resource::Configuration => CREATE_RESOURCE_CONFIGURATION,
        Resource::ConfigurationAssociation => CREATE_RESOURCE_ASSOCIATION,
        Resource::VpcAssociation => CREATE_VPC_ASSOCIATION,
    }
}

/// The operation that deletes one of the four.
fn delete_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Gateway => DELETE_RESOURCE_GATEWAY,
        Resource::Configuration => DELETE_RESOURCE_CONFIGURATION,
        Resource::ConfigurationAssociation => DELETE_RESOURCE_ASSOCIATION,
        Resource::VpcAssociation => DELETE_VPC_ASSOCIATION,
    }
}

/// The operation that reads one of the four, as that service's own model names it, for the
/// drift read below.
fn read_operation(resource_kind: Resource) -> &'static str {
    match resource_kind {
        Resource::Gateway => "GetResourceGateway",
        Resource::Configuration => "GetResourceConfiguration",
        Resource::ConfigurationAssociation => "GetServiceNetworkResourceAssociation",
        Resource::VpcAssociation => "GetServiceNetworkVpcAssociation",
    }
}

/// The members one of those reads reports under the names the declaring type spells them
/// with, for drift detection. A property the read does not report is not evidence of
/// anything ([`super::drift`]).
fn reported_members(resource_kind: Resource) -> &'static [(&'static str, &'static str)] {
    match resource_kind {
        Resource::Gateway => &[
            ("Name", "name"),
            ("VpcIdentifier", "vpcId"),
            ("SubnetIds", "subnetIds"),
            ("SecurityGroupIds", "securityGroupIds"),
            ("IpAddressType", "ipAddressType"),
        ],
        Resource::Configuration => &[
            ("Name", "name"),
            ("ResourceConfigurationType", "type"),
            ("PortRanges", "portRanges"),
            ("Protocol", "protocol"),
            ("ResourceGatewayId", "resourceGatewayId"),
        ],
        Resource::ConfigurationAssociation => &[
            ("ResourceConfigurationId", "resourceConfigurationId"),
            ("ServiceNetworkId", "serviceNetworkId"),
        ],
        Resource::VpcAssociation => &[
            ("ServiceNetworkIdentifier", "serviceNetworkId"),
            ("VpcIdentifier", "vpcId"),
            ("SecurityGroupIds", "securityGroupIds"),
        ],
    }
}

/// The resource's configuration as Amazon VPC Lattice reports it, for drift detection: that
/// service's own read of the ARN that is this resource's physical id. One it no longer holds
/// (`ResourceNotFoundException`, judged on the CODE) was deleted, which is what `None`
/// reports.
pub(super) async fn describe_vpc_lattice_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let uri = format!(
        "{}/{}",
        collection_uri(resource_kind),
        percent_encoded(physical_resource_id)
    );
    let answered = match call_rest_json_named(
        ctx,
        VPC_LATTICE_SIGNING_NAME,
        region,
        read_operation(resource_kind),
        "GET",
        &uri,
        None,
    )
    .await
    {
        Ok(document) => document,
        Err(refusal) if refusal.code == VPC_LATTICE_RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(Some(reported_as_properties(
        &answered,
        reported_members(resource_kind),
    )))
}

/// Which of the four a resource type is, or `None` for a type that is none of them — which
/// is what leaves every other type to the answer [`super::provisioner_for`] already gave it.
pub(super) fn vpc_lattice_provisioner_for(resource_type: &str) -> Option<Resource> {
    match resource_type {
        VPCLATTICE_RESOURCE_GATEWAY => Some(Resource::Gateway),
        VPCLATTICE_RESOURCE_CONFIGURATION => Some(Resource::Configuration),
        VPCLATTICE_SERVICE_NETWORK_RESOURCE_ASSOCIATION => {
            Some(Resource::ConfigurationAssociation)
        }
        VPCLATTICE_SERVICE_NETWORK_VPC_ASSOCIATION => Some(Resource::VpcAssociation),
        _ => None,
    }
}

/// The name the create is made under: the one the declaration states, and otherwise one
/// derived from the resource's own computed identity ([`DERIVED_NAME_PREFIX`]).
fn configured_name(resource: &StoredResource, properties: &Map<String, Value>) -> String {
    properties
        .get(NAME_PROPERTY)
        .and_then(Value::as_str)
        .filter(|declared| !declared.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{DERIVED_NAME_PREFIX}{}",
                crate::state::stable_identifier_digits(
                    &resource.physical_resource_id,
                    DERIVED_NAME_DIGITS
                )
            )
        })
}

/// Copy the handle of the resource a declaration hangs off onto the `…Identifier` member the
/// API takes it as; a declaration that states none sends none.
fn copy_identifier(
    properties: &Map<String, Value>,
    member: (&str, &str),
    request: &mut Map<String, Value>,
) {
    if let Some(identifier) = properties.get(member.0) {
        request.insert(member.1.to_string(), identifier.clone());
    }
}

/// The service network an association names, which the two association types spell
/// differently — a resource association's property is `ServiceNetworkId` and a VPC
/// association's `ServiceNetworkIdentifier` — and which both creates take as
/// `serviceNetworkIdentifier`.
fn copy_service_network_identifier(
    properties: &Map<String, Value>,
    request: &mut Map<String, Value>,
) {
    for property in ["ServiceNetworkIdentifier", "ServiceNetworkId"] {
        if let Some(identifier) = properties.get(property) {
            request.insert("serviceNetworkIdentifier".to_string(), identifier.clone());
            return;
        }
    }
}

/// One declared `ResourceConfigurationDefinition` as the union member the create takes.
///
/// The type declares the union's three alternatives as FLAT values — `IpResource` and
/// `ArnResource` are the address and the ARN themselves, and `DnsResource` is the pair of
/// that member's own fields — while `ResourceConfigurationDefinition`'s members are
/// structures (`ipResource: {ipAddress}`, `arnResource: {arn}`, `dnsResource: {domainName,
/// ipAddressType}`), so each declared alternative is lifted into the member that carries it.
/// A declaration already written as that structure passes through, and one that states none
/// of the three is left as it came for that operation's own validation to refuse rather than
/// being silently dropped.
fn configuration_definition(declared: &Value) -> Value {
    let Some(members) = declared.as_object() else {
        return declared.clone();
    };
    let mut definition = Map::new();
    if let Some(address) = members.get("IpResource") {
        definition.insert("ipResource".to_string(), lifted(address, "ipAddress"));
    }
    if let Some(arn) = members.get("ArnResource") {
        definition.insert("arnResource".to_string(), lifted(arn, "arn"));
    }
    if let Some(dns) = members.get("DnsResource") {
        definition.insert(
            "dnsResource".to_string(),
            match dns.as_object() {
                Some(fields) => Value::Object(
                    fields
                        .iter()
                        .map(|(field, value)| (lower_initial(field), value.clone()))
                        .collect(),
                ),
                None => lifted(dns, "domainName"),
            },
        );
    }
    if let Some(cidr) = members.get("CidrResource") {
        definition.insert("cidrResource".to_string(), lifted(cidr, "cidrRanges"));
    }
    if definition.is_empty() {
        return declared.clone();
    }
    Value::Object(definition)
}

/// One flat declared value as the structure the API's union member takes: the value under
/// the one field that member carries, or the value as it came when the declaration already
/// writes a structure.
///
/// A declared field's own name is re-cased by the shared helper every arm of this catalogue
/// re-cases a member with ([`lower_initial`]).
fn lifted(declared: &Value, field: &str) -> Value {
    if declared.is_object() {
        return declared.clone();
    }
    let mut member = Map::new();
    member.insert(field.to_string(), declared.clone());
    Value::Object(member)
}
