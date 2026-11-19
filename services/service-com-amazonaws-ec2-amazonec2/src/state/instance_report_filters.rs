// SPDX-License-Identifier: CC0-1.0

//! The `DescribeInstances` filters answered off the instance REPORT.
//!
//! "The filters" (`DescribeInstancesRequest$Filters`) documents a name per member of the
//! instance AWS reports, and a filter name EC2 documents is one it RESOLVES: a request
//! carrying it is answered with the instances whose reported value matches, and with an empty
//! reservation set when none does — only a name the operation does not document is refused.
//! So every documented name is listed in [`REPORTED_INSTANCE_FILTERS`] and answered here off
//! the projected [`Instance`]
//! ([`ComAmazonawsEc2Amazonec2State::instance_view`](crate::state::ComAmazonawsEc2Amazonec2State::instance_view)),
//! which is the value the operation REPORTS for that member — so a filter and the report it
//! narrows cannot disagree, and a member this emulator holds no value for simply matches
//! nothing, exactly as an absent attribute does for every other filter of this service.
//!
//! The names whose value the reported instance carries no member for — the members of the
//! stored `NetworkInterface` that `InstanceNetworkInterface` does not repeat — are answered
//! off that one interface store instead, through the same index the instance's own interface
//! projection reads.
//!
//! The names read straight off the stored instance record live beside the record, in
//! `super::instances_and_filters`; this file holds the rest, so each file carries one
//! concern. Both are reached through
//! [`ComAmazonawsEc2Amazonec2State::instance_matches`](crate::state::ComAmazonawsEc2Amazonec2State::instance_matches),
//! and the union of the two catalogs is what
//! [`filter_name_supported`](crate::state::filter_name_supported) accepts.

use super::*;

/// The `DescribeInstances` filter names answered off the instance REPORT.
///
/// Source: the `Filters` member documentation of `com.amazonaws.ec2#DescribeInstancesRequest`
/// (`smithy_query(query="doc",shape="DescribeInstancesRequest",member="Filters")`), every
/// entry of it that [`SUPPORTED_INSTANCE_FILTERS`] does not already answer off the stored
/// instance record. Together the two cover the documented catalog, which is what
/// [`filter_name_supported`] accepts and [`ComAmazonawsEc2Amazonec2State::filter_values`] and
/// [`ComAmazonawsEc2Amazonec2State::reported_instance_filter_values`] between them answer.
pub const REPORTED_INSTANCE_FILTERS: &[&str] = &[
    "affinity",
    "block-device-mapping.attach-time",
    "block-device-mapping.delete-on-termination",
    "block-device-mapping.device-name",
    "block-device-mapping.status",
    "block-device-mapping.volume-id",
    "boot-mode",
    "capacity-reservation-id",
    "capacity-reservation-specification.capacity-reservation-preference",
    "capacity-reservation-specification.capacity-reservation-target.capacity-reservation-id",
    "capacity-reservation-specification.capacity-reservation-target.capacity-reservation-resource-group-arn",
    "current-instance-boot-mode",
    "dns-name",
    "ena-support",
    "enclave-options.enabled",
    "hibernation-options.configured",
    "host-id",
    "hypervisor",
    "iam-instance-profile.id",
    "ipv6-address",
    "launch-time",
    "maintenance-options.auto-recovery",
    "metadata-options.http-endpoint",
    "metadata-options.http-protocol-ipv4",
    "metadata-options.http-protocol-ipv6",
    "metadata-options.http-put-response-hop-limit",
    "metadata-options.http-tokens",
    "metadata-options.instance-metadata-tags",
    "metadata-options.state",
    "network-performance-options.bandwidth-weighting",
    "operator.managed",
    "operator.principal",
    "outpost-arn",
    "placement-group-name",
    "placement-partition-number",
    "platform",
    "platform-details",
    "private-dns-name-options.enable-resource-name-dns-a-record",
    "private-dns-name-options.enable-resource-name-dns-aaaa-record",
    "private-dns-name-options.hostname-type",
    "requester-id",
    "root-device-name",
    "root-device-type",
    "tenancy",
    "tpm-support",
    "usage-operation",
    "usage-operation-update-time",
    "virtualization-type",
    // The interfaces attached to the instance: the members of each one the instance's own
    // interface projection carries, and — read off the stored interface instead — the ones
    // `InstanceNetworkInterface` has no member for.
    "network-interface.addresses.association.allocation-id",
    "network-interface.addresses.association.association-id",
    "network-interface.addresses.association.carrier-ip",
    "network-interface.addresses.association.customer-owned-ip",
    "network-interface.addresses.association.ip-owner-id",
    "network-interface.addresses.association.public-dns-name",
    "network-interface.addresses.association.public-ip",
    "network-interface.association.allocation-id",
    "network-interface.association.association-id",
    "network-interface.association.carrier-ip",
    "network-interface.association.customer-owned-ip",
    "network-interface.association.ip-owner-id",
    "network-interface.association.public-dns-name",
    "network-interface.association.public-ip",
    "network-interface.attachment.attach-time",
    "network-interface.attachment.instance-id",
    "network-interface.attachment.instance-owner-id",
    "network-interface.availability-zone",
    "network-interface.deny-all-igw-traffic",
    "network-interface.ipv4-prefixes.ipv4-prefix",
    "network-interface.ipv6-address",
    "network-interface.ipv6-addresses.ipv6-address",
    "network-interface.ipv6-addresses.is-primary-ipv6",
    "network-interface.ipv6-native",
    "network-interface.ipv6-prefixes.ipv6-prefix",
    "network-interface.operator.managed",
    "network-interface.operator.principal",
    "network-interface.outpost-arn",
    "network-interface.public-dns-name",
    "network-interface.requester-id",
    "network-interface.requester-managed",
    "network-interface.tag-key",
    "network-interface.tag-value",
];

/// Whether the filter `name` is answered off the instance report rather than off the stored
/// record.
///
/// What [`ComAmazonawsEc2Amazonec2State::instance_matches`] asks before projecting that
/// report: a request whose filters all read the record pays for no projection at all.
pub fn filter_reads_instance_report(name: &str) -> bool {
    REPORTED_INSTANCE_FILTERS.contains(&name)
}

impl ComAmazonawsEc2Amazonec2State {
    /// The network interfaces the instance `instance_id` HOLDS, as the records the one
    /// interface store keeps them in.
    ///
    /// The one definition of "the interfaces this instance holds": found through the
    /// per-instance index [`Self::network_interfaces_by_instance`] rather than by scanning
    /// the store, and narrowed to the attachments that still hold — an attachment that has
    /// reached `detached` holds nothing, so its interface is no longer one of the instance's.
    /// [`Self::instance_network_interfaces`] projects exactly these records into the shape
    /// the instance report carries.
    pub fn attached_network_interface_records(&self, instance_id: &str) -> Vec<&NetworkInterface> {
        let Some(held) = self.network_interfaces_by_instance.get(instance_id) else {
            return Vec::new();
        };
        held.iter()
            .filter_map(|network_interface_id| self.network_interfaces.get(network_interface_id))
            .filter(|interface| {
                interface
                    .attachment
                    .as_ref()
                    .is_some_and(|attachment| {
                        !attachment_is_detached(attachment)
                            && attachment.instance_id.as_deref() == Some(instance_id)
                    })
            })
            .collect()
    }

    /// The values the filter `name` reads off the REPORT of one instance, or `None` when this
    /// slice does not resolve that name.
    ///
    /// The companion of [`Self::filter_values`], on the terms the module docs give: the value
    /// compared against is the one `DescribeInstances` reports for that member, so a filter
    /// cannot select on something the report does not show. A name that resolves to an empty
    /// list matches nothing, which is what a member the instance holds no value for does.
    pub fn reported_instance_filter_values(
        &self,
        reported: &Instance,
        instance_id: &str,
        name: &str,
    ) -> Option<Vec<String>> {
        let one = |value: Option<String>| Some(value.into_iter().collect::<Vec<_>>());
        let flag = |value: Option<bool>| {
            Some(
                boolean_filter_values(value)
                    .into_iter()
                    .map(Cow::into_owned)
                    .collect::<Vec<_>>(),
            )
        };
        let number = |value: Option<i32>| {
            Some(
                integer_filter_values(value)
                    .into_iter()
                    .map(Cow::into_owned)
                    .collect::<Vec<_>>(),
            )
        };
        let instant = |value: Option<::aws_smithy_types::DateTime>| {
            Some(
                timestamp_filter_values(value)
                    .into_iter()
                    .map(Cow::into_owned)
                    .collect::<Vec<_>>(),
            )
        };
        // The option sets and the placement the report carries, each read off that one
        // projection so a filter and the member it narrows answer from one value.
        let placement = reported.placement.as_ref();
        let metadata = reported.metadata_options.as_ref();
        let hostname = reported.private_dns_name_options.as_ref();
        let reservation_target = reported
            .capacity_reservation_specification
            .as_ref()
            .and_then(|specification| specification.capacity_reservation_target.as_ref());
        // The volumes the instance holds, the subject of the `block-device-mapping.*` family.
        let volumes = || {
            reported
                .block_device_mappings
                .iter()
                .flatten()
                .filter_map(|mapping| mapping.ebs.as_ref())
        };
        match name {
            "affinity" => one(placement.and_then(|placement| placement.affinity.clone())),
            "host-id" => one(placement.and_then(|placement| placement.host_id.clone())),
            "placement-group-name" => {
                one(placement.and_then(|placement| placement.group_name.clone()))
            }
            "placement-partition-number" => {
                number(placement.and_then(|placement| placement.partition_number))
            }
            "tenancy" => one(placement.and_then(|placement| placement.tenancy.clone())),
            "block-device-mapping.device-name" => Some(
                reported
                    .block_device_mappings
                    .iter()
                    .flatten()
                    .filter_map(|mapping| mapping.device_name.clone())
                    .collect(),
            ),
            "block-device-mapping.attach-time" => Some(
                volumes()
                    .flat_map(|volume| timestamp_filter_values(volume.attach_time))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "block-device-mapping.delete-on-termination" => Some(
                volumes()
                    .flat_map(|volume| boolean_filter_values(volume.delete_on_termination))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "block-device-mapping.status" => Some(
                volumes()
                    .filter_map(|volume| volume.status.clone())
                    .collect(),
            ),
            "block-device-mapping.volume-id" => Some(
                volumes()
                    .filter_map(|volume| volume.volume_id.clone())
                    .collect(),
            ),
            "boot-mode" => one(reported.boot_mode.clone()),
            "current-instance-boot-mode" => one(reported.current_instance_boot_mode.clone()),
            "capacity-reservation-id" => one(reported.capacity_reservation_id.clone()),
            "capacity-reservation-specification.capacity-reservation-preference" => one(
                reported
                    .capacity_reservation_specification
                    .as_ref()
                    .and_then(|specification| {
                        specification.capacity_reservation_preference.clone()
                    }),
            ),
            "capacity-reservation-specification.capacity-reservation-target.capacity-reservation-id" => {
                one(reservation_target
                    .and_then(|target| target.capacity_reservation_id.clone()))
            }
            "capacity-reservation-specification.capacity-reservation-target.capacity-reservation-resource-group-arn" => {
                one(reservation_target
                    .and_then(|target| target.capacity_reservation_resource_group_arn.clone()))
            }
            // "dns-name - The public DNS name of the instance"
            // (`DescribeInstancesRequest$Filters`): the name the report carries, which is
            // EMPTY for an instance whose VPC hands out no public hostnames — so a filter
            // naming a hostname selects only an instance that has one.
            "dns-name" => one(reported.public_dns_name.clone()),
            "ena-support" => flag(reported.ena_support),
            "enclave-options.enabled" => flag(
                reported
                    .enclave_options
                    .as_ref()
                    .and_then(|options| options.enabled),
            ),
            "hibernation-options.configured" => flag(
                reported
                    .hibernation_options
                    .as_ref()
                    .and_then(|options| options.configured),
            ),
            "hypervisor" => one(reported.hypervisor.clone()),
            "iam-instance-profile.id" => one(
                reported
                    .iam_instance_profile
                    .as_ref()
                    .and_then(|profile| profile.id.clone()),
            ),
            "ipv6-address" => one(reported.ipv6_address.clone()),
            "launch-time" => instant(reported.launch_time),
            "maintenance-options.auto-recovery" => one(
                reported
                    .maintenance_options
                    .as_ref()
                    .and_then(|options| options.auto_recovery.clone()),
            ),
            "metadata-options.http-endpoint" => {
                one(metadata.and_then(|options| options.http_endpoint.clone()))
            }
            // "metadata-options.http-protocol-ipv4 - Indicates whether the IPv4 endpoint is
            // enabled" (`DescribeInstancesRequest$Filters`): the IPv4 endpoint IS the
            // instance metadata HTTP endpoint — `InstanceMetadataOptionsResponse` carries a
            // member for turning the IPv6 one on and none for the IPv4 one, which a launch
            // cannot disable on its own — so the filter reads the state of that endpoint,
            // which is the value the report carries for it.
            "metadata-options.http-protocol-ipv4" => {
                one(metadata.and_then(|options| options.http_endpoint.clone()))
            }
            "metadata-options.http-protocol-ipv6" => {
                one(metadata.and_then(|options| options.http_protocol_ipv6.clone()))
            }
            "metadata-options.http-put-response-hop-limit" => {
                number(metadata.and_then(|options| options.http_put_response_hop_limit))
            }
            "metadata-options.http-tokens" => {
                one(metadata.and_then(|options| options.http_tokens.clone()))
            }
            "metadata-options.instance-metadata-tags" => {
                one(metadata.and_then(|options| options.instance_metadata_tags.clone()))
            }
            "metadata-options.state" => one(metadata.and_then(|options| options.state.clone())),
            "network-performance-options.bandwidth-weighting" => one(
                reported
                    .network_performance_options
                    .as_ref()
                    .and_then(|options| options.bandwidth_weighting.clone()),
            ),
            "operator.managed" => flag(
                reported
                    .operator
                    .as_ref()
                    .and_then(|operator| operator.managed),
            ),
            "operator.principal" => one(
                reported
                    .operator
                    .as_ref()
                    .and_then(|operator| operator.principal.clone()),
            ),
            "outpost-arn" => one(reported.outpost_arn.clone()),
            // "platform - The platform. To list only Windows instances, use `windows`"
            // (`DescribeInstancesRequest$Filters`): answered off the value the instance
            // REPORTS for the member ([`INSTANCE_PLATFORM_WINDOWS`]), which is that same
            // word, so the filter and the report cannot disagree. The spelling the modelled
            // enum carries ([`IMAGE_PLATFORM_WINDOWS`]) selects the instance as well, the way
            // `DescribeImages` answers the same filter with both spellings, so a caller that
            // took the value off an image's own report still finds the instances it launched.
            "platform" => Some(
                reported
                    .platform
                    .iter()
                    .flat_map(|platform| {
                        if platform == INSTANCE_PLATFORM_WINDOWS {
                            vec![platform.clone(), IMAGE_PLATFORM_WINDOWS.to_string()]
                        } else {
                            vec![platform.clone()]
                        }
                    })
                    .collect(),
            ),
            "platform-details" => one(reported.platform_details.clone()),
            "private-dns-name-options.enable-resource-name-dns-a-record" => {
                flag(hostname.and_then(|options| options.enable_resource_name_dns_a_record))
            }
            "private-dns-name-options.enable-resource-name-dns-aaaa-record" => {
                flag(hostname.and_then(|options| options.enable_resource_name_dns_aaaa_record))
            }
            "private-dns-name-options.hostname-type" => {
                one(hostname.and_then(|options| options.hostname_type.clone()))
            }
            // "requester-id - The ID of the entity that launched the instance on your behalf"
            // (`DescribeInstancesRequest$Filters`): the reservation's own requester, which an
            // instance the CALLER launched for itself has none of.
            "requester-id" => one(
                self.instances
                    .get(instance_id)
                    .and_then(|instance| self.reservations.get(&instance.reservation_id))
                    .and_then(|reservation| reservation.requester_id.clone()),
            ),
            "root-device-name" => one(reported.root_device_name.clone()),
            "root-device-type" => one(reported.root_device_type.clone()),
            "tpm-support" => one(reported.tpm_support.clone()),
            "usage-operation" => one(reported.usage_operation.clone()),
            "usage-operation-update-time" => instant(reported.usage_operation_update_time),
            "virtualization-type" => one(reported.virtualization_type.clone()),
            _ => self.reported_interface_filter_values(reported, instance_id, name),
        }
    }

    /// The `network-interface.*` half of [`Self::reported_instance_filter_values`].
    ///
    /// The members of the interfaces the instance holds: those the report's own
    /// `InstanceNetworkInterface` carries, read off that projection, and those it has no
    /// member for, read off the stored interface through
    /// [`Self::attached_network_interface_records`] — one interface, so a filter and
    /// `DescribeNetworkInterfaces` cannot disagree about it either.
    fn reported_interface_filter_values(
        &self,
        reported: &Instance,
        instance_id: &str,
        name: &str,
    ) -> Option<Vec<String>> {
        let attached = || reported.network_interfaces.iter().flatten();
        let attachments = || attached().filter_map(|interface| interface.attachment.as_ref());
        // The association an interface carries for the public address bound to it, and the
        // one each PRIVATE address carries for the public address bound to THAT address:
        // "network-interface.association.*" is the first, "network-interface.addresses.
        // association.*" the second (`DescribeInstancesRequest$Filters`).
        let associations = || attached().filter_map(|interface| interface.association.as_ref());
        let address_associations = || {
            attached()
                .flat_map(|interface| interface.private_ip_addresses.iter().flatten())
                .filter_map(|address| address.association.as_ref())
        };
        // The members `InstanceNetworkInterface` does not repeat, off the stored record.
        let records = || self.attached_network_interface_records(instance_id).into_iter();
        match name {
            "network-interface.association.ip-owner-id" => Some(
                associations()
                    .filter_map(|association| association.ip_owner_id.clone())
                    .collect(),
            ),
            "network-interface.association.public-dns-name" => Some(
                associations()
                    .filter_map(|association| association.public_dns_name.clone())
                    .collect(),
            ),
            "network-interface.association.public-ip" => Some(
                associations()
                    .filter_map(|association| association.public_ip.clone())
                    .collect(),
            ),
            "network-interface.association.carrier-ip" => Some(
                associations()
                    .filter_map(|association| association.carrier_ip.clone())
                    .collect(),
            ),
            "network-interface.association.customer-owned-ip" => Some(
                associations()
                    .filter_map(|association| association.customer_owned_ip.clone())
                    .collect(),
            ),
            // The allocation and association ids belong to an Elastic IP the ACCOUNT
            // allocated, which `InstanceNetworkInterfaceAssociation` carries no member for:
            // they are read off the stored interface's own association, where
            // `DescribeNetworkInterfaces` reports them.
            "network-interface.association.allocation-id" => Some(
                records()
                    .filter_map(|interface| interface.association.as_ref())
                    .filter_map(|association| association.allocation_id.clone())
                    .collect(),
            ),
            "network-interface.association.association-id" => Some(
                records()
                    .filter_map(|interface| interface.association.as_ref())
                    .filter_map(|association| association.association_id.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.ip-owner-id" => Some(
                address_associations()
                    .filter_map(|association| association.ip_owner_id.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.public-dns-name" => Some(
                address_associations()
                    .filter_map(|association| association.public_dns_name.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.public-ip" => Some(
                address_associations()
                    .filter_map(|association| association.public_ip.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.carrier-ip" => Some(
                address_associations()
                    .filter_map(|association| association.carrier_ip.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.customer-owned-ip" => Some(
                address_associations()
                    .filter_map(|association| association.customer_owned_ip.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.allocation-id" => Some(
                records()
                    .flat_map(|interface| interface.private_ip_addresses.iter().flatten())
                    .filter_map(|address| address.association.as_ref())
                    .filter_map(|association| association.allocation_id.clone())
                    .collect(),
            ),
            "network-interface.addresses.association.association-id" => Some(
                records()
                    .flat_map(|interface| interface.private_ip_addresses.iter().flatten())
                    .filter_map(|address| address.association.as_ref())
                    .filter_map(|association| association.association_id.clone())
                    .collect(),
            ),
            "network-interface.attachment.attach-time" => Some(
                attachments()
                    .flat_map(|attachment| timestamp_filter_values(attachment.attach_time))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            // The attachment names the instance that holds the interface and the account
            // that owns that instance, which `InstanceNetworkInterfaceAttachment` carries no
            // member for: both are read off the stored attachment.
            "network-interface.attachment.instance-id" => Some(
                records()
                    .filter_map(|interface| interface.attachment.as_ref())
                    .filter_map(|attachment| attachment.instance_id.clone())
                    .collect(),
            ),
            "network-interface.attachment.instance-owner-id" => Some(
                records()
                    .filter_map(|interface| interface.attachment.as_ref())
                    .filter_map(|attachment| attachment.instance_owner_id.clone())
                    .collect(),
            ),
            "network-interface.ipv6-addresses.ipv6-address" | "network-interface.ipv6-address" => {
                Some(
                    attached()
                        .flat_map(|interface| interface.ipv6_addresses.iter().flatten())
                        .filter_map(|address| address.ipv6_address.clone())
                        .collect(),
                )
            }
            "network-interface.ipv6-addresses.is-primary-ipv6" => Some(
                attached()
                    .flat_map(|interface| interface.ipv6_addresses.iter().flatten())
                    .flat_map(|address| boolean_filter_values(address.is_primary_ipv6))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "network-interface.ipv4-prefixes.ipv4-prefix" => Some(
                attached()
                    .flat_map(|interface| interface.ipv4_prefixes.iter().flatten())
                    .filter_map(|prefix| prefix.ipv4_prefix.clone())
                    .collect(),
            ),
            "network-interface.ipv6-prefixes.ipv6-prefix" => Some(
                attached()
                    .flat_map(|interface| interface.ipv6_prefixes.iter().flatten())
                    .filter_map(|prefix| prefix.ipv6_prefix.clone())
                    .collect(),
            ),
            "network-interface.operator.managed" => Some(
                attached()
                    .filter_map(|interface| interface.operator.as_ref())
                    .flat_map(|operator| boolean_filter_values(operator.managed))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "network-interface.operator.principal" => Some(
                attached()
                    .filter_map(|interface| interface.operator.as_ref())
                    .filter_map(|operator| operator.principal.clone())
                    .collect(),
            ),
            "network-interface.availability-zone" => Some(
                records()
                    .filter_map(|interface| interface.availability_zone.clone())
                    .collect(),
            ),
            "network-interface.deny-all-igw-traffic" => Some(
                records()
                    .flat_map(|interface| boolean_filter_values(interface.deny_all_igw_traffic))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "network-interface.ipv6-native" => Some(
                records()
                    .flat_map(|interface| boolean_filter_values(interface.ipv6_native))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "network-interface.outpost-arn" => Some(
                records()
                    .filter_map(|interface| interface.outpost_arn.clone())
                    .collect(),
            ),
            "network-interface.public-dns-name" => Some(
                records()
                    .filter_map(|interface| interface.public_dns_name.clone())
                    .collect(),
            ),
            "network-interface.requester-id" => Some(
                records()
                    .filter_map(|interface| interface.requester_id.clone())
                    .collect(),
            ),
            "network-interface.requester-managed" => Some(
                records()
                    .flat_map(|interface| boolean_filter_values(interface.requester_managed))
                    .map(Cow::into_owned)
                    .collect(),
            ),
            "network-interface.tag-key" => Some(
                records()
                    .flat_map(|interface| interface.tag_set.iter().flatten())
                    .filter_map(|tag| tag.key.clone())
                    .collect(),
            ),
            "network-interface.tag-value" => Some(
                records()
                    .flat_map(|interface| interface.tag_set.iter().flatten())
                    .filter_map(|tag| tag.value.clone())
                    .collect(),
            ),
            _ => None,
        }
    }
}
