Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected
Issue-Key: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected|run#10|AssertionError|assert (code, message) == ('ImageNotFoundException', expected), (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# batch__ecr__lambda__logs: ECR's ImageNotFoundException for a `latest` lookup on an empty repository names an all-zero placeholder digest instead of the requested tag

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: ImageNotFoundException does not echo the imageId the caller asked for.
AWS: Smithy com.amazonaws.ecr#ImageNotFoundException is a client error, HTTP 400, doc "The image requested does not exist in the specified repository." Its message is built from the requested com.amazonaws.ecr#ImageIdentifier, whose imageDigest and imageTag members are BOTH optional. Recorded behavior shows the fixed text `The image with imageId {imageDigest:'<imageDigest>', imageTag:'<imageTag>'} does not exist within the repository with name '<repositoryName>' in the registry with id '<registryId>'`; the variable parts are the requested identifier's imageDigest and imageTag (each rendered as the bare literal `null` when the request omitted it), the request's repositoryName, and the resolved registry id. The error code is correct: the repository exists, so neither RepositoryNotFoundException nor a silent empty list is right — only ImageNotFoundException, and only with the requested reference echoed.
Emulator: for a DescribeImages lookup by imageTag the emulator reports imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000' and imageTag:'null' — an all-zero placeholder digest substituted for a digest the caller never sent, and the requested tag dropped — while the repositoryName and registryId halves are right.
Why: a Batch/CI client that pulls `latest` and reads the refusal to decide whether the TAG is unpushed (retry after a build) or the DIGEST is gone (fail fast) is told the opposite of what it asked, and every absent-tag refusal produces an identical message regardless of the tag, so the message carries no information about the request.
Fix: the ECR ImageNotFoundException message builder behind DescribeImages (shared with BatchGetImage and the other image-lookup handlers) — render the requested ImageIdentifier's own members, emitting `null` only for a member the request omitted.
Traces: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected.
Related: the same root cause is filed for the `ecr`, `codebuild__codepipeline__ecr` and `ec2__ecr__ecs` service headings, because an issue carries one service token.

## Evidence
batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected — CreateRepository(R) then DescribeImages(repositoryName=R, imageIds=[{imageTag:'latest'}]):
  expected: ImageNotFoundException "The image with imageId {imageDigest:'null', imageTag:'latest'} does not exist within the repository with name '<repo>' in the registry with id '<account>'"
  actual:   ImageNotFoundException "The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name '<repo>' in the registry with id '<account>'"
The error code and HTTP status are correct; only the echoed imageId diverges, and it diverges identically for every requested tag.

## Steps to Reproduce
1. ecr.create_repository(repositoryName=R) — note imageTagMutability defaults to MUTABLE.
2. ecr.describe_images(repositoryName=R) returns imageDetails=[] (positive control, repository resolves).
3. ecr.describe_images(repositoryName=R, imageIds=[{'imageTag': 'latest'}]).
Expected: ImageNotFoundException whose message echoes imageDigest:'null', imageTag:'latest'. Actual: the message echoes an all-zero sha256 digest and imageTag:'null'.
