Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#32
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-c9gd-m6gd ec2-catalog-describe-instance-types-m6i-r4 ec2-mr-inv-describe-regions-rejects-unknown-region-name ec2-usg-traj-filter-name-and-page-size-validation ec2-ibp-traj-launch-template-from-failed-ami-poll
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.12xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.12xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.12xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.16xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.16xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.16xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.24xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.24xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.24xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.2xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.32xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.32xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.32xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.32xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.48xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.48xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.48xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.4xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.4xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.8xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.8xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.96xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.96xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.96xlarge"].networkInfo.maximumNetworkCards
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.96xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.96xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.large"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.large"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.large"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].networkInfo.flexibleEnaQueuesSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-48xl"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].networkInfo.flexibleEnaQueuesSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].networkInfo.maximumNetworkCards
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.metal-96xl"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8in.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.12xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.16xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.16xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.16xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.24xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.24xlarge"].networkInfo.efaInfo.maximumEfaInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.24xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.24xlarge"].networkInfo.networkCards.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.24xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.2xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.48xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.48xlarge"].networkInfo.efaInfo.maximumEfaInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.48xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.48xlarge"].networkInfo.networkCards.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.4xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.8xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.8xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.xlarge"].gpuInfo.gpus.item.workloadSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="g6e.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.12xlarge"].networkInfo.networkPerformance
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.12xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.16xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.24xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.2xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.4xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.8xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.large"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-24xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i7i.xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].networkInfo.networkCards.item.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.24xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.2xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.6xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.6xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.6xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.6xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.6xlarge"].vCpuInfo.validCores.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].ebsInfo.ebsOptimizedInfo.baselineIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].processorInfo.sustainedClockSpeedInGhz
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].rebootMigrationSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="inf1.xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.12xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.12xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.16xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.16xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.16xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.24xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.24xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.24xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.2xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.4xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.8xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.large"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-24xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-48xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.metal-48xl"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m7i.xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeRegions|2xx->2xx|$.DescribeRegionsResponse.regionInfo.item[*].regionEndpoint
Issue-Key: ec2|CreateLaunchTemplateVersion|2xx->2xx|$.CreateLaunchTemplateVersionResponse.launchTemplateVersion.launchTemplateId
Issue-Related: 20261113T004910Z-ec2-describe-read-paths-return-stale-and-incomplete.md
Issue-Commit: 9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35

# ec2 response members carry emulator-synthesized values instead of authoritative ones (3 causes)

> _Note: this issue was identified against the code base at commit `9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
Three net-new wire divergences in the ec2 emulator share one fault: a response member that must carry an authoritative value — AWS's published catalog fact, AWS's regional endpoint, or the id of the resource actually addressed — is instead filled with a value the emulator synthesized for itself. All three answer the status AWS answers (200), so a client has no signal that the payload is wrong. THIS ISSUE HAS 3 CAUSES; all three must be closed before submitting.

=== Cause 1 of 3: DescribeInstanceTypes rows are derived by formula instead of carrying AWS's published per-instance-type facts, and the newer ENA-queue members are omitted entirely (MEDIUM) ===

AWS: Smithy (com.amazonaws.ec2) — DescribeInstanceTypes returns InstanceTypeInfo per requested type. NetworkCardInfo declares the members NetworkCardIndex/networkCardIndex, NetworkPerformance/networkPerformance, MaximumNetworkInterfaces/maximumNetworkInterfaces, AdditionalFlexibleNetworkInterfaces/additionalFlexibleNetworkInterfaces ("The number of additional network interfaces that can be attached to an instance when using flexible Elastic Network Adapter (ENA) queues"), BaselineBandwidthInGbps/baselineBandwidthInGbps ("The baseline network performance of the network card, in Gbps"), PeakBandwidthInGbps/peakBandwidthInGbps, DefaultEnaQueueCountPerInterface/defaultEnaQueueCountPerInterface ("The default number of the ENA queues for each interface"), MaximumEnaQueueCount/maximumEnaQueueCount, MaximumEnaQueueCountPerInterface/maximumEnaQueueCountPerInterface and InterfaceTypes/interfaceTypeSet (a list). The surrounding InstanceTypeInfo members exercised here are freeTierEligible, rebootMigrationSupport, networkInfo.ipv4AddressesPerInterface, networkInfo.ipv6AddressesPerInterface, networkInfo.maximumNetworkInterfaces, networkInfo.maximumNetworkCards, networkInfo.flexibleEnaQueuesSupport, networkInfo.efaInfo.maximumEfaInterfaces, ebsInfo.maximumEbsAttachments, ebsInfo.ebsOptimizedInfo.baselineIops, vCpuInfo.validCores (a list), processorInfo.sustainedClockSpeedInGhz, processorInfo.supportedFeatures (a LIST) and gpuInfo.gpus[].workloadSet (a list). For each of these AWS returns the hardware fact published for that instance type; the values are per-type constants, not functions of vCPU count. Status 200.

Emulator: answers 200 but the row is computed. Concretely, from the replay: (a) the ENA-queue family of members is absent for every type — defaultEnaQueueCountPerInterface, maximumEnaQueueCount, maximumEnaQueueCountPerInterface and additionalFlexibleNetworkInterfaces come back `missing ... got None`; (b) per-interface address counts and interface limits are scaled from vCPUs rather than published, so e.g. c8in.12xlarge reports ipv4AddressesPerInterface/ipv6AddressesPerInterface 30 and maximumNetworkInterfaces 8 where AWS reports 50/50 and 12, and c8in.16xlarge reports 50/50/15 where AWS reports 64/64/16; (c) bandwidth is formula-derived — i7i.metal-24xl reports baselineBandwidthInGbps and peakBandwidthInGbps 12.5 where AWS reports 56.25 and 56.25, i7i.metal-48xl reports 25.0 where AWS reports 100.0, m7i.12xlarge reports 6.25/10.0 where AWS reports 18.75/18.75; (d) interfaceTypeSet omits the 'efa-only' element AWS lists for EFA-capable sizes (i7i.metal-48xl, m7i.metal-48xl, g6e.48xlarge, c8in.48xlarge); (e) networkCards is collapsed — g6e.24xlarge is served one card with maximumNetworkInterfaces 7 where AWS serves two cards of 10 each, and the 96xlarge/metal-96xl sizes disagree on maximumNetworkCards; (f) processorInfo.supportedFeatures is serialized as a bare scalar ('nested-virtualization') where AWS serializes a list (['nested-virtualization','metal-compatibility']) — reported as `type_mismatch`; (g) gpuInfo.gpus.item.workloadSet is omitted where AWS emits 'graphics' (g6e family); (h) vCpuInfo.validCores is enumerated as 1..N rather than AWS's published set (inf1.2xlarge: AWS ['2','4'], emulator ['1','2','3','4']); (i) rebootMigrationSupport is 'unsupported' and freeTierEligible 'true' uniformly where AWS reports 'supported'/'false' for these types; (j) processorInfo.sustainedClockSpeedInGhz and ebsInfo.maximumEbsAttachments are likewise computed (inf1.2xlarge 3.0 vs AWS 2.5; inf1.6xlarge 26 vs AWS 23). Hypothesis: a single generator produces every row from vCPU/memory, so the defect is one table, not one type.

Why: these fields are the inputs to capacity and networking decisions. A client sizing ENA queues cannot see defaultEnaQueueCountPerInterface at all and concludes the instance type does not support flexible ENA queues; a client that counts maximumNetworkInterfaces or ipv4AddressesPerInterface before attaching ENIs/secondary IPs provisions the wrong number and the attach later fails; a client that filters on interfaceTypeSet containing 'efa-only' finds no EFA-only capable type; a client that reads processorInfo.supportedFeatures as a list gets a parse/type error because a scalar arrived where a list was modeled; a cost tool reading freeTierEligible 'true' bills wrongly.

Fix: the ec2 instance-type catalog in the service's state/static data must carry AWS's published per-type record rather than deriving it, and the DescribeInstanceTypes serializer must emit the NetworkCardInfo ENA-queue members and emit processorInfo.supportedFeatures, gpuInfo.gpus[].workloadSet, vCpuInfo.validCores and interfaceTypeSet as lists.

Traces: ec2-catalog-describe-instance-types-a1-c9g#task#32 (c8in family, the representative), ec2-catalog-describe-instance-types-c9gd-m6gd#task#11 (g6e), ec2-catalog-describe-instance-types-c9gd-m6gd#task#23 (i7i), ec2-catalog-describe-instance-types-c9gd-m6gd#task#28 (inf1), ec2-catalog-describe-instance-types-m6i-r4#task#8 (m7i). The same cause recurs across the ec2-catalog-describe-instance-types-* fixture family corpus-wide. Open issues 20261114T052927Z-ec2-static-catalogs-serve-synthesized-records-instance-types.md, 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md and 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md describe the same shape of fault but own different rows; the replay reports these steps as owned by nothing, so they are net-new here.

=== Cause 2 of 3: DescribeRegions emits regionEndpoint as ec2.<region>.localhost instead of AWS's ec2.<region>.amazonaws.com (MEDIUM) ===

AWS: Smithy (com.amazonaws.ec2) — Region.Endpoint, wire name regionEndpoint, documented "The Region service endpoint". For the ec2 service AWS returns the regional service endpoint host for each region in the form ec2.<regionName>.amazonaws.com (the recorded responses show ec2.ap-south-2.amazonaws.com, ec2.eu-south-1.amazonaws.com, ec2.ca-central-1.amazonaws.com, ... — one per regionName, same suffix for every row). Status 200.

Emulator: answers 200 but every regionEndpoint is ec2.<regionName>.localhost — e.g. 'ec2.af-south-1.localhost' where the recording has 'ec2.af-south-1.amazonaws.com'. The replay comparator classes each of these `host_not_own`, i.e. the host served is not the emulator's own address, so this is not an endpoint-rewriting artifact of the test environment: it is a value the handler invents. Independently, the regionInfo item sequence the emulator serves does not line up with the recorded one (recorded item[0] is ap-south-2 while the emulator's item[0] is af-south-1), so the region set/ordering also differs from the recording. Only the regionEndpoint member is asserted as diverging; the suffix is the requirement here, not any single region name.

Why: DescribeRegions is the discovery call clients use to build per-region endpoints. A client that takes regionEndpoint and connects resolves a `.localhost` name that points at the caller's own loopback for every region, so cross-region fan-out either fails to resolve or silently targets the local host instead of the intended region.

Fix: the DescribeRegions handler / the ec2 region catalog in state — populate Region.Endpoint from the regional endpoint template for the service rather than from the emulator's own listening host.

Traces: ec2-mr-inv-describe-regions-rejects-unknown-region-name#task#4, ec2-usg-traj-filter-name-and-page-size-validation#task#9.

=== Cause 3 of 3: CreateLaunchTemplateVersion returns a freshly minted launchTemplateId instead of the id of the template it versioned (HIGH) ===

AWS: Smithy (com.amazonaws.ec2) — CreateLaunchTemplateVersion returns LaunchTemplateVersion, whose member LaunchTemplateId (wire name launchTemplateId) is documented "The ID of the launch template." A version is created ON an existing template identified by the request's LaunchTemplateId or LaunchTemplateName, so AWS echoes that template's own id: the value equals the id CreateLaunchTemplate returned for it, and the same id is what DescribeLaunchTemplateVersions and RunInstances accept afterwards. Status 200.

Emulator: answers 200 but launchTemplateId does not identify the template being versioned — the replay asserts the template id bound earlier in the trace from CreateLaunchTemplate and the emulator returns a different, newly minted launch-template id instead (the run showed 'lt-000000000000000a7'; that literal is a per-run generated id and is not itself the requirement — the requirement is the echo relation between the request's target template and the returned launchTemplateId).

Why: a client chains on this id. It creates a template, adds a version, then references the returned launchTemplateId in ModifyLaunchTemplate / DescribeLaunchTemplateVersions / RunInstances. Because the returned id names no template the client created, every one of those follow-ups addresses the wrong resource or is refused as not found, and the client cannot tell which of its two ids is authoritative.

Fix: the CreateLaunchTemplateVersion handler — resolve the target launch template from the request's LaunchTemplateId/LaunchTemplateName and echo that template's stored id into LaunchTemplateVersion.launchTemplateId; do not allocate a new template id on the version path. Related open issue 20261110T100847Z-ec2-createlaunchtemplateversion-drops-the-validation-warning-set-and.md covers different members of the same response (the warning set and keyName) and does not carry this replay key.

Traces: ec2-ibp-traj-launch-template-from-failed-ami-poll#task#9.

## Evidence
Cause 1 — DescribeInstanceTypes, expected 200 / actual 200, body members diverge.
  ec2-catalog-describe-instance-types-a1-c9g#task#32 (c8in):
    $...item[instanceType="c8in.12xlarge"].networkInfo.ipv4AddressesPerInterface: expected '50', got '30'
    $...item[instanceType="c8in.12xlarge"].networkInfo.ipv6AddressesPerInterface: expected '50', got '30'
    $...item[instanceType="c8in.12xlarge"].networkInfo.maximumNetworkInterfaces: expected '12', got '8'
    $...item[instanceType="c8in.12xlarge"].networkInfo.networkCards.item.additionalFlexibleNetworkInterfaces: missing (expected '0', got None)
    $...item[instanceType="c8in.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '16', got None)
    $...item[instanceType="c8in.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCount: missing (expected '192', got None)
    $...item[instanceType="c8in.12xlarge"].networkInfo.networkCards.item.maximumEnaQueueCountPerInterface: missing (expected '64', got None)
    $...item[instanceType="c8in.16xlarge"].networkInfo.ipv4AddressesPerInterface: expected '64', got '50'
    $...item[instanceType="c8in.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[2]: missing ('efa-only')
    $...item[instanceType="c8in.metal-48xl"].rebootMigrationSupport / networkInfo.flexibleEnaQueuesSupport / ebsInfo.maximumEbsAttachments diverge
    $...item[instanceType="c8in.96xlarge"].networkInfo.maximumNetworkCards and .vCpuInfo.validCores.item[0..94] diverge
  ec2-catalog-describe-instance-types-c9gd-m6gd#task#11 (g6e):
    $...item[instanceType="g6e.24xlarge"].gpuInfo.gpus.item.workloadSet.item[0]: missing (expected 'graphics', got None)
    $...item[instanceType="g6e.24xlarge"].networkInfo.efaInfo.maximumEfaInterfaces: expected '2', got '1'
    $...item[instanceType="g6e.24xlarge"].networkInfo.maximumNetworkInterfaces: expected '20', got '15'
    $...item[instanceType="g6e.24xlarge"].networkInfo.networkCards.item[0]: expected {'baselineBandwidthInGbps':'200.0','defaultEnaQueueCountPerInterface':'32','interfaceTypeSet':{'item':['interface','efa','efa-only']},'maximumNetworkInterfaces':'10','networkCardIndex':'0',...}, got {'networkCardIndex':'0','maximumNetworkInterfaces':'7','interfaceTypeSet':{'item':['interface','efa']},...} (and item[1] likewise missing)
  ec2-catalog-describe-instance-types-c9gd-m6gd#task#23 (i7i):
    $...item[instanceType="i7i.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '56.25', got '12.5'
    $...item[instanceType="i7i.metal-24xl"].networkInfo.networkCards.item.peakBandwidthInGbps: expected '56.25', got '12.5'
    $...item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '100.0', got '25.0'
    $...item[instanceType="i7i.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[2]: missing (expected 'efa-only', got None)
    $...item[instanceType="i7i.2xlarge"].processorInfo.supportedFeatures.item: type_mismatch (expected ['nested-virtualization','metal-compatibility'], got 'nested-virtualization')
    $...item[instanceType="i7i.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '4.687', got '1.04'
  ec2-catalog-describe-instance-types-c9gd-m6gd#task#28 (inf1):
    $...item[instanceType="inf1.2xlarge"].networkInfo.ipv4AddressesPerInterface: expected '10', got '15'
    $...item[instanceType="inf1.2xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps: expected '25.0', got '10.0'
    $...item[instanceType="inf1.2xlarge"].processorInfo.sustainedClockSpeedInGhz: expected '2.5', got '3.0'
    $...item[instanceType="inf1.2xlarge"].rebootMigrationSupport: expected 'supported', got 'unsupported'
    $...item[instanceType="inf1.2xlarge"].vCpuInfo.validCores.item[0]: expected '2', got '1'; item[1]: expected '4', got '2'; item[2],item[3]: extra ('3','4')
    $...item[instanceType="inf1.6xlarge"].ebsInfo.maximumEbsAttachments: expected '23', got '26'
  ec2-catalog-describe-instance-types-m6i-r4#task#8 (m7i):
    $...item[instanceType="m7i.12xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '18.75', got '6.25'; peakBandwidthInGbps: expected '18.75', got '10.0'
    $...item[instanceType="m7i.12xlarge"].processorInfo.supportedFeatures.item: type_mismatch (expected ['nested-virtualization','metal-compatibility'], got 'nested-virtualization')
    $...item[instanceType="m7i.metal-24xl"].networkInfo.networkCards.item.baselineBandwidthInGbps: expected '37.5', got '12.5'
    defaultEnaQueueCountPerInterface missing on every m7i size

Cause 2 — DescribeRegions, expected 200 / actual 200.
  ec2-mr-inv-describe-regions-rejects-unknown-region-name#task#4:
    $.DescribeRegionsResponse.regionInfo.item[0].regionEndpoint: host_not_own (expected 'ec2.ap-south-2.amazonaws.com', got 'ec2.af-south-1.localhost')
    item[1]: expected 'ec2.ap-south-1.amazonaws.com', got 'ec2.ap-east-1.localhost'
    item[2]: expected 'ec2.eu-south-1.amazonaws.com', got 'ec2.ap-east-2.localhost'
    ... every item[0..33].regionEndpoint diverges the same way: AWS suffix '.amazonaws.com' vs emulator suffix '.localhost'
  ec2-usg-traj-filter-name-and-page-size-validation#task#9:
    item[0..16].regionEndpoint: same host_not_own divergence, e.g. expected 'ec2.me-south-1.amazonaws.com', got 'ec2.ca-west-1.localhost'

Cause 3 — CreateLaunchTemplateVersion, expected 200 / actual 200.
  ec2-ibp-traj-launch-template-from-failed-ami-poll#task#9:
    $.CreateLaunchTemplateVersionResponse.launchTemplateVersion.launchTemplateId: value_mismatch (expected the launch template id bound earlier in the trace, got a newly minted 'lt-000000000000000a7')

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (replay_wire_corpus), then look at these steps:

Cause 1 — DescribeInstanceTypes catalog rows:
  1. Replay fixture ec2-catalog-describe-instance-types-a1-c9g, step task#32 (c8in sizes). Equivalently: POST the ec2 query endpoint with Action=DescribeInstanceTypes&Version=2016-11-15 and InstanceType.1=c8in.12xlarge.
  2. In the 200 response inspect instanceTypeSet.item.networkInfo: networkCards.item has no defaultEnaQueueCountPerInterface / maximumEnaQueueCount / maximumEnaQueueCountPerInterface / additionalFlexibleNetworkInterfaces, and ipv4AddressesPerInterface/ipv6AddressesPerInterface/maximumNetworkInterfaces read 30/30/8 instead of 50/50/12.
  3. Repeat for the other recorded families: ec2-catalog-describe-instance-types-c9gd-m6gd task#11 (g6e.24xlarge — single network card and no gpuInfo workloadSet), task#23 (i7i.metal-24xl — bandwidth 12.5 instead of 56.25; i7i.2xlarge processorInfo.supportedFeatures arrives as a scalar), task#28 (inf1.2xlarge — validCores 1..4 instead of [2,4]) and ec2-catalog-describe-instance-types-m6i-r4 task#8 (m7i.12xlarge bandwidth 6.25/10.0 instead of 18.75/18.75).

Cause 2 — DescribeRegions regionEndpoint:
  1. Replay fixture ec2-mr-inv-describe-regions-rejects-unknown-region-name, step task#4 (or ec2-usg-traj-filter-name-and-page-size-validation step task#9). Equivalently: POST Action=DescribeRegions&Version=2016-11-15.
  2. Read regionInfo.item[*].regionEndpoint in the 200 response: every entry ends in '.localhost' instead of '.amazonaws.com'.

Cause 3 — CreateLaunchTemplateVersion launchTemplateId:
  1. Replay fixture ec2-ibp-traj-launch-template-from-failed-ami-poll, step task#9.
  2. Equivalently: CreateLaunchTemplate (note the lt-... id it returns), then CreateLaunchTemplateVersion naming that LaunchTemplateId.
  3. Compare launchTemplateVersion.launchTemplateId in the 200 response with the id from step 2 — the emulator returns a different, newly minted lt-... id.
