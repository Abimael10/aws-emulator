// SPDX-License-Identifier: CC0-1.0

//! The egress IP addresses the service assigns a connector whose traffic it routes.
//!
//! `ServiceManagedEgressIpAddresses` is "the list of egress IP addresses of this
//! connector. These IP addresses are assigned automatically when you create the
//! connector", and the `EgressType` that routes a connector's traffic that way is
//! `SERVICE_MANAGED`, which "uses Transfer Family managed NAT gateways". Those gateways
//! belong to the ACCOUNT and Region rather than to one connector, so every
//! service-managed connector of a Region egresses from the same addresses, and a
//! connector that routes through the customer's own VPC (`VPC_LATTICE`) is assigned
//! none at all.
//!
//! A caller reads the set back to allow-list it on the partner's SFTP server, so it is
//! assigned ONCE — when the connector is created — and stored on the connector
//! ([`Connector::service_managed_egress_ip_addresses`]), which is what makes every later
//! read of that connector report the same addresses.
//!
//! The addresses themselves are this emulator's own, as its account id and its ARNs
//! are: it holds no NAT gateway, so they are taken from the IPv4 block reserved for
//! documentation and examples (RFC 5737's `192.0.2.0/24`) rather than from a routable
//! block that belongs to somebody, and the host octet they start at is folded out of the
//! Region's own name, so one Region's set is stable and two Regions' sets differ.

use super::*;

/// How many addresses one Region's service-managed egress is reported as: one per
/// managed gateway, three being what a recorded description of a service-managed
/// connector reports.
const EGRESS_ADDRESS_COUNT: u8 = 3;

/// The first three octets of the block the addresses are taken from — see this module's
/// docs. Every address of the block satisfies `ServiceManagedEgressIpAddress`'s modelled
/// pattern `^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$`.
const EGRESS_ADDRESS_NETWORK: &str = "192.0.2";

/// The egress addresses a connector created in `region` with `egress_type` is assigned:
/// the Region's managed set for a service-managed connector, and NONE for one whose
/// traffic leaves through the customer's own VPC, which the service manages no egress
/// for.
pub fn service_managed_egress_ip_addresses(region: &str, egress_type: &str) -> Vec<String> {
    if egress_type != CONNECTOR_EGRESS_TYPE_SERVICE_MANAGED {
        return Vec::new();
    }
    let first = first_host_octet(region);
    (0..EGRESS_ADDRESS_COUNT)
        .map(|offset| {
            format!(
                "{EGRESS_ADDRESS_NETWORK}.{}",
                first.saturating_add(offset)
            )
        })
        .collect()
}

/// The host octet a Region's set starts at.
///
/// A fold of the Region's own bytes, so the Region decides the set and nothing else can,
/// kept inside the block's host range (`1..=254`) with room for the whole set above it —
/// neither the network address nor the broadcast address is a host. Reduced at every
/// step, so the fold is exact arithmetic rather than a wrapping one.
fn first_host_octet(region: &str) -> u8 {
    let highest_first = u32::from(254 - EGRESS_ADDRESS_COUNT);
    let folded = region.bytes().fold(0u32, |folded, byte| {
        (folded * 31 + u32::from(byte)) % highest_first
    });
    u8::try_from(folded).unwrap_or_default().saturating_add(1)
}

impl Connector {
    /// `ServiceManagedEgressIpAddresses` as a reader reports it.
    ///
    /// The addresses the service assigned this connector, and NOTHING at all for a
    /// connector it assigned none: a member absent because the service manages no egress
    /// for this connector is not the same answer as an empty list of addresses it does
    /// manage.
    pub fn reported_egress_ip_addresses(&self) -> Option<Vec<String>> {
        (!self.service_managed_egress_ip_addresses.is_empty())
            .then(|| self.service_managed_egress_ip_addresses.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One Region's set is the same three addresses of the block every time, two
    /// Regions' sets differ, and a connector whose traffic leaves through the customer's
    /// own VPC is assigned none.
    #[test]
    fn a_regions_service_managed_egress_set_is_stable_and_its_own() {
        let regions = [
            "us-west-2",
            "us-east-1",
            "eu-central-1",
            "ap-southeast-4",
            "il-central-1",
        ];
        let mut sets: Vec<(&str, Vec<String>)> = Vec::new();
        for region in regions {
            let assigned =
                service_managed_egress_ip_addresses(region, CONNECTOR_EGRESS_TYPE_SERVICE_MANAGED);
            assert_eq!(
                assigned.len(),
                usize::from(EGRESS_ADDRESS_COUNT),
                "case {region:?}: expected {EGRESS_ADDRESS_COUNT} addresses, got {assigned:?}"
            );
            for address in &assigned {
                let octets: Vec<&str> = address.split('.').collect();
                let host: u16 = octets
                    .last()
                    .and_then(|octet| octet.parse().ok())
                    .unwrap_or_default();
                assert!(
                    octets.len() == 4
                        && address.starts_with(EGRESS_ADDRESS_NETWORK)
                        && (1..=254).contains(&host),
                    "case {region:?}: expected a host address of \
                     {EGRESS_ADDRESS_NETWORK}.0/24, got {address:?}"
                );
            }
            let again =
                service_managed_egress_ip_addresses(region, CONNECTOR_EGRESS_TYPE_SERVICE_MANAGED);
            assert_eq!(
                again, assigned,
                "case {region:?}: a Region's set must not change between reads, got \
                 {again:?} after {assigned:?}"
            );
            let vpc_lattice =
                service_managed_egress_ip_addresses(region, CONNECTOR_EGRESS_TYPE_VPC_LATTICE);
            assert!(
                vpc_lattice.is_empty(),
                "case {region:?}: a VPC Lattice connector is assigned no service-managed \
                 egress, got {vpc_lattice:?}"
            );
            sets.push((region, assigned));
        }
        for (region, assigned) in &sets {
            for (other_region, other) in &sets {
                if region == other_region {
                    continue;
                }
                assert_ne!(
                    assigned, other,
                    "cases {region:?} and {other_region:?} are different Regions, so their \
                     egress sets must differ, got {assigned:?}"
                );
            }
        }
    }
}
