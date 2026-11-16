Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: workspaces-instances.associate-disassociate-volume-roundtrip
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances.associate-disassociate-volume-roundtrip|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Commit: 592b93fbaec935edd06bdfe9abbe351007288c9e

# AssociateVolume/DisassociateVolume never create or remove the EBS attachment: ec2:DescribeVolumes reports Attachments == [] on an attached WorkSpace Instance volume forever

> _Note: this issue was identified against the code base at commit `592b93fbaec935edd06bdfe9abbe351007288c9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateVolume

## Description
One root cause (Cause 1 of 1): the attach/detach pair of the volume lifecycle is written only to this service's private store and is never propagated to the Amazon EBS volume state that owns the volume.

Cause 1 of 1 — AssociateVolume and DisassociateVolume do not propagate the attachment to EBS.
AWS: Smithy model (services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/smithy-model/com-amazonaws-workspacesinstances-eucmifrontendapiservice.json) — AssociateVolume input {WorkspaceInstanceId ^wsinst-[0-9a-zA-Z]{8,63}$ required, VolumeId ^vol-[0-9a-zA-Z]{1,63}$ required, Device required}, documented as "Attaches a volume to a WorkSpace Instance"; DisassociateVolume takes the same pair plus DisassociateMode. The VolumeId is an Amazon EBS volume id of the caller's account and the WorkSpace Instance is backed by a real EC2 instance (GetWorkspaceInstance.EC2ManagedInstance.InstanceId), so the attachment this operation creates IS an EBS attachment: after a successful AssociateVolume, ec2:DescribeVolumes(VolumeIds=[<that vol- id>]) resolves the volume with exactly one entry in Attachments whose InstanceId is the WorkSpace Instance's EC2 instance id and whose Device is the Device member of the request; after DisassociateVolume the volume has no attachments and continues to exist independently (State is not 'deleted'). Recorded behavior only for the attaching/detaching intermediate states — the check only requires that the attachment appears and then disappears.
Emulator: AssociateVolume stores the relation in this crate alone — services/service-com-amazonaws-workspacesinstances-eucmifrontendapiservice/src/ops/associate_volume.rs sets `volume.attachment = Some(crate::state::VolumeAttachment { workspace_instance_id, device })` on its own VolumeRecord and makes no call into EBS state; DisassociateVolume mirrors that and only clears the local relation. Consequently ec2:DescribeVolumes on the volume keeps answering Attachments == [] after the attach succeeds (HTTP 200), so the roundtrip check's wait for one attachment never becomes true and the check is killed at the 60 s harness cap. The gap is specific to the attach/detach pair: CreateVolume and DeleteVolume in the same crate DO reach EBS (crate::state::create_ebs_volume in src/ops/create_volume.rs, crate::state::delete_ebs_volume in src/ops/delete_volume.rs), which is why the same check gets past its 'volume becomes available' wait.
Why: a client that attaches a volume through workspaces-instances and then inspects, snapshots, monitors or detaches it through EC2/EBS — the normal way an attachment is observed, since AssociateVolume's response models no members at all — sees a volume that is permanently unattached. Any wait-for-attached loop (the documented way to know the attach landed) never terminates, so such workflows hang rather than fail, and tools that reconcile state from EC2 believe the attach never happened.
Fix: the AssociateVolume and DisassociateVolume handlers' state write — propagate the attachment into the EBS volume state the same way CreateVolume/DeleteVolume already propagate creation and deletion, so the attachment is visible to the service that owns the volume.
Traces: workspaces-instances.associate-disassociate-volume-roundtrip.

Related but not covering: the open issue 20261024T122725Z-workspaces-instances-createvolume-event-propagation.md is about CreateVolume/DeleteVolume propagation (that path now reaches EBS state); it does not mention AssociateVolume/DisassociateVolume, and the attachment propagation is still absent.

## Evidence
Check workspaces-instances.associate-disassociate-volume-roundtrip, verdict mismatch: "Check timed out after 60.0s".
The check reaches ALLOCATED, creates the volume, waits until ec2:DescribeVolumes reports State == 'available' (both succeed — the sibling check delete-volume-while-attached-rejected gets past the same two waits), calls wsi.associate_volume(WorkspaceInstanceId=..., VolumeId=..., Device='/dev/sdh') which returns success, then polls `ec2.describe_volumes(VolumeIds=[volume_id])['Volumes'][0]` for `len(v.get('Attachments', [])) == 1`.
Expected: Attachments == [{InstanceId: <EC2ManagedInstance.InstanceId of the WorkSpace Instance>, Device: '/dev/sdh', ...}], then after DisassociateVolume(DisassociateMode='FORCE') Attachments == [] with State != 'deleted'.
Actual: Attachments stays [] indefinitely, so the poll never satisfies and the check is cut off at the harness cap.
Source: src/ops/associate_volume.rs lines 269-279 write `volume.attachment = Some(...)` into this crate's store only; no EBS write exists in that handler, whereas src/ops/create_volume.rs line 353 calls crate::state::create_ebs_volume and src/ops/delete_volume.rs line 204 calls crate::state::delete_ebs_volume.

## Steps to Reproduce
1. Boot the multi-service emulator so both workspaces-instances and ec2 are mounted.
2. CreateWorkspaceInstance and wait until GetWorkspaceInstance reports ProvisionState ALLOCATED; keep EC2ManagedInstance.InstanceId.
3. workspaces-instances CreateVolume in the instance's Availability Zone; wait until ec2:DescribeVolumes reports the volume 'available'.
4. workspaces-instances AssociateVolume with that WorkspaceInstanceId, VolumeId and Device='/dev/sdh' — returns 200.
5. ec2:DescribeVolumes(VolumeIds=[<that vol- id>]) — Attachments is [] and stays []; expected one attachment to the instance id from step 2 at device /dev/sdh.
6. workspaces-instances DisassociateVolume(DisassociateMode='FORCE') then ec2:DescribeVolumes again — nothing changes in EBS state either, because the detach was never propagated.
