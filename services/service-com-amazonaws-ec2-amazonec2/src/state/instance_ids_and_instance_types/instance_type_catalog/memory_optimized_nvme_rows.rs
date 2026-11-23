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
//! above already counts off the published devices), plus the published hibernation
//! prerequisites and the published list of the types offered an Elastic Fabric Adapter for the
//! `n` variant below, whose network hardware is its own while its EBS and local devices are the
//! host's.

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
    // R5dn — the same memory optimized host as the R5d family above, with the `n` capability
    // letter's "network and EBS optimized" hardware on it: the four EBS figures and the local
    // NVMe devices of each size are the R5d rows of the same size, the host's EBS hardware
    // being the same, while the NETWORK is the family's own — a 100 Gigabit host whose sizes up
    // to the `4xlarge` sustain their published share and burst to 25 Gigabit, and whose wider
    // sizes hold a dedicated share of it: 25, 50, 75 and the whole 100 Gigabit. That published
    // share is what the one network card of each size carries.
    // The published hibernation prerequisites name the memory optimized families WITHOUT this
    // capability letter, so no size of this family is offered On-Demand hibernation, whatever
    // its memory; its cores are taken two at a time from the `2xlarge` to the `16xlarge`, the
    // largest virtualized size takes them all at once and the bare metal size publishes every
    // count; the Elastic Fabric Adapter is offered on the two sizes that hold the whole host and
    // not on the `16xlarge` below them; the packets of a session cannot be copied under the
    // guest of its bare metal size; and no size is migrated to another host on a user-initiated
    // reboot or carries a local clock.
    PublishedInstanceTypeFacts::of("r5dn.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .instance_store(1, 75)
        .network_bandwidth(2.1, 25.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.xlarge")
        .ebs(1_150, 6_000, 4_750, 18_750)
        .instance_store(1, 150)
        .network_bandwidth(4.1, 25.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .instance_store(1, 300)
        .network_bandwidth(8.125, 25.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .instance_store(2, 300)
        .network_bandwidth(16.25, 25.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .instance_store(2, 600)
        .network_bandwidth(25.0, 25.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .instance_store(2, 900)
        .network_bandwidth(50.0, 50.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .instance_store(4, 600)
        .network_bandwidth(75.0, 75.0)
        // The published list of the types offered an Elastic Fabric Adapter names the two sizes
        // that hold the whole host and leaves this one out, so it is offered none at all.
        .efa(0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.24xlarge")
        // The largest virtualized size takes its cores ALL at once, where every size below it
        // takes them two at a time.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .instance_store(4, 900)
        .network_bandwidth(100.0, 100.0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("r5dn.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        // The whole host's attachment pool, which this size's local devices are the guest's own
        // hardware in rather than attachments out of.
        .ebs_attachments(31)
        .instance_store(4, 900)
        .network_bandwidth(100.0, 100.0)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(false, false),
];
