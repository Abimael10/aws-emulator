// SPDX-License-Identifier: CC0-1.0

//! The RESOURCE-ACCESS slice of this service: a resource gateway, the resource
//! configurations reached through one, and the association that puts a configuration into a
//! service network.
//!
//! A resource gateway "is a point of ingress into the VPC where a resource resides"
//! (`CreateResourceGateway`'s own documentation) and a resource configuration "defines a
//! specific resource" reached through that gateway, which a
//! `ServiceNetworkResourceAssociation` then shares into a service network. All three are
//! RESOURCES of their own — each has an id, an ARN and a status — so each lives in the one
//! store beside the service networks rather than as a list hanging off another record, and
//! every operation that reports one reads it from here.
//!
//! What a PORT RANGE may say is here too ([`check_port_ranges`]): the modelled `@pattern`
//! admits text a port range cannot denote, so the rule that an entry names a non-empty range
//! of real ports is checked where the configuration is created and reported in the service's
//! own words.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{ResourceConfigurationDefinition, ResourceNotFoundException};

use super::{ComAmazonawsVpclatticeMercurycontrolplaneState, ResourceKind, ServiceRuleViolation};

/// The ARN resource kind of a resource gateway: `.../resourcegateway/rgw-…`
/// (`ResourceGatewayArn`'s modelled pattern).
pub const KIND_RESOURCE_GATEWAY: &str = "resourcegateway";

/// The ARN resource kind of a service-network/resource association:
/// `.../servicenetworkresourceassociation/snra-…`
/// (`ServiceNetworkResourceAssociationArn`'s modelled pattern).
pub const KIND_RESOURCE_ASSOCIATION: &str = "servicenetworkresourceassociation";

/// The id prefix of a resource gateway (`^rgw-[0-9a-z]{17}$`).
pub const PREFIX_RESOURCE_GATEWAY: &str = "rgw-";

/// The id prefix of a service-network/resource association (`^snra-[0-9a-f]{17}$`).
pub const PREFIX_RESOURCE_ASSOCIATION: &str = "snra-";

/// The `@pattern` the model declares on `ResourceGatewayName`, which
/// `CreateResourceGateway.name` targets.
/// SOURCE: `smithy_query(query="shape",shape="ResourceGatewayName")`.
pub const RESOURCE_GATEWAY_NAME_PATTERN: &str =
    r"^(?!rgw-)(?![-])(?!.*[-]$)(?!.*[-]{2})[a-z0-9-]+$";

/// How many resource gateways this emulator will hold for the account.
///
/// The real service caps them with a service quota and `CreateResourceGateway` models
/// `ServiceQuotaExceededException` (HTTP 402) for exceeding it. The number is NOT a fidelity
/// claim about the published quota: it is this emulator's own ceiling, set generously so no
/// realistic workflow meets it, so that a caller looping on create cannot grow this
/// process's memory without bound. Same reasoning as the ceilings in [`super`].
pub const MAX_RESOURCE_GATEWAYS: usize = 1_000;

/// How many resource configurations this emulator will hold for the account. Same reasoning,
/// and `CreateResourceConfiguration` models the same `ServiceQuotaExceededException`.
pub const MAX_RESOURCE_CONFIGURATIONS: usize = 1_000;

/// How many service-network/resource associations this emulator will hold. Same reasoning,
/// and `CreateServiceNetworkResourceAssociation` models the same
/// `ServiceQuotaExceededException`.
pub const MAX_RESOURCE_ASSOCIATIONS: usize = 1_000;

/// The `protocol` a resource configuration gets when its create omits the member.
///
/// From that member's own documentation: "(SINGLE, GROUP, CIDR) The protocol accepted by the
/// resource configuration. The default is TCP."
pub const DEFAULT_RESOURCE_CONFIGURATION_PROTOCOL: &str = "TCP";

/// `allowAssociationToShareableServiceNetwork` a resource configuration gets when its create
/// omits the member: "The default is false" (that member's own documentation).
pub const DEFAULT_ALLOW_SHAREABLE_ASSOCIATION: bool = false;

/// The resource-configuration types the `protocol` and
/// `allowAssociationToShareableServiceNetwork` members apply to, as each member's own
/// documentation scopes them — `(SINGLE, GROUP, CIDR)` for the protocol and
/// `(SINGLE, GROUP, ARN)` for the association setting.
pub const PROTOCOL_TYPES: &[&str] = &["SINGLE", "GROUP", "CIDR"];
/// See [`PROTOCOL_TYPES`].
pub const SHAREABLE_ASSOCIATION_TYPES: &[&str] = &["SINGLE", "GROUP", "ARN"];

/// The ports a port range may name: a TCP or UDP port number runs from 1 to 65535 — port 0
/// is the "any port" placeholder and names no port a consumer can reach a resource on —
/// while the modelled `PortRange` pattern itself admits `0` and up to five digits
/// (`^((\d{1,5}\-\d{1,5})|(\d+))$`), which reaches past both ends.
pub const MIN_PORT: u32 = 1;
/// See [`MIN_PORT`].
pub const MAX_PORT: u32 = 65_535;

/// The member a port-range refusal names.
pub const PORT_RANGES_MEMBER: &str = "portRanges";

/// The transient status a resource of this slice reports until the clock owes it `ACTIVE`,
/// and the status every one of the three statuses models for a create and a delete.
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// See [`STATUS_ACTIVE`].
pub const STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
/// See [`STATUS_ACTIVE`].
pub const STATUS_DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";

/// How AWS words the refusal of a `portRanges` entry that denotes no range of real ports:
/// the entry the request sent, then the fixed sentence.
///
/// SOURCE: recorded AWS behaviour — a `portRanges` entry whose first port is greater than
/// its last is refused HTTP 400 `ValidationException` with `<entry> is not a valid port
/// range`. The entry is the request's; the sentence is the requirement.
pub fn invalid_port_range_message(entry: &str) -> String {
    format!("{entry} is not a valid port range")
}

/// Refuse a `portRanges` list carrying an entry that denotes no range of real ports.
///
/// The RULE: an entry names either one port or a `<first>-<last>` span, and the ports it
/// names have to be ports — each between [`MIN_PORT`] and [`MAX_PORT`] — with the span's
/// first port no greater than its last, since a span from a higher port to a lower one holds
/// nothing. The modelled `@pattern` admits `100-50`, `0` and `99999` alike, so the rule lives
/// here; it is reported as a SERVICE rule over the member that carries the list
/// ([`super::service_rule_exception`]), the way this service reports every rule the model
/// carries no trait for. The FIRST entry at fault is the one reported, so the refusal names
/// a value the request really sent.
pub fn check_port_ranges(port_ranges: Option<&Vec<String>>) -> Result<(), ServiceRuleViolation> {
    let Some(entries) = port_ranges else {
        return Ok(());
    };
    for entry in entries {
        if !port_range_is_valid(entry) {
            return Err(ServiceRuleViolation {
                member: PORT_RANGES_MEMBER.to_string(),
                message: invalid_port_range_message(entry),
            });
        }
    }
    Ok(())
}

/// Whether one `portRanges` entry denotes a non-empty range of real ports; see
/// [`check_port_ranges`].
fn port_range_is_valid(entry: &str) -> bool {
    let (first, last) = match entry.split_once('-') {
        Some((first, last)) => (first, last),
        None => (entry, entry),
    };
    let (Ok(first), Ok(last)) = (first.parse::<u32>(), last.parse::<u32>()) else {
        // Not digits at all: the modelled `@pattern` already refuses that before a handler
        // runs, so nothing here can be the first report of it.
        return false;
    };
    first <= last && first >= MIN_PORT && last <= MAX_PORT
}

/// AWS's `ResourceNotFoundException` for an identifier that names no persisted resource of
/// one of this slice's kinds.
///
/// Each of the three kinds is worded its own way, which is what the recordings fix:
/// a resource gateway reads `Resource not found: <id>`, a resource configuration
/// `Resource Configuration not found: <id>` and a service-network/resource association
/// `Service Network Resource Association not found: <id>` — none of them the
/// `<Kind> <identifier> cannot be found.` construction the service-network family uses
/// ([`ResourceKind::not_found_message`]), which is why this construction is its own.
///
/// The value in the sentence is the resource's own ID: these messages name the `rgw-…`,
/// `rcfg-…` or `snra-…` id even when the caller addressed the resource by its ARN (SOURCE:
/// recorded AWS behaviour — a read of a deleted gateway BY ARN is refused `Resource not
/// found: rgw-…`), so the identifier is resolved to the id it names and only an identifier
/// that resolves to none is reported as it was sent. `resourceId` carries the identifier the
/// request named, and `resourceType` is the kind's wire token, exactly as every other
/// not-found of this service carries them.
pub fn resource_access_not_found(
    kind: ResourceKind,
    identifier: &str,
) -> ResourceNotFoundException {
    let named = super::id_from_identifier(identifier, kind.id_prefix())
        .unwrap_or_else(|| identifier.to_string());
    let message = match kind {
        ResourceKind::ResourceGateway => format!("Resource not found: {named}"),
        ResourceKind::ResourceConfiguration => {
            format!("Resource Configuration not found: {named}")
        }
        ResourceKind::ResourceAssociation => {
            format!("Service Network Resource Association not found: {named}")
        }
        other => other.not_found_message(identifier),
    };
    ResourceNotFoundException {
        message,
        resource_id: identifier.to_string(),
        resource_type: kind.wire_type().to_string(),
    }
}

/// A resource gateway: the point of ingress into the VPC where the resources behind it live.
///
/// `ResourceGatewayStatus` is non-terminal right after a create (`CREATE_IN_PROGRESS`), so
/// the clock owes this record its `ACTIVE` status the way it owes a VPC association one: the
/// due instant is stored and compared on read ([`Self::status`]), and nothing sleeps for it.
#[derive(Debug, Clone)]
pub struct ResourceGateway {
    pub id: String,
    pub arn: String,
    pub name: String,
    /// The `vpcIdentifier` the create sent, which every read reports as `vpcId`.
    pub vpc_id: Option<String>,
    pub subnet_ids: Option<Vec<String>>,
    pub security_group_ids: Option<Vec<String>>,
    /// `ipAddressType`, `ipv4AddressesPerEni` and `resourceConfigDnsResolution` exactly as
    /// the create set them, and absent where it said nothing: the model documents no default
    /// for any of the three, so nothing is invented for them here.
    pub ip_address_type: Option<String>,
    pub ipv4_addresses_per_eni: Option<i32>,
    pub resource_config_dns_resolution: Option<String>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
    /// The instant the clock owes this gateway its `ACTIVE` status.
    pub active_at: SystemTime,
    /// Creation order, so a listing pages deterministically. See [`super::encode_cursor`].
    pub sequence: u64,
}

impl ResourceGateway {
    /// `ResourceGatewayStatus` at `now`: the transient create status until the stored due
    /// instant passes, `ACTIVE` from then on. A pure function of stored state and the clock,
    /// so reading it changes nothing a later caller sees.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now >= self.active_at {
            STATUS_ACTIVE
        } else {
            STATUS_CREATE_IN_PROGRESS
        }
    }
}

/// A resource configuration: what one resource reached through a gateway is, as a client of
/// the service network addresses it.
#[derive(Debug, Clone)]
pub struct ResourceConfiguration {
    pub id: String,
    pub arn: String,
    pub name: String,
    /// `ResourceConfigurationType`, exactly as the create sent it (the member is required
    /// and the model constrains it to that enum).
    pub configuration_type: String,
    pub port_ranges: Option<Vec<String>>,
    /// The protocol the create sent, or the documented default for the types that member
    /// applies to ([`DEFAULT_RESOURCE_CONFIGURATION_PROTOCOL`]).
    pub protocol: Option<String>,
    pub resource_gateway_id: Option<String>,
    pub resource_configuration_group_id: Option<String>,
    /// The definition the create sent, stored as the modelled union so every read reports
    /// the same one.
    pub definition: Option<ResourceConfigurationDefinition>,
    /// The setting the create sent, or the documented default for the types it applies to
    /// ([`DEFAULT_ALLOW_SHAREABLE_ASSOCIATION`]).
    pub allow_association_to_shareable_service_network: Option<bool>,
    pub custom_domain_name: Option<String>,
    pub group_domain: Option<String>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
    /// The instant the clock owes this configuration its `ACTIVE` status.
    pub active_at: SystemTime,
    /// Creation order, so a listing pages deterministically. See [`super::encode_cursor`].
    pub sequence: u64,
}

impl ResourceConfiguration {
    /// `ResourceConfigurationStatus` at `now`, on the same clock-owed rule as
    /// [`ResourceGateway::status`].
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now >= self.active_at {
            STATUS_ACTIVE
        } else {
            STATUS_CREATE_IN_PROGRESS
        }
    }
}

/// An association between one service network and one resource configuration.
#[derive(Debug, Clone)]
pub struct ServiceNetworkResourceAssociation {
    pub id: String,
    pub arn: String,
    pub created_by: String,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
    /// The instant the clock owes this association its `ACTIVE` status.
    pub active_at: SystemTime,
    pub resource_configuration_id: String,
    pub resource_configuration_arn: String,
    pub resource_configuration_name: String,
    pub service_network_id: String,
    pub service_network_arn: String,
    pub service_network_name: String,
    pub private_dns_enabled: Option<bool>,
    pub tags: HashMap<String, String>,
    /// Creation order, so a listing pages deterministically. See [`super::encode_cursor`].
    pub sequence: u64,
}

impl ServiceNetworkResourceAssociation {
    /// `ServiceNetworkResourceAssociationStatus` at `now`, on the same clock-owed rule as
    /// [`ResourceGateway::status`].
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now >= self.active_at {
            STATUS_ACTIVE
        } else {
            STATUS_CREATE_IN_PROGRESS
        }
    }
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// The association of this exact (service network, resource configuration) pair, if
    /// there is one.
    ///
    /// A configuration is shared into one service network at most once, so a create naming a
    /// pair this answers is a `ConflictException` rather than a second association — the same
    /// rule the service and VPC associations follow.
    pub fn resource_association_of(
        &self,
        service_network_id: &str,
        resource_configuration_id: &str,
    ) -> Option<&ServiceNetworkResourceAssociation> {
        self.resource_associations.values().find(|association| {
            association.service_network_id == service_network_id
                && association.resource_configuration_id == resource_configuration_id
        })
    }

    /// Drop one resource gateway, answering the record that was removed (`None` when none is
    /// stored under `resource_gateway_id`).
    ///
    /// The ONE gateway-removal path: its account-unique name leaves the name index so the
    /// name is free again, and no `clientToken` record outlives the resource it created.
    pub fn remove_resource_gateway(&mut self, resource_gateway_id: &str) -> Option<ResourceGateway> {
        let removed = self.resource_gateways.remove(resource_gateway_id)?;
        self.name_index
            .remove(&super::name_key(KIND_RESOURCE_GATEWAY, &removed.name));
        self.idempotency_tokens.forget_resource(&removed.id);
        Some(removed)
    }

    /// Drop one resource configuration, answering the record that was removed (`None` when
    /// none is stored under `resource_configuration_id`). Same releases as
    /// [`Self::remove_resource_gateway`], plus the resource policy a configuration may carry.
    pub fn remove_resource_configuration(
        &mut self,
        resource_configuration_id: &str,
    ) -> Option<ResourceConfiguration> {
        let removed = self.resource_configurations.remove(resource_configuration_id)?;
        self.name_index.remove(&super::name_key(
            super::KIND_RESOURCE_CONFIGURATION,
            &removed.name,
        ));
        self.resource_policies.remove(&removed.id);
        self.idempotency_tokens.forget_resource(&removed.id);
        Some(removed)
    }

    /// Drop one service-network/resource association, answering the record that was removed.
    pub fn remove_resource_association(
        &mut self,
        association_id: &str,
    ) -> Option<ServiceNetworkResourceAssociation> {
        let removed = self.resource_associations.remove(association_id)?;
        self.idempotency_tokens.forget_resource(&removed.id);
        Some(removed)
    }
}
