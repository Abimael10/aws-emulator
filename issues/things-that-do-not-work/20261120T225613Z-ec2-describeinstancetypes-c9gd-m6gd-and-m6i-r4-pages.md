Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-c9gd-m6gd
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-c9gd-m6gd ec2-catalog-describe-instance-types-m6i-r4
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="d2.2xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="d2.4xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="d2.8xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="d2.xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.4xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.4xlarge"].networkInfo.ipv4AddressesPerSecondaryInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.4xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.4xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].networkInfo.ipv4AddressesPerSecondaryInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="gr6.8xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.32xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.32xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.32xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.32xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4i.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7gd.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Related: 20261114T013533Z-ec2-describeinstancetypes-serves-size-derived-hardware-facts-across.md
Issue-Related: 20261120T184451Z-ec2-describeinstancetypes-rows-for-the-c6in-c9g-d2.md
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# ec2 DescribeInstanceTypes c9gd-m6gd and m6i-r4 pages: d2, gr6, i4i and m7gd rows derive hardware facts

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Same defect as the a1-c9g bundle, on four further net-new clusters of the c9gd-m6gd and m6i-r4 catalog pages: DescribeInstanceTypes answers 200 with per-type rows whose facts are computed from the type name (crate::state::instance_type_info derives every member from family+size, per the handler's own doc) rather than served from the published catalog row.

AWS (all causes): Smithy com.amazonaws.ec2 — DescribeInstanceTypes returns InstanceTypeInfoList (xmlName instanceTypeSet), no modeled errors; NetworkCardInfo.DefaultEnaQueueCountPerInterface (integer), BaselineBandwidthInGbps / PeakBandwidthInGbps (double), InterfaceTypes (list, xmlName interfaceTypeSet); ProcessorInfo.SustainedClockSpeedInGhz (double); GpuDeviceInfo's workloadSet and NetworkInfo.ipv4AddressesPerSecondaryInterface are likewise always emitted for a type that has them. The per-type VALUES are recorded behavior only — the catalog facts the recorded AWS responses carry; AWS serves the same row to every caller.

Emulator (all causes): a 200 carrying the name-derived values quoted below, with defaultEnaQueueCountPerInterface and other always-present members absent and multi-valued members collapsed to one entry.

Why (all causes): clients size ENA queues, EFA support, network bandwidth and instance-store capacity from this call. Against this emulator they read fabricated bandwidth, a missing ENA queue count, an absent 'efa-only' interface type and a wrong rebootMigrationSupport/clock speed, so capacity planning and type selection built on AWS's catalog silently pick the wrong type.

Fix (all causes): crate::state::instance_type_info in the ec2 crate — the catalog the DescribeInstanceTypes handler reads — must serve recorded per-type rows (including the omitted members and full lists) instead of deriving them from the type name.

Cause 1 of 4 — d2.* rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#2): d2.xlarge networkCards.baselineBandwidthInGbps 1.25 -> 3.12, peakBandwidthInGbps 2.8 -> 10.0, defaultEnaQueueCountPerInterface 8 omitted, rebootMigrationSupport 'unsupported' -> 'supported'; d2.4xlarge 5.0 -> 12.5 baseline and 10.0 -> 12.5 peak; d2.8xlarge 10.0 -> 25.0 baseline and 10.0 -> 25.0 peak; d2.2xlarge diverges the same way.

Cause 2 of 4 — gr6.* rows (step task#15): gr6.4xlarge gpuInfo.gpus.workloadSet entry 'graphics' omitted, networkInfo.ipv4AddressesPerSecondaryInterface 0 omitted, networkCards.baselineBandwidthInGbps 10.0 -> 12.5, peakBandwidthInGbps 25.0 -> 12.5, defaultEnaQueueCountPerInterface 8 omitted, processorInfo.sustainedClockSpeedInGhz 3.4 -> 3.6, rebootMigrationSupport 'unsupported' -> 'supported'; gr6.8xlarge the same plus interfaceTypeSet entry 'efa-only' omitted and vCpuInfo.validCores collapsed.

Cause 3 of 4 — i4i.* rows (step task#22): i4i.metal networkCards.baselineBandwidthInGbps 75.0 -> 25.0 and peakBandwidthInGbps 75.0 -> 25.0, defaultEnaQueueCountPerInterface 8 omitted, interfaceTypeSet entry 'efa-only' omitted; i4i.8xlarge 18.75 -> 6.25 baseline and 18.75 -> 10.0 peak; i4i.xlarge 1.875 -> 0.78; i4i.32xlarge 75.0 -> 25.0; every i4i size omits defaultEnaQueueCountPerInterface.

Cause 4 of 4 — m7gd.* rows (fixture ec2-catalog-describe-instance-types-m6i-r4, step task#7): m7gd.medium networkCards.baselineBandwidthInGbps 0.52 -> 0.39, peakBandwidthInGbps 12.5 -> 10.0, defaultEnaQueueCountPerInterface 1 omitted; m7gd.8xlarge 15.0 -> 12.5 baseline and peak; m7gd.large 0.937 -> 0.78; m7gd.xlarge ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps 156.25039999999998 -> 156.25 and baselineBandwidthInGbps 1.876 -> 1.56; m7gd.16xlarge/metal omit the interfaceTypeSet 'efa-only' entry.

Related open issues that do NOT carry these clusters' replay keys (the replay reports no owner for all four): 20261108T044413Z-ec2-describeinstancetypes-per-type-attributes-still-diverge-on-the.md, 20261111T015735Z-ec2-describeinstancetypes-catalog-pages-c9gd-m6gd-and-m6i.md, 20261120T184451Z-ec2-describeinstancetypes-rows-for-the-c6in-c9g-d2.md.

## Evidence
Replay of trace set simparity-wire-aws-v1; 4 unowned clusters, verdict=mismatch, expected_status 200 / actual_status 200.

Representative (ec2-catalog-describe-instance-types-c9gd-m6gd#task#2):
  item[instanceType="d2.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '1.25', got '3.12'
  item[instanceType="d2.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '8', got None)
  item[instanceType="d2.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps: expected '2.8', got '10.0'
  item[instanceType="d2.xlarge"].rebootMigrationSupport: expected 'unsupported', got 'supported'
  item[instanceType="d2.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '10.0', got '25.0'

Other bundled causes:
  c9gd-m6gd#task#15 (gr6): gpuInfo.gpus.item.workloadSet.item[0] expected 'graphics' got None; networkInfo.ipv4AddressesPerSecondaryInterface expected '0' got None; baselineBandwidthInGbps expected '10.0' got '12.5'; peakBandwidthInGbps expected '25.0' got '12.5'; defaultEnaQueueCountPerInterface expected '8' got None; sustainedClockSpeedInGhz expected '3.4' got '3.6'; rebootMigrationSupport expected 'unsupported' got 'supported'
  c9gd-m6gd#task#22 (i4i): i4i.metal baseline/peak expected '75.0' got '25.0'; interfaceTypeSet.item[2] expected 'efa-only' got None; i4i.8xlarge baseline expected '18.75' got '6.25'; i4i.xlarge baseline expected '1.875' got '0.78'
  m6i-r4#task#7 (m7gd): m7gd.medium baseline expected '0.52' got '0.39', peak expected '12.5' got '10.0', defaultEnaQueueCountPerInterface expected '1' got None; m7gd.xlarge ebsOptimizedInfo.baselineThroughputInMBps expected '156.25039999999998' got '156.25'

Model grounding: services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json (NetworkCardInfo, VCpuInfo, ProcessorInfo as above). Emulator source: services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instance_types.rs lines 10-16.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture ec2-catalog-describe-instance-types-c9gd-m6gd.
2. At step task#2 (d2 sizes) compare the instanceTypeSet rows with the recording: networkCards baseline/peak bandwidth and rebootMigrationSupport diverge and defaultEnaQueueCountPerInterface is absent.
3. Repeat at step task#15 (gr6 — also missing gpuInfo workloadSet entry and ipv4AddressesPerSecondaryInterface) and task#22 (i4i — also missing the 'efa-only' interface type).
4. Replay fixture ec2-catalog-describe-instance-types-m6i-r4 step task#7 (m7gd) for the same classes of divergence.
