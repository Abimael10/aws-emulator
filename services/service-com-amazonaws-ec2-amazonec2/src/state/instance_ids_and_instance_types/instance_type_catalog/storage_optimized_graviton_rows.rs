// SPDX-License-Identifier: CC0-1.0

//! The published rows of the storage optimized AWS Graviton families, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! A family whose name reads as a storage family does not thereby publish the DEVICES it
//! carries: the local store of these types is one AWS Nitro SSD per size up to the width at
//! which the host's devices are handed over whole, and both the count and the size of a device
//! are figures of the published specification rather than of the size multiplier. Neither are
//! the EBS figures the store's bandwidth is paired with, the frequency its processor sustains,
//! or the host features it is offered with — hibernation, an enclave, a trusted platform
//! module, a Dedicated Host, reboot migration and a local PTP hardware clock — each of which
//! AWS answers per instance type.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, local devices, network performance), the "Amazon EBS
//! optimized instance types" table (the four EBS figures), "Instance volume limits" (how many
//! volumes a type holds attached) and the "IP addresses per network interface per instance
//! type" table (the interface and address limits).

use super::*;

/// The published hardware facts of the storage optimized Graviton families' instance types,
/// one row per type.
pub(super) static STORAGE_OPTIMIZED_GRAVITON_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // Is4gen — storage optimized on AWS Graviton2 at a sustained 2.5 GHz, carrying the AWS
    // Nitro SSD devices of a 32-vCPU host: one device per size, of a size of its own up to the
    // `2xlarge`, then the host's whole 7,500 GB devices, two of them at the `4xlarge` and four
    // at the size that holds the whole host. Its EBS figures are that store's rows of "Amazon
    // EBS optimized instance types" — the sizes below the `4xlarge` publish a baseline they
    // burst above, up to 10,000 Mbps and 40,000 IOPS, and the bandwidth is dedicated from that
    // size up, reaching 20,000 Mbps and 80,000 IOPS on the size that holds the whole host — and
    // its network figures are the family's published ones: up to 25 Gigabit below the
    // `4xlarge`, then that size's dedicated 25 and the whole host's 50. The EBS volumes share
    // the Nitro attachment pool with the local devices and the interfaces, so the limit each
    // size reports is what that pool leaves once its published devices are counted, which the
    // derivation above already does. Hibernation IS offered on the family, up to the memory the
    // hibernation prerequisites bound an instance at, so the size that holds the whole host —
    // which carries more — is not offered it, which its own row states rather than leaving to
    // the derivation. Nitro Enclaves is offered at NO size, the
    // published list of the types that take one leaving the family out, and neither is NitroTPM;
    // the family is not offered on a Dedicated Host either, and its hardware generation carries
    // no local PTP hardware clock and does not migrate an instance on a user-initiated reboot,
    // the local devices being what a reboot migration could not carry over. ENA Express is
    // offered at NO size of the family, the published list of the types whose adapter carries
    // that transport leaving it out, and no private address at all is available on an
    // interface attached from a secondary network, which the family takes none of. Its smallest
    // size takes two interfaces of four private addresses rather than the three of six its step
    // of the size ladder publishes.
    PublishedInstanceTypeFacts::of("is4gen.medium")
        .ebs(625, 2_500, 10_000, 40_000)
        .instance_store(1, 937)
        .network("Up to 25 Gigabit")
        .interfaces(2)
        .ipv4_per_interface(4)
        .processor("AWS", 2.5)
        .hibernation(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("is4gen.large")
        .ebs(1_250, 5_000, 10_000, 40_000)
        .instance_store(1, 1_875)
        .network("Up to 25 Gigabit")
        .processor("AWS", 2.5)
        .hibernation(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("is4gen.xlarge")
        .ebs(2_500, 10_000, 10_000, 40_000)
        .instance_store(1, 3_750)
        .network("Up to 25 Gigabit")
        .processor("AWS", 2.5)
        .hibernation(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("is4gen.2xlarge")
        .ebs(5_000, 20_000, 10_000, 40_000)
        .instance_store(1, 7_500)
        .network("Up to 25 Gigabit")
        .processor("AWS", 2.5)
        .hibernation(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("is4gen.4xlarge")
        .ebs(10_000, 40_000, 10_000, 40_000)
        .instance_store(2, 7_500)
        .network("25 Gigabit")
        .processor("AWS", 2.5)
        .hibernation(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
    PublishedInstanceTypeFacts::of("is4gen.8xlarge")
        .ebs(20_000, 80_000, 20_000, 80_000)
        .instance_store(4, 7_500)
        .network("50 Gigabit")
        .processor("AWS", 2.5)
        .hibernation(false)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .dedicated_hosts(false)
        .ena_srd(false)
        .host_features(false, false)
        .ipv4_per_secondary_interface(0),
];
