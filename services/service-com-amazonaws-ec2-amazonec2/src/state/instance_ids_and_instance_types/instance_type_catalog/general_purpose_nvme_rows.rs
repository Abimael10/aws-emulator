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
//! The figures come from the family's own specification ("General purpose instances": the
//! instance store devices and the network performance), the "Amazon EBS optimized instance
//! types" table (the EBS figures) and the published list of the sizes offered an Elastic Fabric
//! Adapter.

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
];
