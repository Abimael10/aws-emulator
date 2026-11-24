// SPDX-License-Identifier: CC0-1.0

//! The published rows of the general purpose families that carry local NVMe devices, read
//! through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A `d` general purpose instance's local devices are published per type as a COUNT and a
//! size, and the two do not follow one ratio: the ladder keeps one device while the sizes
//! double, then splits the store over two and four devices whose own size goes back down. The
//! EBS volume limit follows from that device count — the devices and the volumes share one
//! attachment pool — and the four EBS figures and the network performance are the family's own
//! published rows.
//!
//! The figures come from each family's own specification ("General purpose instances": the
//! instance store devices and the network performance), the "Amazon EBS optimized instance
//! types" table (the EBS figures), the "IP addresses per network interface per instance type"
//! table, "Boot modes" (the modes a bare metal size is offered) and the published list of the
//! sizes offered an Elastic Fabric Adapter.

use super::*;

/// The published hardware facts of the general purpose NVMe families' instance types, one row
/// per type.
pub(super) static GENERAL_PURPOSE_NVME_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M6id — general purpose with local NVMe devices, Intel Xeon 8375C (Ice Lake) at a
    // sustained 3.5 GHz. Its EBS figures are the sixth-generation Intel ladder's: 1,250 Mbps
    // per `xlarge` of size, with an IOPS column of its own that starts at 3,600 and reaches
    // 160,000 — the sizes below the `8xlarge` publish a baseline they burst above up to 10,000
    // Mbps and 40,000 IOPS, and the figures are dedicated from that size up, 15,000 Mbps and
    // 60,000 IOPS at the `12xlarge`. The local store is ONE device up to the `8xlarge`, two
    // from the `12xlarge` and four from the `24xlarge`, each of the published size of its own
    // row, and the EBS volumes share their attachment pool with those devices. The network
    // figures are the family's share of a 50 Gigabit host, and an Elastic Fabric Adapter is
    // offered on the size that holds the whole host and on the bare metal one.
    PublishedInstanceTypeFacts::of("m6id.large")
        .ebs(625, 3_600, 10_000, 40_000)
        .instance_store(1, 118)
        .network("Up to 12.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.xlarge")
        .ebs(1_250, 6_000, 10_000, 40_000)
        .instance_store(1, 237)
        .network("Up to 12.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.2xlarge")
        .ebs(2_500, 12_000, 10_000, 40_000)
        .instance_store(1, 474)
        .network("Up to 12.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.4xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .instance_store(1, 950)
        .network("Up to 12.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.8xlarge")
        .ebs(10_000, 40_000, 10_000, 40_000)
        .instance_store(1, 1_900)
        .network("12.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.12xlarge")
        .ebs(15_000, 60_000, 15_000, 60_000)
        .instance_store(2, 1_425)
        .network("18.75 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.16xlarge")
        .ebs(20_000, 80_000, 20_000, 80_000)
        .instance_store(2, 1_900)
        .network("25 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.24xlarge")
        .ebs(30_000, 120_000, 30_000, 120_000)
        .instance_store(4, 1_425)
        .network("37.5 Gigabit"),
    PublishedInstanceTypeFacts::of("m6id.32xlarge")
        .ebs(40_000, 160_000, 40_000, 160_000)
        .instance_store(4, 1_900)
        .network("50 Gigabit")
        .efa(1),
    PublishedInstanceTypeFacts::of("m6id.metal")
        .ebs(40_000, 160_000, 40_000, 160_000)
        .instance_store(4, 1_900)
        .network("50 Gigabit")
        .efa(1),
    // M5d — the general purpose family of the first generation of the AWS Nitro System with
    // local NVMe devices, an Intel Xeon Platinum (Skylake) host at a sustained 3.1 GHz. It
    // shares the host of the family without those devices, so it publishes that family's EBS
    // rows — a baseline the sizes below the `4xlarge` burst above, up to that size's dedicated
    // 4,750 Mbps and 18,750 IOPS, then a dedicated figure per size reaching 19,000 Mbps and
    // 80,000 IOPS on the whole host — and that family's network column, the share of a 25 Gbps
    // host the size sustains beside the 10 Gigabit the narrower sizes burst to. What it
    // publishes of its OWN is the local store: ONE device up to the `2xlarge`, two from the
    // `4xlarge` and four from the `16xlarge`, each of the published size of its own row rather
    // than of one ratio, with the EBS volumes sharing their attachment pool with those devices.
    // Each of its interfaces is given the eight ENA queues its adapter carries, whatever the
    // size, and the published core counts go up in twos from the `xlarge`, the largest
    // virtualized size taking its cores all at once and the size that holds the whole host
    // taking any count. Neither a PTP hardware clock nor reboot migration is offered on the
    // family — a migration cannot carry the data of the local devices over to another host —
    // while the packets of a session are copied under every size, the bare metal one included,
    // by the host's own card.
    PublishedInstanceTypeFacts::of("m5d.large")
        .ebs(650, 3_600, 4_750, 18_750)
        .instance_store(1, 75)
        .network_bandwidth(0.75, 10.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.xlarge")
        .cores_in_steps_of(2)
        .ebs(1_150, 6_000, 4_750, 18_750)
        .instance_store(1, 150)
        .network_bandwidth(1.25, 10.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.2xlarge")
        .cores_in_steps_of(2)
        .ebs(2_300, 12_000, 4_750, 18_750)
        .instance_store(1, 300)
        .network_bandwidth(2.5, 10.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.4xlarge")
        .cores_in_steps_of(2)
        .ebs(4_750, 18_750, 4_750, 18_750)
        .instance_store(2, 300)
        .network_bandwidth(5.0, 10.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.8xlarge")
        .cores_in_steps_of(2)
        .ebs(6_800, 30_000, 6_800, 30_000)
        .instance_store(2, 600)
        .network_bandwidth(10.0, 10.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.12xlarge")
        .cores_in_steps_of(2)
        .ebs(9_500, 40_000, 9_500, 40_000)
        .instance_store(2, 900)
        .network_bandwidth(12.0, 12.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.16xlarge")
        .cores_in_steps_of(2)
        .ebs(13_600, 60_000, 13_600, 60_000)
        .instance_store(4, 600)
        .network_bandwidth(20.0, 20.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.24xlarge")
        // The largest virtualized size takes its cores ALL at once, so the one count it can be
        // configured with is its default, where every size between the `large` and it takes
        // them two at a time and the size that holds the whole host takes any count.
        .cores_all_at_once()
        .ebs(19_000, 80_000, 19_000, 80_000)
        .instance_store(4, 900)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .host_features(false, false),
    PublishedInstanceTypeFacts::of("m5d.metal")
        .ebs(19_000, 80_000, 19_000, 80_000)
        // A bare metal instance's volumes come out of the larger attachment pool the whole host
        // carries — the same 31 the family without local devices publishes, the local devices
        // of this one being attached to the host itself rather than out of that pool.
        .ebs_attachments(31)
        .instance_store(4, 900)
        .network_bandwidth(25.0, 25.0)
        .ena_queues(M5D_ENA_QUEUES_PER_INTERFACE)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        // The packets of a session are copied by the host's own card, which a bare metal
        // instance still has.
        .traffic_mirroring(true)
        .host_features(false, false),
];

/// The ENA queues each interface of an M5d instance is given by default
/// (`NetworkCardInfo$DefaultEnaQueueCountPerInterface`): the queue count of the adapter the
/// family's network card carries, which every size of it publishes whatever its vCPUs.
const M5D_ENA_QUEUES_PER_INTERFACE: i32 = 8;
