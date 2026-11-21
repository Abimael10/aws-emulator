Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: workspaces-instances.associate-volume-duplicate-device-rejected workspaces-instances.create-volume-iops-exceeds-range-rejected workspaces-instances.delete-volume-while-attached-rejected
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances.associate-volume-duplicate-device-rejected|run#21|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message}"
Issue-Key: workspaces-instances.create-volume-iops-exceeds-range-rejected|run#15|AssertionError|assert message == (
Issue-Key: workspaces-instances.delete-volume-while-attached-rejected|run#16|AssertionError|assert message == f"EBS Volume is not in available state for volume {volume_id}", message
Issue-Commit: 592b93fbaec935edd06bdfe9abbe351007288c9e

# WorkSpace Instance volume refusals use this service's own wording and the wrong error class: duplicate attach device answers ConflictException instead of ValidationException, and the out-of-range-Iops and attached-volume-delete messages are not AWS's

> _Note: this issue was identified against the code base at commit `592b93fbaec935edd06bdfe9abbe351007288c9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateVolume

## Description
Three net-new root causes in this one service, all in the refusal path of the volume operations. The refusals this service serves for conditions the backing Amazon EBS/EC2 service detects are re-worded in this crate's own voice instead of carrying the wrapped backing-service sentence AWS returns, and in one case they are also classified under the wrong modeled error.

Cause 1 of 3 — AssociateVolume at an already-occupied device name is ConflictException with local wording, not ValidationException carrying EC2's refusal.
AWS: Smithy: AssociateVolume models both ValidationException (HTTP 400) and ConflictException (HTTP 409); input Device (DeviceName, @length 0..32) required. The attach is performed by EC2 against the underlying instance, so when the attachment point is already taken EC2 refuses with InvalidParameterValue and this service surfaces that as ValidationException (HTTP 400) whose message is the wrapped EC2 refusal. Message template — fixed text: "Ec2 can't associate volume " <VolumeId> " to instance " <EC2 instance id> " (InvalidParameterValue: Invalid value '" <Device> "' for unixDevice. Attachment point " <Device> " is already in use)"; variable parts: VolumeId and Device come from the request, the instance id is the WorkSpace Instance's EC2ManagedInstance.InstanceId from this service's state (never a literal id copied from one run).
Emulator: answers ConflictException (HTTP 409) with message "the device is already in use by another volume on the workspace instance" — crate::state::DEVICE_ALREADY_IN_USE_MESSAGE applied through crate::state::resource_conflict in the device-occupied branch of the AssociateVolume handler, which also names the occupying volume as the conflicting resource rather than reporting a rejected parameter value.
Why: the error class is the part a client branches on. Code expecting ValidationException for an unusable Device falls into its ConflictException arm instead, where the documented remedy is to wait or to retry after the conflicting state clears — a state that never clears here, since the request itself is what is wrong. A 409 also marks the call as a state conflict to retry policies that treat 400 as terminal, turning a deterministic bad-parameter rejection into an endless retry.
Traces: workspaces-instances.associate-volume-duplicate-device-rejected.

Cause 2 of 3 — CreateVolume with an out-of-range Iops is refused with this crate's constraint sentence instead of the wrapped EBS refusal.
AWS: Smithy: CreateVolume input Iops (NonNegativeInteger, @range min 0), VolumeType (VolumeTypeEnum); errors include ValidationException (HTTP 400). The per-volume-type IOPS window is enforced by the backing EBS CreateVolume, so an Iops above the type's maximum comes back as ValidationException (HTTP 400 — the emulator matches here) whose message is the wrapped EBS refusal. Message template — fixed text: "Create volume failed (InvalidParameterValue: Volume iops of " <Iops> " is too high; maximum is " <the maximum IOPS of the requested VolumeType> ".)"; variable parts: the Iops value from the request and the type's maximum from the service's own volume-type table (80000 for gp3).
Emulator: answers ValidationException with message "Iops must be from 3000 to 80000 for a gp3 volume" — the field-constraint wording produced by spec.require_accepted_configuration / crate::state::field_validation_failed in validate_configuration of the CreateVolume handler. It states a range instead of reporting the submitted value as too high, and carries neither the "Create volume failed (...)" wrapper nor the InvalidParameterValue code of the refusing service.
Why: callers and operators match on the message to tell an out-of-range performance value from the other CreateVolume rejections (capacity source missing, KmsKeyId without Encrypted, zone not in Region), and the wrapped InvalidParameterValue text is what identifies the refusal as EBS's. The emulator's wording makes that classification fail even though the status and code are right.
Traces: workspaces-instances.create-volume-iops-exceeds-range-rejected.

Cause 3 of 3 — DeleteVolume on an attached volume carries local wording instead of AWS's not-available sentence.
AWS: Smithy: DeleteVolume input VolumeId (^vol-[0-9a-zA-Z]{1,63}$) required; errors include ConflictException (HTTP 409). A volume attached to a WorkSpace Instance is not in the available state, so the delete is refused with ConflictException (HTTP 409 — the emulator matches here). Message template — fixed text: "EBS Volume is not in available state for volume " <VolumeId>; the only variable part is the VolumeId from the request.
Emulator: answers ConflictException with message "Workspace managed EBS volume is attached to a workspace instance and must be detached before it is deleted" — crate::state::VOLUME_IN_USE_MESSAGE applied through crate::state::resource_conflict in the attached-volume branch of the DeleteVolume handler; the message names no volume id at all.
Why: a client cleaning up a set of volumes matches this message to decide whether to detach and retry, and logs/alarms keyed on AWS's sentence do not fire. Because the message omits the VolumeId, a caller deleting several volumes concurrently cannot even tell which volume the refusal is about.
Fix (all three causes): the refusal branches of these three handlers — the device-occupied branch of the AssociateVolume handler (error classification AND message), the Iops/performance validation in the CreateVolume handler's validate_configuration, and the attached-volume branch of the DeleteVolume handler — must emit the modeled error AWS uses with AWS's message template, filling the variable parts from the request and this service's state.

## Evidence
Check workspaces-instances.associate-volume-duplicate-device-rejected (mismatch):
  AssertionError: expected ValidationException, got ConflictException: the device is already in use by another volume on the workspace instance
  Expected: code ValidationException, message "Ec2 can't associate volume <volume_b> to instance <ec2_instance_id> (InvalidParameterValue: Invalid value '/dev/sdf' for unixDevice. Attachment point /dev/sdf is already in use)" where /dev/sdf is the Device sent, volume_b the VolumeId sent and ec2_instance_id the instance's EC2ManagedInstance.InstanceId.
  Source: src/ops/associate_volume.rs lines 256-267 (crate::state::DEVICE_ALREADY_IN_USE_MESSAGE via resource_conflict, variant ConflictException).

Check workspaces-instances.create-volume-iops-exceeds-range-rejected (mismatch):
  AssertionError: Iops must be from 3000 to 80000 for a gp3 volume
  The code assertion (ValidationException) passed; the message assertion failed. Expected: "Create volume failed (InvalidParameterValue: Volume iops of <Iops sent> is too high; maximum is <max for the VolumeType>.)" — for a gp3 request with Iops=999999 that is "...Volume iops of 999999 is too high; maximum is 80000.)".
  Source: src/ops/create_volume.rs validate_configuration -> spec.require_accepted_configuration (lines 192-199) / crate::state::field_validation_failed.

Check workspaces-instances.delete-volume-while-attached-rejected (mismatch):
  AssertionError: Workspace managed EBS volume is attached to a workspace instance and must be detached before it is deleted
  The code assertion (ConflictException) passed; the message assertion failed. Expected: "EBS Volume is not in available state for volume <the VolumeId sent>".
  Source: src/ops/delete_volume.rs lines 182-191 (crate::state::VOLUME_IN_USE_MESSAGE via resource_conflict).

## Steps to Reproduce
Cause 1: boot the emulator with workspaces-instances and ec2 mounted; CreateWorkspaceInstance and wait for ProvisionState ALLOCATED; CreateVolume twice in the instance's AZ; AssociateVolume volume A at Device=/dev/sdf (succeeds); AssociateVolume volume B at the same Device=/dev/sdf — the emulator answers HTTP 409 ConflictException "the device is already in use by another volume on the workspace instance"; expected HTTP 400 ValidationException "Ec2 can't associate volume <B> to instance <the instance's EC2 instance id> (InvalidParameterValue: Invalid value '/dev/sdf' for unixDevice. Attachment point /dev/sdf is already in use)".
Cause 2: CreateVolume with {AvailabilityZone: <a zone of the signed Region>, SizeInGB: 10, VolumeType: "gp3", Iops: 999999} — the emulator answers ValidationException "Iops must be from 3000 to 80000 for a gp3 volume"; expected ValidationException "Create volume failed (InvalidParameterValue: Volume iops of 999999 is too high; maximum is 80000.)".
Cause 3: with a volume attached to a WorkSpace Instance via AssociateVolume, call DeleteVolume for that VolumeId — the emulator answers ConflictException "Workspace managed EBS volume is attached to a workspace instance and must be detached before it is deleted"; expected ConflictException "EBS Volume is not in available state for volume <that VolumeId>".
