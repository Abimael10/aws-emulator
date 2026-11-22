Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#27
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-c9gd-m6gd
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.48xlarge"].networkInfo.networkCards.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8gn.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].instanceStorageInfo.disks.item.sizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].instanceStorageInfo.totalSizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].networkInfo.ipv4AddressesPerSecondaryInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.12xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].instanceStorageInfo.disks.item.sizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].instanceStorageInfo.totalSizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].networkInfo.ipv4AddressesPerSecondaryInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.48xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].ebsInfo.ebsOptimizedInfo.maximumBandwidthInMbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].ebsInfo.ebsOptimizedInfo.maximumIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].ebsInfo.ebsOptimizedInfo.maximumThroughputInMBps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].instanceStorageInfo.disks.item.sizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].instanceStorageInfo.totalSizeInGB
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.ipv4AddressesPerSecondaryInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.networkCards.item.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="f2.6xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.12xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.12xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.16xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.24xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.2xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.48xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.48xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.4xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.8xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.8xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.16xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.2xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.4xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.8xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.metal"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g5g.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Related: 20261106T072955Z-ec2-describeinstancetypes-serves-an-emulator-authored-instance-type.md
Issue-Commit: cbbc12f10a0b2ca1aa413637dc076121229fbbb3

# ec2 DescribeInstanceTypes catalog rows are emulator-derived rather than EC2's published instance-type data, so family-specific hardware fields are wrong or absent

> _Note: this issue was identified against the code base at commit `cbbc12f10a0b2ca1aa413637dc076121229fbbb3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Four net-new DescribeInstanceTypes clusters, one per instance family, all showing the same defect: the emulator does not serve EC2's published instance-type catalog but synthesizes each InstanceTypeInfo row from a generic size-scaling formula. The result is that per-family hardware characteristics are computed instead of looked up, and members EC2 only emits for certain families are omitted entirely. Every one of these steps answers HTTP 200 on both sides — the divergence is purely in the projected catalog values.

Two open ec2 issues describe this same root cause for OTHER instance families and already carry their replay keys (20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md and 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md); the replay report reports owned_by: [] for the four clusters below, so they are not covered by either. The same root cause recurs across roughly a hundred further DescribeInstanceTypes clusters in this corpus; closing it properly means replacing the derivation with the real catalog, not patching the four families named here.

Cause 1 of 4: the c8gn family's EBS, network-card and PHC characteristics are derived, not catalogued.
  AWS: recorded behavior only (DescribeInstanceTypes has no modelled errors; InstanceTypeInfo members ebsInfo.attachmentLimitType, ebsInfo.ebsOptimizedInfo.*, ebsInfo.maximumEbsAttachments, networkInfo.connectionTrackingConfiguration.defaultTcpEstablishedTimeout, networkInfo.ipv4AddressesPerSecondaryInterface, networkInfo.maximumNetworkInterfaces, networkInfo.networkCards[].*, networkInfo.secondaryNetworkSupported, networkInfo.trafficMirroringSupport, phcSupport, processorInfo.sustainedClockSpeedInGhz and vCpuInfo.validCores are all fixed per-instance-type facts published by EC2). For c8gn sizes the recording reports attachmentLimitType 'dedicated', dedicated-EBS bandwidth/IOPS/throughput an order of magnitude above the shared-EBS baseline, a defaultTcpEstablishedTimeout of 350, ipv4AddressesPerSecondaryInterface 0, and a maximumNetworkInterfaces that does not follow the generic size curve.
  Emulator: reports attachmentLimitType 'shared', EBS baseline/maximum bandwidth, IOPS and throughput from its shared-EBS formula, defaultTcpEstablishedTimeout 432000 (the connection-tracking default rather than the family's value), ipv4AddressesPerSecondaryInterface scaled from the interface count, a lower maximumNetworkInterfaces, and the enumerated vCpuInfo.validCores list computed rather than catalogued.
  Why: capacity planners, Auto Scaling mixed-instances policies, attribute-based instance selection and EBS/ENI quota pre-checks all read these fields to choose an instance type. Derived values make the emulator recommend or admit types that cannot carry the workload (or reject types that can), and 'shared' vs 'dedicated' attachmentLimitType changes the EBS attachment accounting entirely.
  Fix: the DescribeInstanceTypes read path must project a real per-instance-type catalog (the ec2 instance-type catalog data source behind that handler) instead of deriving fields from the type's size suffix.
  Traces: ec2-catalog-describe-instance-types-a1-c9g#task#27.

Cause 2 of 4: the f2 family's instance-store size, clock speed, network bandwidth and valid-core list are derived.
  AWS: recorded behavior only. For f2 sizes the recording reports instanceStorageInfo.disks[].sizeInGB and instanceStorageInfo.totalSizeInGB at the catalogued values, processorInfo.sustainedClockSpeedInGhz as the family's published clock, networkInfo.networkCards[].baselineBandwidthInGbps / peakBandwidthInGbps as the published per-card figures, networkInfo.ipv4AddressesPerSecondaryInterface present (0 for this family), networkInfo.networkCards[].defaultEnaQueueCountPerInterface present, and a vCpuInfo.validCores list that steps in the family's own core increments.
  Emulator: doubles the instance-store disk and total size, reports a different sustained clock, derives baseline bandwidth as a fixed fraction of a generic peak, omits ipv4AddressesPerSecondaryInterface and defaultEnaQueueCountPerInterface altogether, and enumerates validCores as consecutive integers.
  Why: a client sizing ephemeral storage or ENA queue capacity from the catalog gets values that do not exist on any real f2 instance, and the two omitted members are simply absent, so code that reads them sees null where AWS always sends a number.
  Fix: same component — the DescribeInstanceTypes catalog projection; the omitted members must be emitted for every family EC2 emits them for.
  Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#6.

Cause 3 of 4: the g5 family's GPU workload set, ENA queue count, EFA-only interface type and EBS attachment limit are wrong or absent.
  AWS: recorded behavior only. For g5 sizes the recording reports gpuInfo.gpus[].workloadSet containing 'graphics', networkInfo.networkCards[].defaultEnaQueueCountPerInterface 32, networkInfo.networkCards[].interfaceTypeSet containing 'efa-only', ebsInfo.maximumEbsAttachments at the catalogued value (9 for the largest size), and per-card baseline/peak bandwidth at the published figures.
  Emulator: omits gpuInfo.gpus[].workloadSet entries entirely, omits defaultEnaQueueCountPerInterface, omits the 'efa-only' interfaceTypeSet entry, reports a larger maximumEbsAttachments derived from the size, and derives baseline bandwidth as a fraction of a generic peak.
  Why: workloadSet is how a caller tells a graphics GPU type from a compute-only one, and 'efa-only' is how it tells whether EFA-only interfaces can be attached; with both absent, GPU/EFA-aware placement logic cannot select or validate a g5 type at all.
  Fix: same component — the DescribeInstanceTypes catalog projection, including gpuInfo.workloadSet and the network-card interfaceTypeSet.
  Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#8.

Cause 4 of 4: the g5g family repeats the same omissions and derives per-card baseline bandwidth.
  AWS: recorded behavior only. For g5g sizes the recording reports gpuInfo.gpus[].workloadSet containing 'graphics', networkInfo.networkCards[].defaultEnaQueueCountPerInterface 8, and networkInfo.networkCards[].baselineBandwidthInGbps at the published per-size figures.
  Emulator: omits workloadSet and defaultEnaQueueCountPerInterface, and computes baselineBandwidthInGbps from its generic peak-bandwidth fraction rather than reading the catalogued value.
  Why: as above — the family is Graviton+GPU, and a caller that filters on workloadSet or sizes ENA queues sees nothing to filter on.
  Fix: same component — the DescribeInstanceTypes catalog projection.
  Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up: true, bootstrap_signal: false, 4230 steps, 3533 passed, 622 mismatched, score 83.5%). All four clusters report expected_status 200 / actual_status 200 with owned_by: [] and stub_path: "".

Cause 1 — ec2-catalog-describe-instance-types-a1-c9g#task#27, DescribeInstanceTypes (excerpt, instanceType="c8gn.12xlarge"):
  ebsInfo.attachmentLimitType: expected 'dedicated', got 'shared'
  ebsInfo.ebsOptimizedInfo.baselineBandwidthInMbps: expected '30000', got '10200'
  ebsInfo.ebsOptimizedInfo.baselineIops: expected '120000', got '45000'
  ebsInfo.ebsOptimizedInfo.baselineThroughputInMBps: expected '3750.0', got '1275.0'
  ebsInfo.ebsOptimizedInfo.maximumBandwidthInMbps: expected '30000', got '10200'
  ebsInfo.ebsOptimizedInfo.maximumIops: expected '120000', got '45000'
  ebsInfo.ebsOptimizedInfo.maximumThroughputInMBps: expected '3750.0', got '1275.0'
  ebsInfo.maximumEbsAttachments: expected '32', got '27'
  networkInfo.connectionTrackingConfiguration.defaultTcpEstablishedTimeout: expected '350', got '432000'
  networkInfo.ipv4AddressesPerSecondaryInterface: expected '0', got '30'
  networkInfo.maximumNetworkInterfaces: expected '12', got '8'
  (the cluster's divergence_key additionally lists the same members plus networkCards[].additionalFlexibleNetworkInterfaces / baselineBandwidthInGbps / defaultEnaQueueCountPerInterface / maximumEnaQueueCount / maximumEnaQueueCountPerInterface / maximumNetworkInterfaces / networkPerformance / peakBandwidthInGbps / interfaceTypeSet, networkInfo.networkPerformance, networkInfo.secondaryNetworkSupported, networkInfo.trafficMirroringSupport, networkInfo.enaSrdSupported, networkInfo.efaInfo.maximumEfaInterfaces, phcSupport, processorInfo.sustainedClockSpeedInGhz, ebsInfo.ebsCardSet, ebsInfo.maximumEbsCards and the whole vCpuInfo.validCores list, for the c8gn.2xlarge / 12xlarge / 16xlarge / 24xlarge / 48xlarge rows)

Cause 2 — ec2-catalog-describe-instance-types-c9gd-m6gd#task#6, DescribeInstanceTypes (excerpt, instanceType="f2.12xlarge"):
  instanceStorageInfo.disks.item.sizeInGB: expected '940', got '1900'
  instanceStorageInfo.totalSizeInGB: expected '1880', got '3800'
  networkInfo.ipv4AddressesPerSecondaryInterface: missing (expected '0', got None)
  networkInfo.networkCards.item.baselineBandwidthInGbps: expected '25.0', got '6.25'
  networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '8', got None)
  networkInfo.networkCards.item.peakBandwidthInGbps: expected '25.0', got '10.0'
  processorInfo.sustainedClockSpeedInGhz: expected '3.4', got '3.6'
  vCpuInfo.validCores.item[3..7]: expected '6','9','12','15','18', got '4','5','6','7','8'
  (same pattern repeated for the f2.6xlarge and f2.48xlarge rows)

Cause 3 — ec2-catalog-describe-instance-types-c9gd-m6gd#task#8, DescribeInstanceTypes (excerpt, g5 rows):
  g5.12xlarge gpuInfo.gpus.item.workloadSet.item[0]: missing (expected 'graphics', got None)
  g5.12xlarge networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '32', got None)
  g5.12xlarge networkInfo.networkCards.item.interfaceTypeSet.item[2]: missing (expected 'efa-only', got None)
  g5.48xlarge ebsInfo.maximumEbsAttachments: expected '9', got '17'
  g5.4xlarge networkInfo.networkCards.item.baselineBandwidthInGbps: expected '10.0', got '2.08'
  g5.4xlarge networkInfo.networkCards.item.peakBandwidthInGbps: expected '25.0', got '10.0'
  (same three omissions on the g5.xlarge / 2xlarge / 8xlarge / 16xlarge / 24xlarge rows)

Cause 4 — ec2-catalog-describe-instance-types-c9gd-m6gd#task#9, DescribeInstanceTypes (excerpt, g5g rows):
  g5g.4xlarge gpuInfo.gpus.item.workloadSet.item[0]: missing (expected 'graphics', got None)
  g5g.4xlarge networkInfo.networkCards.item.baselineBandwidthInGbps: expected '5.0', got '6.25'
  g5g.4xlarge networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '8', got None)
  g5g.2xlarge networkInfo.networkCards.item.baselineBandwidthInGbps: expected '2.5', got '3.12'
  (same omissions on the g5g.xlarge / 8xlarge / 16xlarge / metal rows)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1) against it.
3. Inspect these four DescribeInstanceTypes steps, each reported with owned_by: [] and stub_path: "":
   - ec2-catalog-describe-instance-types-a1-c9g#task#27 (c8gn family)
   - ec2-catalog-describe-instance-types-c9gd-m6gd#task#6 (f2 family)
   - ec2-catalog-describe-instance-types-c9gd-m6gd#task#8 (g5 family)
   - ec2-catalog-describe-instance-types-c9gd-m6gd#task#9 (g5g family)
4. For each row in instanceTypeSet, diff the emulator's ebsInfo, networkInfo (including networkCards and connectionTrackingConfiguration), gpuInfo, instanceStorageInfo, processorInfo, phcSupport and vCpuInfo.validCores members against the recorded response.
5. All four must be closed: re-replay every fixture#step listed above, not just the representative. Note the same root cause affects many further DescribeInstanceTypes clusters in this corpus.
