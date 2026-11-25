Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.image-register-describe-deregister-lifecycle ec2.op-contract-deregister-image ec2.snapshot-in-use-by-registered-image-delete-rejected ec2.op-contract-restore-image-from-recycle-bin
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.image-register-describe-deregister-lifecycle|run#24|AssertionError|assert final == [], f"expected DescribeImages to list no deregistered image, got {final!r}"
Issue-Key: ec2|DescribeImages|InvalidAMIID.Malformed
Issue-Key: ec2.op-contract-restore-image-from-recycle-bin|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# ec2: 2 net-new causes — DescribeImages refuses a deregistered AMI id InvalidAMIID.Malformed instead of answering an empty Images list, and the Recycle Bin AMI restore round trip reaches no verdict

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Two net-new root causes in the ec2 AMI slice. Both must be closed before this issue is submitted.

=== Cause 1 of 2: DescribeImages on a just-deregistered, well-formed AMI id is refused `InvalidAMIID.Malformed` instead of returning 200 with an empty `Images` list ===

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2) declares NO modelled errors for DescribeImages or DeregisterImage (ec2Query; errors are recorded behavior only), and `DescribeImagesResult$Images` (xmlName `imagesSet`) is a list — "Describes the specified images". Recorded behavior: once DeregisterImage succeeds, a DescribeImages naming that id answers 200 with an EMPTY `Images` list, immediately and durably; the id is not reclassified. `InvalidAMIID.Malformed` with message `Invalid id: "<value>"` is reserved for a value that is not shaped like an AMI id at all (e.g. `ami-not-a-valid-image-id`) — the fixed text is `Invalid id: ` followed by the caller's value in double quotes. `InvalidAMIID.NotFound` is reserved for a well-formed id that never existed. A second DeregisterImage on the dead id is refused `InvalidAMIID.Unavailable` with the fixed text `The image ID '` + the request's image id + `' is no longer available`.

Emulator: after DeregisterImage succeeds, `DescribeImages(ImageIds=[<that id>])` is refused 400 `InvalidAMIID.Malformed` with message `Invalid id: "ami-000000000000000a9"` — the emulator's own freshly minted, well-formed id (`ami-` plus 17 lowercase hex characters). The classification is produced before the existence check, in services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_images.rs lines 318-331, where `!image_id_is_well_formed(image_id) || store.image_id_identifies_another_resource(image_id)` yields `invalid_ami_id_malformed(image_id)`; once the image row is gone the deregistered id falls into that malformed branch rather than producing an empty page. The id value quoted here is a per-run value; the requirement is that any id the emulator itself minted and then deregistered must read back as an empty `Images` list.

Why: a client that deregisters an AMI and then polls DescribeImages to confirm it is gone gets a hard 400 that its error handling reads as "you sent garbage", not "the image is gone". Botocore raises instead of returning a page, so AMI-cleanup loops, CloudFormation-style deletion waiters and any "is it deregistered yet" poll abort; worse, the wrong code tells the caller its id is syntactically invalid, which is false and unactionable. The malformed/not-found/empty-page distinction is exactly what callers key retry logic on.

Fix: the DescribeImages handler's id-resolution block in services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_images.rs (and the shared `image_id_identifies_another_resource` / deregistered-image bookkeeping in crate::state it calls): a syntactically valid AMI id that this account deregistered must select no image and return 200 with an empty `Images` list, `Malformed` must stay restricted to values that are not AMI ids, and `NotFound` to well-formed ids that never existed.

Failing checks for Cause 1: ec2.image-register-describe-deregister-lifecycle, ec2.op-contract-deregister-image, ec2.snapshot-in-use-by-registered-image-delete-rejected.

=== Cause 2 of 2: the Recycle Bin AMI retain-and-restore round trip reaches no verdict — the check is cut off at the harness cap with nothing observable ===

AWS: Smithy models ListImagesInRecycleBin (output `ListImagesInRecycleBinResult$Images`, xmlName `imageSet`, paginated on NextToken/MaxResults 1-1000) and RestoreImageFromRecycleBin (output `Return`), with no modelled errors on either. Recorded behavior: with an available Recycle Bin retention rule whose ResourceTags match, DeregisterImage puts the AMI in the bin instead of destroying it; ListImagesInRecycleBin then lists that ImageId, RestoreImageFromRecycleBin(ImageId) returns Return=true, and DescribeImages reports the AMI `available` again. RestoreImageFromRecycleBin on an id the bin holds nothing for is refused `InvalidAMIID.Malformed` with the fixed text `The image ID '` + the request's image id + `' is malformed`.

Emulator: the check ends `Check timed out after 60.0s` with no verdict. ListImagesInRecycleBin is still an unimplemented scaffold stub — services/service-com-amazonaws-ec2-amazonec2/src/ops/list_images_in_recycle_bin.rs returns `Err(crate::scaffolded::Scaffolded::not_implemented("ListImagesInRecycleBin"))` (line 158) — so the bin is never observable, the bounded poll for a binned id never settles and the scenario exhausts its wall clock before reaching RestoreImageFromRecycleBin at all.

Why: a client cannot verify that a deregistered AMI was retained, cannot find its id in the bin, and therefore cannot restore it — the entire Recycle Bin recovery path for EBS-backed AMIs is unusable even though RestoreImageFromRecycleBin itself is implemented.

Fix: implement the ListImagesInRecycleBin handler in services/service-com-amazonaws-ec2-amazonec2/src/ops/list_images_in_recycle_bin.rs over the recycled-image store that `restore_image_from_recycle_bin.rs` already reads through `crate::state::require_recycled_image`, including its NextToken/MaxResults paging, and confirm DeregisterImage files the AMI in the bin while a matching rbin EC2_IMAGE rule is available.

Failing checks for Cause 2: ec2.op-contract-restore-image-from-recycle-bin. The auto-filed stub issue for ListImagesInRecycleBin names that operation, but the replay did not mark this check as covered by it, so the scenario is recorded here; if the stub is implemented first, re-replay this check before closing.

## Evidence
Cause 1 — Expected: `DeregisterImage(ImageId=ami-X)` succeeds, then `DescribeImages(ImageIds=[ami-X])` answers HTTP 200 with `Images: []` (and the id is absent from `DescribeImages(Owners=["self"])`), while an unparseable value such as `ami-not-a-valid-image-id` is refused 400 `InvalidAMIID.Malformed` message `Invalid id: "ami-not-a-valid-image-id"`, and a second DeregisterImage on ami-X is refused `InvalidAMIID.Unavailable` message `The image ID 'ami-X' is no longer available`.
Actual: `botocore.exceptions.ClientError: An error occurred (InvalidAMIID.Malformed) when calling the DescribeImages operation: Invalid id: "ami-000000000000000a9"` (and `Invalid id: "ami-000000000000000aa"` on the sibling check) — the emulator's own minted, well-formed id reported as malformed. In ec2.image-register-describe-deregister-lifecycle the poll records the refusal as the final state: `AssertionError: expected DescribeImages to list no deregistered image, got ('InvalidAMIID.Malformed', 'Invalid id: "ami-000000000000000a9"')`. Verdicts: mismatch, error, error.

Cause 2 — Expected: with an available rbin EC2_IMAGE retention rule matching the AMI's tags, DeregisterImage bins the AMI, ListImagesInRecycleBin lists its ImageId, RestoreImageFromRecycleBin returns Return=true and DescribeImages reports the AMI `available` again; RestoreImageFromRecycleBin("ami-1234567890abcdef0") is refused `InvalidAMIID.Malformed` message `The image ID 'ami-1234567890abcdef0' is malformed`.
Actual: `Error: Check timed out after 60.0s` — no verdict. ListImagesInRecycleBin answers 501 NotImplemented (services/service-com-amazonaws-ec2-amazonec2/src/ops/list_images_in_recycle_bin.rs:158), so the bounded search of the bin never finds the deregistered AMI. Verdict: mismatch (timeout).

## Steps to Reproduce
Cause 1:
1. CreateVolume(Size=1) in an available AZ, wait for `available`; CreateSnapshot on it, wait for `completed`.
2. RegisterImage with Architecture=x86_64, RootDeviceName=/dev/xvda, VirtualizationType=hvm and a BlockDeviceMapping whose Ebs.SnapshotId is that snapshot; note the returned ImageId and wait for state `available`.
3. DeregisterImage(ImageId=<that id>).
4. Call DescribeImages(ImageIds=[<that id>]).
5. Observe HTTP 400 InvalidAMIID.Malformed `Invalid id: "<that id>"` instead of 200 with an empty Images list.

Cause 2:
1. rbin CreateRule(ResourceType="EC2_IMAGE", RetentionPeriod=1 DAYS, ResourceTags=[{k,v}]) and wait for Status `available`.
2. CreateVolume + CreateSnapshot, then RegisterImage tagged with {k,v}; wait for the AMI to be `available`.
3. DeregisterImage on it.
4. Call ListImagesInRecycleBin (unfiltered) looking for that ImageId, then RestoreImageFromRecycleBin on it.
5. Observe ListImagesInRecycleBin answers 501 NotImplemented, so the AMI is never found in the bin and the scenario exhausts its time budget with no verdict.
