Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.copy-fpga-image-invalid-source-region-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.copy-fpga-image-invalid-source-region-rejected|run#3|AssertionError|assert notfound_code == "InvalidFpgaImageID.NotFound", (
Issue-Commit: 2d1d2911b5095265487653cd392f1f2d36a8c5ca

# CopyFpgaImage collapses a well-formed-but-unknown source AFI id into InvalidFpgaImageID.Malformed instead of InvalidFpgaImageID.NotFound

> _Note: this issue was identified against the code base at commit `2d1d2911b5095265487653cd392f1f2d36a8c5ca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyFpgaImage

## Description
Cause 1 of 1: CopyFpgaImage has only ONE refusal for any source AFI it cannot resolve, so a well-formed AFI id that simply names no AFI is refused as malformed rather than as not found. Failing check id: ec2.copy-fpga-image-invalid-source-region-rejected.

AWS: recorded behavior only — the EC2 Smithy model declares no errors for CopyFpgaImage (smithy_query operation=CopyFpgaImage on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json returns errors: []), and SourceFpgaImageId/SourceRegion are the only two required members. The recording shows EC2 separating two distinct refusals of the SAME request shape:
  (a) a SourceRegion value that names no Region -> HTTP 400, code InvalidParameterValue, message "Region name '<SourceRegion>' is invalid." — fixed text "Region name '" ... "' is invalid.", variable part the SourceRegion the request sent;
  (b) the same SourceFpgaImageId with a VALID SourceRegion, where the id is inside the issued AFI-id form but names no AFI of the account -> HTTP 400, code InvalidFpgaImageID.NotFound, message "Image ID '<SourceFpgaImageId>' not found." — fixed text "Image ID '" ... "' not found.", variable part the SourceFpgaImageId the request sent.
The recorded (b) therefore establishes that AWS treats an `afi-` id of the short issued form as WELL FORMED and reports it as a lookup miss, and that the region check and the image lookup are two independent refusals with two different codes.

Emulator: step (a) matches, but step (b) diverges. With a valid SourceRegion and an unknown source AFI id the emulator answers code InvalidFpgaImageID.Malformed (message "Image ID '<id>' is invalid.", from crate::state::invalid_fpga_image_id_malformed) where AWS answers InvalidFpgaImageID.NotFound with "Image ID '<id>' not found.". Replay output: `AssertionError: expected InvalidFpgaImageID.NotFound once the region is valid, got 'InvalidFpgaImageID.Malformed'`. The code assertion fails first, so the message assertion never ran, but the emulator's malformed sentence ("... is invalid.") is a second divergence from AWS's not-found sentence ("... not found.") on the same path. This is deliberate in the handler, which documents one single answer — 400 InvalidFpgaImageID.Malformed — for every unresolvable source, both a value that is not an AFI id at all and a well-formed id the store holds no AFI under.

Why: a client cannot tell a typo/garbage identifier from an AFI that does not exist (or was deleted) in this account. Code-driven retry and error-handling logic that branches on InvalidFpgaImageID.NotFound (the code DeleteFpgaImage and ResetFpgaImageAttribute already answer for the same condition) never fires, and callers that surface the message show "Image ID 'afi-...' is invalid." for an id that is perfectly valid, sending users to fix a well-formed id instead of to create or look up the AFI. It also makes CopyFpgaImage inconsistent with the rest of the service's AFI operations.

Fix: the CopyFpgaImage handler's source-resolution path and its refusal helper. The form check and the store lookup must answer with DIFFERENT refusals: a value outside the issued AFI-id form keeps InvalidFpgaImageID.Malformed, while a well-formed id the store holds no AFI under must answer InvalidFpgaImageID.NotFound with the "Image ID '<id>' not found." sentence — the helper the service already has for exactly this condition. The ordering stays malformed-before-not-found, and the SourceRegion validation stays ahead of both, as the recording shows.

Traces: ec2.copy-fpga-image-invalid-source-region-rejected. Code citations: services/service-com-amazonaws-ec2-amazonec2/src/ops/copy_fpga_image.rs lines 127-146 (unresolvable_source_fpga_image_id, the single collapsed refusal), lines 193-201 (form check), lines 292-296 (store lookup reusing the SAME malformed refusal), and the module doc rule 1 at lines 9-21 which states the collapse; the correct helper is crate::state::invalid_fpga_image_id_not_found, already used by DeleteFpgaImage and ResetFpgaImageAttribute.

## Evidence
Check: ec2.copy-fpga-image-invalid-source-region-rejected [mismatch]

Step 1 (passes): CopyFpgaImage(SourceFpgaImageId=<unknown afi id>, SourceRegion=<a string that is no Region>)
  Expected: 400 InvalidParameterValue, "Region name '<SourceRegion>' is invalid."
  Actual:   matches.

Step 2 (fails): CopyFpgaImage(SourceFpgaImageId=<same unknown afi id>, SourceRegion=<the caller's own valid Region>)
  Expected (AWS, from the recording): HTTP 400, Error.Code = "InvalidFpgaImageID.NotFound", Error.Message = "Image ID '<SourceFpgaImageId>' not found."
  Actual (emulator):                 HTTP 400, Error.Code = "InvalidFpgaImageID.Malformed", Error.Message = "Image ID '<SourceFpgaImageId>' is invalid."

Replay output:
  File "check_ec2_copy_fpga_image_invalid_source_region_rejected.py", line 50, in run
    assert notfound_code == "InvalidFpgaImageID.NotFound", (
  AssertionError: expected InvalidFpgaImageID.NotFound once the region is valid, got 'InvalidFpgaImageID.Malformed'

The check also asserts the two refusals differ from each other (they do), so the divergence is specifically the classification of the image-lookup miss, not the region check.

Smithy: CopyFpgaImage input = {SourceFpgaImageId (required), SourceRegion (required), Name, Description, ClientToken, DryRun}; output = {FpgaImageId}; errors = [] (EC2 query protocol models no per-operation errors), so the error code and message are established by the recording.

## Steps to Reproduce
1. Boot the emulator and point an EC2 client at it in some region R.
2. Confirm the account owns no AFIs: ec2.describe_fpga_images(Owners=["self"]) returns an empty FpgaImages list.
3. Call ec2.copy_fpga_image(SourceFpgaImageId="<a well-formed but unknown afi- id>", SourceRegion="<a string that names no Region>") and observe the refusal: 400 InvalidParameterValue, "Region name '<that string>' is invalid." (correct).
4. Call ec2.copy_fpga_image(SourceFpgaImageId="<the SAME afi- id>", SourceRegion=R) — i.e. the identical request with a valid Region.
5. Observe Error.Code == "InvalidFpgaImageID.Malformed" and Error.Message == "Image ID '<id>' is invalid."; AWS answers Error.Code == "InvalidFpgaImageID.NotFound" and Error.Message == "Image ID '<id>' not found.".
6. For contrast, call ec2.delete_fpga_image(FpgaImageId="<the same id>") — that operation does answer InvalidFpgaImageID.NotFound for the same condition, showing the inconsistency inside the service.

Replay the suite check directly: ec2.copy-fpga-image-invalid-source-region-rejected
