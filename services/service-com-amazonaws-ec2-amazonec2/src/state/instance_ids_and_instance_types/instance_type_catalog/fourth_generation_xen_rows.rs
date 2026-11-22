// SPDX-License-Identifier: CC0-1.0

//! The published rows of the fourth generation families that run on the Xen hypervisor, read
//! through [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! These families came before the AWS Nitro System, and almost nothing about them follows
//! from their size: EBS optimization gives ONE fixed figure per size rather than a baseline
//! an instance bursts above, the EBS volumes share one attachment total with the interfaces
//! and the local devices rather than having a limit of their own, the memory of the memory
//! optimized family is not a whole number of GiB per vCPU, enhanced networking with the
//! Elastic Network Adapter reaches only the sizes whose hardware carries that adapter, the
//! network performance column is published as WORDS on the general purpose family, the
//! interface and address limits of the smallest and largest sizes are the type's own, and the
//! core counts a two-socket size can be configured with go up one per socket. Neither family
//! is the latest generation of its family, while both are offered on a Dedicated Host and
//! both are migrated to another host on a user-initiated reboot.
//!
//! The figures come from each family's own specification page (vCPUs, memory, network
//! performance), the "Amazon EBS optimized instance types" table (the EBS figures),
//! "Instance volume limits" (the shared attachment total), "Enable enhanced networking with
//! ENA" (which sizes carry that adapter) and the "IP addresses per network interface per
//! instance type" table (the interface and address limits).

use super::*;

/// The published hardware facts of the fourth generation Xen families' instance types, one row
/// per type.
pub(super) static FOURTH_GENERATION_XEN_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M4 — general purpose, Intel Xeon E5-2676 v3 (Haswell) at a sustained 2.4 GHz, with no
    // local instance store at any size. Each size publishes ONE EBS figure, the family's own
    // row of the published table rather than a share of its host. The smallest size takes two
    // interfaces where its step of the size ladder publishes three, and the largest takes
    // eight of thirty addresses where that step publishes fifteen of fifty. Enhanced
    // networking with ENA reaches the largest size alone; every smaller size is offered the
    // older adapter instead, which the member reports as no ENA at all. The two sizes whose
    // cores sit on two processor sockets take them one per socket. The Dedicated Host carries
    // the whole host, so the size that IS the whole host is not offered one.
    PublishedInstanceTypeFacts::of("m4.large")
        .previous_generation()
        .ebs(450, 3_600, 450, 3_600)
        .shared_ebs_attachments(40)
        .network("Moderate")
        .interfaces(2)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m4.xlarge")
        .previous_generation()
        .ebs(750, 6_000, 750, 6_000)
        .shared_ebs_attachments(40)
        .network("High")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m4.2xlarge")
        .previous_generation()
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(40)
        .network("High")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m4.4xlarge")
        .previous_generation()
        .ebs(2_000, 16_000, 2_000, 16_000)
        .shared_ebs_attachments(40)
        .network("High")
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m4.10xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(4_000, 32_000, 4_000, 32_000)
        .shared_ebs_attachments(40)
        .network_bandwidth(10.0, 10.0)
        .ena(ENA_UNSUPPORTED)
        .processor("Intel", 2.4)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m4.16xlarge")
        .previous_generation()
        .cores_in_steps_of(2)
        .ebs(10_000, 65_000, 10_000, 65_000)
        .shared_ebs_attachments(40)
        .network_bandwidth(25.0, 25.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        // The size that holds the whole host is built on the Broadwell processor, a sustained
        // 2.3 GHz, where every size below it is built on the Haswell one.
        .processor("Intel", 2.3)
        .dedicated_hosts(false)
        .host_features(true, false),
    // R4 — memory optimized, Intel Xeon E5-2686 v4 (Broadwell) at a sustained 2.3 GHz, with no
    // local instance store. Its published memory is 15.25 GiB per `large` unit of size, which
    // is not a whole number of GiB at any size, and each size publishes ONE EBS figure of its
    // own. Enhanced networking with ENA is offered on the whole family, so no row says
    // otherwise. The largest size's cores sit on two processor sockets and are taken one per
    // socket.
    PublishedInstanceTypeFacts::of("r4.large")
        .previous_generation()
        .memory_mib(15_616)
        .ebs(425, 3_000, 425, 3_000)
        .shared_ebs_attachments(40)
        .network("Up to 10 Gigabit")
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r4.xlarge")
        .previous_generation()
        .memory_mib(31_232)
        .ebs(850, 6_000, 850, 6_000)
        .shared_ebs_attachments(40)
        .network("Up to 10 Gigabit")
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r4.2xlarge")
        .previous_generation()
        .memory_mib(62_464)
        .ebs(1_700, 12_000, 1_700, 12_000)
        .shared_ebs_attachments(40)
        .network("Up to 10 Gigabit")
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r4.4xlarge")
        .previous_generation()
        .memory_mib(124_928)
        .ebs(3_500, 18_750, 3_500, 18_750)
        .shared_ebs_attachments(40)
        .network("Up to 10 Gigabit")
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r4.8xlarge")
        .previous_generation()
        .memory_mib(249_856)
        .ebs(7_000, 37_500, 7_000, 37_500)
        .shared_ebs_attachments(40)
        .network_bandwidth(10.0, 10.0)
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("r4.16xlarge")
        .previous_generation()
        .memory_mib(499_712)
        .cores_in_steps_of(2)
        .ebs(14_000, 75_000, 14_000, 75_000)
        .shared_ebs_attachments(40)
        .network_bandwidth(25.0, 25.0)
        .processor("Intel", 2.3)
        .dedicated_hosts(true)
        .host_features(true, false),
];
