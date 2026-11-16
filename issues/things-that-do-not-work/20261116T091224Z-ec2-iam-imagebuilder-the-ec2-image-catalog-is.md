Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__imagebuilder.ibp-pipeline-and-parent-image-arns-resolve-only-in-their-own-region
Issue-Key: ec2__iam__imagebuilder.ibp-pipeline-and-parent-image-arns-resolve-only-in-their-own-region|_refusal#13|AssertionError|raise AssertionError(f"expected a refusal, but the call succeeded: {resp!r}")
Issue-Commit: a1915270e40223bfb23a5a7de86fe74aa15a3df0

# ec2__iam__imagebuilder: the EC2 image catalog is not Region-scoped — DescribeImages in one Region resolves an AMI that exists only in another, instead of refusing InvalidAMIID.NotFound

> _Note: this issue was identified against the code base at commit `a1915270e40223bfb23a5a7de86fe74aa15a3df0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: an AMI id that belongs to one Region is described successfully by another Region's EC2 endpoint, so the image catalogue is a single global store.
AWS: Smithy: DescribeImages (com.amazonaws.ec2#AmazonEC2) declares errors: [] — EC2 has no modeled errors, so this is recorded behavior only. AMIs are Region-scoped: an AMI id (including an Amazon-owned public AMI backing an Image Builder parent image such as arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x) resolves only through the endpoint of the Region that holds it. DescribeImages(ImageIds=[<id from another Region>]) is refused with error code `InvalidAMIID.NotFound` and the message "The image id '[<ami-id>]' does not exist" — fixed text "The image id '[" ... "]' does not exist" with the requested AMI id substituted from the request, and nothing returned. Correspondingly, imagebuilder:GetImage for a parent image ARN of another Region is refused `ResourceNotFoundException` with "The following required resource 'Image' cannot be found: '<arn>'."
Emulator: the cross-Region DescribeImages answers HTTP 200 with the full Image structure of the other Region's AMI (State 'available', OwnerId '137112412989', the Amazon Linux 2 name/location and its snapshot id), so no refusal is raised and the region-scoping assertion never gets its expected error. Open issue 20261111T133150Z-ec2-4-net-new-causes-resources-leak-across.md describes the same shared-store symptom for key pairs, subnets and security groups, and 20261104T142958Z-ec2-regional-endpoints-share-one-store-cross-region.md for other resources; neither carries this check or names the image catalogue, so the AMI/Image Builder parent-image path is still unfixed.
Why: a client's multi-Region AMI topology collapses into one — an AMI id copied from the wrong Region is accepted instead of refused, a distribution that is supposed to be visible only in its destination Region appears everywhere, and region-isolation checks (an Image Builder parent image or a distributed AMI resolving only in its own Region) cannot be validated at all. The refusal is also the signal clients use to decide whether a copy/distribution is still required.
Fix: the EC2 service's image state and the DescribeImages handler — key the image catalogue (public Amazon-owned AMIs included) by Region and resolve ImageIds only within the request's Region, refusing unknown ids with InvalidAMIID.NotFound and AWS's message template.
Traces: ec2__iam__imagebuilder.ibp-pipeline-and-parent-image-arns-resolve-only-in-their-own-region.

## Evidence
Expected (us-east-1 describing an AMI that only us-east-2 holds): error code InvalidAMIID.NotFound with message "The image id '[<ami-id>]' does not exist" (fixed text around the AMI id taken from the request).
Actual: "AssertionError: expected a refusal, but the call succeeded: {'Images': [{... 'Name': 'amzn2-ami-hvm-2.0.20241113.1-x86_64-gp2', 'ImageId': 'ami-0000000000000001f', 'ImageLocation': 'amazon/amzn2-ami-hvm-2.0.20241113.1-x86_64-gp2', 'State': 'available', 'OwnerId': '137112412989', 'Public': True, ...}], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}" — HTTP 200 and the other Region's image. (The generated ami-/snap- ids and the request id are per-run values.)
The check's control step passed: the same AMI id does resolve through the us-east-2 endpoint, and the us-east-2 parent image's outputResources.amis lists only us-east-2 — so the failure is specifically the home Region answering for a foreign AMI id.

## Steps to Reproduce
Replay the behavioral suite and re-run ec2__iam__imagebuilder.ibp-pipeline-and-parent-image-arns-resolve-only-in-their-own-region. Individually:
1. In us-east-2, call imagebuilder:GetImage(imageBuildVersionArn='arn:aws:imagebuilder:us-east-2:aws:image/amazon-linux-2-x86/x.x.x') and take image.outputResources.amis[0].image (the us-east-2 AMI id).
2. Call ec2:DescribeImages(ImageIds=[<that id>]) against the us-east-2 endpoint — it must return that one image.
3. Call ec2:DescribeImages(ImageIds=[<that id>]) against the us-east-1 endpoint — expect a refusal ('InvalidAMIID.NotFound', "The image id '[<ami-id>]' does not exist"); the emulator returns HTTP 200 with the image.
4. For the same scoping rule on the Image Builder side, call imagebuilder:GetImage in us-east-1 for the us-east-2 parent ARN — expect ('ResourceNotFoundException', "The following required resource 'Image' cannot be found: '<arn>'.").
