Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecr__ecs.ecsdeploy-task-definition-image-tags-missing-from-repo-rejected
Issue-Key: ec2__ecr__ecs.ecsdeploy-task-definition-image-tags-missing-from-repo-rejected|run#13|AssertionError|assert got_err == want_err, (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ec2__ecr__ecs: ECR's ImageNotFoundException for a task definition's unpushed image tag names an all-zero placeholder digest instead of the requested tag

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: ImageNotFoundException does not echo the imageId the caller asked for.
AWS: Smithy com.amazonaws.ecr#ImageNotFoundException is a client error, HTTP 400, doc "The image requested does not exist in the specified repository." Its message is built from the requested com.amazonaws.ecr#ImageIdentifier, whose imageDigest and imageTag members are both optional. Recorded behavior shows the fixed text `The image with imageId {imageDigest:'<imageDigest>', imageTag:'<imageTag>'} does not exist within the repository with name '<repositoryName>' in the registry with id '<registryId>'`; the variable parts are the requested identifier's imageDigest and imageTag (each rendered as the bare literal `null` when the request omitted it), the request's repositoryName and the resolved registry id.
Emulator: a DescribeImages lookup for an absent tag is refused with the right code and status but with imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000' and imageTag:'null' — an all-zero placeholder digest substituted for a digest never sent, and the requested tag discarded.
Why: ECS accepts a task definition whose container images do not exist yet; ECR is then the component that tells the deployer which image references are still unpushed. With the requested tag erased from the refusal, a deploy gate or diagnostic that enumerates a task definition's image tags and reads each refusal cannot attribute the failure to a tag, and all of them look like the same bogus digest lookup.
Fix: the ECR ImageNotFoundException message builder behind DescribeImages (shared with BatchGetImage) — render the requested ImageIdentifier's own imageDigest/imageTag, emitting `null` only for a member the request omitted.
Traces: ec2__ecr__ecs.ecsdeploy-task-definition-image-tags-missing-from-repo-rejected.
Related: the same root cause is filed for the `ecr`, `batch__ecr__lambda__logs` and `codebuild__codepipeline__ecr` service headings, because an issue carries one service token.

## Evidence
ec2__ecr__ecs.ecsdeploy-task-definition-image-tags-missing-from-repo-rejected — DescribeImages for the absent tag '1.0' derived from a registered task definition's container image:
  expected: ('ImageNotFoundException', "The image with imageId {imageDigest:'null', imageTag:'1.0'} does not exist within the repository with name '<repo>' in the registry with id '<account>'")
  actual:   ('ImageNotFoundException', "The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name '<repo>' in the registry with id '<account>'")
Error code and HTTP status are correct; only the echoed imageId diverges.

## Steps to Reproduce
1. ecr.create_repository(repositoryName=R).
2. ecs.register_task_definition with a container image of the form <registry>/R:1.0 that has never been pushed (ECS accepts it).
3. ecr.describe_images(repositoryName=R, imageIds=[{'imageTag': '1.0'}]).
Expected: ImageNotFoundException echoing imageDigest:'null', imageTag:'1.0'. Actual: the message echoes an all-zero sha256 digest and imageTag:'null'.
