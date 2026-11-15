Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.image-scan-on-archived-image-rejected
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.image-scan-on-archived-image-rejected|run#9|AssertionError|assert control_result == {
Issue-Commit: 9f0ba49f597317e71eb5c77786cda0fc5584f7cf

# ECR basic-scan FAILED description is a single constant: an image with no identifiable OS/package manager reports 'The image is not supported.' instead of AWS's OS/package-manager wording (1 cause)

> _Note: this issue was identified against the code base at commit `9f0ba49f597317e71eb5c77786cda0fc5584f7cf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImageScanFindings

## Description
One root cause, in ECR.

Context on already-filed work that does NOT cover this check (the replay report marked this check neither ALREADY FILED nor OWNED by any open issue): the resolved issue 20261030T073809Z-ecr-image-scan-status-surface-reports-the-wrong.md (Cause 2) fixed the OTHER unsupported-image recording — a wire fixture where AWS serves "UnsupportedImageError: The image is not supported." — by replacing the then-current sentence with that one as a single constant. The resolved issue 20261025T025458Z-ecr-basic-image-scanning-can-never-reach-complete.md asked for the terminal status to be DERIVED from the scanned image and explicitly recorded the contrast case this check now grades. This is NOT a REOPENS: the call is not refused, DescribeImageScanFindings answers HTTP 200 with a terminal status; the diverging value is the description sentence.

Cause 1 of 1: the terminal FAILED scan description is one constant for every unsupported image, so the basic-scan "no operating system / no package manager" outcome is reported with the wrong sentence.
AWS: Smithy (ecr): StartImageScan and DescribeImageScanFindings both return imageScanStatus targeting ImageScanStatus{status: ScanStatus, description: ScanStatusDescription}; ScanStatus values are [IN_PROGRESS, COMPLETE, FAILED, UNSUPPORTED_IMAGE, ACTIVE, PENDING, SCAN_ELIGIBILITY_EXPIRED, FINDINGS_UNAVAILABLE, LIMIT_EXCEEDED, IMAGE_ARCHIVED]; ScanStatusDescription is an unconstrained string documented only as "The description of the image scan status", so the wording is recorded behavior only. Recorded behavior: with BASIC scanning enabled registry-wide (PutRegistryScanningConfiguration scanType=BASIC, rules=[]), StartImageScan on a pushed single-layer image whose layer is an EMPTY gzipped tar — no OS release file and no package database — settles at the terminal status FAILED with description "UnsupportedImageError: The operating system and/or package manager are not supported." (HTTP 200). The whole sentence is FIXED text: it has no part taken from the request or from state, and the "UnsupportedImageError: " prefix is the machine-readable part clients key on. The separately recorded sentence "UnsupportedImageError: The image is not supported." belongs to a DIFFERENT unsupported-image outcome (the one captured by wire fixture api-image-scanning-and-findings), so ECR words at least two distinct unsupported outcomes differently and the description is a function of WHY the image could not be examined. Hypothesis: the OS/package-manager sentence is the one basic scanning reports when it reached the image content but found nothing it can enumerate packages from, while the shorter sentence covers an image type/format basic scanning does not scan at all.
Emulator: answers HTTP 200 with imageScanStatus {"status": "FAILED", "description": "UnsupportedImageError: The image is not supported."} for the OS-less image above, i.e. the right status and prefix followed by the sentence belonging to the other outcome. The description is a single compile-time constant used for every non-COMPLETE basic scan, so only one of AWS's two unsupported sentences is reachable for any image.
Why: the scan-status description is the only place the reason an image was not scanned is reported (the read still answers 200, not an error), so a client that matches or surfaces that sentence — a CI gate that distinguishes "this image has no package manager, nothing to scan" from "this image type is not scannable", or a dashboard echoing the reason — gets a different reason text from the emulator than from AWS for the same image, and cannot tell the two outcomes apart at all. It also halts the rest of this check (the ImageArchivedException / ScanNotFoundException / UpdateImageStorageClass restore assertions after it never ran), so those behaviors are currently ungraded.
Fix: the ECR basic-scan outcome evaluation and its status-description constants, in the ECR crate's state: the unsupported description must be selected by the outcome basic_scan_outcome derives from the scanned image and its stored layer blobs rather than being one constant for all of them, with the OS/package-manager sentence used for an image whose content carries no identifiable operating system or package manager. Code sites (for orientation only): services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state.rs lines 264-269 (UNSUPPORTED_IMAGE_SCAN_DESCRIPTION) and lines 2387-2420 (Repo::start_basic_scan, which calls basic_scan_outcome / basic_scan_findings).
Traces: ecr.image-scan-on-archived-image-rejected.

## Evidence
Cause 1 — behavioral check ecr.image-scan-on-archived-image-rejected, verdict [mismatch]:

  AssertionError: {'status': 'FAILED', 'description': 'UnsupportedImageError: The image is not supported.'}

The failing assertion is the check's positive control, before anything is archived:

  assert control_result == {
      "status": "FAILED",
      "description": "UnsupportedImageError: The operating system and/or package manager are not supported.",
  }, control_result

where control_result is the terminal imageScanStatus polled out of describe_image_scan_findings (poll accepts COMPLETE/FAILED/UNSUPPORTED_IMAGE, budget 120 s, interval 5 s) for an image pushed by the check as: an empty gzipped tar layer, a config JSON {architecture: amd64, os: linux, config: {Labels: {...nonce}}, rootfs: {type: layers, diff_ids: [<layer digest>]}}, and a schemaVersion 2 application/vnd.docker.distribution.manifest.v2+json manifest naming that config and that single layer. Registry scanning was set to scanType=BASIC with rules=[] first.

Expected (recorded AWS): status 'FAILED', description 'UnsupportedImageError: The operating system and/or package manager are not supported.'
Actual (emulator): status 'FAILED', description 'UnsupportedImageError: The image is not supported.'
Status matches; only the description sentence diverges. Both sides HTTP 200.

Source of the constant in the checkout: services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state.rs:268-269 defines UNSUPPORTED_IMAGE_SCAN_DESCRIPTION = "UnsupportedImageError: The image is not supported." and its doc comment states it is the wording for "a run whose image carries no operating system basic scanning can examine" — that is exactly the case this check exercises, and AWS words it differently.

Model facts used: ecr#ImageScanStatus{status: ScanStatus, description: ScanStatusDescription}; ecr#ScanStatus values [IN_PROGRESS, COMPLETE, FAILED, UNSUPPORTED_IMAGE, ACTIVE, PENDING, SCAN_ELIGIBILITY_EXPIRED, FINDINGS_UNAVAILABLE, LIMIT_EXCEEDED, IMAGE_ARCHIVED]; ecr#ScanStatusDescription is an unconstrained string (no wording fixed by the model); DescribeImageScanFindings returns imageScanStatus + imageScanFindings and models ScanNotFoundException; StartImageScan models ImageArchivedException.

## Steps to Reproduce
Against the emulator's ECR endpoint:
1. get_registry_scanning_configuration() to capture the current configuration, then put_registry_scanning_configuration(scanType='BASIC', rules=[]) so only an explicit StartImageScan triggers a scan (restore the original afterwards).
2. create_repository(repositoryName=<name>).
3. Build the minimal image: layer = an EMPTY gzipped tar (tarfile.open(mode='w:gz') with nothing added); config = {"architecture": "amd64", "os": "linux", "config": {"Labels": {"nonce": <uuid>}}, "rootfs": {"type": "layers", "diff_ids": ["sha256:"+sha256(layer)]}}.
4. Push both blobs with initiate_layer_upload / upload_layer_part / complete_layer_upload, then put_image(repositoryName=<name>, imageManifest=<schemaVersion 2 manifest.v2+json naming the config and the one layer>, imageTag='control'). Keep image['imageId']['imageDigest'].
5. start_image_scan(repositoryName=<name>, imageId={'imageDigest': d}).
6. Poll describe_image_scan_findings(repositoryName=<name>, imageId={'imageDigest': d}) until imageScanStatus.status is terminal and read imageScanStatus: the emulator reports {'status': 'FAILED', 'description': 'UnsupportedImageError: The image is not supported.'}; AWS reports {'status': 'FAILED', 'description': 'UnsupportedImageError: The operating system and/or package manager are not supported.'}.
7. Regression to keep: the other recorded unsupported outcome (wire fixture api-image-scanning-and-findings) must still report 'UnsupportedImageError: The image is not supported.' — the two sentences must be selected by the outcome, not collapsed to one constant again.
8. Once the description matches, the rest of this check becomes reachable and must also hold: UpdateImageStorageClass(targetStorageClass='ARCHIVE') then StartImageScan on that image raises ImageArchivedException, DescribeImageScanFindings on it raises ScanNotFoundException, and UpdateImageStorageClass(targetStorageClass='STANDARD') returns imageStatus in (ACTIVATING, ACTIVE).
