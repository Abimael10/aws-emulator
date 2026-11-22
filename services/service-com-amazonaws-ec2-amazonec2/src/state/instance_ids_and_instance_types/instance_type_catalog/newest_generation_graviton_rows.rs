// SPDX-License-Identifier: CC0-1.0

//! The published rows of the compute optimized family on the newest AWS Graviton hardware,
//! read through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! What this hardware generation publishes that no size multiplier yields: its network and EBS
//! bandwidth is CONFIGURABLE, so every size publishes the weightings a launch may ask for
//! (the families whose split is fixed publish none at all); its EBS volumes are attached to
//! EBS hardware of the type's own rather than out of the pool shared with its interfaces, so a
//! virtualized size reports the `dedicated` kind of limit while the bare metal size shares its
//! host's pool; its connection tracking holds an established TCP connection for the shorter
//! idle timeout the newest hardware publishes; it takes no interface attachment from a
//! secondary network, so no private address at all is available on one; and the packets of a
//! session cannot be copied under its guest. The Elastic Fabric Adapter is offered on the two
//! sizes that hold a whole host.
//!
//! The bare metal size runs the caller's software on the host with no hypervisor under it, and
//! what it publishes is the HOST's own features rather than a hypervisor's: CloudWatch action
//! based recovery and the move to another host on a user-initiated reboot are the host's to
//! make, and the PTP hardware clock, the trusted platform module and the ENA Express transport
//! sit on its Nitro card whether or not a hypervisor sits under the guest.
//!
//! The figures come from the family's own instance-type specification page (vCPUs, memory,
//! network performance), the "Amazon EBS optimized instance types" table — whose recorded
//! `large` row is the one this family's ladder is read from: the sustained 760 Mbps and 3,040
//! IOPS of that row per `large` of width, the 12,000 Mbps and 48,000 IOPS every size below the
//! dedicated one bursts to, and the 72,000 Mbps and 288,000 IOPS this hardware generation's
//! whole host carries, which the widest sizes of its sibling family publish and which caps the
//! ladder rather than letting it scale on past the host — "Instance volume limits" (whether the
//! volume limit is the volumes' own) and "Connection tracking timeouts".

use super::*;

/// The published hardware facts of the newest generation AWS Graviton compute optimized
/// family's instance types, one row per type.
pub(super) static NEWEST_GENERATION_GRAVITON_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // C9g — compute optimized on the newest AWS Graviton hardware, with no local instance
    // store. The sizes below the `8xlarge` sustain their share of the family's EBS bandwidth
    // and burst to its published ceiling; from that size up the share passes the ceiling and
    // the bandwidth is dedicated, up to the whole host's own figure, which the two widest sizes
    // reach and cannot pass.
    PublishedInstanceTypeFacts::of("c9g.medium")
        .ebs(380, 1_520, 12_000, 48_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.large")
        .ebs(760, 3_040, 12_000, 48_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.xlarge")
        .ebs(1_520, 6_080, 12_000, 48_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.2xlarge")
        .ebs(3_040, 12_160, 12_000, 48_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.4xlarge")
        .ebs(6_080, 24_320, 12_000, 48_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.8xlarge")
        .ebs(12_160, 48_640, 12_160, 48_640)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.12xlarge")
        .ebs(18_240, 72_960, 18_240, 72_960)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.16xlarge")
        .ebs(24_320, 97_280, 24_320, 97_280)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.24xlarge")
        .ebs(36_480, 145_920, 36_480, 145_920)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c9g.48xlarge")
        .ebs(72_000, 288_000, 72_000, 288_000)
        .dedicated_attachment_limit()
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .efa(NEWEST_GENERATION_EFA_INTERFACES),
    PublishedInstanceTypeFacts::of("c9g.metal-48xl")
        .ebs(72_000, 288_000, 72_000, 288_000)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .efa(NEWEST_GENERATION_EFA_INTERFACES)
        .auto_recovery(true)
        .ena_srd(true)
        .nitro_tpm(true)
        .host_features(true, true),
];
