// SPDX-License-Identifier: CC0-1.0

//! What `Ref` returns for each `AWS::ServiceDiscovery::*` type, and what `Fn::GetAtt` reads
//! off one.
//!
//! The per-type half of [`super::return_values`] for the AWS Cloud Map types, in a module of
//! its own so neither file grows past the length a change to this repository may touch, and
//! reached through the one arm that file carries for them.
//!
//! ## What identifies one of these resources
//!
//! Cloud Map ASSIGNS the identifier of a namespace and of a service. `CreateHttpNamespace`,
//! `CreatePublicDnsNamespace` and `CreatePrivateDnsNamespace` each take the namespace's NAME
//! and no identifier and answer an `OperationId`; the `ns-…` id that service minted reaches
//! the caller through that operation's `Targets` map ("NAMESPACE: the namespace ID is returned
//! in the `ResourceId` property"), and every later call on the namespace — `GetNamespace`,
//! `CreateService`, `DeleteNamespace` — is addressed by it. `CreateService` answers the
//! `srv-…` id it minted, which `GetService`, `RegisterInstance` and `DeleteService` take.
//! Neither is a value a naming rule could compute, so what this module produces is the
//! STAND-IN a declaration is resolved against until the create has answered
//! ([`super::identity_is_service_assigned`] names those four types, and
//! [`crate::state::provisioning::service_discovery`] is what creates them and reports the
//! identifiers Cloud Map answered).
//!
//! An INSTANCE is the other way round: `RegisterInstance$InstanceId` is "an identifier that
//! you want to associate with the instance" — the caller's, which the type publishes as its
//! `InstanceId` property — so the identifier in force is the one the declaration states, or
//! the name CloudFormation generated for a declaration stating none.
//!
//! ## The hosted zone
//!
//! A DNS namespace's `HostedZoneId` is the id of the Route 53 hosted zone Cloud Map creates
//! WITH the namespace ("Cloud Map automatically creates an Amazon Route 53 … hosted zone that
//! has the same name as the namespace"), so only the owning service can report it: it stands
//! in as the value [`super::service_assigned_document`] computes for a return value only that
//! service has, and the create re-points it at the zone Route 53 really holds.

use super::*;

/// The `AWS::<cloudFormationName>::<Resource>` types of the AWS Cloud Map estate. The middle
/// segment is that model's own `awsService.cloudFormationName` (`ServiceDiscovery`).
pub const SERVICEDISCOVERY_HTTP_NAMESPACE: &str = "AWS::ServiceDiscovery::HttpNamespace";
pub const SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE: &str =
    "AWS::ServiceDiscovery::PublicDnsNamespace";
pub const SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE: &str =
    "AWS::ServiceDiscovery::PrivateDnsNamespace";
pub const SERVICEDISCOVERY_SERVICE: &str = "AWS::ServiceDiscovery::Service";
pub const SERVICEDISCOVERY_INSTANCE: &str = "AWS::ServiceDiscovery::Instance";

/// The ARN namespace (and endpoint prefix) Cloud Map mints its resources under — that model's
/// `awsService.arnNamespace`.
pub const SERVICEDISCOVERY_ARN_NAMESPACE: &str = "servicediscovery";

/// The property an instance states its own identifier in, which is this resource's physical
/// id ([`super::declared_name_property`] reads it as the instance's name for that reason).
pub const SERVICEDISCOVERY_INSTANCE_ID_PROPERTY: &str = "InstanceId";

/// The property an instance names the service it is registered in, and a service names the
/// namespace it lives in: each is the resource the create is addressed by rather than a value
/// of this resource.
pub const SERVICEDISCOVERY_SERVICE_ID_PROPERTY: &str = "ServiceId";
/// See [`SERVICEDISCOVERY_SERVICE_ID_PROPERTY`].
pub const SERVICEDISCOVERY_NAMESPACE_ID_PROPERTY: &str = "NamespaceId";

/// The attribute a DNS namespace publishes the Route 53 hosted zone Cloud Map created for it
/// under ("HostedZoneId: the ID for the Route 53 hosted zone that Cloud Map creates when you
/// create a namespace", `DnsProperties$HostedZoneId`).
pub const SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE: &str = "HostedZoneId";

/// The salt the stand-in namespace id is derived under, and the one a service's is, so the two
/// ids of one estate are different digits ([`derived_digits`]).
const NAMESPACE_ID_SALT: &str = "servicediscovery/namespace-id";
/// See [`NAMESPACE_ID_SALT`].
const SERVICE_ID_SALT: &str = "servicediscovery/service-id";

/// The prefix Cloud Map writes a namespace id with, and the one it writes a service id with:
/// the ids that service mints are `ns-`/`srv-` followed by a fixed-length run of lower-case
/// alphanumerics, and a stand-in of the same shape is what a declaration reads until the
/// create answers the real one.
const NAMESPACE_ID_PREFIX: &str = "ns";
/// See [`NAMESPACE_ID_PREFIX`].
const SERVICE_ID_PREFIX: &str = "srv";

/// How many characters follow that prefix.
const CLOUD_MAP_ID_CHARS: usize = 16;

/// Whether `resource_type` is one of the Cloud Map types this module carries.
pub(super) fn is_service_discovery_type(resource_type: &str) -> bool {
    matches!(
        resource_type,
        SERVICEDISCOVERY_HTTP_NAMESPACE
            | SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE
            | SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE
            | SERVICEDISCOVERY_SERVICE
            | SERVICEDISCOVERY_INSTANCE
    )
}

/// Whether the PHYSICAL id of a resource of this type is one Cloud Map assigns: every type
/// here but the instance, whose id its own declaration states.
pub(super) fn identity_is_assigned(resource_type: &str) -> bool {
    is_service_discovery_type(resource_type) && resource_type != SERVICEDISCOVERY_INSTANCE
}

/// The physical resource id and the attributes a resource of one of these types reports,
/// as [`super::return_values`]'s own arm answers them.
pub(super) fn service_discovery_return_values(
    resource_type: &str,
    name: &str,
    properties: &Value,
    scope: &Scope<'_>,
    budget: &mut Budget,
) -> (String, BTreeMap<String, String>) {
    let region = scope.stack.region;
    let mut attributes = BTreeMap::new();
    match resource_type {
        // A namespace of any of the three kinds: "Ref returns the namespace ID", and
        // `Fn::GetAtt` reads that id and the ARN Cloud Map built on it. A DNS namespace
        // publishes the hosted zone beside them, which only the owning service can report.
        SERVICEDISCOVERY_HTTP_NAMESPACE
        | SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE
        | SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE => {
            let namespace_id = stand_in_namespace_id(name);
            attributes.insert("Id".to_string(), namespace_id.clone());
            attributes.insert(
                "Arn".to_string(),
                namespace_arn(region, &namespace_id),
            );
            if resource_type != SERVICEDISCOVERY_HTTP_NAMESPACE {
                attributes.insert(
                    SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE.to_string(),
                    service_assigned_document(name, SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE),
                );
            }
            (namespace_id, attributes)
        }
        // A service of a namespace: "Ref returns the service ID", and `Fn::GetAtt` reads that
        // id, the ARN Cloud Map built on it and the name the service was created under.
        SERVICEDISCOVERY_SERVICE => {
            let service_id = stand_in_service_id(name);
            attributes.insert("Id".to_string(), service_id.clone());
            attributes.insert("Arn".to_string(), service_arn(region, &service_id));
            attributes.insert("Name".to_string(), name.to_string());
            (service_id, attributes)
        }
        // One registered instance: "Ref returns the instance ID", which is the `InstanceId`
        // the declaration states — Cloud Map mints none — and the type publishes no
        // `Fn::GetAtt` attribute of its own.
        SERVICEDISCOVERY_INSTANCE => (
            declared_text(properties, scope, budget, SERVICEDISCOVERY_INSTANCE_ID_PROPERTY)
                .unwrap_or_else(|| name.to_string()),
            attributes,
        ),
        // No other type reaches here: the one arm that calls this names exactly the five
        // above ([`is_service_discovery_type`]), and a type outside them keeps the physical
        // name the naming rules computed, as every type this catalogue does not carry does.
        _ => (name.to_string(), attributes),
    }
}

/// The NAMES of the attributes a resource of one of these types publishes — the other half
/// of the catalogue above, read without a resource ([`super::published_attribute_names`]).
pub(super) fn published_attributes(resource_type: &str) -> &'static [&'static str] {
    match resource_type {
        SERVICEDISCOVERY_HTTP_NAMESPACE => &["Arn", "Id"],
        SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE | SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE => {
            &["Arn", "Id", SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE]
        }
        SERVICEDISCOVERY_SERVICE => &["Arn", "Id", "Name"],
        // An instance publishes no attribute of its own.
        _ => &[],
    }
}

/// The ARN of one namespace, built on the id Cloud Map minted for it, in the resource path
/// that service writes one in (`namespace/<namespace id>`).
fn namespace_arn(region: &str, namespace_id: &str) -> String {
    ::emulator_server::emulator_arn(
        SERVICEDISCOVERY_ARN_NAMESPACE,
        region,
        &format!("namespace/{namespace_id}"),
    )
}

/// The ARN of one service, in the resource path Cloud Map writes one in
/// (`service/<service id>`).
fn service_arn(region: &str, service_id: &str) -> String {
    ::emulator_server::emulator_arn(
        SERVICEDISCOVERY_ARN_NAMESPACE,
        region,
        &format!("service/{service_id}"),
    )
}

/// One declared property as the text it resolves to, or `None` where the declaration states
/// none or states it empty — an empty identifier is no identifier a caller could address.
fn declared_text(
    properties: &Value,
    scope: &Scope<'_>,
    budget: &mut Budget,
    property: &str,
) -> Option<String> {
    properties
        .get(property)
        .and_then(|value| text(value, scope, 0, budget))
        .filter(|resolved| !resolved.is_empty())
}

/// The stand-in a NAMESPACE id is resolved to before Cloud Map has been asked for one:
/// [`CLOUD_MAP_ID_CHARS`] characters derived from the resource's own name under this type's
/// salt, written with the prefix that service writes a namespace id with — stable across
/// reads and across a later deployment of the same stack.
fn stand_in_namespace_id(name: &str) -> String {
    stand_in_id(NAMESPACE_ID_PREFIX, NAMESPACE_ID_SALT, name)
}

/// [`stand_in_namespace_id`] for a SERVICE id.
fn stand_in_service_id(name: &str) -> String {
    stand_in_id(SERVICE_ID_PREFIX, SERVICE_ID_SALT, name)
}

/// One stand-in of Cloud Map's own id shape.
fn stand_in_id(prefix: &str, salt: &str, name: &str) -> String {
    format!(
        "{prefix}-{}",
        derived_digits(salt, name, CLOUD_MAP_ID_CHARS, Alphabet::Lower)
    )
}
