Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected
Issue-Key: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected|run#10|AssertionError|assert (code, message) == ('ImageNotFoundException', expected), (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# ECR DescribeImages' ImageNotFoundException does not echo the requested imageTag — it reports a zero imageDigest and imageTag 'null'

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: the not-found refusal of a tag lookup in an existing but empty repository names the wrong half of the image reference.
AWS: Smithy ecr DescribeImages models ImageNotFoundException (client error, HTTP 400) alongside RepositoryNotFoundException and InvalidParameterException; a DescribeImages naming an existing repository and imageIds=[{imageTag: <tag>}] with no matching image raises ImageNotFoundException whose message echoes the requested reference. Its fixed text is "The image with imageId {imageDigest:'<digest>', imageTag:'<tag>'} does not exist within the repository with name '<repositoryName>' in the registry with id '<registryId>'"; the variable parts are taken from the request — the imageDigest and imageTag exactly as supplied, with an omitted member rendered as the literal null — plus the repository name from the request and the registry id of the account.
Emulator: the same call raises ImageNotFoundException (correct class and status) but the message is built from a synthesized identifier instead of the request: the digest is reported as an all-zero sha256 and the tag the caller actually asked for is reported as null — i.e. the two members are inverted relative to the request.
Why: the message is the only place the API tells a caller WHICH half of the reference was missing. A client (or a human reading a build log) is told a digest it never sent does not exist, and that it asked for no tag, so 'tag not pushed yet' is indistinguishable from 'digest mismatch'.
Fix: the DescribeImages handler's image-lookup failure path — the ImageNotFoundException message must be formatted from the ImageIdentifier in the request, not from a default/zero identifier.
Failing check: batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected

## Evidence
[mismatch] batch__ecr__lambda__logs.refarch-batch-empty-repository-tag-latest-not-found-rejected — AssertionError: DescribeImages for tag latest on an empty repository raised 'ImageNotFoundException': "The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name '<repo>' in the registry with id '<account>'", expected ImageNotFoundException "The image with imageId {imageDigest:'null', imageTag:'latest'} does not exist within the repository with name '<repo>' in the registry with id '<account>'". Fixed text is identical; only the echoed imageDigest/imageTag values diverge (the repository name and registry id in the recording are per-run values that fill the template).

## Steps to Reproduce
1. CreateRepository (no images pushed).
2. Call DescribeImages(repositoryName=<repo>, imageIds=[{'imageTag': 'latest'}]).
3. Inspect the ImageNotFoundException message: it must echo imageDigest:'null', imageTag:'latest'.
