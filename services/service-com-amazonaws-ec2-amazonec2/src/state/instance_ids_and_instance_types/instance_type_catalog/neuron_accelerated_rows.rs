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

/// The `UsageClassType` values a type offered neither a Capacity Block nor anything but
/// On-Demand and Spot publishes: a Capacity Block reserves the accelerated hardware a WHOLE
/// host at a time, so a size that holds less than one is not offered that class.
const ON_DEMAND_AND_SPOT_USAGE_CLASSES: &[&str] = &["on-demand", "spot"];

/// The Neuron device the first generation Trainium family carries, as `NeuronDeviceInfo$Name`
/// publishes it, with the NeuronCores one device carries, the version of those cores and the
/// memory of one device in MiB.
const TRAINIUM_DEVICE: (&str, i32, i32, i32) = ("Trainium", 2, 2, 32_768);

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
    // Trn1 — training accelerated on the 3rd generation Intel Xeon Scalable (Ice Lake) host at
    // a sustained 3.5 GHz, carrying AWS Trainium devices of 32 GiB, each with two NeuronCores
    // of the second version: ONE device on the narrow size and the host's whole sixteen on the
    // size that holds it, which is 512 GiB of accelerator memory. The family publishes NO
    // Inference accelerator inventory of those devices, only the Neuron one.
    // Its EBS figures are the published 80,000 Mbps and 260,000 IOPS of the whole host and the
    // narrow size's sixteenth of both, which that size bursts above up to its published 20,000
    // Mbps ceiling; the whole-host size reports its host's entire attachment pool rather than
    // the pool less the devices it carries, the local devices of that size being the guest's
    // own hardware, while the narrow size publishes 25 volumes of that pool. The local store is
    // the published four 1,900 GB devices of the host and one 475 GB device — a sixteenth of
    // them — on the narrow size. The network is the family's own: the whole-host size carries
    // the eight cards' 800 Gigabit and eight Elastic Fabric Adapters, while the narrow size
    // bursts up to 12.5 Gigabit on one card and is offered no adapter; the interfaces the
    // whole-host size publishes are not recorded here, so it keeps the figure its step of the
    // published interface ladder gives. Its adapter encrypts traffic between instances, the
    // published list of the types that do naming the family; its hardware generation offers
    // neither Nitro Enclaves, nor NitroTPM, nor the UEFI boot mode, nor reboot migration, nor a
    // local PTP hardware clock; its cores are taken two at a time; and a Capacity Block reserves
    // a whole accelerated host, so only the size that holds one is offered that usage class.
    PublishedInstanceTypeFacts::of("trn1.2xlarge")
        .cores_in_steps_of(2)
        .ebs(5_000, 16_250, 20_000, 65_000)
        .ebs_attachments(25)
        .instance_store(1, 475)
        .network_bandwidth(3.125, 12.5)
        .processor("Intel", 3.5)
        .usage_classes(ON_DEMAND_AND_SPOT_USAGE_CLASSES)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .encryption_in_transit(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .host_features(false, false)
        .neurons(
            1,
            TRAINIUM_DEVICE.0,
            TRAINIUM_DEVICE.1,
            TRAINIUM_DEVICE.2,
            TRAINIUM_DEVICE.3,
        ),
    PublishedInstanceTypeFacts::of("trn1.32xlarge")
        .cores_in_steps_of(2)
        .ebs(80_000, 260_000, 80_000, 260_000)
        .ebs_attachments(28)
        .instance_store(4, 1_900)
        .network_bandwidth(800.0, 800.0)
        .efa(8)
        .processor("Intel", 3.5)
        .boot_modes(LEGACY_BIOS_BOOT_MODE)
        .encryption_in_transit(true)
        .nitro_enclaves(false)
        .nitro_tpm(false)
        .host_features(false, false)
        .neurons(
            16,
            TRAINIUM_DEVICE.0,
            TRAINIUM_DEVICE.1,
            TRAINIUM_DEVICE.2,
            TRAINIUM_DEVICE.3,
        ),
];
