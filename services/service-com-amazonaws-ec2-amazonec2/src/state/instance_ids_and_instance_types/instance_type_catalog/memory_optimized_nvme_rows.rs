// SPDX-License-Identifier: CC0-1.0

//! The published rows of the memory optimized families with local NVMe devices, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! What such a family publishes that no size multiplier yields: how many local devices each
//! size carries and how big they are — the count grows in steps of its own while the size of
//! one device changes with it, so two sizes with the same total have different hardware — the
//! four EBS figures of its own row, the network bandwidth it sustains beside the ceiling it
//! bursts to, the core counts a caller may configure (its cores are taken two at a time from
//! the `xlarge` up), and the boot mode and host features of the size that holds the whole
//! host.
//!
//! The figures come from the family's own specification page (vCPUs, memory, instance store
//! devices, network bandwidth), the "Amazon EBS optimized instance types" table (the four EBS
//! figures) and "Instance volume limits" (the shared attachment total, which the derivation
//! above already counts off the published devices).

use super::*;

/// The published hardware facts of the memory optimized NVMe families' instance types, one row
/// per type.
pub(super) static MEMORY_OPTIMIZED_NVME_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // R5d — memory optimized on the AWS Nitro System with local NVMe devices: the four EBS
    // figures are the family's own rows, a baseline the four smallest sizes burst above up to
    // 4,750 Mbps and 18,750 IOPS and a dedicated figure from the `4xlarge` up, with the
    // `8xlarge` and the `12xlarge` each publishing their own. The local devices are the published
    // ones: one device up to the `2xlarge`, two from the `4xlarge` and four from the
    // `16xlarge`, each of a size of its own. The network figure is the share of the host's
    // bandwidth the size sustains beside the 10 Gigabit it bursts to, dedicated from the
    // `8xlarge` up. Its cores are taken two at a time from the `xlarge` to the largest
    // virtualized size, while the size that holds the whole host publishes every count and is
    // offered the legacy BIOS boot mode alone. No size of the family is migrated to another
    // host on a user-initiated reboot, and none carries a local clock.
    PublishedInstanceTypeFacts::of("r5d.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .instance_store(1, 75)
        .network_bandwidth(0.75, 10.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.xlarge")
        .cores_in_steps_of(2)
        .ebs(1_150, 6_000, 4_750, 18_750)
        .instance_store(1, 150)
        .network_bandwidth(1.25, 10.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .instance_store(1, 300)
        .network_bandwidth(2.5, 10.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .instance_store(2, 300)
        .network_bandwidth(5.0, 10.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .instance_store(2, 600)
        .network_bandwidth(10.0, 10.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .instance_store(2, 900)
        .network_bandwidth(12.0, 12.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .instance_store(4, 600)
        .network_bandwidth(20.0, 20.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.24xlarge")
        // The largest virtualized size takes its cores ALL at once, so the one count it can
        // be configured with is its default, where every size below it takes them two at a
        // time.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .instance_store(4, 900)
        .network_bandwidth(25.0, 25.0)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5d.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        // A bare metal instance's local devices are the guest's own hardware, so they draw
        // nothing from the attachment pool its volumes come out of, and the pool itself is the
        // larger one the whole host carries.
        .ebs_attachments(31)
        .instance_store(4, 900)
        .network_bandwidth(25.0, 25.0)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        // The packets of a session are copied by the host's own card, which a bare metal
        // instance still has.
        .traffic_mirroring(true)
        .host_features(false, false),
];
