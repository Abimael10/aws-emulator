// SPDX-License-Identifier: CC0-1.0

//! The published rows of the families whose accelerators are AWS Neuron devices, read through
//! [`super::published_instance_type_facts`] like every other row of the catalog.
//!
//! An accelerated family publishes TWO inventories of the same devices: the Inference
//! accelerators (`InstanceTypeInfo$InferenceAcceleratorInfo`) and the Neuron devices
//! (`$NeuronInfo`), which name the NeuronCores one device carries and the version of those
//! cores beside its memory. Neither follows from the type's name, and neither does the number
//! of devices a size holds: the ladder hands a size one device for several steps and then the
//! host's whole set, so the count is a figure of the published specification rather than of the
//! size multiplier. Nor is the memory per vCPU the one the family's root would give.
//!
//! Each family's comment says where its figures come from: the family's own instance-type
//! specification page (vCPUs, memory, accelerators, network performance), the "Amazon EBS
//! optimized instance types" table (the four EBS figures), "Instance volume limits" (how many
//! volumes a type holds attached) and the published list of the types that encrypt in-transit
//! traffic.

use super::*;

/// The published hardware facts of the Neuron accelerated families' instance types, one row
/// per type.
pub(super) static NEURON_ACCELERATED_INSTANCE_TYPE_FACTS: &[PublishedInstanceTypeFacts] = &[
    // Inf1 — inference accelerated on the 2nd generation Intel Xeon Scalable (Cascade Lake)
    // host at a sustained 3.0 GHz, carrying AWS Inferentia devices of 8 GiB, each with four
    // NeuronCores of the first version: one device up to the `2xlarge`, four on the `6xlarge`
    // and the host's whole sixteen on the size that holds it. Its memory is 2 GiB per vCPU
    // rather than the ratio the accelerated root would give, and its EBS figures are its rows
    // of "Amazon EBS optimized instance types" — the two narrowest sizes publish a baseline
    // they burst above, up to 4,750 Mbps and 20,000 IOPS, and the bandwidth is dedicated from
    // the `6xlarge` up, reaching 19,000 Mbps and 80,000 IOPS on the whole host. The volumes
    // are limited to 26 attachments, below what the shared Nitro pool would leave. Its adapter
    // encrypts traffic between instances, the published list of the types that do naming the
    // family; its hardware generation neither migrates an instance on a user-initiated reboot
    // nor carries a local PTP hardware clock; a Dedicated Host is offered for the two sizes
    // that hold at least a quarter of one and not for the narrower two; and the cores of the
    // size that holds the whole host sit on two processor sockets, so they are taken one per
    // socket.
    PublishedInstanceTypeFacts::of("inf1.xlarge")
        .memory_gib(8)
        .ebs(1_190, 6_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("Up to 25 Gigabit")
        .processor("Intel", 3.0)
        .dedicated_hosts(false)
        .encryption_in_transit(true)
        .host_features(false, false)
        .inference(1, "Inferentia", "AWS", 8_192)
        .neurons(1, "Inferentia", 4, 1, 8_192),
    PublishedInstanceTypeFacts::of("inf1.2xlarge")
        .memory_gib(16)
        .ebs(1_190, 6_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("Up to 25 Gigabit")
        .processor("Intel", 3.0)
        .dedicated_hosts(false)
        .encryption_in_transit(true)
        .host_features(false, false)
        .inference(1, "Inferentia", "AWS", 8_192)
        .neurons(1, "Inferentia", 4, 1, 8_192),
    PublishedInstanceTypeFacts::of("inf1.6xlarge")
        .memory_gib(48)
        .ebs(4_750, 20_000, 4_750, 20_000)
        .ebs_attachments(26)
        .network("25 Gigabit")
        .processor("Intel", 3.0)
        .encryption_in_transit(true)
        .host_features(false, false)
        .inference(4, "Inferentia", "AWS", 8_192)
        .neurons(4, "Inferentia", 4, 1, 8_192),
    PublishedInstanceTypeFacts::of("inf1.24xlarge")
        .memory_gib(192)
        .cores_in_steps_of(2)
        .ebs(19_000, 80_000, 19_000, 80_000)
        .ebs_attachments(26)
        .network("100 Gigabit")
        .processor("Intel", 3.0)
        .encryption_in_transit(true)
        .host_features(false, false)
        .inference(16, "Inferentia", "AWS", 8_192)
        .neurons(16, "Inferentia", 4, 1, 8_192),
];
