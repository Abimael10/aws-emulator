// SPDX-License-Identifier: CC0-1.0

//! The published rows of the dense HDD storage optimized families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A dense storage instance's local devices are the whole point of the family, and their
//! geometry is a fact AWS publishes per type: how many magnetic disks the type carries and how
//! big each one is. No function of the size multiplier yields it — the two families here carry
//! disks of DIFFERENT sizes, three of them at their smallest size and twenty-four at their
//! largest — and the EBS volume limit follows from that device count, the devices and the
//! volumes sharing one attachment total. The same holds for the four EBS figures, the network
//! performance words and, on the older family, whether it is the latest generation of its
//! family at all and whether it has an EBS-optimized mode.
//!
//! The figures come from each family's own specification ("Storage optimized instances": the
//! instance store devices, the network performance and the processor), the
//! "Amazon EBS optimized instance types" table (the EBS figures), "Instance volume limits"
//! (the attachment total of the pre-Nitro family, the Nitro one's following from its devices),
//! the "Previous generation instances" page and the published list of the types offered
//! enhanced networking with ENA, which names neither family.

use super::*;

/// The `EbsOptimizedSupport` of a type that is EBS-optimized whenever it is launched, which is
/// what the "EBS optimized" column of a dense storage family's row publishes: the mode is on
/// by default rather than being an option of the launch.
const EBS_OPTIMIZED_BY_DEFAULT: &str = "default";

/// The published hardware facts of the dense HDD storage families' instance types, one row per
/// type.
pub(super) static DENSE_HDD_STORAGE_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // D2 — dense HDD storage of a PREVIOUS generation, Intel Xeon E5-2676 v3 (Haswell) at a
    // sustained 2.4 GHz on the Xen hypervisor, EBS-optimized by DEFAULT with one fixed
    // bandwidth and one fixed IOPS figure per size (a pre-Nitro type has no baseline it bursts
    // above). Its magnetic disks are 2,048 GB each — three at the `xlarge` and twice as many
    // at each step of the ladder — and its EBS volumes share 40 attachments with those disks
    // and its interfaces. Its AMIs may be backed by the local store as well as by an EBS
    // volume, the family predates the Elastic Network Adapter, its memory is the published
    // 30.5 GiB per four vCPUs (not a whole number of GiB at any size), Amazon EBS encryption
    // IS offered on it, Dedicated Hosts are, an instance is migrated on a user-initiated
    // reboot, and it carries no local PTP hardware clock. The largest size carries 36 vCPUs
    // rather than the 32 its step of the ladder would give, on two processor sockets, so its
    // cores are configured two at a time.
    PublishedInstanceTypeFacts::of("d2.xlarge")
        .previous_generation()
        .memory_mib(31_232)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(750, 6_000, 750, 6_000)
        .shared_ebs_attachments(40)
        .instance_store(3, 2_048)
        .network("Moderate")
        .ena(ENA_UNSUPPORTED)
        .ebs_encryption(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("d2.2xlarge")
        .previous_generation()
        .memory_mib(62_464)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(40)
        .instance_store(6, 2_048)
        .network("High")
        .ena(ENA_UNSUPPORTED)
        .ebs_encryption(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("d2.4xlarge")
        .previous_generation()
        .memory_mib(124_928)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(2_000, 16_000, 2_000, 16_000)
        .shared_ebs_attachments(40)
        .instance_store(12, 2_048)
        .network("High")
        .ena(ENA_UNSUPPORTED)
        .ebs_encryption(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("d2.8xlarge")
        .previous_generation()
        .memory_mib(249_856)
        .vcpus(36)
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(4_000, 32_000, 4_000, 32_000)
        .shared_ebs_attachments(40)
        .instance_store(24, 2_048)
        .network("10 Gigabit")
        .ena(ENA_UNSUPPORTED)
        .ebs_encryption(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    // D3 — dense HDD storage on the Nitro System, Intel Xeon Platinum 8259CL (Cascade Lake) at
    // a sustained 3.1 GHz. Its magnetic disks are 1,980 GB each, three at the `xlarge` and
    // doubling with each step of the ladder, and its EBS volumes come out of the shared
    // attachment pool those disks are counted against — which is why the largest size, with
    // twenty-four of them, publishes only a handful of volumes. The EBS figures are the
    // family's own: a baseline per size that bursts up to the figure the size holding the whole
    // host carries. The family is NOT offered on a Dedicated Host.
    PublishedInstanceTypeFacts::of("d3.xlarge")
        .ebs(850, 5_000, 5_000, 30_000)
        .instance_store(3, 1_980)
        .network("Up to 15 Gigabit")
        .processor("Intel", 3.1)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("d3.2xlarge")
        .ebs(1_700, 10_000, 5_000, 30_000)
        .instance_store(6, 1_980)
        .network("Up to 15 Gigabit")
        .processor("Intel", 3.1)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("d3.4xlarge")
        .ebs(2_800, 15_000, 5_000, 30_000)
        .instance_store(12, 1_980)
        .network("Up to 15 Gigabit")
        .processor("Intel", 3.1)
        .dedicated_hosts(false),
    PublishedInstanceTypeFacts::of("d3.8xlarge")
        .ebs(5_000, 30_000, 5_000, 30_000)
        .instance_store(24, 1_980)
        .network("25 Gigabit")
        .processor("Intel", 3.1)
        .dedicated_hosts(false),
];
