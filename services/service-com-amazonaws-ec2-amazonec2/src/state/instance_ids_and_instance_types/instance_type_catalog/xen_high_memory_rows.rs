// SPDX-License-Identifier: CC0-1.0

//! The published rows of the two high-memory families on the Xen hypervisor, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! Two families built on the same four-socket Intel host, whose published facts no function of
//! a size suffix yields: their EBS-optimized mode is on at every size, with ONE fixed figure
//! per size rather than a baseline bursting above it (a pre-Nitro type does not burst), so each
//! row's maximum IS its baseline; their EBS volumes share a limit of 40 with the interfaces and
//! the local devices, the figure their hypervisor allows; their local store is a WHOLE number
//! of one published device, so a size carries one device of its own size rather than a total
//! split into equal parts; their interface limits sit one step BELOW the step of the size
//! ladder they are on; the packets of a session are copied by Nitro hardware these hosts do not
//! have — while the packets of a session are copied under the guest at every size all the same;
//! and CloudWatch action based recovery IS offered at every size, local devices and all.
//!
//! The cores of the widest size of each sit on FOUR processor sockets and are taken one per
//! socket, so its published core counts go up in fours, while the size below it carries two
//! sockets and goes up in twos. The smaller family's AMIs may be backed by its instance store
//! as well as by an EBS volume, which the larger-memory one's may not — the root device types
//! each publishes.
//!
//! The figures come from each family's own instance-type specification page (the vCPUs, the
//! local devices, the network performance, the processor and its sustained frequency), the
//! "Amazon EBS optimized instance types" table (the fixed EBS figures per size), "Instance
//! volume limits" (the attachment limit the hypervisor allows) and the "IP addresses per
//! network interface per instance type" table (the interface limits).

use super::*;

/// The sustained all-core frequency, in GHz, of the Intel Xeon E7-8880 v3 (Haswell) part both
/// families run on.
const HIGH_MEMORY_XEN_SUSTAINED_GHZ: f64 = 2.3;

/// The EBS volumes a size of either family holds attached: the limit its hypervisor allows,
/// shared with the interfaces and the local devices.
const XEN_HIGH_MEMORY_EBS_ATTACHMENTS: i32 = 40;

/// The `EbsOptimizedSupport` of a family whose EBS-optimized mode is always on.
const EBS_OPTIMIZED_BY_DEFAULT: &str = "default";

/// The local device both families' store is a whole number of, in GB: the 1,920 GB SSD the
/// widest sizes carry one or two of.
const HIGH_MEMORY_LOCAL_DEVICE_GB: i64 = 1_920;

/// The published hardware facts of the two Xen high-memory families' instance types, one row
/// per type.
pub(super) static XEN_HIGH_MEMORY_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // X1 — memory optimized on the four-socket Xen host, 1,920 GB SSD devices and EBS figures
    // of 7,000 Mbps and 40,000 IOPS per half host that the mode carries whether or not the
    // launch asks for it. Its AMIs may be backed by its instance store as well as by an EBS
    // volume.
    PublishedInstanceTypeFacts::of("x1.16xlarge")
        // Half the host: two processor sockets, so the published core counts go up in twos.
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(7_000, 40_000, 7_000, 40_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, HIGH_MEMORY_LOCAL_DEVICE_GB)
        .network_bandwidth(10.0, 10.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES),
    PublishedInstanceTypeFacts::of("x1.32xlarge")
        // The whole host: four processor sockets, whose cores are taken one per socket.
        .cores_in_steps_of(4)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(14_000, 80_000, 14_000, 80_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(2, HIGH_MEMORY_LOCAL_DEVICE_GB)
        .network_bandwidth(25.0, 25.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true)
        .root_devices(EBS_AND_INSTANCE_STORE_ROOT_DEVICES),
    // X1e — the same host with twice the memory per vCPU, whose store is ONE device of a
    // published size per size of the ladder until the widest one, and whose EBS figures are the
    // family's own rows: 500 Mbps at the `xlarge`, then 1,000, 1,750, 3,500, 7,000 and 14,000,
    // with an IOPS column that starts at 3,700 rather than at a multiple of the ladder.
    PublishedInstanceTypeFacts::of("x1e.xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(500, 3_700, 500, 3_700)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, 120)
        .network("Up to 10 Gigabit")
        .interfaces(3)
        .ipv4_per_interface(10)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
    PublishedInstanceTypeFacts::of("x1e.2xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(1_000, 7_400, 1_000, 7_400)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, 240)
        .network("Up to 10 Gigabit")
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
    PublishedInstanceTypeFacts::of("x1e.4xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(1_750, 10_000, 1_750, 10_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, 480)
        .network("Up to 10 Gigabit")
        .interfaces(4)
        .ipv4_per_interface(15)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
    PublishedInstanceTypeFacts::of("x1e.8xlarge")
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(3_500, 20_000, 3_500, 20_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, 960)
        .network("Up to 10 Gigabit")
        .interfaces(4)
        .ipv4_per_interface(15)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
    PublishedInstanceTypeFacts::of("x1e.16xlarge")
        .cores_in_steps_of(2)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(7_000, 40_000, 7_000, 40_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(1, HIGH_MEMORY_LOCAL_DEVICE_GB)
        .network_bandwidth(10.0, 10.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
    PublishedInstanceTypeFacts::of("x1e.32xlarge")
        .cores_in_steps_of(4)
        .ebs_optimized(EBS_OPTIMIZED_BY_DEFAULT)
        .ebs(14_000, 80_000, 14_000, 80_000)
        .shared_ebs_attachments(XEN_HIGH_MEMORY_EBS_ATTACHMENTS)
        .instance_store(2, HIGH_MEMORY_LOCAL_DEVICE_GB)
        .network_bandwidth(25.0, 25.0)
        .interfaces(8)
        .ipv4_per_interface(30)
        .processor("Intel", HIGH_MEMORY_XEN_SUSTAINED_GHZ)
        .auto_recovery(true),
];
