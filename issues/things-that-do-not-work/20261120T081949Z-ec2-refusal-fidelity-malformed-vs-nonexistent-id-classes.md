Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-copy-fpga-image-error-handling
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-copy-fpga-image-error-handling ec2-fpga-image-error-paths ec2-t12-eni-attach-and-private-ip ec2-volume-snapshot-copy-lifecycle ec2-describe-application-status-check-associations-contract ec2-create-tags-delete-tags-key-pair-existence
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CopyFpgaImage|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CopyFpgaImage|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DetachNetworkInterface|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateSnapshot|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateSnapshot|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DescribeApplicationStatusCheckAssociations|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeApplicationStatusCheckAssociations|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DeleteTags|2xx->4xx|$.DeleteTagsResponse
Issue-Key: ec2|DeleteTags|2xx->4xx|status
Issue-Related: 20261117T065015Z-ec2-refusal-error-codes-and-messages-for-malformed.md
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# EC2 refusal fidelity: malformed-vs-nonexistent id classes, unknown filter name and an over-strict DeleteTags

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyFpgaImage

## Description
Five net-new causes in the ec2 emulator, all of them refusal-verdict defects: the emulator picks the wrong error class (and therefore the wrong message template) for a resource identifier that fails EC2's id-shape/checksum check versus one that is merely absent, mislabels an unknown filter NAME as a bad parameter VALUE, and refuses a DeleteTags call AWS answers 200. Each cause is reached by its own replayed fixture step; the status code is right in four of them, so the divergence is entirely in the error code and message a client branches on.

=== Cause 1 of 5: CopyFpgaImage reports a shape-invalid source AFI id as not-found ===
AWS: recorded behavior only - the EC2 Smithy model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares CopyFpgaImage with required SourceFpgaImageId and models no errors (ec2Query carries untyped errors), so the recorded response is the contract. AWS refuses an AFI id that does not pass EC2's long-id validation with HTTP 400, error code `InvalidFpgaImageID.Malformed` and message `Image ID '<source-fpga-image-id>' is invalid.` - fixed text "Image ID '" and "' is invalid.", variable part the SourceFpgaImageId taken verbatim from the request.
Emulator: HTTP 400 but error code `InvalidFpgaImageID.NotFound` with message `Image ID '<source-fpga-image-id>' not found.` - the class EC2 reserves for a checksum-valid id this account does not hold.
Why: a client cannot distinguish a fabricated/typo'd id (a permanent client fault) from a real AFI that was deleted (a lifecycle fault), so retry and error-mapping logic keyed on the Malformed class never fires.
Fix: the operation's input validation - services/service-com-amazonaws-ec2-amazonec2/src/ops/copy_fpga_image.rs: run the AFI id-shape/checksum check before the state lookup and raise InvalidFpgaImageID.Malformed with AWS's template. The open issue 20261118T110421Z-ec2-copyfpgaimage-collapses-a-well-formed-but-unknown-source.md describes the OPPOSITE mapping (a well-formed unknown id answered Malformed); both directions must hold at once - shape failure => Malformed, shape-valid-but-absent => NotFound - and this fixture step is not covered by that issue.
Traces: ec2-copy-fpga-image-error-handling#task#2 (cluster also covers ec2-fpga-image-error-paths#task#1).

=== Cause 2 of 5: DetachNetworkInterface reports a shape-invalid attachment id as not-found ===
AWS: recorded behavior only - the model declares DetachNetworkInterface with required AttachmentId and no modeled errors. AWS refuses an attachment id that fails the id-shape check with HTTP 400, error code `InvalidNetworkInterfaceAttachmentId.Malformed` and message `Invalid id: "<attachment-id>"` - fixed text `Invalid id: "` and the closing `"`, variable part the AttachmentId from the request (note AWS uses double quotes around the id in this template, not single quotes).
Emulator: HTTP 400 with error code `InvalidAttachmentID.NotFound` and message `The interface attachment '<attachment-id>' does not exist.`
Why: besides the wrong class, the emulator's code is not even the code AWS uses for an absent ENI attachment, so neither branch of a client's handling matches; a malformed id is reported as a transient resource state.
Fix: the operation's input validation - services/service-com-amazonaws-ec2-amazonec2/src/ops/detach_network_interface.rs: validate the eni-attach- id shape first and raise InvalidNetworkInterfaceAttachmentId.Malformed with AWS's message template.
Traces: ec2-t12-eni-attach-and-private-ip#task#9.

=== Cause 3 of 5: CreateSnapshot reports a shape-valid but absent VolumeId as malformed ===
AWS: recorded behavior only - the model declares CreateSnapshot.VolumeId as required and models no errors. For a volume id that passes EC2's long-id validation but is not held by the account, AWS answers HTTP 400 with error code `InvalidVolume.NotFound` and message `The volume '<volume-id>' does not exist.` - fixed text "The volume '" and "' does not exist.", variable part the VolumeId from the request.
Emulator: HTTP 400 with error code `InvalidVolumeID.Malformed` and message `Value ( <volume-id> ) for parameter VOLUME is invalid.` - it now classifies every unknown volume id as malformed.
Why: this is the mirror image of cause 1 and it breaks the same client branch from the other side: a legitimately deleted or not-yet-visible volume is reported as a permanent client fault, so a caller that treats Malformed as non-retryable gives up on a lifecycle race. The behavioral issue 20261119T232925Z-ec2-4-net-new-causes-applysecuritygroupstoclientvpntargetnetwork-invents-an.md asks for Malformed on a FABRICATED id; it carries no wire replay key and does not cover this step - both rules must coexist (bad checksum => InvalidVolumeID.Malformed, valid checksum but absent => InvalidVolume.NotFound).
Fix: the operation's validation/state lookup ordering - services/service-com-amazonaws-ec2-amazonec2/src/ops/create_snapshot.rs.
Traces: ec2-volume-snapshot-copy-lifecycle#task#1.

=== Cause 4 of 5: DescribeApplicationStatusCheckAssociations misclassifies an unknown filter name ===
AWS: recorded behavior only - the model declares DescribeApplicationStatusCheckAssociations with Filters (FilterList) and models no errors. Every EC2 Describe* operation refuses an unrecognized filter NAME with HTTP 400, error code `InvalidFilter` and message `Invalid filter name: '<name>'` - fixed text "Invalid filter name: '" and "'", variable part Filters[].Name taken verbatim from the request.
Emulator: HTTP 400 with error code `InvalidParameterValue` and message `The filter '<name>' is invalid` - EC2's class for a bad filter VALUE or scalar parameter, with a reworded message and no trailing period parity.
Why: filter-name and filter-value faults are distinct in EC2; generic SDK/tooling layers that special-case InvalidFilter to report "this API has no such filter" never fire, and the caller is told its value was wrong instead of its filter name.
Fix: this operation's filter validation and the shared filter-name check it should route through - services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_application_status_check_associations.rs: raise InvalidFilter before any value or pagination validation. The behavioral issue named under cause 3 lists the same root cause as its cause 3 but carries no wire replay key for this step.
Traces: ec2-describe-application-status-check-associations-contract#task#1.

=== Cause 5 of 5: DeleteTags refuses a deletion AWS accepts ===
AWS: recorded behavior only - the model declares DeleteTags with required Resources and optional Tags, and documents it as "Deletes the specified set of tags from the specified set of resources" with no modeled errors; the recorded response for this step is HTTP 200 carrying `<DeleteTagsResponse>` with a requestId and `<return>true</return>`. EC2's tag deletion is idempotent: a tag key that is not present on the resource is ignored rather than refused.
Emulator: the same request is refused HTTP 400 and no DeleteTagsResponse body is produced at all.
Why: a client tearing down tags (or re-running an idempotent cleanup) gets a hard failure on a call AWS treats as a no-op, so teardown and convergence loops abort.
Fix: the operation's validation/state path - services/service-com-amazonaws-ec2-amazonec2/src/ops/delete_tags.rs: resolve the named resources, drop the listed keys that are present, and answer 200 with return=true instead of refusing when a key (or the tag set) is already absent.
Traces: ec2-create-tags-delete-tags-key-pair-existence#task#6.

## Evidence
Cause 1 - fixture ec2-copy-fpga-image-error-handling step task#2 (CopyFpgaImage), status 400 expected and 400 observed:
  $.Response.Errors.Error.Code: expected 'InvalidFpgaImageID.Malformed', got 'InvalidFpgaImageID.NotFound'
  $.Response.Errors.Error.Message: expected "Image ID 'afi-0123456789abcdef0' is invalid.", got "Image ID 'afi-0123456789abcdef0' not found."
  (the afi- id is the request's SourceFpgaImageId; the fixed text is what is required)

Cause 2 - fixture ec2-t12-eni-attach-and-private-ip step task#9 (DetachNetworkInterface), 400 vs 400:
  Code: expected 'InvalidNetworkInterfaceAttachmentId.Malformed', got 'InvalidAttachmentID.NotFound'
  Message: expected 'Invalid id: "eni-attach-0123456789abcdef0"', got "The interface attachment 'eni-attach-0123456789abcdef0' does not exist."

Cause 3 - fixture ec2-volume-snapshot-copy-lifecycle step task#1 (CreateSnapshot), 400 vs 400:
  Code: expected 'InvalidVolume.NotFound', got 'InvalidVolumeID.Malformed'
  Message: expected "The volume 'vol-0b40dcc22096bb199' does not exist.", got 'Value ( vol-0b40dcc22096bb199 ) for parameter VOLUME is invalid.'

Cause 4 - fixture ec2-describe-application-status-check-associations-contract step task#1, 400 vs 400:
  Code: expected 'InvalidFilter', got 'InvalidParameterValue'
  Message: expected "Invalid filter name: 'sim2real-not-a-real-filter'", got "The filter 'sim2real-not-a-real-filter' is invalid"

Cause 5 - fixture ec2-create-tags-delete-tags-key-pair-existence step task#6 (DeleteTags):
  status: expected 200, got 400
  $.DeleteTagsResponse: expected {'requestId': <shaped>, 'return': 'true'}, got None

## Steps to Reproduce
Cause 1: ec2 CopyFpgaImage(SourceFpgaImageId="afi-0123456789abcdef0", SourceRegion=<region>) -> expect 400 InvalidFpgaImageID.Malformed / "Image ID 'afi-0123456789abcdef0' is invalid."; observe InvalidFpgaImageID.NotFound / "... not found.".
Cause 2: ec2 DetachNetworkInterface(AttachmentId="eni-attach-0123456789abcdef0") -> expect 400 InvalidNetworkInterfaceAttachmentId.Malformed / 'Invalid id: "eni-attach-0123456789abcdef0"'; observe InvalidAttachmentID.NotFound.
Cause 3: ec2 CreateSnapshot(VolumeId="vol-0b40dcc22096bb199") for a volume this account does not hold -> expect 400 InvalidVolume.NotFound / "The volume 'vol-0b40dcc22096bb199' does not exist."; observe InvalidVolumeID.Malformed.
Cause 4: ec2 DescribeApplicationStatusCheckAssociations(Filters=[{"Name":"sim2real-not-a-real-filter","Values":["x"]}]) -> expect 400 InvalidFilter / "Invalid filter name: 'sim2real-not-a-real-filter'"; observe InvalidParameterValue.
Cause 5: replay fixture ec2-create-tags-delete-tags-key-pair-existence to step task#6 (CreateTags on a key pair, then DeleteTags) -> expect 200 with <return>true</return>; observe 400 with no DeleteTagsResponse.
