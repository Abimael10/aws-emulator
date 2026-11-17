Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-spot-fleet-request-lifecycle-monitoring
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-spot-fleet-request-lifecycle-monitoring ec2-trajwire-deb-deb-al2023-ami-lookup-ssm-and-describe-images
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeImages|2xx->2xx|$.DescribeImagesResponse.imagesSet.item[*]
Issue-Commit: 688abb98d0ad3ae2c07107aa110c7330aa2e99f9

# ec2 DescribeImages result set does not match the public AMI catalog AWS serves

> _Note: this issue was identified against the code base at commit `688abb98d0ad3ae2c07107aa110c7330aa2e99f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Two net-new causes, both DescribeImages, both the same fault seen from opposite sides: the set of rows DescribeImages returns for a filtered query does not match the set AWS returns, because the emulator serves its own synthesized image catalogue rather than reproducing the public Amazon-owned AMI catalogue and because its filter evaluation does not narrow the set the same way. Related open issues 20261116T091224Z-ec2-iam-imagebuilder-the-ec2-image-catalog-is.md (catalogue not Region-scoped) and 20261117T002936Z-ec2-describeimages-returns-placeholder-attributes-for-an-image.md (placeholder attributes) describe neighbouring faults but carry neither of these replay keys.

Cause 1 of 2: a filtered DescribeImages returns no row where AWS returns the public Amazon Linux 2 AMI.
AWS: recorded behavior only. For the recorded filter, imagesSet contains an Amazon-owned public AMI whose row is fully populated: imageId, imageLocation ('amazon/<name>'), imageState 'available', imageOwnerId (the Amazon Linux publisher account), creationDate, isPublic 'true', architecture, imageType 'machine', sriovNetSupport 'simple', imageOwnerAlias 'amazon', name, description, rootDeviceType 'ebs', rootDeviceName, a blockDeviceMapping item with an ebs block (snapshotId, volumeSize, deleteOnTermination, volumeType, encrypted), virtualizationType 'hvm', hypervisor 'xen', enaSupport 'true', platformDetails 'Linux/UNIX', usageOperation 'RunInstances', deprecationTime, freeTierEligible and publicSsmParameterName. The particular image id, name, snapshot id and dates in the recording are that catalogue's values, not the requirement; the requirement is that the publicly offered Amazon AMI matching the filter be returned, with those members populated.
Emulator: 200 with imagesSet[0] absent entirely — the query matches nothing.
Why: every bootstrap path that resolves a base AMI by filtering DescribeImages (owner-alias amazon plus a name pattern) gets an empty set and cannot launch anything; callers see this as "no such image in this region".
Fix: the AMI catalogue in the ec2 service crate's state (it must carry the public Amazon-owned images with the members above) and the DescribeImages handler's filter evaluation.
Traces: ec2-spot-fleet-request-lifecycle-monitoring#task#1.

Cause 2 of 2: a filtered DescribeImages returns an extra row AWS does not return.
AWS: recorded behavior only. For the recorded name-filtered query AWS returns exactly three rows; there is no fourth element in imagesSet.
Emulator: 200 with a fourth imagesSet item present — an Amazon Linux 2023 row (imageLocation 'amazon/al2023-ami-...', imageOwnerAlias 'amazon', imageState 'available') that the filter should have excluded, so the result set over-matches.
Why: a caller that resolves "the" AMI by taking the newest row of a filtered DescribeImages, or that asserts on the row count, picks an image AWS would not have offered for that filter — and the choice is silent.
Fix: the DescribeImages handler's filter evaluation (name/owner-alias matching) in the ec2 service crate, together with the catalogue rows it filters over.
Traces: ec2-trajwire-deb-deb-al2023-ami-lookup-ssm-and-describe-images#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1. Two mismatch clusters, service=ec2, empty stub_path, empty owned_by:

1. ec2-spot-fleet-request-lifecycle-monitoring#task#1 DescribeImages 200->200: $.DescribeImagesResponse.imagesSet.item[0] value_mismatch — expected a full Amazon-owned public AMI row {'imageId': 'ami-009387cae09958285', 'imageLocation': 'amazon/amzn2-ami-hvm-2.0.20261013.0-x86_64-gp2', 'imageState': 'available', 'imageOwnerId': '137112412989', 'creationDate': ..., 'isPublic': 'true', 'architecture': 'x86_64', 'imageType': 'machine', 'sriovNetSupport': 'simple', 'imageOwnerAlias': 'amazon', 'name': ..., 'description': ..., 'rootDeviceType': 'ebs', 'rootDeviceName': '/dev/xvda', 'blockDeviceMapping': {...}, 'virtualizationType': 'hvm', 'hypervisor': 'xen', 'enaSupport': 'true', 'platformDetails': 'Linux/UNIX', 'usageOperation': 'RunInstances', 'deprecationTime': ..., 'freeTierEligible': 'true', 'publicSsmParameterName': ...}, got None.
2. ec2-trajwire-deb-deb-al2023-ami-lookup-ssm-and-describe-images#task#4 DescribeImages 200->200: $.DescribeImagesResponse.imagesSet.item[3] value_mismatch — expected None, got an extra al2023 row {'imageId': 'ami-00000000000000065', 'imageLocation': 'amazon/al2023-ami-2023.5.20240903.0-kernel-6.1-x86_64', 'imageState': 'available', 'imageOwnerAlias': 'amazon', ...}.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Re-replay ec2-spot-fleet-request-lifecycle-monitoring#task#1 — the ec2Query DescribeImages response must contain the public Amazon-owned AMI row matching the request's filters — and ec2-trajwire-deb-deb-al2023-ami-lookup-ssm-and-describe-images#task#4, whose imagesSet must contain only the rows matching the request's name filter (no fourth item).
