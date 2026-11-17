Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-create-image-builds-an-available-ami
Issue-Step: task#21
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-create-image-builds-an-available-ami
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.architecture
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.blockDeviceMapping
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.bootMode
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.enaSupport
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.imdsSupport
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.rootDeviceType
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.sourceImageId
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.sourceImageRegion
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.sourceInstanceId
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.sriovNetSupport
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.tagSet
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item.virtualizationType
Issue-Commit: e014230fb9b53a8f1acc538e4bd0b4dbf598fc56

# EC2 DescribeImages returns placeholder attributes for an Image Builder registered AMI

> _Note: this issue was identified against the code base at commit `e014230fb9b53a8f1acc538e4bd0b4dbf598fc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: DescribeImages describes an AMI registered by an Image Builder build with hard-coded placeholder attributes instead of the AMI's actual registration, and omits the enhanced-networking, boot-mode and provenance members entirely.
AWS: Smithy (com.amazonaws.ec2) DescribeImages -> DescribeImagesResult.Images : Image (ec2Query/XML, so the wire names are the members' xmlName values under imagesSet/item). The Image structure models, among others, `Architecture` (xmlName architecture, target ArchitectureValues enum), `RootDeviceType` (xmlName rootDeviceType, target DeviceType enum — ebs | instance-store, doc "The type of root device used by the AMI"), `VirtualizationType` (xmlName virtualizationType, target VirtualizationType enum), `BlockDeviceMappings` (xmlName blockDeviceMapping, target BlockDeviceMappingList, doc "Any block device mapping entries"), `Tags` (xmlName tagSet, target TagList, doc "Any tags assigned to the image"), `EnaSupport` (xmlName enaSupport, boolean), `SriovNetSupport` (xmlName sriovNetSupport, string), `BootMode` (xmlName bootMode, target BootModeValues enum), `ImdsSupport` (xmlName imdsSupport, target ImdsSupportValues enum — v2.0), `SourceInstanceId` (xmlName sourceInstanceId, doc "The ID of the instance that the AMI was created from if the AMI was created using CreateImage"), `SourceImageId` (xmlName sourceImageId, doc "The ID of the source AMI from which the AMI was created") and `SourceImageRegion` (xmlName sourceImageRegion, doc "The Region of the source AMI"). The rule: every one of these describes the AMI AS REGISTERED — architecture, root-device type, virtualization type, block device mappings and tags are the values supplied at registration (for an Image Builder output AMI, derived from the build instance and the image recipe: the recipe's architecture, an EBS root device with the recipe's block device mapping, hvm virtualization, the enhanced-networking/boot-mode/IMDS attributes of the registered snapshotted instance, and the Image Builder tags the service applies, which the recording shows as the keys Ec2ImageBuilderArn and CreatedBy), and the three source* members plus sourceInstanceId record the provenance of an AMI created from an instance/source AMI. None of these are service-chosen defaults.
Emulator: for the AMI the Image Builder build registered, DescribeImages answers 200 but the item is a placeholder: architecture is reported as 'i386' where the registration is a 64-bit x86 build, rootDeviceType as 'instance-store' where the AMI is EBS-backed, virtualizationType as 'paravirtual' where the AMI is hvm, blockDeviceMapping and tagSet are serialized as EMPTY strings rather than as the modeled list structures (blockDeviceMapping/item with the root device's ebs sub-structure; tagSet/item with the Image Builder tag keys), and sriovNetSupport, enaSupport, bootMode, imdsSupport, sourceInstanceId, sourceImageId and sourceImageRegion are absent altogether.
Why: a client reading this AMI back gets an image it cannot use and cannot trust. Launch tooling that gates on architecture/virtualizationType/rootDeviceType picks the wrong instance types or refuses the AMI outright (i386 + paravirtual + instance-store describes an image no modern instance type can run); anything that sizes or snapshots root volumes from blockDeviceMapping sees no volumes, and the empty-string serialization is not even the modeled list type, so a strict XML-to-struct deserializer in an SDK can fail to parse the member rather than merely see it empty; tag-based discovery of Image Builder output AMIs (Ec2ImageBuilderArn / CreatedBy) matches nothing; and the absent enaSupport/sriovNetSupport/bootMode/imdsSupport/source* members hide both the networking and boot capabilities callers branch on and the provenance used to audit where an AMI came from.
Fix: the EC2 DescribeImages read path in the ec2 service crate — the Image projection/serializer that renders imagesSet/item (it must emit blockDeviceMapping and tagSet as the modeled BlockDeviceMappingList/TagList structures rather than empty scalars, and must not substitute defaults for architecture/rootDeviceType/virtualizationType) and the AMI registration state it reads from (the register path, including the Image Builder-driven registration, must persist the architecture, root device type and mapping, virtualization type, ena/sriov/bootMode/imdsSupport attributes, the service-applied tags, and the sourceInstanceId/sourceImageId/sourceImageRegion provenance).
Traces: imagebuilder-create-image-builds-an-available-ami#task#21.

(Related but NOT covering: open issue 20261116T091224Z-ec2-iam-imagebuilder-the-ec2-image-catalog-is.md reports that the EC2 image catalog is not Region-scoped, a different fault in the same catalog, and it does not carry this fixture step.)

## Evidence
Replay of the simparity-wire-aws-v1 wire fixture corpus against the emulator built from this checkout (emulator up, 339 steps, 307 passed).

Fixture imagebuilder-create-image-builds-an-available-ami, step task#21, service ec2, operation DescribeImages, verdict mismatch, expected status 200 / actual status 200 — the body diverges on twelve members of $.DescribeImagesResponse.imagesSet.item:
  architecture: value_mismatch (expected 'x86_64', got 'i386')
  rootDeviceType: value_mismatch (expected 'ebs', got 'instance-store')
  virtualizationType: value_mismatch (expected 'hvm', got 'paravirtual')
  blockDeviceMapping: type_mismatch (expected the modeled list structure — in the recording {'item': {'deviceName': '/dev/xvda', 'ebs': {'snapshotId': <not asserted>, 'volumeSize': '8', 'deleteOnTermination': 'true', 'volumeType': 'gp3', 'iops': '3000', 'encrypted': 'false', 'throughput': '125'}}} — got '' (an empty string))
  tagSet: type_mismatch (expected the modeled list structure carrying the Image Builder tag keys — in the recording {'item': [{'key': 'Ec2ImageBuilderArn', 'value': <the image's Image Builder ARN>}, {'key': 'CreatedBy', 'value': 'EC2 Image Builder'}]} — got '' (an empty string))
  sriovNetSupport: missing (expected 'simple', got None)
  enaSupport: missing (expected 'true', got None)
  bootMode: missing (expected 'uefi-preferred', got None)
  imdsSupport: missing (expected 'v2.0', got None)
  sourceInstanceId: missing (expected present, value not asserted, got None)
  sourceImageId: missing (expected the source AMI id of the build's parent image, got None)
  sourceImageRegion: missing (expected the Region of that source AMI, got None)

The quoted values are the recording's; the requirement is that each member reflect the AMI's own registration and recipe, not that these literals appear.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Replay fixture imagebuilder-create-image-builds-an-available-ami and stop at step task#21 — the fixture creates an Image Builder component, image recipe, infrastructure configuration and image, waits for the image to reach AVAILABLE, reads it back with GetImage (task#20), then calls ec2 DescribeImages on the AMI id the build produced.
4. Inspect $.DescribeImagesResponse.imagesSet.item: compare architecture, rootDeviceType and virtualizationType against the recipe/build, check that blockDeviceMapping and tagSet are serialized as list structures (blockDeviceMapping/item, tagSet/item) rather than empty scalars, and check that sriovNetSupport, enaSupport, bootMode, imdsSupport, sourceInstanceId, sourceImageId and sourceImageRegion are present.
5. Re-replay imagebuilder-create-image-builds-an-available-ami#task#21 to confirm the cause is closed.
