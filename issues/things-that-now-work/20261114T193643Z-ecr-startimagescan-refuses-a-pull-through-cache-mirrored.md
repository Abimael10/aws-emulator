Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.e2e-repository-full-lifecycle-scan-and-replicate
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr|StartImageScan|UnsupportedImageTypeException
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ecr: StartImageScan refuses a pull-through-cache mirrored image as unscannable, and its UnsupportedImageTypeException message carries emulator-authored trailing prose (2 causes)

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartImageScan

## Description
Two net-new root causes under the `ecr` heading, both grounded in ecr.e2e-repository-full-lifecycle-scan-and-replicate (the check is NOT marked OWNED or ALREADY FILED by the replay report).

Cause 1 of 2: StartImageScan refuses an image that was mirrored into the registry through a pull-through cache rule, so the repository lifecycle (mirror -> scan -> delete -> refuse) can never be exercised past the scan step.
AWS: Smithy (ecr) StartImageScan(repositoryName required, imageId required) -> imageScanStatus, errors ImageNotFoundException / UnsupportedImageTypeException / RepositoryNotFoundException / InvalidParameterException / LimitExceededException / ImageArchivedException. A basic scan of an ordinary single-platform OS image (here a public.ecr.aws amazonlinux image mirrored by a pull-through cache rule, fetched into the repository by BatchGetImage) is accepted: StartImageScan returns imageScanStatus PENDING/IN_PROGRESS and DescribeImageScanFindings eventually reports imageScanStatus.status COMPLETE with imageScanFindings.imageScanCompletedAt and an integer-valued findingSeverityCounts map. UnsupportedImageTypeException is documented only as "The image is of a type that cannot be scanned." and is reserved for manifests a basic scan genuinely cannot handle (e.g. a multi-arch manifest list or a non-OS artifact), not for a normal mirrored OS image.
Emulator: BatchGetImage does return an image for the mirrored tag, and the image's manifest is not a manifest list (the check therefore scans it by tag), but StartImageScan then refuses every attempt with HTTP 400 UnsupportedImageTypeException. The check only retries ImageNotFoundException, so the refusal is terminal and the whole lifecycle check errors out. The emulator's own message states the reason: the manifest it mirrored "declares none" of the filesystem layers a basic scan needs — i.e. the pull-through mirror materializes a layerless manifest, which the scan path then classifies as unscannable.
Why: a client that mirrors an upstream image through a pull-through cache rule and then scans it (the documented basic-scanning workflow, and the standard CI gate before promoting a mirrored base image) gets a hard 400 that AWS never returns, and can never obtain findings for a mirrored image. Because the refusal is a modeled client error, retry/backoff logic treats it as a permanent property of the image rather than a missing mirror body.
Fix: the pull-through-cache mirror path that materializes the mirrored manifest and its layer references in ECR state, and the StartImageScan handler's scannability classification that reads those layers. Nearest open filings, neither of which carries this check's replay key: 20261024T014332Z-ecr-asynchronous-effects-never-occur-and-several-enforcement.md (pull-through mirroring and other asynchronous effects absent) and 20261114T111637Z-ecr-basic-scan-failed-description-is-a-single.md (the FAILED scan description constant).
Traces: ecr.e2e-repository-full-lifecycle-scan-and-replicate.

Cause 2 of 2: the UnsupportedImageTypeException message appends an emulator-authored explanation to AWS's sentence.
AWS: Smithy (ecr) UnsupportedImageTypeException documentation: "The image is of a type that cannot be scanned." The wire message AWS returns for StartImageScan is exactly the sentence "The image with 'imageId {imageDigest:'<digest or null>', imageTag:'<tag or null>'}' in the repository with name '<repositoryName>' in the registry with id '<registryId>' is of a type that cannot be scanned" and stops there. Fixed text: everything outside the quoted placeholders, including the closing clause "is of a type that cannot be scanned". Variable parts taken from the request and state: the imageDigest and imageTag echoed from the request's imageId (the member the caller omitted is rendered as null), the repositoryName from the request, and the registry id of the caller's account.
Emulator: the same sentence is emitted but continues with ": a basic scan examines the operating system packages in an image's filesystem layers, and its manifest declares none" — prose that is in neither the model nor AWS's response. Status 400 and error code UnsupportedImageTypeException are themselves correct.
Why: clients and tests that match on the error message (log scrapers, CI assertions, SDK-level message comparisons) see text AWS never sends, and the invented explanation attributes the refusal to the image rather than to the emulator's own empty manifest.
Fix: the StartImageScan handler's error construction for UnsupportedImageTypeException (message template), not its status or code.
Traces: ecr.e2e-repository-full-lifecycle-scan-and-replicate.

## Evidence
Check [error] ecr.e2e-repository-full-lifecycle-scan-and-replicate:
  botocore.errorfactory.UnsupportedImageTypeException: An error occurred (UnsupportedImageTypeException) when calling the StartImageScan operation: The image with 'imageId {imageDigest:'null', imageTag:'latest'}' in the repository with name '<prefix>/amazonlinux/amazonlinux' in the registry with id '<account>' is of a type that cannot be scanned: a basic scan examines the operating system packages in an image's filesystem layers, and its manifest declares none

Check body: create_pull_through_cache_rule(ecrRepositoryPrefix=<prefix>, upstreamRegistryUrl="public.ecr.aws"); create_repository(<prefix>/amazonlinux/amazonlinux); poll batch_get_image(imageIds=[{imageTag:"latest"}]) until an image is returned; if the returned manifest mediaType is a manifest list, scan the first child by digest, otherwise scan {"imageTag": "latest"}; poll start_image_scan (retrying only ImageNotFoundException); poll describe_image_scan_findings until imageScanStatus.status leaves PENDING/IN_PROGRESS; assert status == COMPLETE, imageScanCompletedAt present, findingSeverityCounts an int-valued dict; then delete the repository and assert DescribeRepositories/StartImageScan both answer RepositoryNotFoundException with the message "The repository with name '<name>' does not exist in the registry with id '<account>'".

Expected (AWS): the mirrored image is accepted for scanning; StartImageScan answers 200 with an imageScanStatus, and findings settle at COMPLETE. If an image genuinely cannot be scanned, the 400 UnsupportedImageTypeException message ends at "is of a type that cannot be scanned".
Actual (emulator): every StartImageScan attempt answers 400 UnsupportedImageTypeException with an extra trailing clause, so the check never reaches the findings, delete, or post-delete refusal assertions.

Smithy consulted (model services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921): StartImageScan input {registryId, repositoryName required, imageId required} / output {registryId, repositoryName, imageId, imageScanStatus}; errors include UnsupportedImageTypeException (client, httpError 400) documented "The image is of a type that cannot be scanned."

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; ecr.e2e-repository-full-lifecycle-scan-and-replicate reproduces.
2. CreatePullThroughCacheRule(ecrRepositoryPrefix=<prefix>, upstreamRegistryUrl="public.ecr.aws").
3. CreateRepository(repositoryName="<prefix>/amazonlinux/amazonlinux").
4. BatchGetImage(repositoryName=<that name>, imageIds=[{imageTag:"latest"}]) until it returns an image; note the returned imageManifest is a single manifest, not a manifest list.
5. StartImageScan(repositoryName=<that name>, imageId={imageTag:"latest"}). Expected: 200 with imageScanStatus. Actual: 400 UnsupportedImageTypeException whose message ends with ": a basic scan examines the operating system packages in an image's filesystem layers, and its manifest declares none".
6. Re-replay the id in trace_ids to confirm the fix.
