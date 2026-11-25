// SPDX-License-Identifier: CC0-1.0

//! The published rows of the sixth generation AWS Graviton general purpose family, read
//! through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! The family is carved out of the same 64-vCPU Graviton2 host its compute optimized sibling
//! sits on, so the hardware figures of a size are that host's share — and nothing about them
//! follows the size multiplier in the name: the sustained EBS bandwidth of the three smallest
//! sizes is above a share of the largest size's figure, the IOPS beside each bandwidth is the
//! figure the published table carries at THAT bandwidth, the ladder's bandwidth is dedicated
//! from the `4xlarge` up while its network still bursts two sizes further, and the
//! `12xlarge`'s IOPS stops below the share its bandwidth would give it.
//!
//! The figures come from the family's own instance-type specification page (network
//! performance, the processor's sustained frequency), the "Amazon EBS optimized instance
//! types" table (the four EBS figures per size), the "IP addresses per network interface per
//! instance type" table (the smallest size's interfaces and addresses, which its step of the
//! size ladder does not give), "Instance volume limits" (the volumes a bare metal instance
//! holds attached) and the published lists of the types offered ENA Express, Nitro Enclaves,
//! CloudWatch action based recovery, reboot migration and Traffic Mirroring.

use super::*;

/// The published hardware facts of the sixth generation AWS Graviton general purpose family's
/// instance types, one row per type.
pub(super) static SIXTH_GENERATION_GRAVITON_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M6g — general purpose on AWS Graviton2, at a sustained 2.5 GHz, with no local instance
    // store. Its EBS figures are the Graviton2 ladder's rows of "Amazon EBS optimized instance
    // types": the sizes below the `4xlarge` publish a baseline they burst above, up to 4,750
    // Mbps and 20,000 IOPS, and the bandwidth is dedicated from that size up. The network
    // figures are the family's published ones — up to 10 Gigabit below the `8xlarge`, then 12,
    // 20 and the host's 25 — and its smallest size takes two interfaces of four private
    // addresses each rather than the count its step of the size ladder publishes. The volumes
    // come out of the shared attachment pool, so the virtualized sizes leave the limit the
    // derivation counts, while the bare metal size takes the 31 EBS volumes published for a
    // bare metal instance and IS offered CloudWatch action based recovery. ENA Express is not
    // offered on this hardware generation, and the Elastic Fabric Adapter on no size. An
    // enclave is offered from two vCPUs up on this architecture rather than from the four an
    // x86 type needs, so the `large` size carries one; the bare metal size is offered reboot
    // migration and copies a session's packets for Traffic Mirroring, while it carries no
    // local PTP hardware clock.
    PublishedInstanceTypeFacts::of("m6g.medium")
        .ebs(315, 2_500, 4_750, 20_000)
        .network("Up to 10 Gigabit")
        .interfaces(2)
        .ipv4_per_interface(4)
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.large")
        .ebs(630, 3_600, 4_750, 20_000)
        .network("Up to 10 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false)
        .nitro_enclaves(true),
    PublishedInstanceTypeFacts::of("m6g.xlarge")
        .ebs(1_188, 6_000, 4_750, 20_000)
        .network("Up to 10 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.2xlarge")
        .ebs(2_375, 12_000, 4_750, 20_000)
        .network("Up to 10 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.4xlarge")
        .ebs(4_750, 20_000, 4_750, 20_000)
        .network("Up to 10 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.8xlarge")
        .ebs(9_500, 40_000, 9_500, 40_000)
        .network("12 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.12xlarge")
        .ebs(14_250, 50_000, 14_250, 50_000)
        .network("20 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.16xlarge")
        .ebs(19_000, 80_000, 19_000, 80_000)
        .network("25 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false),
    PublishedInstanceTypeFacts::of("m6g.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(31)
        .network("25 Gigabit")
        .processor("AWS", GRAVITON2_SUSTAINED_GHZ)
        .ena_srd(false)
        .auto_recovery(true)
        .host_features(true, false)
        .traffic_mirroring(true),
];
