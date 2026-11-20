// SPDX-License-Identifier: CC0-1.0

//! The published rows of the eighth generation Intel compute optimized families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! Two families of ONE hardware generation that publish different EBS hardware: the flexible
//! (`flex`) family, whose ladder stops at the `16xlarge` and whose volumes are attached to EBS
//! hardware of its own with a limit of its own, and the block storage optimized (`b`) family,
//! whose figures are an order of magnitude above it at the same size and whose limit grows with
//! the size. Neither follows from the size multiplier in the name: at the `8xlarge` the two
//! differ by a factor of five, and the `flex` sizes burst to a ceiling the other family's
//! baseline passes four sizes below its own.
//!
//! The figures come from each family's own specification, the "Amazon EBS optimized instance
//! types" table (the four EBS figures per size, the recorded `8xlarge` and `48xlarge` rows
//! being the ones the two ladders here are read from), "Instance volume limits" (the dedicated
//! attachment limit per size) and "Connection tracking timeouts" (the established-TCP idle
//! timeout of this hardware generation).

use super::*;

/// The published hardware facts of the eighth generation Intel families' instance types, one
/// row per type.
pub(super) static EIGHTH_GENERATION_INTEL_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // C8i-flex — compute optimized, flexible: the sizes of this family carry the Intel
    // ladder's EBS figures of HALF their width — the `8xlarge`'s 5,000 Mbps and 20,000 IOPS
    // are the figures that ladder's `4xlarge` row publishes — and each size can burst to twice
    // its own sustained figure, never below the 10,000 Mbps and 40,000 IOPS every size of the
    // ladder reaches. Its volumes are attached to EBS hardware of the type's own — a limit of
    // 32 volumes of the `dedicated` kind at every size, rather than one shared with its
    // interfaces and devices. Its network and EBS bandwidth can be weighted towards either
    // side, so every size publishes the weightings a launch may ask for; its connection
    // tracking holds an established TCP connection for the 350 s of this hardware generation;
    // and the family is NOT offered on a Dedicated Host.
    PublishedInstanceTypeFacts::of("c8i-flex.large")
        .ebs(325, 1_800, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.xlarge")
        .ebs(650, 3_600, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.2xlarge")
        .ebs(1_250, 6_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.4xlarge")
        .ebs(2_500, 12_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.8xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.12xlarge")
        .ebs(7_500, 30_000, 15_000, 60_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("c8i-flex.16xlarge")
        .ebs(10_000, 40_000, 20_000, 80_000)
        .dedicated_ebs_attachments(32)
        .bandwidth_weightings(CONFIGURABLE_BANDWIDTH_WEIGHTINGS)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .dedicated_hosts(false),
    // C8ib — compute optimized, block storage optimized: the `b` capability letter is "block
    // storage optimized" (the naming conventions), and what it buys is EBS hardware of the
    // type's own carrying 3,125 Mbps and 15,000 IOPS per `xlarge` of size — the `48xlarge`'s
    // 150,000 Mbps and 720,000 IOPS, and twice that on the size that holds the whole host. The
    // sizes below the `8xlarge` publish a baseline they burst above, up to that size's 25,000
    // Mbps and 120,000 IOPS, and from it the bandwidth is dedicated. The volumes are attached
    // to that hardware rather than out of the shared pool, so the limit is the type's own and
    // of the `dedicated` kind: 32 volumes up to the `12xlarge`, then 48, 64 and 128 as the size
    // grows. Its processor is the Intel Xeon 6 the eighth generation is built on, at a
    // sustained 3.9 GHz; its connection tracking holds an established TCP connection for the
    // 350 s of this hardware generation; it takes no interface attachment from a secondary
    // network — so no private address at all is available on one; the packets of a session
    // cannot be copied under its guest and it carries no local PTP hardware clock, while an
    // instance IS migrated on a user-initiated reboot; an Elastic Fabric Adapter is offered on
    // the two largest sizes; and the size that holds the whole host carries the cores of two
    // processor sockets, taken one per socket. ENA Express is offered from the `4xlarge` up,
    // the sizes under it carrying less dedicated network bandwidth than its transport needs.
    PublishedInstanceTypeFacts::of("c8ib.large")
        .ebs_in_half_mbps(3_125, 7_500, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .ena_srd(false)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.xlarge")
        .ebs(3_125, 15_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .ena_srd(false)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.2xlarge")
        .ebs(6_250, 30_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .ena_srd(false)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.4xlarge")
        .ebs(12_500, 60_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.8xlarge")
        .ebs(25_000, 120_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.12xlarge")
        .ebs(37_500, 180_000, 37_500, 180_000)
        .dedicated_ebs_attachments(32)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.16xlarge")
        .ebs(50_000, 240_000, 50_000, 240_000)
        .dedicated_ebs_attachments(48)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.24xlarge")
        .ebs(75_000, 360_000, 75_000, 360_000)
        .dedicated_ebs_attachments(64)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.32xlarge")
        .ebs(100_000, 480_000, 100_000, 480_000)
        .dedicated_ebs_attachments(64)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("c8ib.48xlarge")
        .ebs(150_000, 720_000, 150_000, 720_000)
        .dedicated_ebs_attachments(128)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .efa(1),
    PublishedInstanceTypeFacts::of("c8ib.96xlarge")
        .cores_in_steps_of(2)
        .ebs(300_000, 1_440_000, 300_000, 1_440_000)
        .dedicated_ebs_attachments(128)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .processor("Intel", 3.9)
        .host_features(true, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0)
        .efa(1),
];
