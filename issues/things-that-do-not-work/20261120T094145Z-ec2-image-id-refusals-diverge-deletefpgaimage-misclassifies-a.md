Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-delete-fpga-image-rejected ec2.op-contract-get-image-ancestry
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-delete-fpga-image-rejected|run#11|AssertionError|assert not_found_code == "InvalidFpgaImageID.NotFound", (
Issue-Key: ec2.op-contract-get-image-ancestry|_error#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: c5b9149d70e2a9aad6a1bceb8aa9795c91a9cab7

# EC2 image-id refusals diverge: DeleteFpgaImage misclassifies a well-formed AFI id as Malformed, GetImageAncestry accepts a nonexistent AMI id (2 net-new causes)

> _Note: this issue was identified against the code base at commit `c5b9149d70e2a9aad6a1bceb8aa9795c91a9cab7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteFpgaImage

## Description
Two net-new root causes in the EC2 image-id handling paths. Both are refusal-fidelity defects on well-formed-but-absent resource ids; neither is covered by an open issue (no failing check for either is marked OWNED or ALREADY FILED in this replay).

Cause 1 of 2: DeleteFpgaImage classifies a well-formed, unassigned AFI id as Malformed instead of NotFound.
AWS: recorded behavior only — Smithy (services/service-com-amazonaws-ec2-amazonec2) declares DeleteFpgaImage with required input member FpgaImageId (target FpgaImageId) and models no errors, so the refusal contract comes from the recording. The recording shows three distinct outcomes for the same operation: with DryRun=true the call is refused with code DryRunOperation and the fixed message "Request would have succeeded, but DryRun flag is set."; an AFI id in the 17-hex-digit long-ID form is rejected on FORMAT alone (AFI ids never adopted the long-ID form) with code InvalidFpgaImageID.Malformed and message "Image ID '<id>' is invalid."; and an id in the valid 8-hex-digit AFI form that is not assigned to any image reaches the existence check and is refused with code InvalidFpgaImageID.NotFound and message "Image ID '<id>' not found." (HTTP 400 in each case). In those messages the fixed text is "Image ID '" ... "' is invalid." / "Image ID '" ... "' not found."; the only variable part is the FpgaImageId exactly as the request sent it — never a value from one run.
Emulator: the 8-hex-digit well-formed id is refused with InvalidFpgaImageID.Malformed, i.e. the format check rejects the valid short AFI form and the existence check is never reached, so NotFound is unreachable for any AFI id.
Why: a client cannot distinguish a malformed id (a caller bug, never retried) from an absent one (already deleted, which makes delete idempotent). Cleanup and teardown code that treats InvalidFpgaImageID.NotFound as "already gone" aborts with a Malformed error instead, and callers that validate input shape locally conclude their own id generation is wrong.
Fix: the DeleteFpgaImage handler's FpgaImageId validation — accept the 8-hex-digit AFI form as well-formed and fall through to the image lookup, which must raise InvalidFpgaImageID.NotFound for an unassigned id; keep Malformed for the 17-hex-digit form and keep DryRun ordered ahead of both.
Traces: ec2.op-contract-delete-fpga-image-rejected

Cause 2 of 2: GetImageAncestry accepts a well-formed but nonexistent AMI id and fabricates an ancestry entry for it.
AWS: recorded behavior only — Smithy declares GetImageAncestry with required input ImageId and output ImageAncestryEntries (ec2QueryName ImageAncestrySet) and models no errors. The recording shows that a well-formed AMI id that does not exist is refused exactly the way DescribeImages refuses one: HTTP 400, code InvalidAMIID.NotFound, message "The image id '[<image-id>]' does not exist" — fixed text "The image id '[" ... "]' does not exist", the one variable part being the ImageId as the request sent it (AWS brackets the id inside the quotes). For an existing AMI the chain is returned with the AMI itself as the first entry.
Emulator: the same call answers HTTP 200 with ImageAncestryEntries holding one entry whose ImageId is the nonexistent id that was asked for, plus an ImageOwnerAlias of "amazon" and a CreationDate — an ancestry invented for an image the emulator does not hold. No refusal is produced at all.
Why: every caller that uses GetImageAncestry to validate or trace an AMI treats a typo'd, deregistered or cross-region id as a live Amazon-owned image. Provenance decisions (is this AMI derived from an approved parent?) are then made from fabricated lineage, and error handling that expects InvalidAMIID.NotFound never runs.
Fix: the GetImageAncestry handler's ImageId resolution — look the id up in image state and refuse an unknown one with InvalidAMIID.NotFound before any ancestry entry is built; only a resolved image may seed the chain.
Traces: ec2.op-contract-get-image-ancestry

## Evidence
Cause 1 — ec2.op-contract-delete-fpga-image-rejected:
Expected (AWS): DeleteFpgaImage(FpgaImageId="afi-12345678") -> HTTP 400, Code=InvalidFpgaImageID.NotFound, Message="Image ID 'afi-12345678' not found." (the 17-hex-digit probe correctly yields InvalidFpgaImageID.Malformed / "Image ID '<id>' is invalid.", and DryRun yields DryRunOperation / "Request would have succeeded, but DryRun flag is set.").
Actual (emulator): AssertionError: expected InvalidFpgaImageID.NotFound, got 'InvalidFpgaImageID.Malformed' — the well-formed short id is refused as malformed, so the existence check never runs.

Cause 2 — ec2.op-contract-get-image-ancestry:
Expected (AWS): GetImageAncestry(ImageId=<well-formed, nonexistent AMI id>) -> HTTP 400, Code=InvalidAMIID.NotFound, Message="The image id '[<that id>]' does not exist".
Actual (emulator): AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'ImageAncestryEntries': [{'CreationDate': ..., 'ImageId': '<the nonexistent id>', 'ImageOwnerAlias': 'amazon'}], ... 'HTTPStatusCode': 200}. The id, timestamp and request id in that payload are per-run values; the requirement is the 400/InvalidAMIID.NotFound refusal, not those values.

## Steps to Reproduce
Replay the behavioral suite and re-run these two checks:
1. ec2.op-contract-delete-fpga-image-rejected — call DeleteFpgaImage with FpgaImageId="afi-00000000000000019" (17 hex digits) and assert InvalidFpgaImageID.Malformed / "Image ID '<id>' is invalid.", then call it with FpgaImageId="afi-12345678" (8 hex digits, unassigned) and assert InvalidFpgaImageID.NotFound / "Image ID '<id>' not found."; also assert DryRun=true gives DryRunOperation with the fixed DryRun message.
2. ec2.op-contract-get-image-ancestry — DescribeImages(Owners=["amazon"]) to pick a live AMI, assert GetImageAncestry on it returns a chain whose first entry is that AMI, then flip the AMI id's last hex digit and assert GetImageAncestry on the resulting (well-formed, absent) id is refused with InvalidAMIID.NotFound and message "The image id '[<id>]' does not exist" instead of a 200 payload.
