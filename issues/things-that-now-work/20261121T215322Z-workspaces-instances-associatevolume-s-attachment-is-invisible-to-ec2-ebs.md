Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: workspaces-instances.associate-volume-duplicate-device-rejected
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances.associate-volume-duplicate-device-rejected|run#28|AssertionError|assert len(attachments_a) == 1, f"expected volume_a's 1 attachment untouched, got {attachments_a!r}"
Issue-Commit: 73bfb72a32d657999c670b9c4ea935969cd38be3

# AssociateVolume's attachment is invisible to EC2/EBS: after a duplicate-device rejection, ec2:DescribeVolumes reports the first (successfully attached) volume as having no attachments

> _Note: this issue was identified against the code base at commit `73bfb72a32d657999c670b9c4ea935969cd38be3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateVolume

## Description
One root cause (Cause 1 of 1).

Cause 1 of 1 — AssociateVolume persists the attachment only in the workspaces-instances crate's own store and never propagates it to the Amazon EBS volume state, so the attachment it creates is not observable through EC2/EBS. This check is NOT the one the open issue 20261116T070652Z-workspaces-instances-associatevolume-disassociatevolume-never-create-or-remove-the-ebs.md carries (that issue's replay key names only workspaces-instances.associate-disassociate-volume-roundtrip, and it describes the attach/detach roundtrip rather than the post-rejection invariant), so it is filed here and names that issue as the closely related one; a fix must satisfy both.

AWS: Smithy (services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/smithy-model/com-amazonaws-workspacesinstances-eucmifrontendapiservice.json) — AssociateVolume input {WorkspaceInstanceId required, pattern ^wsinst-[0-9a-zA-Z]{8,63}$, length 15..70; VolumeId required, pattern ^vol-[0-9a-zA-Z]{1,63}$; Device required, length 0..32}, AssociateVolumeResponse models NO members, errors {ValidationException 400, ConflictException 409, ResourceNotFoundException 404, AccessDeniedException 403, ThrottlingException 429, InternalServerException 500}. Because the response carries no members, the ONLY observable effect of a successful AssociateVolume is the EBS attachment: the VolumeId is an Amazon EBS volume of the caller's account and the WorkSpace Instance is backed by a real EC2 instance (GetWorkspaceInstance.EC2ManagedInstance.InstanceId), so after a successful AssociateVolume, ec2:DescribeVolumes(VolumeIds=[<that vol- id>]) resolves that volume with exactly one Attachments entry whose InstanceId is the instance's EC2 instance id and whose Device is the Device member of the request. AWS also keeps that attachment untouched when a LATER AssociateVolume of a DIFFERENT volume at the SAME device is refused: the refusal is ValidationException (HTTP 400) whose message is fixed text with values taken from the request and state — "Ec2 can't associate volume <VolumeId of the refused request> to instance <the instance's EC2ManagedInstance.InstanceId> (InvalidParameterValue: Invalid value '<Device>' for unixDevice. Attachment point '<Device>' is already in use)" with the device string from the request — and a refused call writes nothing, so the first volume keeps its one attachment and the refused volume has none.

Emulator: the refusal itself is right — the emulator returns ValidationException with exactly that message (the check's code and message assertions pass), which proves it DID record the first attach in its private store. But ec2:DescribeVolumes on the FIRST volume, the one whose AssociateVolume returned HTTP 200, answers Attachments == [] instead of one attachment, so the check fails at `assert len(attachments_a) == 1 ... got []`. src/ops/associate_volume.rs line 302 writes `volume.attachment = Some(crate::state::VolumeAttachment { workspace_instance_id, device })` on this crate's own VolumeRecord and makes no write into EBS state — unlike CreateVolume/DeleteVolume in the same crate, which do reach EBS (crate::state::create_ebs_volume / delete_ebs_volume), which is why the check gets past its 'volume becomes available' waits.

Why: a client that attaches volumes through workspaces-instances observes, snapshots, monitors or reconciles them through EC2/EBS, because AssociateVolume's response models nothing to observe. Such a client sees every attached volume as permanently unattached; any wait-for-attached loop never terminates (it hangs rather than fails), and the post-rejection invariant a caller checks after a duplicate-device refusal — "my first volume is still attached, only the second call was rejected" — reads as "the first attachment was lost too", which looks like the refusal rolled back unrelated state.

Fix: the state write of the AssociateVolume handler (and symmetrically DisassociateVolume) — propagate the attachment into the EBS volume state that owns the volume, the same way CreateVolume/DeleteVolume already propagate creation and deletion, so the attachment is visible to ec2:DescribeVolumes and survives a subsequent refused attach of another volume at the same device.

Traces: workspaces-instances.associate-volume-duplicate-device-rejected (file and line citations above are from services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/ops/associate_volume.rs).

## Evidence
Check workspaces-instances.associate-volume-duplicate-device-rejected, verdict mismatch:

  File "check_workspaces_instances_associate_volume_duplicate_device_rejected.py", line 87, in run
    assert len(attachments_a) == 1, f"expected volume_a's 1 attachment untouched, got {attachments_a!r}"
  AssertionError: expected volume_a's 1 attachment untouched, got []

Assertions that PASSED before it, pinning the divergence: the duplicate-device AssociateVolume was refused with code ValidationException and a message exactly equal to "Ec2 can't associate volume <volume_b> to instance <ec2_instance_id> (InvalidParameterValue: Invalid value '/dev/sdf' for unixDevice. Attachment point /dev/sdf is already in use)" (fixed text; <volume_b> and <ec2_instance_id> and the device '/dev/sdf' are values from the request/state), and ec2:DescribeVolumes on volume_b correctly reported no attachments.

Expected: ec2:DescribeVolumes(VolumeIds=[volume_a])['Volumes'][0]['Attachments'] == [{InstanceId: <GetWorkspaceInstance.EC2ManagedInstance.InstanceId>, Device: '/dev/sdf', ...}] — one entry, untouched by the refused second call.
Actual: Attachments == [] — the EBS volume state never learned about the attach at all; the emulator's own store did (hence the correct duplicate-device refusal), so the two views disagree.
Source: src/ops/associate_volume.rs line 302 sets `volume.attachment` on the crate-local VolumeRecord only; no call into EBS state exists in that handler, while src/ops/create_volume.rs calls crate::state::create_ebs_volume and src/ops/delete_volume.rs calls crate::state::delete_ebs_volume.

## Steps to Reproduce
1. Boot the multi-service emulator so both workspaces-instances and ec2 are mounted.
2. Ensure the service-linked role exists (iam:CreateServiceLinkedRole for workspaces-instances.amazonaws.com), then CreateWorkspaceInstance in a subnet of the shared network and poll GetWorkspaceInstance until ProvisionState == ALLOCATED; keep EC2ManagedInstance.InstanceId.
3. workspaces-instances CreateVolume twice in the instance's Availability Zone (SizeInGB=8, VolumeType=gp3) -> volume_a, volume_b; wait until ec2:DescribeVolumes reports each 'available'.
4. AssociateVolume(WorkspaceInstanceId=<instance>, VolumeId=volume_a, Device='/dev/sdf') -> HTTP 200.
5. AssociateVolume(WorkspaceInstanceId=<instance>, VolumeId=volume_b, Device='/dev/sdf') -> correctly refused with ValidationException and the 'Attachment point /dev/sdf is already in use' message.
6. ec2:DescribeVolumes(VolumeIds=[volume_b]) -> Attachments == [] (correct).
7. ec2:DescribeVolumes(VolumeIds=[volume_a]) -> Attachments == [], but exactly one attachment to the EC2 instance id from step 2 at device /dev/sdf is expected.
