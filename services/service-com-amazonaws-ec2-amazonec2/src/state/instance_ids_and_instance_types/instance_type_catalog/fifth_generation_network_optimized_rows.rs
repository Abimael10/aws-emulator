// SPDX-License-Identifier: CC0-1.0

//! The published rows of the fifth generation network optimized families — M5n, M5dn, R5n and
//! M5zn — read through [`super::published_instance_type_facts`] like every other row of the
//! catalog.
//!
//! Each carries the `n` capability letter, "network and EBS optimized" (the instance type naming
//! conventions), on the same Intel host as the family without it: what the letter buys is a 100
//! Gigabit host whose sizes up to the `4xlarge` sustain their published share and burst to 25
//! Gigabit while the wider ones hold a dedicated share of it — 25, 50, 75 and the whole 100
//! Gigabit — and the Elastic Fabric Adapter on the sizes that hold a whole host. None of that
//! follows from the size multiplier in the name, which is why these rows exist.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, local devices, network performance), the "Amazon EBS
//! optimized instance types" table (the four EBS figures), "Instance volume limits" (how many
//! volumes a type holds attached, which the derivation above already counts off the published
//! devices for a virtualized size), the published queue counts of the family's adapter (the ENA
//! queues one interface is given), the published hibernation prerequisites — which name the
//! families WITHOUT this capability letter, so no size of these three is offered On-Demand
//! hibernation whatever its memory — and the published list of the types offered an Elastic
//! Fabric Adapter.

use super::*;

/// The published hardware facts of the four families' instance types, one row per type.
pub(super) static FIFTH_GENERATION_NETWORK_OPTIMIZED_INSTANCE_TYPE_FACTS:
    &[PublishedInstanceTypeFacts] = &[
    // M5n — general purpose with the network optimized hardware of the `n` letter, on the first
    // generation of the AWS Nitro System: an Intel Xeon Platinum host at a sustained 3.1 GHz with
    // no local instance store at any size. Its EBS figures are the rows of the host it shares
    // with the family without the letter — a baseline the sizes below the `4xlarge` burst above,
    // up to that size's dedicated 4,750 Mbps and 18,750 IOPS, then a dedicated figure per size
    // reaching 19,000 Mbps and 80,000 IOPS on the whole host — while the NETWORK is the family's
    // own 100 Gigabit host, the share of it each size publishes being what the one network card
    // of that size carries. Each interface is given the queues of the family's adapter: eight on
    // the two narrowest sizes, sixteen on the `2xlarge` and the adapter's full thirty-two from
    // the `4xlarge` up. The published core counts go up in twos from the `2xlarge`, the largest
    // virtualized size takes its cores all at once and the size that holds the whole host takes
    // any count. The Elastic Fabric Adapter is offered on the two sizes that hold the whole host
    // and NOT on the `16xlarge` below them. Every size is migrated to another host on a
    // user-initiated reboot — the bare metal one included, where the move is the HOST's to make
    // rather than the hypervisor's it has none of — and none carries a local PTP hardware clock,
    // while that bare metal size is offered the legacy BIOS boot mode alone, carries the larger
    // attachment pool of a whole host and supports CloudWatch action based recovery, which the
    // virtualized sizes with no local devices already do.
    PublishedInstanceTypeFacts::of("m5n.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .network_bandwidth(2.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.xlarge")
        .ebs(1_150, 6_000, 4_750, 18_750)
        .network_bandwidth(4.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .network_bandwidth(8.125, 25.0)
        .ena_queues(16)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .network_bandwidth(16.25, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .network_bandwidth(50.0, 50.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .network_bandwidth(75.0, 75.0)
        .ena_queues(32)
        // The published list of the types offered an Elastic Fabric Adapter names the two sizes
        // that hold the whole host and leaves this one out, so it is offered none at all.
        .efa(0)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.24xlarge")
        // The largest virtualized size takes its cores ALL at once, so the one count it can be
        // configured with is its default, where every size below it takes them two at a time and
        // the size that holds the whole host takes any count.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m5n.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        // A bare metal instance's volumes come out of the larger attachment pool the whole host
        // carries, and it has no hypervisor passing anything else through to draw on it.
        .ebs_attachments(31)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        // CloudWatch action based recovery is the HOST's to perform rather than the hypervisor's
        // a bare metal instance has none of, and this size carries no local device whose data a
        // recovery could not carry over.
        .auto_recovery(true)
        .host_features(true, false),
    // M5dn — the same network optimized general purpose host as M5n, with the `d` letter's local
    // NVMe devices on it: the EBS figures and the network share of each size are the M5n rows of
    // the same size, the host being the same, while the local store is the family's own published
    // one — ONE device up to the `2xlarge`, two from the `4xlarge` and four from the `16xlarge`,
    // each of the published size of its own row rather than of one ratio. Those devices share
    // their attachment pool with the EBS volumes, so a size's volume limit is the pool less them.
    // Local devices also mean no CloudWatch action based recovery and no reboot migration at any
    // size, the bare metal one included: neither can carry the data of those devices over to
    // another host.
    PublishedInstanceTypeFacts::of("m5dn.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .instance_store(1, 75)
        .network_bandwidth(2.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.xlarge")
        .ebs(1_150, 6_000, 4_750, 18_750)
        .instance_store(1, 150)
        .network_bandwidth(4.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .instance_store(1, 300)
        .network_bandwidth(8.125, 25.0)
        .ena_queues(16)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .instance_store(2, 300)
        .network_bandwidth(16.25, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .instance_store(2, 600)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .instance_store(2, 900)
        .network_bandwidth(50.0, 50.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .instance_store(4, 600)
        .network_bandwidth(75.0, 75.0)
        .ena_queues(32)
        .efa(0)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.24xlarge")
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .instance_store(4, 900)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5dn.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        // The whole host's attachment pool, which this size's local devices are the guest's own
        // hardware in rather than attachments out of.
        .ebs_attachments(31)
        .instance_store(4, 900)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .host_features(false, false),
    // R5n — memory optimized with the same network optimized hardware, on the memory optimized
    // host of the family without the letter: that family's EBS rows, the 100 Gigabit network of
    // the letter, and no local instance store at any size. Its ladder, its adapter's queue
    // counts, its core counts, the one Elastic Fabric Adapter of the sizes that hold a whole host
    // and the host features it is offered are the M5n ones; what differs is the memory of each
    // size, which the family's own ratio gives.
    PublishedInstanceTypeFacts::of("r5n.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .network_bandwidth(2.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.xlarge")
        .ebs(1_150, 6_000, 4_750, 18_750)
        .network_bandwidth(4.1, 25.0)
        .ena_queues(8)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .network_bandwidth(8.125, 25.0)
        .ena_queues(16)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .network_bandwidth(16.25, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .network_bandwidth(50.0, 50.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .network_bandwidth(75.0, 75.0)
        .ena_queues(32)
        .efa(0)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.24xlarge")
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .hibernation(false)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r5n.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(31)
        .network_bandwidth(100.0, 100.0)
        .ena_queues(32)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .auto_recovery(true)
        .host_features(true, false),
    // M5zn — the same network optimized general purpose hardware built for FREQUENCY: a 2nd
    // generation Intel Xeon Scalable host whose cores run at a sustained 4.5 GHz, the highest
    // figure this generation publishes, with the letter's 100 Gigabit network, no local
    // instance store at any size and a ladder that stops at the `12xlarge` that holds the whole
    // host. Nothing of that follows from the size multiplier. Its EBS figures are the family's
    // own rows of "Amazon EBS optimized instance types": the two narrowest sizes publish a
    // baseline they burst above, up to the 3,170 Mbps and 13,333 IOPS the `2xlarge` holds
    // dedicated, and the bandwidth is dedicated from that size up, reaching 19,000 Mbps and
    // 80,000 IOPS on the whole host. The network column is the published one — up to 25 Gigabit
    // on the four narrowest sizes, then half of the host's 100 and all of it. The `3xlarge`
    // takes the eight interfaces of thirty private addresses its own row of the "IP addresses
    // per network interface per instance type" table publishes rather than the four of fifteen
    // its step of the size ladder gives, and the sizes that hold the whole host take fifteen of
    // fifty. The Elastic Fabric Adapter is offered on those whole-host sizes, which carry the
    // family's whole network share, and on no size below them. The published hibernation
    // prerequisites name the general purpose families WITHOUT this capability letter, so no size
    // of this one is offered On-Demand hibernation whatever its memory. The published core
    // counts go up in twos from the `2xlarge`, the largest virtualized size takes its cores all
    // at once and the size that holds the whole host takes any count; that bare metal size
    // carries the 31 EBS volumes "Instance volume limits" publishes for a bare metal instance,
    // is offered the legacy BIOS boot mode alone, supports CloudWatch action based recovery and
    // is migrated to another host on a user-initiated reboot — the move being the HOST's to make
    // rather than the hypervisor's it has none of — while carrying no local PTP hardware clock.
    PublishedInstanceTypeFacts::of("m5zn.large")
        .ebs(800, 3_333, 3_170, 13_333)
        .network("Up to 25 Gigabit")
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.xlarge")
        // The sustained figure of this size is its share of the host's 19,000 Mbps and 80,000
        // IOPS, the integer bandwidth member reporting the whole Mbps of it.
        .ebs(1_583, 6_667, 3_170, 13_333)
        .network("Up to 25 Gigabit")
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.2xlarge")
        .cores_in_steps_of(2)
        .ebs(3_170, 13_333, 3_170, 13_333)
        .network("Up to 25 Gigabit")
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.3xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 20_000, 4_750, 20_000)
        .network("Up to 25 Gigabit")
        .interfaces(8)
        .ipv4_per_interface(30)
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.6xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .network("50 Gigabit")
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.12xlarge")
        // The largest virtualized size takes its cores ALL at once, so the one count it can be
        // configured with is its default, where the sizes between the `xlarge` and it take them
        // two at a time and the size that holds the whole host takes any count.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .network("100 Gigabit")
        .interfaces(15)
        .ipv4_per_interface(50)
        .efa(1)
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false),
    PublishedInstanceTypeFacts::of("m5zn.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(31)
        .network("100 Gigabit")
        .interfaces(15)
        .ipv4_per_interface(50)
        .efa(1)
        .processor("Intel", M5ZN_SUSTAINED_GHZ)
        .hibernation(false)
        .auto_recovery(true)
        .host_features(true, false)
        .boot_modes(LEGACY_BIOS_BOOT_MODE),
];

/// The sustained frequency, in GHz, of the frequency-optimized fifth generation host: the
/// all-core turbo its specification page publishes, which is what
/// `ProcessorInfo$SustainedClockSpeedInGhz` reports for every size of the family.
const M5ZN_SUSTAINED_GHZ: f64 = 4.5;
