Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-c9gd-m6gd ec2-catalog-describe-instance-types-m6i-r4 ec2-catalog-describe-instance-types-r9g-z1d ec2-catalog-describe-fpga-images
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.2xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.4xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.4xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.8xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.8xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.large"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.large"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c4.xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.12xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.12xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.18xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.18xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.24xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.24xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.2xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.4xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.4xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.9xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.9xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.large"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.large"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.metal"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.metal"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c5.xlarge"].networkInfo.ipv6Supported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i4g.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.small"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.small"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.small"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m1.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m2.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.2xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.2xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.large"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.large"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.medium"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.medium"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m3.xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r3.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.112xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.112xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.112xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.112xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.56xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.56xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.56xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="u-6tb1.56xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeFpgaImages|2xx->2xx|$.DescribeFpgaImagesResponse.fpgaImageSet.item[*]
Issue-Related: 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md
Issue-Related: 20261112T211315Z-ec2-describeinstancetypes-catalog-pages-serve-synthesized-per-type.md
Issue-Related: 20261114T013533Z-ec2-describeinstancetypes-serves-size-derived-hardware-facts-across.md
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# EC2 instance-type and FPGA-image catalog reads serve formula-derived rows instead of AWS's catalog

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Nine net-new clusters with one shared fault: EC2's catalog read paths answer 200 but the rows are computed from a size/family formula (or invented outright) instead of carrying AWS's published facts, and one modeled LIST is serialized as a scalar. The same fault recurs across roughly 130 replayed DescribeInstanceTypes steps in the ec2-catalog-describe-instance-types-* fixtures; the eight DescribeInstanceTypes steps listed here (plus the FPGA-image step) are the clusters this pass names, and the per-row facts below are what must be served.

=== Cause 1 of 9: networkCards facts are formula-derived and defaultEnaQueueCountPerInterface is never emitted (a1-c9g page, c4 family) ===
AWS: the model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares NetworkCardInfo.BaselineBandwidthInGbps (double, xmlName baselineBandwidthInGbps), PeakBandwidthInGbps, DefaultEnaQueueCountPerInterface (integer, "The default number of the ENA queues for each interface"), MaximumEnaQueueCount(PerInterface) and InterfaceTypes; NetworkInfo carries ipv6Supported and ipv6AddressesPerInterface. Each value is a per-instance-type published fact (e.g. c4.large baseline 0.625 / peak 1.2 Gbps, 8 default ENA queues, ipv6Supported true with 10 IPv6 addresses per interface).
Emulator: baseline/peak bandwidth are derived from the type's size (c4.large reports baseline 1.56 and peak 10.0), defaultEnaQueueCountPerInterface is omitted on every row, and ipv6Supported is 'false' with ipv6AddressesPerInterface '0' for families that do support IPv6.
Why: capacity planners, ENA/IPv6 preflight checks and cost/size selection logic read these fields; formula values and a missing member make every such decision wrong for the whole family, and an IPv6 preflight concludes the type cannot take IPv6 addresses.
Fix: the instance-type catalog data behind the read path - services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instance_types.rs and the catalog table it projects: carry per-type recorded facts instead of deriving them.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#4.

=== Cause 2 of 9: same fault on the c5 family page, including ebsInfo.maximumEbsAttachments and trafficMirroringSupport ===
AWS: per-type published facts (c5.18xlarge baseline/peak 25.0 Gbps, 8 default ENA queues; c5.metal has its own maximumEbsAttachments and trafficMirroringSupport values; ipv6Supported true with 15-50 addresses per interface by size).
Emulator: formula-derived bandwidths (c5.18xlarge baseline 18.75), missing defaultEnaQueueCountPerInterface, ipv6Supported false / ipv6AddressesPerInterface 0, and wrong ebsInfo.maximumEbsAttachments and networkInfo.trafficMirroringSupport on c5.metal.
Why: as cause 1, plus EBS-attachment limits that a client uses to decide how many volumes it may attach.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#5.

=== Cause 3 of 9: i4g rows also drop a supported interface type ===
AWS: i4g rows carry per-type bandwidths (i4g.large baseline 0.781, i4g.16xlarge 37.5) and networkCards.interfaceTypeSet includes 'efa-only' on i4g.16xlarge.
Emulator: formula bandwidths (0.78 / 25.0), defaultEnaQueueCountPerInterface omitted, and interfaceTypeSet.item[2] 'efa-only' missing.
Why: an EFA-capable type is reported as not supporting efa-only interfaces, so HPC placement logic rejects it.
Fix: as cause 1 (the catalog's interfaceTypes list per network card).
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#21.

=== Cause 4 of 9: legacy m1 rows ===
AWS: m1.small baseline 0.3 / peak 1.2 Gbps with 1 default ENA queue; m1.large 0.7/2.8 with 2; m1.xlarge 1.0 with 4.
Emulator: 3.12/10.0, 12.5, 25.0 etc. from the size formula, and no defaultEnaQueueCountPerInterface.
Why: as cause 1.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#31.

=== Cause 5 of 9: legacy m2 rows ===
AWS: m2.xlarge 0.3/1.2 Gbps, m2.2xlarge 0.7/2.8, m2.4xlarge 1.0/10.0, each with 1 default ENA queue.
Emulator: 6.25/10.0, 12.5/12.5, 25.0/25.0 and the member omitted.
Why: as cause 1.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#32.

=== Cause 6 of 9: m3 rows also invert trafficMirroringSupport and rebootMigrationSupport ===
AWS: m3.* bandwidths 0.7/2.8 (large) and 1.0/10.0 (xlarge), with networkInfo.trafficMirroringSupport 'unsupported' and rebootMigrationSupport 'unsupported'.
Emulator: formula bandwidths and BOTH support flags reported 'supported'.
Why: a client that gates traffic mirroring or reboot migration on these flags attempts an operation the type cannot perform.
Fix: as cause 1 (per-type support flags, not family defaults).
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#33.

=== Cause 7 of 9: r3 rows ===
AWS: r3.large 0.5/1.2 Gbps, r3.4xlarge 2.0/10.0, r3.8xlarge 5.0/10.0, r3.2xlarge 1.0, each with 8 default ENA queues.
Emulator: 1.56/10.0, 12.5, 25.0, 6.25 and the member omitted.
Why: as cause 1.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-m6i-r4#task#43.

=== Cause 8 of 9: u-6tb1 rows, and vCpuInfo.validCores serialized as a scalar instead of a list ===
AWS: the model declares VCpuInfo.ValidCores as a LIST (target CoreCountList, xmlName validCores), so AWS emits one <item> per valid core count (16, 24, 32, ... 224 for u-6tb1.112xlarge); the type's network card is 100.0 Gbps baseline and peak with 32 default ENA queues.
Emulator: validCores.item is a single scalar value ('224') rather than the full repeated list, and the bandwidths are the formula values (25.0 / 12.5).
Why: the scalar breaks the shape itself - an SDK deserializing CoreCountList gets one element (or a type error) instead of the enumerable set, so CPU-options validation cannot tell which core counts are selectable.
Fix: the serializer for VCpuInfo.validCores in the instance-type projection (services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instance_types.rs) must emit the repeated <item> list, and the catalog must hold the full set.
Traces: ec2-catalog-describe-instance-types-r9g-z1d#task#12.

=== Cause 9 of 9: DescribeFpgaImages serves an invented AFI catalog ===
AWS: recorded behavior only - the amazon-owned AFI set AWS returns consists of specific published images (e.g. fpgaImageId afi-02cc6c89c7f0ae2cc / fpgaImageGlobalId agfi-0925b211f5a81b071, name cl_sde_0x10212415, shellVersion 0x10212415, ownerId 095707098027, with its own createTime/updateTime and pciId).
Emulator: returns a wholly different, emulator-authored list (afi-0c2b7c9d4e5f6a8b1 'cl_hello_world' with shellVersion 0x04261818, ownerId 137112412989, createTime/updateTime fixed at '2025-11-20T00:00:00Z', etc.), so every item in the set diverges.
Why: a client enumerating the AFI catalog and then using an id from it operates on images that do not exist (the same contradiction the behavioral issue 20261119T232925Z-ec2-4-net-new-causes-applysecuritygroupstoclientvpntargetnetwork-invents-an.md records for ModifyFpgaImageAttribute, which carries no wire replay key for this step).
Fix: the FPGA-image read path and its seed data - services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_fpga_images.rs.
Traces: ec2-catalog-describe-fpga-images#task#1.

Related open issues that do NOT carry these replay keys: 20261117T065015Z-ec2-describeinstancetypes-serves-synthesized-instance-type-catalog-rows.md, 20261117T072456Z-ec2-describeinstancetypes-a1-c9g-page-rows-carry-formula.md, 20261117T164306Z-ec2-describeinstancetypes-serves-formula-derived-instance-type-rows.md.

## Evidence
All nine clusters answered 200 where 200 was expected; the divergence is in the row data.

Cause 1 - ec2-catalog-describe-instance-types-a1-c9g#task#4 (DescribeInstanceTypes):
  item[instanceType="c4.large"].networkInfo.ipv6AddressesPerInterface: expected '10', got '0'; ipv6Supported: expected 'true', got 'false'
  ...networkCards.item.baselineBandwidthInGbps: expected '0.625', got '1.56'; peakBandwidthInGbps: expected '1.2', got '10.0'; defaultEnaQueueCountPerInterface: expected '8', got None
  (same pattern on c4.xlarge/2xlarge/4xlarge/8xlarge)

Cause 2 - a1-c9g#task#5: c5.18xlarge baselineBandwidthInGbps expected '25.0', got '18.75'; c5.2xlarge expected '2.5', got '2.08'; defaultEnaQueueCountPerInterface missing on every c5 row; ipv6Supported expected 'true', got 'false'; c5.metal ebsInfo.maximumEbsAttachments and networkInfo.trafficMirroringSupport diverge.

Cause 3 - c9gd-m6gd#task#21: i4g.large baseline expected '0.781', got '0.78'; i4g.4xlarge expected '9.375', got '6.25'; i4g.16xlarge expected '37.5', got '25.0'; i4g.16xlarge networkCards.item.interfaceTypeSet.item[2] expected 'efa-only', got None; defaultEnaQueueCountPerInterface missing on every row.

Cause 4 - c9gd-m6gd#task#31: m1.small baseline expected '0.3', got '3.12'; m1.medium expected '0.3', got '6.25'; m1.large expected '0.7', got '12.5'; m1.xlarge expected '1.0', got '25.0'; peak expected 1.2/1.2/2.8 got 10.0/10.0/12.5; defaultEnaQueueCountPerInterface expected 1/1/2/4, got None.

Cause 5 - c9gd-m6gd#task#32: m2.xlarge expected 0.3/1.2, got 6.25/10.0; m2.2xlarge expected 0.7/2.8, got 12.5/12.5; m2.4xlarge expected 1.0/10.0, got 25.0/25.0; defaultEnaQueueCountPerInterface expected '1', got None.

Cause 6 - c9gd-m6gd#task#33: m3.large/xlarge/2xlarge/medium networkInfo.trafficMirroringSupport expected 'unsupported', got 'supported'; rebootMigrationSupport expected 'unsupported', got 'supported'; baseline expected 0.7/1.0/1.0, got 6.25/12.5/25.0; defaultEnaQueueCountPerInterface expected 2/4, got None.

Cause 7 - m6i-r4#task#43: r3.large expected 0.5/1.2, got 1.56/10.0; r3.4xlarge expected 2.0/10.0, got 12.5/12.5; r3.8xlarge expected 5.0/10.0, got 25.0/25.0; r3.2xlarge expected 1.0, got 6.25; defaultEnaQueueCountPerInterface expected '8', got None.

Cause 8 - r9g-z1d#task#12: u-6tb1.112xlarge and u-6tb1.56xlarge vCpuInfo.validCores.item: type_mismatch - expected the list ['16','24',...,'224'], got the scalar '224'; baseline/peak expected '100.0', got '25.0' and '12.5'; defaultEnaQueueCountPerInterface expected '32', got None.

Cause 9 - ec2-catalog-describe-fpga-images#task#1: all 20 fpgaImageSet items diverge, e.g. item[0] expected {fpgaImageId afi-02cc6c89c7f0ae2cc, fpgaImageGlobalId agfi-0925b211f5a81b071, name cl_sde_0x10212415, shellVersion 0x10212415, ownerId 095707098027, pciId DeviceId 0xf002}, got {fpgaImageId afi-0c2b7c9d4e5f6a8b1, name cl_hello_world, shellVersion 0x04261818, ownerId 137112412989, createTime 2025-11-20T00:00:00Z}.

## Steps to Reproduce
Causes 1-8: call ec2 DescribeInstanceTypes for the families each cluster pages (c4, c5, i4g, m1, m2, m3, r3, u-6tb1) - e.g. DescribeInstanceTypes(InstanceTypes=["c4.large"]) - and compare networkInfo.ipv6Supported/ipv6AddressesPerInterface, networkInfo.networkCards[].baselineBandwidthInGbps/peakBandwidthInGbps/defaultEnaQueueCountPerInterface/interfaceTypes, ebsInfo.maximumEbsAttachments, networkInfo.trafficMirroringSupport, rebootMigrationSupport, freeTierEligible and vCpuInfo.validCores against the recorded rows: the bandwidths track instance size rather than the published per-type values, defaultEnaQueueCountPerInterface is never emitted, IPv6 support is reported absent, and validCores comes back as a single value instead of a list.
Cause 9: ec2 DescribeFpgaImages (amazon-owned images) and compare the returned fpgaImageSet with the recorded catalog: every item differs.
