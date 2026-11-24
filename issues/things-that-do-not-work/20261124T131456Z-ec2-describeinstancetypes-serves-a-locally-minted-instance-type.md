Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-types-a1-c9g
Issue-Step: task#31
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-types-a1-c9g ec2-catalog-describe-instance-types-c9gd-m6gd ec2-catalog-describe-instance-types-r5-r8in ec2-catalog-describe-instance-types-r9g-z1d
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.12xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.12xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.12xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.12xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.16xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.16xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.16xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.16xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.24xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.24xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.24xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.24xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.24xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.2xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.2xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.2xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.32xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.48xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.4xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.4xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.4xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.8xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.8xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.8xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.8xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.96xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.large"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.large"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.large"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].ebsInfo.maximumEbsAttachments
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].networkInfo.flexibleEnaQueuesSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].nitroTpmSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-48xl"].supportedBootModes.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.flexibleEnaQueuesSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.maximumNetworkInterfaces
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].nitroTpmSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.metal-96xl"].supportedBootModes.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.xlarge"].networkInfo.ipv4AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.xlarge"].networkInfo.ipv6AddressesPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="c8id.xlarge"].processorInfo.supportedFeatures.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.16xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.2xlarge"].ebsInfo.ebsOptimizedInfo.baselineIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.2xlarge"].ebsInfo.ebsOptimizedInfo.maximumIops
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.2xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.4xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="h1.8xlarge"].networkInfo.trafficMirroringSupport
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i2.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="i3.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.4xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.8xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.medium"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.medium"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.medium"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="is4gen.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.10xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.large"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.large"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="m4.xlarge"].networkInfo.networkCards.item.peakBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.24xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5d.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.12xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.24xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.24xlarge"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.24xlarge"].vCpuInfo.validCores.item
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.large"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.metal"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.metal"].networkInfo.networkCards.item.interfaceTypeSet.item[*]
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="r5dn.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.16xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.2xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.2xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.32xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.4xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.4xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.8xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.8xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.xlarge"].networkInfo.networkCards.item.baselineBandwidthInGbps
Issue-Key: ec2|DescribeInstanceTypes|2xx->2xx|$.DescribeInstanceTypesResponse.instanceTypeSet.item[instanceType="x1e.xlarge"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface
Issue-Related: 20261112T231741Z-ec2-describeinstancetypes-catalog-rows-are-synthesized-from-a.md
Issue-Commit: b387edc96a71c03e0645f0d43b5b778504810db6

# ec2 DescribeInstanceTypes serves a locally minted instance-type catalog: ENA queue members omitted, network-card bandwidth, EBS IOPS and validCores wrong (9 causes)

> _Note: this issue was identified against the code base at commit `b387edc96a71c03e0645f0d43b5b778504810db6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypes

## Description
SHARED FAULT (9 net-new causes, all ec2 DescribeInstanceTypes, every one a 200 -> 200 body mismatch): the instance-type table DescribeInstanceTypes reads from is produced locally -- network-card bandwidth is derived from a size-class formula, the ENA-queue members are not materialised at all, vCpuInfo.validCores is collapsed to a single number, and several boolean/enum capability members carry a default instead of the type's published value. AWS serves a fixed, published row per instance type, so every member below is a constant of the instance type and not an environment-dependent value.

Model grounding used for all nine causes (Smithy, com.amazonaws.ec2): NetworkCardInfo members baselineBandwidthInGbps (double), peakBandwidthInGbps (double), defaultEnaQueueCountPerInterface (integer), maximumEnaQueueCount (integer), maximumEnaQueueCountPerInterface (integer), additionalFlexibleNetworkInterfaces (integer), maximumNetworkInterfaces (integer) and interfaceTypeSet (NetworkCardInterfaceTypeList); VCpuInfo.validCores targets CoreCountList (a LIST of the valid core counts, not a scalar); EbsOptimizedInfo.baselineIops / maximumIops (integer). The per-type values themselves are recorded behavior only -- they come from the recorded AWS responses, cited below as examples of the rule, not as the rule.

RELATED OPEN ISSUES that do NOT carry these replay keys (so they do not cover these clusters): 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md, 20261112T182924Z-ec2-describeinstancetypes-rows-for-the-c4-c9gd-m6i.md, 20261124T114924Z-ec2-describeinstancetypes-validcores-ena-queue-counts-and-efa.md.

Cause 1 of 9 -- c8id rows (fixture ec2-catalog-describe-instance-types-a1-c9g, step task#31).
AWS: emits every NetworkCardInfo member for the type, with the per-type published values (recorded behavior: c8id.metal-96xl -> maximumNetworkInterfaces 24, ipv4AddressesPerInterface and ipv6AddressesPerInterface 64, networkCards.item baselineBandwidthInGbps 100.0, defaultEnaQueueCountPerInterface 32, maximumEnaQueueCount 1536, maximumEnaQueueCountPerInterface 128, additionalFlexibleNetworkInterfaces 0, interfaceTypeSet containing 'efa-only', networkInfo.flexibleEnaQueuesSupport 'supported').
Emulator: omits defaultEnaQueueCountPerInterface, maximumEnaQueueCount, maximumEnaQueueCountPerInterface and additionalFlexibleNetworkInterfaces entirely (serialised as absent); reports ipv4AddressesPerInterface/ipv6AddressesPerInterface 50 and maximumNetworkInterfaces 15 for the whole family; serves formula-derived card bandwidth (25.0 where the type publishes 100.0); reports flexibleEnaQueuesSupport 'unsupported'; interfaceTypeSet has no 'efa-only' entry.
Why: a client sizing ENA queues, interface budgets or EFA placement from this row cannot read the queue members at all (absent, not zero) and gets a smaller-than-real interface/bandwidth budget, so instance-type selection and capacity validation that AWS accepts fail here.
Fix: the ec2 instance-type catalog data behind DescribeInstanceTypes (services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instance_types.rs and the catalog table it reads) -- store the published per-type row instead of deriving members, and serialise the ENA-queue members.
Traces: ec2-catalog-describe-instance-types-a1-c9g#task#31.

Cause 2 of 9 -- h1 rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#17).
AWS: h1 rows carry defaultEnaQueueCountPerInterface 8, card baseline/peak bandwidth equal to the type's published figure (h1.8xlarge 12.0/12.0, h1.4xlarge baseline 5.0, h1.2xlarge baseline 2.5), networkInfo.trafficMirroringSupport 'supported', and ebsOptimizedInfo baselineIops/maximumIops 12000 for h1.2xlarge.
Emulator: defaultEnaBuffer member absent (defaultEnaQueueCountPerInterface missing); bandwidth comes out of the size formula (12.5 / 6.25 / 3.12); trafficMirroringSupport 'unsupported'; h1.2xlarge EBS IOPS 10000.
Why: EBS-optimised throughput planning and traffic-mirroring eligibility checks read the wrong numbers, and the queue member is unreadable.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#17.

Cause 3 of 9 -- i2 rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#18).
AWS: i2 card bandwidth is the published pair per size (i2.xlarge baseline 0.7 / peak 2.8, i2.4xlarge 2.0 / 10.0, i2.8xlarge 10.0 / 10.0) and defaultEnaQueueCountPerInterface is 8.
Emulator: serves formula values (3.12 / 10.0, 12.5 / 12.5, 25.0 / 25.0) and omits defaultEnaQueueCountPerInterface.
Why: a legacy-family row reports several times the real network ceiling, so throughput expectations taken from the API are wrong.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#18.

Cause 4 of 9 -- i3 rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#19).
AWS: i3 rows publish baselineBandwidthInGbps per size (i3.large 0.75, i3.xlarge 1.25, i3.2xlarge 2.5, i3.4xlarge 5.0, i3.8xlarge 12.0 with peak 12.0) and defaultEnaQueueCountPerInterface 8.
Emulator: 0.78 / 1.56 / 3.12 / 6.25 / 10.0 from the formula, peak 10.0 on i3.8xlarge, and no defaultEnaQueueCountPerInterface member.
Why: as cause 3 -- the published per-type figure is a contract value clients compare against; the formula never reproduces it.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#19.

Cause 5 of 9 -- is4gen rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#30).
AWS: is4gen rows publish baseline/peak pairs where peak is the burst ceiling of the family (is4gen.medium 1.562 / 25.0, is4gen.large 3.125 / 25.0, is4gen.4xlarge 25.0, is4gen.8xlarge 50.0 / 50.0) and per-size defaultEnaQueueCountPerInterface (1, 2, 16, 32).
Emulator: doubles the baselines (3.12, 6.25, 50.0, 100.0), reports peak 10.0 for the small sizes and 100.0 for is4gen.8xlarge, and omits defaultEnaQueueCountPerInterface.
Why: burst-vs-baseline semantics are inverted for this family, so a client cannot tell baseline from peak capacity.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#30.

Cause 6 of 9 -- m4 rows (fixture ec2-catalog-describe-instance-types-c9gd-m6gd, step task#34).
AWS: m4 rows publish baseline/peak per size (m4.large 0.45 / 1.2, m4.xlarge 0.75 / 2.8, m4.2xlarge 1.0, m4.4xlarge 2.0) and defaultEnaQueueCountPerInterface 8 on every size including m4.10xlarge and m4.16xlarge.
Emulator: 0.78 / 10.0, 1.56 / 10.0, 3.12, 6.25 and no defaultEnaQueueCountPerInterface anywhere in the family.
Why: the emulator advertises a 10 Gbps peak on instance types AWS caps far lower, so network-bound sizing decisions are wrong.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-c9gd-m6gd#task#34.

Cause 7 of 9 -- r5d rows (fixture ec2-catalog-describe-instance-types-r5-r8in, step task#5).
AWS: every r5d size carries defaultEnaQueueCountPerInterface 8, and vCpuInfo.validCores is the LIST of configurable core counts (r5d.24xlarge: 4,6,8,...,48 -- CoreCountList per the model).
Emulator: omits defaultEnaQueueCountPerInterface on all ten r5d sizes and serialises validCores as a single scalar equal to the maximum ('48') instead of the list.
Why: validCores is modelled as a list; a client that enumerates CPU-options choices gets one value and rejects every legal smaller core count, and SDKs that decode a list from a scalar fail to parse.
Fix: same catalog table/serializer as cause 1, plus the validCores member serialisation (list, not scalar).
Traces: ec2-catalog-describe-instance-types-r5-r8in#task#5.

Cause 8 of 9 -- r5dn rows (fixture ec2-catalog-describe-instance-types-r5-r8in, step task#6).
AWS: r5dn sizes carry per-size defaultEnaQueueCountPerInterface (8 on the small sizes, 32 from 4xlarge up), interfaceTypeSet including 'efa-only' on r5dn.24xlarge and r5dn.metal, and validCores as the list 2,4,...,48 on r5dn.24xlarge.
Emulator: omits defaultEnaQueueCountPerInterface for the whole family, drops the 'efa-only' interface type, and serialises validCores as '48'.
Why: EFA-capable sizes look EFA-incapable, so a client provisioning EFA interfaces on a type AWS allows is blocked by its own pre-flight check against this row.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-r5-r8in#task#6.

Cause 9 of 9 -- x1e rows (fixture ec2-catalog-describe-instance-types-r9g-z1d, step task#21).
AWS: x1e rows publish baselineBandwidthInGbps per size (x1e.xlarge 0.625, x1e.2xlarge 1.25, x1e.4xlarge 2.5, x1e.8xlarge 5.0) and defaultEnaQueueCountPerInterface 8 on every size up to x1e.32xlarge.
Emulator: 0.78 / 1.56 / 3.12 / 6.25 from the formula and no defaultEnaQueueCountPerInterface member.
Why: as causes 3, 4 and 6.
Fix: same catalog table/serializer as cause 1.
Traces: ec2-catalog-describe-instance-types-r9g-z1d#task#21.

NOTE FOR THE FIXER: the same locally-derived catalog table produces further unowned DescribeInstanceTypes clusters in these four ec2-catalog-describe-instance-types-* fixtures beyond the nine named above (the replay reports 88 unowned DescribeInstanceTypes clusters in total). The nine causes here are the ones carrying replay keys in this issue; after fixing the catalog table, re-replaying the four fixtures end to end is the way to confirm the rest.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator up, /__admin/health 200). All nine clusters are verdict=mismatch with expected_status 200 and actual_status 200 -- the status and the operation are right, the body members are wrong.

Cause 1 (ec2-catalog-describe-instance-types-a1-c9g#task#31), e.g. c8id.metal-96xl:
  $...instanceTypeSet.item[instanceType="c8id.metal-96xl"].networkInfo.networkCards.item.defaultEnaQueueCountPerInterface: missing (expected '32', got None)
  ...networkCards.item.maximumEnaQueueCount: missing (expected '1536', got None)
  ...networkCards.item.maximumEnaQueueCountPerInterface: missing (expected '128', got None)
  ...networkCards.item.additionalFlexibleNetworkInterfaces: missing (expected '0', got None)
  ...networkCards.item.baselineBandwidthInGbps: value_mismatch (expected '100.0', got '25.0')
  ...networkCards.item.maximumNetworkInterfaces: value_mismatch (expected '24', got '15')
  ...networkInfo.ipv4AddressesPerInterface / ipv6AddressesPerInterface: value_mismatch (expected '64', got '50')
  ...networkInfo.flexibleEnaQueuesSupport: value_mismatch (expected 'supported', got 'unsupported')
  ...networkCards.item.interfaceTypeSet.item[2]: missing (expected 'efa-only', got None)

Cause 2 (…c9gd-m6gd#task#17), h1:
  h1.8xlarge networkCards.item.baselineBandwidthInGbps: expected '12.0', got '12.5'; peakBandwidthInGbps: expected '12.0', got '12.5'; defaultEnaQueueCountPerInterface: missing (expected '8')
  h1.4xlarge baselineBandwidthInGbps: expected '5.0', got '6.25'; networkInfo.trafficMirroringSupport: expected 'supported', got 'unsupported'
  h1.2xlarge ebsInfo.ebsOptimizedInfo.baselineIops and maximumIops: expected '12000', got '10000'; baselineBandwidthInGbps expected '2.5', got '3.12'

Cause 3 (…c9gd-m6gd#task#18), i2: i2.xlarge baseline expected '0.7' got '3.12', peak expected '2.8' got '10.0'; i2.4xlarge baseline expected '2.0' got '12.5', peak expected '10.0' got '12.5'; i2.8xlarge baseline expected '10.0' got '25.0', peak expected '10.0' got '25.0'; defaultEnaQueueCountPerInterface missing (expected '8') on every size.

Cause 4 (…c9gd-m6gd#task#19), i3: i3.large baseline expected '0.75' got '0.78'; i3.xlarge expected '1.25' got '1.56'; i3.2xlarge expected '2.5' got '3.12'; i3.4xlarge expected '5.0' got '6.25'; i3.8xlarge baseline and peak expected '12.0' got '10.0'; defaultEnaQueueCountPerInterface missing (expected '8') on every size.

Cause 5 (…c9gd-m6gd#task#30), is4gen: medium baseline expected '1.562' got '3.12', peak expected '25.0' got '10.0', defaultEnaQueueCountPerInterface missing (expected '1'); large baseline expected '3.125' got '6.25', peak expected '25.0' got '10.0' (expected '2'); 4xlarge baseline expected '25.0' got '50.0' (expected '16'); 8xlarge baseline and peak expected '50.0' got '100.0' (expected '32').

Cause 6 (…c9gd-m6gd#task#34), m4: m4.large baseline expected '0.45' got '0.78', peak expected '1.2' got '10.0'; m4.xlarge baseline expected '0.75' got '1.56', peak expected '2.8' got '10.0'; m4.2xlarge expected '1.0' got '3.12'; m4.4xlarge expected '2.0' got '6.25'; defaultEnaQueueCountPerInterface missing (expected '8') on m4.large/xlarge/2xlarge/4xlarge/10xlarge/16xlarge.

Cause 7 (ec2-catalog-describe-instance-types-r5-r8in#task#5), r5d: defaultEnaQueueCountPerInterface missing (expected '8') on large, xlarge, 2xlarge, 4xlarge, 8xlarge, 12xlarge, 16xlarge, 24xlarge, metal; r5d.24xlarge vCpuInfo.validCores.item: type_mismatch (expected ['4','6','8','10','12','14','16','18','20','22','24','26','28','30','32','34','36','38','40','42','44','46','48'], got '48').

Cause 8 (…r5-r8in#task#6), r5dn: defaultEnaQueueCountPerInterface missing (expected '8' on large/xlarge/2xlarge, '32' on 4xlarge/8xlarge/12xlarge/16xlarge/24xlarge/metal); r5dn.24xlarge and r5dn.metal networkCards.item.interfaceTypeSet.item[2]: missing (expected 'efa-only', got None); r5dn.24xlarge validCores.item: type_mismatch (expected ['2','4',...,'48'], got '48').

Cause 9 (ec2-catalog-describe-instance-types-r9g-z1d#task#21), x1e: x1e.xlarge baseline expected '0.625' got '0.78'; x1e.2xlarge expected '1.25' got '1.56'; x1e.4xlarge expected '2.5' got '3.12'; x1e.8xlarge expected '5.0' got '6.25'; defaultEnaQueueCountPerInterface missing (expected '8') on xlarge/2xlarge/4xlarge/8xlarge/16xlarge/32xlarge.

No status, error code or error message diverges in any of the nine clusters; the divergence is entirely in the catalog values and in members the emulator never serialises.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Representative cause: fixture ec2-catalog-describe-instance-types-a1-c9g, step task#31 -- a DescribeInstanceTypes call over the c8id family (ec2 Query protocol, Action=DescribeInstanceTypes with the family's InstanceType.N values). The response is HTTP 200 but instanceTypeSet.item rows omit networkCards.item.defaultEnaQueueCountPerInterface / maximumEnaQueueCount / maximumEnaQueueCountPerInterface / additionalFlexibleNetworkInterfaces and carry formula-derived baselineBandwidthInGbps/peakBandwidthInGbps, ipv4AddressesPerInterface/ipv6AddressesPerInterface 50, maximumNetworkInterfaces 15, flexibleEnaQueuesSupport 'unsupported' and an interfaceTypeSet without 'efa-only'.
4. The other eight causes are the same request shape over a different family page: ec2-catalog-describe-instance-types-c9gd-m6gd task#17 (h1), task#18 (i2), task#19 (i3), task#30 (is4gen), task#34 (m4); ec2-catalog-describe-instance-types-r5-r8in task#5 (r5d), task#6 (r5dn); ec2-catalog-describe-instance-types-r9g-z1d task#21 (x1e).
5. Compare each instanceTypeSet.item against the recorded response: the ENA-queue members are absent, the network-card bandwidth pair is the size-class formula rather than the published pair, vCpuInfo.validCores is a scalar instead of CoreCountList, and h1.2xlarge's ebsOptimizedInfo IOPS pair is 10000 instead of 12000.
