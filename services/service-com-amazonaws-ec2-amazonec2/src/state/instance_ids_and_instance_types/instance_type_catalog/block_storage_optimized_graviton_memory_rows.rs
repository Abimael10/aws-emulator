// SPDX-License-Identifier: CC0-1.0

//! The published rows of the block storage optimized memory family on the newest AWS Graviton
//! hardware, read through [`super::published_instance_type_facts`] like every other row of the
//! catalog.
//!
//! The `b` capability letter is "block storage optimized" (the naming conventions), and what it
//! buys is EBS hardware of the type's own: 3,125 Mbps and 15,000 IOPS per `large` unit of size,
//! so the figures are an order of magnitude above those of the plain memory family of the same
//! generation at the same size, and the volumes are attached to that hardware rather than out
//! of the pool shared with the interfaces — which is why every size reports a limit of its own,
//! of the `dedicated` kind, growing with the share of the host it holds (32 volumes up to the
//! `12xlarge`, then 48, 64 and 128) and the widest size spreads its figures over two EBS cards.
//! The four sizes below the `4xlarge` sustain a baseline they burst above, up to that size's
//! 25,000 Mbps and 120,000 IOPS, and from it the bandwidth is dedicated. The split between
//! network and EBS bandwidth is FIXED on this family, so no size publishes a bandwidth
//! weighting, where the plain memory family of the same generation publishes the weightings a
//! launch may ask for.
//!
//! Its network figures are that generation's memory ladder, its connection tracking holds an
//! established TCP connection for the 350 s of this hardware generation, and the smallest size
//! takes two interfaces of four private addresses each rather than the count its step of the
//! size ladder gives. An enclave is offered from two vCPUs up on this architecture, so the
//! `large` size carries one, and the Elastic Fabric Adapter is offered on the three widest
//! sizes. ENA Express is not offered on the family's adapter, the number of its ENA queues
//! cannot be changed, it takes no interface attachment from a secondary network — so no private
//! address at all is available on one — the packets of a session cannot be copied under its
//! guest, it carries no local PTP hardware clock and an instance of it is not migrated to
//! another host on a user-initiated reboot. The size that holds the whole host carries the cores
//! of two processor sockets, taken one per socket, so its published core counts go up in twos.
//!
//! The figures come from the family's own instance-type specification page, the "Amazon EBS
//! optimized instance types" table (the four EBS figures per size, the recorded `large` row
//! being the one that ladder is read from), "Instance volume limits" and "Connection tracking
//! timeouts".

use super::*;

/// The published hardware facts of the block storage optimized Graviton memory family's
/// instance types, one row per type.
pub(super) static BLOCK_STORAGE_OPTIMIZED_GRAVITON_MEMORY_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    PublishedInstanceTypeFacts::of("r8gb.medium")
        // Half a `large` of size is half that unit's 3,125 Mbps, which is not a whole number of
        // Mbps, so the sustained figure is given in halves of one.
        .ebs_in_half_mbps(3_125, 7_500, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .network("Up to 12.5 Gigabit")
        .interfaces(2)
        .ipv4_per_interface(4)
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.large")
        .ebs(3_125, 15_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .network("Up to 12.5 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .nitro_enclaves(true)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.xlarge")
        .ebs(6_250, 30_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .network("Up to 12.5 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.2xlarge")
        .ebs(12_500, 60_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .network("Up to 15 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.4xlarge")
        .ebs(25_000, 120_000, 25_000, 120_000)
        .dedicated_ebs_attachments(32)
        .network("Up to 15 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.8xlarge")
        .ebs(50_000, 240_000, 50_000, 240_000)
        .dedicated_ebs_attachments(32)
        .network("15 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.12xlarge")
        .ebs(75_000, 360_000, 75_000, 360_000)
        .dedicated_ebs_attachments(32)
        .network("22.5 Gigabit")
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.16xlarge")
        .ebs(100_000, 480_000, 100_000, 480_000)
        .dedicated_ebs_attachments(48)
        .network("30 Gigabit")
        .efa(1)
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.24xlarge")
        .ebs(150_000, 720_000, 150_000, 720_000)
        .dedicated_ebs_attachments(64)
        .network("40 Gigabit")
        .efa(1)
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("r8gb.48xlarge")
        .cores_in_steps_of(2)
        .ebs(300_000, 1_440_000, 300_000, 1_440_000)
        .dedicated_ebs_attachments(128)
        .ebs_cards(2)
        .network("50 Gigabit")
        .efa(1)
        .ena_srd(false)
        .flexible_ena_queues(false)
        .tcp_established_timeout(SHORT_TCP_ESTABLISHED_TIMEOUT)
        .host_features(false, false)
        .traffic_mirroring(false)
        .secondary_network(false)
        .ipv4_per_secondary_interface(0),
];
