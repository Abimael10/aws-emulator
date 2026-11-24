// SPDX-License-Identifier: CC0-1.0

//! The published rows of two more instance-type families — the fifth generation Intel general
//! purpose family and the inference accelerated family built on AWS Inferentia2 devices —
//! driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test is the one `published_instance_type_hardware_facts` pins: AN INSTANCE
//! TYPE'S HARDWARE AND CAPABILITIES ARE PUBLISHED FACTS OF THAT TYPE, read off a per-type
//! table, not computed from the vCPUs and memory its name gives. So a family can be reported
//! as no longer the latest generation of its family while its generation NUMBER is high, carry
//! accelerators whose count stays the same over two steps of the ladder and then jumps to the
//! host's whole set, publish a network figure its share of the host does not give, and answer
//! `unsupported` to the host features its hypervisor would otherwise provide.
//!
//! Every table below spans what the rule generalises over: two families of two different
//! designs, the smallest, a middle, the largest virtualized and (for the family that has one)
//! a bare metal size of each, an accelerated family beside a general purpose one, and — for
//! each capability member — one type that has it beside one that does not. The sizes the trace
//! that filed this happened to name (`m5.8xlarge`, `inf2.24xlarge`) are two rows among many,
//! and the closing test holds a family of each design against the family one generation above
//! it at the SAME size, so no row of these tables can be read as the specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, indexed};

/// The Region every call here is signed for; the catalog is the same in each.
const REGION: &str = "us-east-1";

/// The megabits in a megabyte, which is all that separates the `Mbps` figures of
/// `EbsOptimizedInfo` from its `MB/s` ones.
const MEGABITS_PER_MEGABYTE: f64 = 8.0;

/// The body `DescribeInstanceTypes` answers for exactly one instance type.
async fn described(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstanceTypes",
        &indexed("InstanceType", &[instance_type]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {instance_type}, got {status} with body {body}"
    );
    body
}

/// One member of a reported type, or a panic naming the case when it is absent.
fn required(case: &str, xml: &str, name: &str) -> String {
    element(xml, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> reported, got {xml}"))
}

/// One member of a reported type as a number.
fn number(case: &str, xml: &str, name: &str) -> f64 {
    let text = required(case, xml, name);
    text.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <{name}> must be a number, got {text:?}"))
}

/// The entries of a reported list member, each as its text.
fn list(case: &str, xml: &str, name: &str) -> Vec<String> {
    elements(&required(case, xml, name), "item")
}

/// The published hardware figures of one instance type.
#[derive(Debug, Clone, Copy)]
struct Published {
    /// `m5.8xlarge` and the like.
    instance_type: &'static str,
    /// (sustained Mbps, sustained IOPS, Mbps it reaches, IOPS it reaches).
    ebs: (i64, i64, i64, i64),
    /// The EBS volumes it can hold attached.
    ebs_attachments: i64,
    /// Its network performance, as the published table writes it.
    network_performance: &'static str,
    /// The ENA queues each of its interfaces is given by default.
    ena_queues: i64,
    /// Its processor's maker and that processor's sustained frequency, in GHz.
    processor: (&'static str, f64),
}

/// Rule: the EBS, network and processor figures of these two families are the ones AWS
/// publishes for each type.
#[tokio::test]
async fn hardware_rows_of_the_general_purpose_and_inference_families_are_published_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: five sizes of the general purpose family — whose baseline bursts to one ceiling
    // below the `4xlarge` and is dedicated from it up, at a slope that CHANGES there — and all
    // four sizes of the accelerated one, whose narrowest size bursts to a ceiling of its own
    // and whose widest holds the whole host.
    let rows = [
        Published {
            instance_type: "m5.large",
            ebs: (650, 3_600, 4_750, 18_750),
            ebs_attachments: 27,
            network_performance: "Up to 10 Gigabit",
            ena_queues: 2,
            processor: ("Intel", 3.1),
        },
        Published {
            instance_type: "m5.4xlarge",
            ebs: (4_750, 18_750, 4_750, 18_750),
            ebs_attachments: 27,
            network_performance: "Up to 10 Gigabit",
            ena_queues: 8,
            processor: ("Intel", 3.1),
        },
        Published {
            instance_type: "m5.8xlarge",
            ebs: (6_800, 30_000, 6_800, 30_000),
            ebs_attachments: 27,
            network_performance: "10 Gigabit",
            ena_queues: 8,
            processor: ("Intel", 3.1),
        },
        Published {
            instance_type: "m5.24xlarge",
            ebs: (19_000, 80_000, 19_000, 80_000),
            ebs_attachments: 27,
            network_performance: "25 Gigabit",
            ena_queues: 8,
            processor: ("Intel", 3.1),
        },
        Published {
            instance_type: "m5.metal",
            ebs: (19_000, 80_000, 19_000, 80_000),
            ebs_attachments: 31,
            network_performance: "25 Gigabit",
            ena_queues: 8,
            processor: ("Intel", 3.1),
        },
        Published {
            instance_type: "inf2.xlarge",
            ebs: (1_250, 6_000, 10_000, 40_000),
            ebs_attachments: 26,
            network_performance: "Up to 15 Gigabit",
            ena_queues: 4,
            processor: ("AMD", 3.6),
        },
        Published {
            instance_type: "inf2.8xlarge",
            ebs: (10_000, 40_000, 10_000, 40_000),
            ebs_attachments: 26,
            network_performance: "Up to 25 Gigabit",
            ena_queues: 8,
            processor: ("AMD", 3.6),
        },
        Published {
            instance_type: "inf2.24xlarge",
            ebs: (30_000, 120_000, 30_000, 120_000),
            ebs_attachments: 27,
            network_performance: "50 Gigabit",
            ena_queues: 8,
            processor: ("AMD", 3.6),
        },
        Published {
            instance_type: "inf2.48xlarge",
            ebs: (60_000, 240_000, 60_000, 240_000),
            ebs_attachments: 27,
            network_performance: "100 Gigabit",
            ena_queues: 8,
            processor: ("AMD", 3.6),
        },
    ];

    for row in rows {
        let case = row.instance_type;
        let body = described(case, &router, row.instance_type).await;
        let ebs_info = required(case, &body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        let (baseline_mbps, baseline_iops, maximum_mbps, maximum_iops) = row.ebs;
        for (name, expected) in [
            ("baselineBandwidthInMbps", baseline_mbps),
            ("baselineIops", baseline_iops),
            ("maximumBandwidthInMbps", maximum_mbps),
            ("maximumIops", maximum_iops),
        ] {
            let reported = number(case, &performance, name) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }
        // The throughput members are the bandwidth ones in the other unit, which is how the
        // published table's Mbps and MB/s columns relate.
        for (name, bandwidth) in [
            ("baselineThroughputInMBps", baseline_mbps),
            ("maximumThroughputInMBps", maximum_mbps),
        ] {
            let expected = bandwidth as f64 / MEGABITS_PER_MEGABYTE;
            let reported = number(case, &performance, name);
            assert!(
                (reported - expected).abs() < 0.001,
                "case {case:?}: expected {name} {expected}, got {reported}"
            );
        }

        let reported = number(case, &ebs_info, "maximumEbsAttachments") as i64;
        assert_eq!(
            reported, row.ebs_attachments,
            "case {case:?}: expected maximumEbsAttachments {}, got {reported}",
            row.ebs_attachments
        );

        // Neither family carries a local instance store at any size, so neither describes one.
        let storage = element(&body, "instanceStorageInfo");
        assert!(
            storage.is_none(),
            "case {case:?}: a type with no local devices reports no instanceStorageInfo, got \
             {storage:?}"
        );

        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "networkPerformance");
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );
        // The ONE card of each of these types carries the instance's whole network, so it
        // reports the same text and the queues one of its interfaces is given.
        let cards = required(case, &network_info, "networkCards");
        let card = required(case, &cards, "item");
        let reported = required(case, &card, "networkPerformance");
        assert_eq!(
            reported, row.network_performance,
            "case {case:?}: expected the card's networkPerformance {:?}, got {reported:?}",
            row.network_performance
        );
        let reported = number(case, &card, "defaultEnaQueueCountPerInterface") as i64;
        assert_eq!(
            reported, row.ena_queues,
            "case {case:?}: expected defaultEnaQueueCountPerInterface {}, got {reported}",
            row.ena_queues
        );

        let processor_info = required(case, &body, "processorInfo");
        let (manufacturer, sustained_ghz) = row.processor;
        let reported = required(case, &processor_info, "manufacturer");
        assert_eq!(
            reported, manufacturer,
            "case {case:?}: expected processor manufacturer {manufacturer:?}, got {reported:?}"
        );
        let reported = number(case, &processor_info, "sustainedClockSpeedInGhz");
        assert!(
            (reported - sustained_ghz).abs() < 0.001,
            "case {case:?}: expected sustainedClockSpeedInGhz {sustained_ghz}, got {reported}"
        );
    }
}

/// Rule: the accelerators a type carries are published per type, and a type that carries them
/// describes them BOTH ways — as Inference accelerators and as Neuron devices — while a type
/// that carries none describes neither.
#[tokio::test]
async fn accelerator_inventories_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // Rows: the four sizes of the accelerated family, whose device count stays at one over two
    // steps of the ladder and then jumps, beside two general purpose sizes that carry none.
    for (instance_type, devices) in [
        ("inf2.xlarge", Some(1_i64)),
        ("inf2.8xlarge", Some(1)),
        ("inf2.24xlarge", Some(6)),
        ("inf2.48xlarge", Some(12)),
        ("m5.large", None),
        ("m5.24xlarge", None),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let accelerators = element(&body, "inferenceAcceleratorInfo");
        let neurons = element(&body, "neuronInfo");
        let Some(devices) = devices else {
            assert!(
                accelerators.is_none() && neurons.is_none(),
                "case {case:?}: a type with no accelerators describes none, got \
                 {accelerators:?} and {neurons:?}"
            );
            continue;
        };
        let accelerators = accelerators
            .unwrap_or_else(|| panic!("case {case:?}: expected inferenceAcceleratorInfo"));
        let neurons = neurons.unwrap_or_else(|| panic!("case {case:?}: expected neuronInfo"));
        // One device description and a count, the memory of one device being 32 GiB and the
        // total being that memory times the count. The two inventories wrap their entries in
        // the element each list's own member declares.
        let memory_each = 32_768_i64;
        for (inventory, count_path, entry, total) in [
            (
                &accelerators,
                "accelerators",
                "member",
                "totalInferenceMemoryInMiB",
            ),
            (
                &neurons,
                "neuronDevices",
                "item",
                "totalNeuronDeviceMemoryInMiB",
            ),
        ] {
            let device = required(case, &required(case, inventory, count_path), entry);
            let reported = number(case, &device, "count") as i64;
            assert_eq!(
                reported, devices,
                "case {case:?}: expected {devices} devices in {count_path}, got {reported}"
            );
            let reported = number(case, &device, "sizeInMiB") as i64;
            assert_eq!(
                reported, memory_each,
                "case {case:?}: expected devices of {memory_each} MiB, got {reported}"
            );
            let reported = number(case, inventory, total) as i64;
            assert_eq!(
                reported,
                devices * memory_each,
                "case {case:?}: expected {total} {}, got {reported}",
                devices * memory_each
            );
        }
        let reported = required(case, &accelerators, "name");
        assert_eq!(
            reported, "Inferentia2",
            "case {case:?}: expected the published accelerator name, got {reported:?}"
        );
        // The Neuron inventory carries the half the Inference one does not: the NeuronCores one
        // device holds and the version of those cores.
        let core_info = required(case, &neurons, "coreInfo");
        for (name, expected) in [("count", 2_i64), ("version", 2)] {
            let reported = number(case, &core_info, name) as i64;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the cores' {name} {expected}, got {reported}"
            );
        }
    }
}

/// Rule: the capabilities and host features a type is offered with are published per type, so
/// neither its hypervisor nor the generation number in its name answers for it.
#[tokio::test]
async fn capabilities_and_host_features_are_published_per_type() {
    let (router, _engine) = super::ec2_query::emulator();

    // For each member, one type that has the capability beside one that does not: the
    // accelerated family is offered NO Elastic Fabric Adapter at any size while its adapter
    // encrypts traffic between instances, and the general purpose family answers the opposite
    // for both.
    let network_rows = [
        ("inf2.xlarge", "efaSupported", "false"),
        ("inf2.24xlarge", "efaSupported", "false"),
        ("inf2.48xlarge", "efaSupported", "false"),
        ("m5.large", "efaSupported", "false"),
        ("inf2.24xlarge", "encryptionInTransitSupported", "true"),
        ("inf2.48xlarge", "encryptionInTransitSupported", "true"),
        ("m5.8xlarge", "encryptionInTransitSupported", "false"),
        ("m5.metal", "encryptionInTransitSupported", "false"),
        ("m5.8xlarge", "enaSrdSupported", "false"),
        ("m5.8xlarge", "flexibleEnaQueuesSupport", "unsupported"),
        ("m5.8xlarge", "secondaryNetworkSupported", "false"),
        ("m5.8xlarge", "maximumNetworkCards", "1"),
        ("m5.8xlarge", "defaultNetworkCardIndex", "0"),
        ("m5.8xlarge", "trafficMirroringSupport", "supported"),
        ("m5.metal", "trafficMirroringSupport", "supported"),
        ("inf2.24xlarge", "trafficMirroringSupport", "supported"),
    ];

    for (instance_type, member, expected) in network_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let network_info = required(&case, &body, "networkInfo");
        let reported = required(&case, &network_info, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The host features, each a flag a client branches on: whether the family is still the
    // latest generation of its family, whether an instance is migrated on a user-initiated
    // reboot, whether it carries a local clock, whether action based recovery is offered and
    // whether a Dedicated Host carries the size.
    let feature_rows = [
        ("m5.large", "currentGeneration", "false"),
        ("m5.8xlarge", "currentGeneration", "false"),
        ("m5.metal", "currentGeneration", "false"),
        ("inf2.24xlarge", "currentGeneration", "true"),
        ("m5.8xlarge", "rebootMigrationSupport", "supported"),
        ("m5.metal", "rebootMigrationSupport", "supported"),
        ("m5.8xlarge", "phcSupport", "unsupported"),
        ("inf2.24xlarge", "phcSupport", "unsupported"),
        ("m5.8xlarge", "autoRecoverySupported", "true"),
        ("m5.metal", "autoRecoverySupported", "true"),
        ("inf2.xlarge", "autoRecoverySupported", "false"),
        ("inf2.48xlarge", "autoRecoverySupported", "false"),
        ("m5.large", "dedicatedHostsSupported", "true"),
        ("m5.metal", "dedicatedHostsSupported", "true"),
        ("inf2.xlarge", "dedicatedHostsSupported", "false"),
        ("inf2.8xlarge", "dedicatedHostsSupported", "false"),
        ("inf2.24xlarge", "dedicatedHostsSupported", "true"),
        ("inf2.48xlarge", "dedicatedHostsSupported", "true"),
        ("m5.8xlarge", "cpuBurstModel", "none"),
        ("inf2.24xlarge", "cpuBurstModel", "none"),
    ];

    for (instance_type, member, expected) in feature_rows {
        let case = format!("{instance_type} {member}");
        let body = described(&case, &router, instance_type).await;
        let reported = required(&case, &body, member);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {member} {expected:?}, got {reported:?}"
        );
    }

    // A previous generation family still offers IPv6 addresses where AWS names it beside the
    // current ones, so the two facts are read apart rather than off one flag.
    for instance_type in ["m5.large", "m5.8xlarge", "m5.metal"] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let network_info = required(case, &body, "networkInfo");
        let reported = required(case, &network_info, "ipv6Supported");
        assert_eq!(
            reported, "true",
            "case {case:?}: expected ipv6Supported true, got {reported:?}"
        );
    }

    // The boot modes a bare metal size of this generation is offered are the legacy BIOS mode
    // alone, where every virtualized size of the same family is offered both.
    for (instance_type, expected) in [
        ("m5.metal", vec!["legacy-bios"]),
        ("m5.8xlarge", vec!["legacy-bios", "uefi"]),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let reported = list(case, &body, "supportedBootModes");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected supportedBootModes {expected:?}, got {reported:?}"
        );
    }
}

/// Rule, the sharp edge: the published figures are NOT a function of the vCPUs and memory the
/// name gives. One size of two families disagrees on every figure, and the core counts a
/// caller may configure are the published list rather than every count up to the default.
#[tokio::test]
async fn the_published_rows_are_not_a_function_of_the_size() {
    let (router, _engine) = super::ec2_query::emulator();

    let ebs_pair = |body: &str, case: &str| -> (i64, i64) {
        let ebs_info = required(case, body, "ebsInfo");
        let performance = required(case, &ebs_info, "ebsOptimizedInfo");
        (
            number(case, &performance, "baselineBandwidthInMbps") as i64,
            number(case, &performance, "maximumBandwidthInMbps") as i64,
        )
    };

    // Two general purpose families two generations apart, ONE size — 32 vCPUs each —
    // disagreeing on the EBS figures, on the KIND of their EBS attachment limit and on whether
    // the family is still the latest generation of its family, which no function of the size
    // produces.
    let left = described("m5.8xlarge", &router, "m5.8xlarge").await;
    let right = described("m7i.8xlarge", &router, "m7i.8xlarge").await;
    for (case, body) in [("m5.8xlarge", &left), ("m7i.8xlarge", &right)] {
        let vcpus = number(case, body, "defaultVCpus") as i64;
        assert_eq!(
            vcpus, 32,
            "case {case:?}: both rows must be the same size, got {vcpus} vCPUs"
        );
    }
    assert_ne!(
        ebs_pair(&left, "m5.8xlarge"),
        ebs_pair(&right, "m7i.8xlarge"),
        "two families of one size publish different EBS figures"
    );
    assert_ne!(
        required(
            "m5.8xlarge",
            &required("m5.8xlarge", &left, "ebsInfo"),
            "attachmentLimitType"
        ),
        required(
            "m7i.8xlarge",
            &required("m7i.8xlarge", &right, "ebsInfo"),
            "attachmentLimitType"
        ),
        "two families of one size publish different kinds of EBS attachment limit"
    );
    assert_eq!(
        required("m7i.8xlarge", &right, "currentGeneration"),
        "true",
        "the family two generations above is still the latest of its family"
    );

    // The core counts a caller may configure: the narrowest size of each family takes its
    // cores one at a time, every size of the general purpose family between it and the largest
    // virtualized one two at a time, and the sizes that publish their default alone take them
    // all at once — while the bare metal size publishes every count. Four different lists over
    // two ladders.
    for (instance_type, expected) in [
        ("inf2.xlarge", vec![1_i64, 2]),
        ("inf2.8xlarge", vec![16]),
        ("inf2.48xlarge", vec![96]),
        ("m5.large", vec![1]),
        ("m5.4xlarge", (1..=4).map(|step| step * 2).collect()),
        ("m5.24xlarge", vec![48]),
        ("m5.metal", (1..=48).collect()),
    ] {
        let case = instance_type;
        let body = described(case, &router, instance_type).await;
        let v_cpu_info = required(case, &body, "vCpuInfo");
        let reported: Vec<i64> = list(case, &v_cpu_info, "validCores")
            .iter()
            .map(|core| {
                core.parse()
                    .unwrap_or_else(|_| panic!("case {case:?}: a core count, got {core:?}"))
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected validCores {expected:?}, got {reported:?}"
        );
    }
}
