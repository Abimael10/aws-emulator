Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-host-reservation-describes
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-host-reservation-describes
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|GetHostReservationPurchasePreview|4xx->4xx|$.Response.Errors.Error.Code
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# ec2 GetHostReservationPurchasePreview refuses a malformed host id as InvalidHostID.NotFound instead of InvalidHostId.Malformed

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetHostReservationPurchasePreview

## Description
Cause 1 of 1: a GetHostReservationPurchasePreview whose HostIdSet carries a host id that is not a well-formed Dedicated Host id is refused with the wrong error code.

AWS: recorded behavior only — the Smithy model (com.amazonaws.ec2, GetHostReservationPurchasePreview: HostIdSet required list of RequestHostIdSet, OfferingId required) declares no errors for this operation, so the contract here is the recording: HTTP 400 with $.Response.Errors.Error.Code = 'InvalidHostId.Malformed'. EC2 classifies id handling in two steps: an id that does not match the h-<hex> id form is rejected by request validation as a Malformed id ('.Malformed' code suffix, and for host ids AWS spells the resource 'InvalidHostId', mixed case, not 'InvalidHostID'); only a well-formed id that names nothing is a NotFound. The Message for this class is the id-parse message whose only variable part is the offending id taken from the request; this cluster asserts the Code, so the Code is the requirement being missed.

Emulator: answers 400 with Code 'InvalidHostID.NotFound' — it skips the id-form check and reports the state lookup's miss, and it also spells the resource 'HostID' rather than 'HostId'.

Why: a client that branches on the error code treats a typo'd or truncated host id as 'the host is gone' instead of 'the request is invalid'. The two classes drive opposite handling: Malformed is a permanent request defect to surface to the caller, NotFound is a resource-state answer a caller may legitimately treat as 'already released' and continue past. Code-based waiters and retry policies keyed on AWS's exact code string (including the 'Id' casing) never match.

Fix: the operation's validation in the ec2 crate — GetHostReservationPurchasePreview must parse every HostIdSet member against the host-id form and refuse a malformed one with InvalidHostId.Malformed BEFORE the state lookup that produces InvalidHostID.NotFound; the same ordering and the same 'InvalidHostId' spelling apply to every operation taking a host id.

Traces: ec2-host-reservation-describes#task#6 (the replay reports no owning issue for this cluster). Related but not covering it: 20261120T081949Z-ec2-refusal-fidelity-malformed-vs-nonexistent-id-classes.md and 20261103T071133Z-ec2-confuses-malformed-and-not-found-ids-and.md, neither of which carries this cluster's replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture ec2-host-reservation-describes step task#6, operation GetHostReservationPurchasePreview, verdict=mismatch, expected_status 400 / actual_status 400:
  $.Response.Errors.Error.Code: value_mismatch (expected 'InvalidHostId.Malformed', got 'InvalidHostID.NotFound')

Model grounding: services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json — GetHostReservationPurchasePreview input HostIdSet (required, list) / OfferingId (required), errors: [] (EC2 Query carries refusals in the Response/Errors/Error envelope rather than modeled error shapes).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture ec2-host-reservation-describes.
2. At step task#6 the fixture calls GetHostReservationPurchasePreview with a host id that is not a well-formed h-<hex> id.
3. Read the 400 body: Response/Errors/Error/Code is 'InvalidHostID.NotFound'; AWS answers 'InvalidHostId.Malformed' for an id that fails the id-form check.
