Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__codepipeline__ecr.codebuild-missing-ecr-login-fails-docker-push codebuild__codepipeline__ecr.codebuild-role-missing-ecr-push-permission-blocks-image-upload codebuild__codepipeline__ecr.ecr-repository-policy-denies-codebuild-role-blocks-pull
Issue-Key: codebuild__codepipeline__ecr.codebuild-missing-ecr-login-fails-docker-push|run#11|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: codebuild__codepipeline__ecr.codebuild-role-missing-ecr-push-permission-blocks-image-upload|run#13|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: codebuild__codepipeline__ecr.ecr-repository-policy-denies-codebuild-role-blocks-pull|run#17|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# codebuild__codepipeline__ecr: ECR DescribeImages' ImageNotFoundException message does not echo the requested image identifier — it reports a zeroed imageDigest and imageTag:'null' instead of the imageTag that was asked for

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: ECR DescribeImages renders the ImageNotFoundException message from a synthesized image identifier rather than from the imageIds entry in the request, so the message names an all-zero sha256 digest and a null tag instead of the tag the caller asked about. Failing checks: codebuild__codepipeline__ecr.codebuild-missing-ecr-login-fails-docker-push, codebuild__codepipeline__ecr.codebuild-role-missing-ecr-push-permission-blocks-image-upload, codebuild__codepipeline__ecr.ecr-repository-policy-denies-codebuild-role-blocks-pull.

AWS: Smithy: DescribeImages takes imageIds: ImageIdentifierList of ImageIdentifier{imageDigest, imageTag} and declares the error ImageNotFoundException (client, HTTP 400, "The image requested does not exist in the specified repository"). Recorded behavior for the message: when the request names an image by tag only, AWS echoes the requested identifier back, rendering the member that was NOT supplied as the literal null:
    The image with imageId {imageDigest:'null', imageTag:'<requested tag>'} does not exist within the repository with name '<repositoryName>' in the registry with id '<registryId>'
Fixed text: "The image with imageId {imageDigest:'", "', imageTag:'", "'} does not exist within the repository with name '", "' in the registry with id '". Variable parts, all taken from the request and the resolved registry: imageDigest and imageTag are the two members of the requested ImageIdentifier, each rendered as the literal null when the caller omitted it; repositoryName is the request's repositoryName; registryId is the resolved registry id (the caller's account when the request omits registryId). The error code (ImageNotFoundException) and the HTTP 400 status are already correct in the emulator.

Emulator: the two identifier members are swapped and both are wrong — the digest slot carries a fabricated all-zero digest and the tag slot carries null, while the requested tag is dropped entirely:
    The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name '<repo>' in the registry with id '<registry id>'
The repositoryName and registryId halves of the sentence are correct; only the imageId rendering diverges. This reproduces identically for three different requested tags across three checks, so it is one message-construction site, not a per-case accident.

Why: the imageId in this message is the only place a caller learns WHICH image was not found. A client that resolves an image by tag — a CI job verifying that a push landed, a deploy step resolving a tag to a digest, or any log-scraping/diagnostic path — is told an all-zero digest is missing and never sees its own tag. Worse, the fabricated digest is a syntactically valid sha256, so a caller or operator reading the message is pointed at an image identifier that was never requested and does not exist.

Fix: the serializer/message construction for ECR's ImageNotFoundException on the DescribeImages read path — build the imageId rendering from the ImageIdentifier entry in the request's imageIds (emitting the literal null for whichever of imageDigest/imageTag the caller omitted) instead of from a default/zero-valued identifier.

Traces: codebuild__codepipeline__ecr.codebuild-missing-ecr-login-fails-docker-push (requested tag 'no-login'), codebuild__codepipeline__ecr.codebuild-role-missing-ecr-push-permission-blocks-image-upload (requested tag 'blocked'), codebuild__codepipeline__ecr.ecr-repository-policy-denies-codebuild-role-blocks-pull (requested tag 'pull-verified'). In each case the negative half of the check is what trips: the build correctly failed and the image correctly does not exist, and DescribeImages correctly answers ImageNotFoundException — only the message's imageId is wrong. No open ECR issue carries this cause: 20261116T084727Z-ecr-paginated-and-image-read-paths-return-wrong.md covers pagination and imageSizeInBytes on the SUCCESS path, and 20261111T000311Z-ecr-read-path-responses-diverge-listimages-refuses-a.md covers ListImages/GetAuthorizationToken; neither names this message.

## Evidence
Three checks, one identical divergence. In each, ecr.describe_images(repositoryName=<repo>, imageIds=[{"imageTag": <tag>}]) correctly raises ImageNotFoundException (code assertion passes) but with the wrong message.

1) codebuild-missing-ecr-login-fails-docker-push (tag 'no-login'):
  expected: "The image with imageId {imageDigest:'null', imageTag:'no-login'} does not exist within the repository with name '<repo>' in the registry with id '<registry id>'"
  got:      "The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name '<repo>' in the registry with id '<registry id>'"

2) codebuild-role-missing-ecr-push-permission-blocks-image-upload (tag 'blocked'):
  expected: "...{imageDigest:'null', imageTag:'blocked'}..."
  got:      "...{imageDigest:'sha256:0000...0000', imageTag:'null'}..."

3) ecr-repository-policy-denies-codebuild-role-blocks-pull (tag 'pull-verified'):
  expected: "...{imageDigest:'null', imageTag:'pull-verified'}..."
  got:      "...{imageDigest:'sha256:0000...0000', imageTag:'null'}..."

The requested tag differs in every case and is dropped in every case; the fabricated digest is byte-identical in every case. The repository names and the registry id in the recorded messages are per-run/per-environment values and are not part of the requirement — the requirement is that imageDigest/imageTag be echoed from the request's imageIds entry, with the omitted member rendered as the literal null.

## Steps to Reproduce
1. Create an ECR repository, e.g. repositoryName = "r1".
2. Do not push any image tagged "wanted-tag" to it.
3. Call ECR DescribeImages with repositoryName="r1" and imageIds=[{"imageTag": "wanted-tag"}] (omit imageDigest, and omit registryId so the caller's account is used).
4. Observe the error:
   - Code: ImageNotFoundException (correct), HTTP 400 (correct).
   - Message observed: "The image with imageId {imageDigest:'sha256:0000000000000000000000000000000000000000000000000000000000000000', imageTag:'null'} does not exist within the repository with name 'r1' in the registry with id '<account>'"
   - Message expected: "The image with imageId {imageDigest:'null', imageTag:'wanted-tag'} does not exist within the repository with name 'r1' in the registry with id '<account>'"
5. Repeat with imageIds=[{"imageDigest": "sha256:<some absent digest>"}] — the message must then echo that digest and render imageTag:'null'.
