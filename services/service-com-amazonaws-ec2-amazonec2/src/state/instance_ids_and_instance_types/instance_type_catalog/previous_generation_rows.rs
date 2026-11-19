// SPDX-License-Identifier: CC0-1.0

//! The published rows of the previous generation families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A previous generation family is not a smaller version of a current one: its memory is the
//! figure its own specification states rather than a whole number of GiB per vCPU, its local
//! devices are the ones it shipped with, EBS optimization is an OPTION of the launch on the
//! sizes offered it and absent on the rest, its EBS volumes share one attachment total with
//! its interfaces and those devices, and IPv6 reaches only the few previous generation
//! families AWS names beside the current ones. The size multiplier in the name yields none of
//! that, which is why these rows exist.
//!
//! The figures come from the "Previous generation instances" page and the family's own
//! specification (vCPUs, memory, instance store devices, network performance), the
//! "Amazon EBS optimized instance types" table (the EBS figures of the sizes offered the
//! option), "Instance volume limits" (the shared attachment total) and the "IP addresses per
//! network interface per instance type" table (the interface and address limits).

use super::*;

/// The published hardware facts of the previous generation families' instance types, one row
/// per type.
pub(super) static PREVIOUS_GENERATION_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // M3 — general purpose of a PREVIOUS generation, Intel Xeon E5-2670 v2 (Ivy Bridge) at a
    // sustained 2.5 GHz on the Xen hypervisor, with local SATA SSD devices: one device up to
    // the `large` and two from the `xlarge` up, each of a size of its own. Its memory is the
    // family's published 3.75 GiB per vCPU, which is not a whole number of GiB at any size.
    // EBS optimization is an OPTION of the launch on the two larger sizes, each of which
    // publishes ONE fixed figure — a pre-Nitro instance has no baseline it bursts above — and
    // is not offered at all on the `medium` and the `large`, which therefore have no
    // EBS-optimized performance to report. The EBS volumes share their 39 attachments with the
    // interfaces and the local devices. The network figures are the published words of the
    // "Network performance" column rather than a figure in Gigabit, and the smallest size
    // takes two interfaces where its step of the size ladder publishes three. Dedicated Hosts
    // and CloudWatch action based recovery are offered on the family, and an instance is
    // migrated on a user-initiated reboot. IPv6 reaches the C3, R3 and I2 previous generation
    // families alone (`IPV6_PREVIOUS_GENERATION_FAMILIES` holds that list), so no size of this
    // one is offered an IPv6 address, while Amazon EBS
    // encryption — which that same page offers M3 beside those three — is supported on every
    // size.
    PublishedInstanceTypeFacts::of("m3.medium")
        .memory_mib(3_840)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(1, 4)
        .network("Moderate")
        .interfaces(2)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.large")
        .memory_mib(7_680)
        .ebs_optimized(EBS_OPTIMIZED_UNSUPPORTED)
        .shared_ebs_attachments(39)
        .instance_store(1, 32)
        .network("Moderate")
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.xlarge")
        .memory_mib(15_360)
        .ebs(500, 4_000, 500, 4_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 40)
        .network("High")
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
    PublishedInstanceTypeFacts::of("m3.2xlarge")
        .memory_mib(30_720)
        .ebs(1_000, 8_000, 1_000, 8_000)
        .shared_ebs_attachments(39)
        .instance_store(2, 80)
        .network("High")
        .ipv4_per_interface(30)
        .processor("Intel", 2.5)
        .dedicated_hosts(true)
        .auto_recovery(true)
        .host_features(true, false),
];
