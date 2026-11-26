// SPDX-License-Identifier: CC0-1.0

//! The GATEWAYS this account holds: the record `CreateGateway` wrote, and the one
//! projection every gateway read reports it through.
//!
//! `CreateGateway`: "A gateway defines and dynamically manages ingress paths for your Amazon
//! ECS services, handling traffic routing and infrastructure across multiple services while
//! optimizing infrastructure usage." It is a gateway of the ACCOUNT rather than of a cluster
//! — "The name must be unique within your account" — so the store holds one entry per name,
//! exactly as it holds one cluster per cluster name.
//!
//! Two things are settled here rather than in a handler, so every operation that reads a
//! gateway reports the same gateway:
//!
//! * the create RESOLVES the configuration it is given — each member the request leaves out
//!   takes the value its own model doc documents as the default — so a read reports the
//!   configuration the gateway RUNS rather than the half of it a request happened to spell;
//! * `accessType` is DERIVED from the resolved scheme (`Gateway$accessType`: "The gateway
//!   derives this value from its scheme when you create the gateway, and the value is
//!   immutable"), so it is never taken from a request.
//!
//! A created gateway manages nothing yet: "The load balancers and other infrastructure that a
//! gateway manages aren't created until a service in an associated cluster begins using the
//! gateway", which is what [`GatewayRecord::allocated_albs`] counts.

use ::aws_smithy_types::DateTime;

use crate::ops::types::{
    DescribeGatewayAlbConfiguration, DescribeGatewayConfiguration, Gateway, GatewayConfiguration,
    GatewayElbNetworkConfiguration, HostSharedAlbConfiguration, Tag,
};

use super::{
    member_name_pattern_violation, ComAmazonawsEcsAmazonec2containerservicev20141113State,
    ARN_NAMESPACE,
};

/// The resource-type segment a gateway's ARN carries.
pub const GATEWAY_RESOURCE_TYPE: &str = "gateway";

/// The member a request names the gateway it creates by, as the refusals of that name report
/// it (`CreateGatewayRequest$name`).
pub const GATEWAY_NAME_MEMBER: &str = "name";

/// How many gateways this emulator holds. EMULATOR-CHOSEN, so a caller cannot grow the
/// in-memory store without limit — the figure [`super::MAX_CLUSTERS`] is chosen for.
pub const MAX_GATEWAYS: usize = 64;

/// `GatewayStatus` of a gateway that is there and serving: "CreateGateway is a synchronous
/// operation and the gateway is returned with a status of ACTIVE."
pub const GATEWAY_ACTIVE: &str = "ACTIVE";

/// The two `GatewayElbScheme` values, as the model spells them.
pub const GATEWAY_SCHEME_INTERNET_FACING: &str = "internet-facing";
pub const GATEWAY_SCHEME_INTERNAL: &str = "internal";

/// The scheme a `hostSharedAlb` configuration that names none takes —
/// `HostSharedAlbConfiguration$scheme`: "The default is `internet-facing`."
pub const DEFAULT_GATEWAY_SCHEME: &str = GATEWAY_SCHEME_INTERNET_FACING;

/// The IP address type one that names none takes —
/// `GatewayElbNetworkConfiguration$ipAddressType`: "The default is `dualstack`."
pub const DEFAULT_GATEWAY_IP_ADDRESS_TYPE: &str = "dualstack";

/// The desync mitigation mode one that names none takes —
/// `HostSharedAlbConfiguration$httpDesync`: "The default is `defensive`."
pub const DEFAULT_GATEWAY_HTTP_DESYNC: &str = "defensive";

/// How many load balancers a gateway that names no `maxAlbCount` manages at most —
/// `HostSharedAlbConfiguration$maxAlbCount`: "The default is 10."
pub const DEFAULT_GATEWAY_MAX_ALB_COUNT: i32 = 10;

/// The ONE port a gateway's load balancer listener uses, and the ONE protocol it speaks —
/// `HostSharedAlbConfiguration$port`: "The port that the load balancer listener uses is 443.
/// The gateway supports only HTTPS at port 443", and `$protocol`: "The protocol for the load
/// balancer listener is `HTTPS`. No other protocol is supported."
pub const GATEWAY_LISTENER_PORT: i32 = 443;
pub const GATEWAY_LISTENER_PROTOCOL: &str = "HTTPS";

/// Whether a gateway a request names no `deletionProtection` for is protected —
/// `CreateGatewayRequest$deletionProtection`: "The default is true."
pub const DEFAULT_GATEWAY_DELETION_PROTECTION: bool = true;

/// Whether such a gateway propagates its tags — `CreateGatewayRequest$propagateTags`: "The
/// default is GATEWAY."
pub const DEFAULT_PROPAGATE_GATEWAY_TAGS: &str = "GATEWAY";

/// The two `AccessType` values, as `Gateway$accessType` reports one.
pub const ACCESS_TYPE_PUBLIC: &str = "PUBLIC";
pub const ACCESS_TYPE_PRIVATE: &str = "PRIVATE";

/// `InvalidParameterException$message` for a `name` that spells no gateway name at all — the
/// recorded wording of that refusal, fixed text naming the member Amazon ECS calls the
/// gateway's name and nothing of the value.
pub const GATEWAY_NAME_REQUIRED_MESSAGE: &str = "GatewayName must be provided.";

/// `InvalidParameterException$message` for an `infrastructureRoleArn` that names no role.
///
/// The wording of the class as this service states it for a role member left blank — the
/// recorded `CreateExpressGatewayService` refusal is "ExecutionRoleArn can not be blank." —
/// with the member this operation hands its role over through. Fixed text: the member at
/// fault is what the caller has to fill in, and it carried nothing to quote back.
pub const INFRASTRUCTURE_ROLE_BLANK_MESSAGE: &str = "InfrastructureRoleArn can not be blank.";

/// `InvalidParameterException$message` for a listener a gateway cannot serve: the model states
/// the rule as one sentence on `HostSharedAlbConfiguration$port`, so the refusal is that
/// sentence — the whole of it fixed, since the rule names both values it allows.
pub const GATEWAY_LISTENER_MESSAGE: &str = "The gateway supports only HTTPS at port 443.";

/// `GatewayNotFoundException$message` for a reference that names no gateway of this account
/// and Region: the fixed sentence of the class, worded as every other missing resource of this
/// service is ("Cluster not found.").
pub const GATEWAY_NOT_FOUND_MESSAGE: &str = "Gateway not found.";

/// `InvalidParameterException$message` for a `CreateGateway` that repeats a name this account
/// holds a gateway under while carrying no token saying it is the same create: the wording of
/// the class for the creates of this service that take a `clientToken`
/// ([`super::SERVICE_CREATION_NOT_IDEMPOTENT_MESSAGE`]), for the resource this one creates.
pub const GATEWAY_CREATION_NOT_IDEMPOTENT_MESSAGE: &str = "Creation of gateway was not idempotent.";

/// The ARN of a gateway: `arn:aws:ecs:<region>:<account>:gateway/<name>`.
pub fn gateway_arn(region: &str, name: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{GATEWAY_RESOURCE_TYPE}/{name}"),
    )
}

/// The segment a gateway ARN carries before the gateway's own name.
const GATEWAY_ARN_SEGMENT: &str = ":gateway/";

/// The gateway NAME a `gateway` member refers to: the name as given, or the trailing segment
/// of a gateway ARN. `DescribeGateway$gateway`: "The name or Amazon Resource Name (ARN) of the
/// gateway", and every other gateway member documents the same two forms — so naming one
/// gateway either way names the same gateway.
pub fn gateway_name_from_reference(reference: &str) -> &str {
    let reference = reference.trim();
    match reference.rsplit_once(GATEWAY_ARN_SEGMENT) {
        Some((_, name)) => name,
        None => reference,
    }
}

/// The refusal a caller-supplied gateway `name` earns, or `None` for one this account could
/// hold a gateway under.
///
/// A value that spells nothing is the member being left blank, which is the refusal the member
/// has its own sentence for ([`GATEWAY_NAME_REQUIRED_MESSAGE`]); anything else is held to the
/// name rule every named resource of this service is held to ("can be up to 255 characters",
/// [`super::RESOURCE_NAME_PATTERN`]).
pub fn requested_gateway_name_violation(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some(GATEWAY_NAME_REQUIRED_MESSAGE.to_string());
    }
    member_name_pattern_violation(GATEWAY_NAME_MEMBER, name)
}

/// The refusal a requested gateway configuration earns for a listener the gateway cannot
/// serve, or `None` when it names the one port and the one protocol the model allows.
///
/// A member the request leaves out is no listener of its own: it takes the value the gateway
/// uses, which is that same port and protocol ([`resolved_gateway_configuration`]).
pub fn gateway_listener_violation(configuration: &GatewayConfiguration) -> Option<&'static str> {
    let GatewayConfiguration::hostSharedAlb(requested) = configuration;
    let port_refused = requested
        .port
        .is_some_and(|port| port != GATEWAY_LISTENER_PORT);
    let protocol_refused = requested
        .protocol
        .as_deref()
        .is_some_and(|protocol| protocol != GATEWAY_LISTENER_PROTOCOL);
    (port_refused || protocol_refused).then_some(GATEWAY_LISTENER_MESSAGE)
}

/// Whether a gateway whose load balancers carry `scheme` is reachable from the internet —
/// `AccessType`: `PUBLIC` is "publicly accessible from the internet", `PRIVATE` is "reachable
/// only from within the VPC", and an `internet-facing` load balancer is the former.
pub fn gateway_access_type(scheme: &str) -> &'static str {
    if scheme == GATEWAY_SCHEME_INTERNAL {
        ACCESS_TYPE_PRIVATE
    } else {
        ACCESS_TYPE_PUBLIC
    }
}

/// The configuration a gateway RUNS, resolved from the one the request asked for: every member
/// it named kept as it named it, and every member it left out filled in with the value that
/// member's own doc documents as the default.
///
/// The listener's port and protocol are the gateway's own rather than the request's — the
/// model states them as facts of the gateway ("The port that the load balancer listener uses
/// is 443", "The protocol for the load balancer listener is `HTTPS`"), and a request naming
/// any other is refused before this runs ([`gateway_listener_violation`]).
pub fn resolved_gateway_configuration(
    configuration: &GatewayConfiguration,
) -> DescribeGatewayAlbConfiguration {
    let GatewayConfiguration::hostSharedAlb(requested) = configuration;
    let HostSharedAlbConfiguration {
        network_configuration,
        scheme,
        web_acl_arn,
        access_logs,
        port: _,
        protocol: _,
        max_alb_count,
        http_desync,
        test_traffic_rules,
    } = requested.clone();
    let network_configuration = network_configuration.unwrap_or_default();
    DescribeGatewayAlbConfiguration {
        network_configuration: Some(GatewayElbNetworkConfiguration {
            inbound_security_group: network_configuration.inbound_security_group,
            subnets: network_configuration.subnets,
            ip_address_type: Some(
                network_configuration
                    .ip_address_type
                    .unwrap_or_else(|| DEFAULT_GATEWAY_IP_ADDRESS_TYPE.to_string()),
            ),
        }),
        scheme: Some(scheme.unwrap_or_else(|| DEFAULT_GATEWAY_SCHEME.to_string())),
        access_logs,
        web_acl_arn,
        port: Some(GATEWAY_LISTENER_PORT),
        protocol: Some(GATEWAY_LISTENER_PROTOCOL.to_string()),
        max_alb_count: Some(max_alb_count.unwrap_or(DEFAULT_GATEWAY_MAX_ALB_COUNT)),
        // Filled by the record it is reported off, which is what counts the load balancers
        // the gateway has actually allocated.
        current_alb_count: None,
        http_desync: Some(http_desync.unwrap_or_else(|| DEFAULT_GATEWAY_HTTP_DESYNC.to_string())),
        test_traffic_rules,
    }
}

/// ONE gateway: the record `CreateGateway` wrote, and what the calls and the clock since have
/// left.
#[derive(Debug, Clone)]
pub struct GatewayRecord {
    pub arn: String,
    pub name: String,
    /// `Gateway$status`, which a created gateway is always [`GATEWAY_ACTIVE`] in.
    pub status: String,
    pub infrastructure_role_arn: String,
    pub deletion_protection: bool,
    pub propagate_tags: String,
    /// `Gateway$accessType`, derived from the resolved scheme when the gateway was created and
    /// immutable afterwards ([`gateway_access_type`]).
    pub access_type: String,
    /// The configuration the gateway RUNS ([`resolved_gateway_configuration`]).
    pub configuration: DescribeGatewayAlbConfiguration,
    pub tags: Vec<Tag>,
    /// The `clientToken` the create carried, so a repeat of that create is recognised as the
    /// same one ("If this token matches a previous request, the service ignores the request,
    /// but does not return an error"). `None` for a create that carried none, which is a
    /// create no later call can claim to be a repeat of.
    pub client_token: Option<String>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    /// How many Application Load Balancers this gateway has allocated — `currentAlbCount`.
    /// ZERO until a service of an associated cluster uses the gateway: "The load balancers and
    /// other infrastructure that a gateway manages aren't created until a service in an
    /// associated cluster begins using the gateway."
    pub allocated_albs: i32,
}

impl GatewayRecord {
    /// The `Gateway` every gateway read reports this record through.
    ///
    /// `statusReason` and `failures` are what a gateway in a FAILURE state carries
    /// (`Gateway$statusReason`: "populated when the gateway is in a failure state, such as
    /// `UPDATE_FAILED`"), and nothing has left this one in one.
    pub fn view(&self) -> Gateway {
        let mut configuration = self.configuration.clone();
        configuration.current_alb_count = Some(self.allocated_albs);
        Gateway {
            gateway_arn: Some(self.arn.clone()),
            name: Some(self.name.clone()),
            status: Some(self.status.clone()),
            status_reason: None,
            failures: None,
            infrastructure_role_arn: Some(self.infrastructure_role_arn.clone()),
            deletion_protection: Some(self.deletion_protection),
            configuration: Some(DescribeGatewayConfiguration::hostSharedAlb(configuration)),
            access_type: Some(self.access_type.clone()),
            propagate_tags: Some(self.propagate_tags.clone()),
            tags: Some(self.tags.clone()),
            created_at: Some(self.created_at),
            updated_at: Some(self.updated_at),
        }
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The gateway a `gateway` reference names, and `None` for one this account holds none
    /// under — whichever of the two documented forms the reference is spelled in
    /// ([`gateway_name_from_reference`]).
    pub fn gateway(&self, reference: &str) -> Option<&GatewayRecord> {
        self.gateways.get(gateway_name_from_reference(reference))
    }
}
