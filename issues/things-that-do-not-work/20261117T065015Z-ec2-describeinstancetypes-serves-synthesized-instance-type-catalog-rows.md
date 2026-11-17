Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-m6i-r4 ec2-catalog-describe-instance-types-r9g-z1d
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.12xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.16xlarge"].hibernationSupported
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.16xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.24xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.32xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.32xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.32xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.32xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.48xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.8xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6a.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c6g.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.metal"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.metal"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.xlarge"].ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c7g.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item.instanceStorageInfo.disks.item.count
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item.instanceStorageInfo.disks.item.sizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item.instanceStorageInfo.totalSizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item.supportedUsageClasses.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item.vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.32xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.32xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x2iedn.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Related: 20261030T063325Z-ec2-static-catalogs-diverge-from-aws-instance-type.md
Issue-Related: 20261030T124526Z-ec2-describeinstancetypes-serves-a-synthesized-instance-type-catalog.md
Issue-Related: 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md
Issue-Related: 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md
Issue-Commit: 688abb98d0ad3ae2c07107aa110c7330aa2e99f9

# ec2 DescribeInstanceTypes serves synthesized instance-type catalog rows for whole families

> _Note: this issue was identified against the code base at commit `688abb98d0ad3ae2c07107aa110c7330aa2e99f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Five net-new clusters, one root cause: DescribeInstanceTypes does not serve AWS's instance-type catalogue, it DERIVES each row from the type's name (vCPU count, size suffix), so almost every numeric and capability member is wrong and several modelled members are never emitted at all. The faults below recur identically across the remaining DescribeInstanceTypes clusters in this replay (the corpus reports 474 divergences in total, the great majority of them DescribeInstanceTypes rows across the ec2-catalog-describe-instance-types-* fixtures); the five clusters named here are the ones not already carried by an open issue. The open issue 20261116T190237Z-ec2-describeinstancetypes-serves-synthesized-network-card-and-vcpu.md describes the same family and 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md owns some rows, but neither carries these replay keys.

All of it is recorded behavior only for the VALUES — the Smithy model fixes the members and their wire names but not their per-type values. Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json, shape NetworkCardInfo): members BaselineBandwidthInGbps (xmlName baselineBandwidthInGbps, double), PeakBandwidthInGbps (peakBandwidthInGbps, double), DefaultEnaQueueCountPerInterface (defaultEnaQueueCountPerInterface, integer), MaximumEnaQueueCountPerInterface, InterfaceTypes (interfaceTypeSet, list). The requirement is that each member equal the value AWS publishes for that instance type; the specific numbers quoted below are examples from the recording.

Cause 1 of 5: the c6a family — vCpuInfo.validCores is enumerated as 1,2,3,...,N instead of AWS's valid core counts for the type, baseline/peak network bandwidth are computed by formula rather than taken from the catalogue, defaultEnaQueueCountPerInterface is never emitted, and hibernationSupported is wrong.
Emulator: for c6a.32xlarge it serves validCores 1..64 where AWS publishes the sparse list 8,12,16,20,24,28,32,64; baselineBandwidthInGbps/peakBandwidthInGbps 16.66 where AWS publishes 50.0/50.0; and no defaultEnaQueueCountPerInterface element where AWS emits 8.
Fix: the instance-type catalogue in the ec2 service crate's state (per-type rows), not the DescribeInstanceTypes serializer.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#10.

Cause 2 of 5: the c6g family — defaultEnaQueueCountPerInterface is missing on every row, and baselineBandwidthInGbps is a formula result rather than the published figure.
Emulator: no defaultEnaQueueCountPerInterface element on c6g.metal/16xlarge/12xlarge/8xlarge/4xlarge/2xlarge/xlarge/large/medium (AWS emits 8); baselineBandwidthInGbps 1.56 for c6g.xlarge (AWS 1.25), 0.39 for c6g.medium (AWS 0.5), 18.75 for c6g.12xlarge (AWS 20.0), 12.5 for c6g.8xlarge (AWS 12.0), with peakBandwidthInGbps diverging the same way.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#11.

Cause 3 of 5: the c7g family — in addition to the bandwidth and ENA-queue faults, networkCards.interfaceTypeSet omits a supported interface type and ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps is wrong.
Emulator: interfaceTypeSet has no third element where AWS lists 'efa-only' on c7g.16xlarge and c7g.metal; baselineBandwidthInGbps/peakBandwidthInGbps 25.0 where AWS publishes 30.0/30.0; 0.78/10.0 on c7g.large where AWS publishes 0.937/12.5; defaultEnaQueueCountPerInterface absent (AWS 8 on the large sizes, 2 on c7g.large).
Why an interfaceTypeSet gap matters: a caller choosing a type for an EFA-only placement filters on that list and rules the type out.
Fix: as cause 1, plus the per-type interfaceTypeSet and ebsOptimizedInfo rows.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#18.

Cause 4 of 5: an accelerated/high-memory row is wrong in nearly every dimension — gpuInfo is not emitted at all, memory, instance storage, EBS-optimized figures, maximumEbsAttachments, processorInfo.sustainedClockSpeedInGhz, nitroTpmSupport, rebootMigrationSupport and supportedUsageClasses all diverge.
Emulator: gpuInfo absent where AWS emits a gpus list (count, gpuPartitionSize, logicalGpuCount, manufacturer, memoryInfo.sizeInMiB, name, workloadSet) and totalGpuMemoryInMiB; memoryInfo.sizeInMiB 1179648 where AWS publishes 786432; instanceStorageInfo disks count 7 / sizeInGB 1714 / totalSizeInGB 11998 where AWS publishes 2 / 900 / 1800; ebsOptimizedInfo baseline+maximum bandwidth 20400 vs 19000, iops 90000 vs 80000, throughput 2550.0 vs 2375.0; maximumEbsAttachments 20 vs 17.
Why: a caller that selects an accelerated type by reading gpuInfo sees no GPU at all, and capacity planning against memory/storage figures is wrong by a wide margin.
Fix: as cause 1 — the per-type row must carry the real GPU, memory, storage and EBS figures instead of deriving them.
Traces: ec2-catalog-describe-instance-types-m6i-r4#task#35.

Cause 5 of 5: the x2iedn family — defaultEnaQueueCountPerInterface missing on every size, bandwidth figures derived, interfaceTypeSet short an element.
Emulator: no defaultEnaQueueCountPerInterface on x2iedn.xlarge/2xlarge/4xlarge/8xlarge/16xlarge/24xlarge/32xlarge/metal (AWS emits 4/8/8/8/16/16/.../32 by size); baselineBandwidthInGbps 6.25 vs 5.0 and peakBandwidthInGbps 10.0 vs 25.0 on x2iedn.2xlarge, 3.12 vs 1.875 and 10.0 vs 25.0 on x2iedn.xlarge, 12.5 vs 25.0 peak on x2iedn.4xlarge; interfaceTypeSet third element absent on x2iedn.32xlarge and x2iedn.metal.
Fix: as cause 1.
Traces: ec2-catalog-describe-instance-types-r9g-z1d#task#24.

Why, for all five: DescribeInstanceTypes is the catalogue every capacity, placement and cost decision is made from. Burst-vs-baseline bandwidth, valid core counts, ENA queue defaults, EFA support and GPU presence are all read programmatically; a client that sizes a workload, validates a CpuOptions core count, or filters for an accelerator against these rows reaches a different answer than it would against AWS, and nothing in the response signals that the data is synthesized.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 80.4%, 474 divergence clusters). Five mismatch clusters, service=ec2, operation DescribeInstanceTypes, status 200->200, empty stub_path, empty owned_by:

1. ec2-catalog-describe-instance-types-a1-c9g#task#10 — item[instanceType="c6a.32xlarge"]: networkInfo.networkCards.item.baselineBandwidthInGbps expected '50.0' got '16.66'; .defaultEnaQueueCountPerInterface expected '8' got None; .peakBandwidthInGbps expected '50.0' got '16.66'; vCpuInfo.validCores.item[0..7] expected 8,12,16,20,24,28,32,64 got 1,2,3,4,5,6,7,8, with items [8..] extra ('9', ...). Same keys for c6a.12xlarge/16xlarge/24xlarge/2xlarge/48xlarge, plus c6a.16xlarge hibernationSupported and c6a.48xlarge interfaceTypeSet.item[2].
2. ec2-catalog-describe-instance-types-a1-c9g#task#11 — c6g.*: defaultEnaQueueCountPerInterface expected '8' got None on metal/16xlarge/12xlarge/8xlarge/4xlarge/2xlarge/xlarge/large/medium; baselineBandwidthInGbps expected '1.25' got '1.56' (xlarge), '0.5' got '0.39' (medium), '20.0' got '18.75' (12xlarge), '12.0' got '12.5' (8xlarge); peakBandwidthInGbps likewise.
3. ec2-catalog-describe-instance-types-a1-c9g#task#18 — c7g.*: baselineBandwidthInGbps expected '30.0' got '25.0' (16xlarge, metal); interfaceTypeSet.item[2] expected 'efa-only' got None (16xlarge, metal); peakBandwidthInGbps expected '30.0' got '25.0'; c7g.large baseline expected '0.937' got '0.78', peak expected '12.5' got '10.0', defaultEnaQueueCountPerInterface expected '2' got None; c7g.xlarge ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps also diverges.
4. ec2-catalog-describe-instance-types-m6i-r4#task#35 — ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps expected '19000' got '20400'; .baselineIops expected '80000' got '90000'; .baselineThroughputInMBps expected '2375.0' got '2550.0'; .maximumBandwidthInMbps/.maximumIops/.maximumThroughputInMBps likewise; ebsInfo.maximumEbsAttachments expected '17' got '20'; gpuInfo missing (expected {'gpus': {'item': {'count': '8', 'gpuPartitionSize': '1.0', 'logicalGpuCount': '8', 'manufacturer': 'NVIDIA', 'memoryInfo': {'sizeInMiB': '32768'}, 'name': 'V100', 'workloadSet': {'item': 'ml-ai'}}}, 'totalGpuMemoryInMiB': '262144'}, got None); instanceStorageInfo.disks.item.count expected '2' got '7'; .sizeInGB expected '900' got '1714'; .totalSizeInGB expected '1800' got '11998'; memoryInfo.sizeInMiB expected '786432' got '1179648'; networkCards defaultEnaQueueCountPerInterface expected '32' got None; interfaceTypeSet.item[2], nitroTpmSupport, processorInfo.sustainedClockSpeedInGhz, rebootMigrationSupport, supportedUsageClasses.item[0..2] and vCpuInfo.validCores.item[0..47] all in the cluster key.
5. ec2-catalog-describe-instance-types-r9g-z1d#task#24 — x2iedn.*: defaultEnaQueueCountPerInterface expected '16' got None (16xlarge, 24xlarge), '8' got None (2xlarge, 4xlarge, 8xlarge), '4' got None (xlarge), '32' got None (metal); baselineBandwidthInGbps expected '5.0' got '6.25' (2xlarge), '1.875' got '3.12' (xlarge); peakBandwidthInGbps expected '25.0' got '10.0' (2xlarge, xlarge), '25.0' got '12.5' (4xlarge); interfaceTypeSet.item[2] on 32xlarge and metal.

Model grounding (smithy_query shape=NetworkCardInfo): NetworkCardIndex, NetworkPerformance, MaximumNetworkInterfaces, AdditionalFlexibleNetworkInterfaces, BaselineBandwidthInGbps (double), PeakBandwidthInGbps (double), DefaultEnaQueueCountPerInterface (integer, xmlName defaultEnaQueueCountPerInterface), MaximumEnaQueueCount, MaximumEnaQueueCountPerInterface, InterfaceTypes (xmlName interfaceTypeSet). The members are modelled; their per-type values are recorded behaviour.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Re-replay ec2-catalog-describe-instance-types-a1-c9g#task#10 and each step in trace_ids. Each is an ec2Query DescribeInstanceTypes POST; compare every instanceTypeSet item's networkInfo.networkCards (baselineBandwidthInGbps, peakBandwidthInGbps, defaultEnaQueueCountPerInterface, interfaceTypeSet), vCpuInfo.validCores, ebsInfo.ebsOptimizedInfo, memoryInfo, instanceStorageInfo and gpuInfo against the recorded response for the same instance type.
