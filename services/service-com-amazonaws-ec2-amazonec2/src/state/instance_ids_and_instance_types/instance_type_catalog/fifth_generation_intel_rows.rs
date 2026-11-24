// SPDX-License-Identifier: CC0-1.0

//! The published rows of the fifth generation Intel general purpose family, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! The family that moved the general purpose ladder onto the AWS Nitro System, and almost
//! nothing it publishes follows from its size: its EBS bandwidth is a baseline the four
//! narrowest sizes burst above and a dedicated figure from the `4xlarge` up, whose slope
//! CHANGES at that size rather than carrying one figure per `xlarge`; the network column is
//! the size's own share of its host beside the ceiling it bursts to; the core counts a caller
//! may configure are the even ones from the `xlarge` up, the largest virtualized size takes
//! its cores all at once and the size that holds the whole host takes any count; and the
//! family is no longer the latest generation of its family — a
//! fact of AWS's own "Previous generation instances" reading of the family rather than of the
//! generation number in the name.
//!
//! The figures come from the family's own specification page (network bandwidth, the ENA
//! queues one interface is given), the "Amazon EBS optimized instance types" table (the four
//! EBS figures per size), "Instance volume limits" (the attachments the size that holds the
//! whole host carries) and "Boot modes" (the modes a bare metal size of this generation is
//! offered).

use super::*;

/// The published hardware facts of the fifth generation Intel general purpose family's
/// instance types, one row per type.
pub(super) static FIFTH_GENERATION_INTEL_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M5 — general purpose on the first generation of the AWS Nitro System, an Intel Xeon
    // Platinum (Skylake) host at a sustained 3.1 GHz with no local instance store at any size.
    // Its EBS figures are the family's own rows: a baseline the sizes below the `4xlarge`
    // burst above, up to that size's dedicated 4,750 Mbps and 18,750 IOPS, and a dedicated
    // figure per size above it, reaching 19,000 Mbps and 80,000 IOPS on the whole host. The
    // network column is the share of the host's 25 Gbps the size sustains beside the 10 Gigabit
    // the narrower sizes burst to, and each of its interfaces is given the lesser of the size's
    // vCPUs and the eight queues its adapter carries. The published core counts go up in
    // twos from the `xlarge`, the largest virtualized size taking its cores all at once and the
    // size that holds the whole host publishing every count. The
    // family is offered on a Dedicated Host at EVERY size, the size that holds the whole host
    // included, and the family is no longer the latest generation of its family — AWS's own
    // reading of it, which the generation number in the name does not give — while IPv6 reaches
    // it all the same. That bare metal size is offered the legacy BIOS boot mode alone, carries
    // the larger attachment pool of a whole host, supports CloudWatch action based recovery —
    // which the virtualized sizes with no instance store already do — and is both mirrored and
    // migrated on a reboot by the host's own hardware.
    PublishedInstanceTypeFacts::of("m5.large")
        .previous_generation()
        .ebs(650, 3_600, 4_750, 18_750)
        .network_bandwidth(0.75, 10.0)
        .ena_queues(2)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(1_150, 6_000, 4_750, 18_750)
        .network_bandwidth(1.25, 10.0)
        .ena_queues(4)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.2xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .network_bandwidth(2.5, 10.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.4xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .network_bandwidth(5.0, 10.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.8xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .network_bandwidth(10.0, 10.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.12xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .network_bandwidth(12.0, 12.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.16xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .network_bandwidth(20.0, 20.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.24xlarge")
        .previous_generation()
        // The largest virtualized size takes its cores ALL at once, so the one count it can be
        // configured with is its default, where every size between the `large` and it takes
        // them two at a time and the size that holds the whole host takes any count.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(8)
        .dedicated_hosts(true),
    PublishedInstanceTypeFacts::of("m5.metal")
        .previous_generation()
        .ebs(19_000, 80_000, 19_000, 80_000)
        // A bare metal instance's volumes come out of the larger attachment pool the whole host
        // carries, and it has no hypervisor passing anything else through to draw on it.
        .ebs_attachments(31)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(8)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        // The packets of a session are copied by the host's own card, which a bare metal
        // instance still has, and the move to another host on a user-initiated reboot is the
        // HOST's to make rather than the hypervisor's a bare metal instance has none of.
        .traffic_mirroring(true)
        .host_features(true, false),
];
