// SPDX-License-Identifier: CC0-1.0

//! The published rows of the two storage optimized families on the Xen hypervisor, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! Two families whose local devices ARE the family: the dense HDD one, whose magnetic devices
//! are 2 TB each at every size, and the older SSD one, whose devices are 800 GB each. Neither
//! geometry follows from the size: the store of a size is a WHOLE number of the family's own
//! device, so the count grows with the size while the device itself does not change, which is
//! what makes the total the product of the two rather than a figure split into equal parts.
//!
//! What else these two publish that no name yields: their EBS-optimized mode and the fixed
//! figures that mode carries — a pre-Nitro type has one figure rather than a baseline it bursts
//! above, which is why each row's maximum IS its baseline — the volume limit of 40 their
//! hypervisor allows, shared with their interfaces and devices, their network performance as
//! the published column writes it (the older family's "Moderate" and "High" being text rather
//! than a figure at all), their processors' sustained frequencies, and the host features a Xen
//! type is not offered: the packets of a session are copied by Nitro hardware these hosts do not
//! have, so Traffic Mirroring is offered on neither family. The widest size of each carries the
//! cores of TWO processor sockets, taken one per socket, so its published core counts go up in
//! twos.
//!
//! The dense HDD family's store is ENCRYPTED with a key the host holds, which its row publishes
//! apart from the device model: the devices are magnetic rather than NVMe, and the store is
//! encrypted all the same. The older SSD family is offered on a Dedicated Host, is offered
//! every placement group strategy, supports Amazon EBS encryption, and its AMIs are HVM ones
//! alone while their root device may still be its instance store — the root device types its
//! rows publish.
//!
//! The figures come from each family's own instance-type specification page, the "Amazon EBS
//! optimized instance types" table (the four EBS figures per size, the recorded `8xlarge` row of
//! the dense family and `4xlarge` row of the older one being the ones those ladders are read
//! from), "Instance volume limits" and the "IP addresses per network interface per instance
//! type" table.

use super::*;

/// The sustained all-core frequency, in GHz, of the Broadwell part the dense HDD family runs on.
const DENSE_HDD_SUSTAINED_GHZ: f64 = 2.3;

/// The sustained all-core frequency, in GHz, of the Ivy Bridge part the older SSD storage
/// family runs on.
const SSD_STORAGE_SUSTAINED_GHZ: f64 = 2.5;

/// The EBS volumes a size of either family holds attached: the limit its hypervisor allows,
/// shared with the interfaces and the local devices.
const XEN_STORAGE_EBS_ATTACHMENTS: i32 = 40;

/// The `EbsOptimizedSupport` of a family whose EBS-optimized mode is always on.
const EBS_OPTIMIZED_BY_DEFAULT: &str = "default";

/// The `EbsOptimizedSupport` of a family whose EBS-optimized mode is an option of the launch.
const EBS_OPTIMIZED_ON_REQUEST: &str = "supported";

/// The published hardware facts of the two Xen storage optimized families' instance types, one
/// row per type.
pub(super) static XEN_STORAGE_OPTIMIZED_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // The dense HDD storage family: 2 TB magnetic devices, one per 8 vCPUs, and EBS figures of
    // 875 Mbps and 5,000 IOPS per 4 vCPUs that the mode carries whether or not the launch asks
    // for it.
    PublishedInstanceTypeFacts::of("h1.2xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(1_750, 10_000, 1_750, 10_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .instance_store(1, 2_000)
        .instance_store_encryption(true)
        .network("Up to 10 Gigabit")
        .processor("Intel", DENSE_HDD_SUSTAINED_GHZ)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("h1.4xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(3_500, 20_000, 3_500, 20_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .instance_store(2, 2_000)
        .instance_store_encryption(true)
        .network("Up to 10 Gigabit")
        .processor("Intel", DENSE_HDD_SUSTAINED_GHZ)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("h1.8xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(7_000, 40_000, 7_000, 40_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .instance_store(4, 2_000)
        .instance_store_encryption(true)
        .network("10 Gigabit")
        .processor("Intel", DENSE_HDD_SUSTAINED_GHZ)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("h1.16xlarge")
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(14_000, 80_000, 14_000, 80_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .instance_store(8, 2_000)
        .instance_store_encryption(true)
        .network("25 Gigabit")
        // The size that holds the whole host takes the interfaces its own row publishes rather
        // than the count its step of the size ladder gives, while the addresses one of them
        // takes are that step's.
        .interfaces(8)
        .processor("Intel", DENSE_HDD_SUSTAINED_GHZ)
        .traffic_mirroring(false),
    // The older SSD storage family: 800 GB devices, one per 4 vCPUs, EBS-optimized on request
    // at 500 Mbps and 4,000 IOPS per 4 vCPUs — and NOT at all on the size that holds the whole
    // host, which has no EBS-optimized mode and so no figures to publish.
    PublishedInstanceTypeFacts::of("i2.xlarge")
        .ebs_optimized(EBS_OPTIMIZED_ON_REQUEST)
        .ebs(500, 4_000, 500, 4_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .ebs_encryption(true)
        .instance_store(1, 800)
        .network("Moderate")
        .processor("Intel", SSD_STORAGE_SUSTAINED_GHZ)
        .dedicated_hosts(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("i2.2xlarge")
        .ebs_optimized(EBS_OPTIMIZED_ON_REQUEST)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .ebs_encryption(true)
        .instance_store(2, 800)
        .network("High")
        .processor("Intel", SSD_STORAGE_SUSTAINED_GHZ)
        .dedicated_hosts(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("i2.4xlarge")
        .ebs_optimized(EBS_OPTIMIZED_ON_REQUEST)
        .ebs(2_000, 16_000, 2_000, 16_000)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .ebs_encryption(true)
        .instance_store(4, 800)
        .network("High")
        .processor("Intel", SSD_STORAGE_SUSTAINED_GHZ)
        .dedicated_hosts(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .traffic_mirroring(false),
    PublishedInstanceTypeFacts::of("i2.8xlarge")
        .cores_in_steps_of(2)
        .shared_ebs_attachments(XEN_STORAGE_EBS_ATTACHMENTS)
        .ebs_encryption(true)
        .instance_store(8, 800)
        .network("10 Gigabit")
        .processor("Intel", SSD_STORAGE_SUSTAINED_GHZ)
        .dedicated_hosts(true)
        .placement_strategies(EVERY_PLACEMENT_STRATEGY)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .traffic_mirroring(false),
];
