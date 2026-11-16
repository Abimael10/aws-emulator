Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connect-contact-lens.deleted-instance-lookup-rejected
Issue-Shape-Id: com.amazonaws.connectcontactlens#AmazonConnectContactLens
Issue-Key: connect-contact-lens.deleted-instance-lookup-rejected|run#17|AssertionError|assert describe_error["Message"] == f"Instance {instance_id} not found", (
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# Connect DescribeInstance refuses a deleted instance id with the service-wide fixed "Resource not found" text instead of AWS's "Instance <id> not found"

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstance

## Description
One net-new root cause.

Cause 1 of 1: Amazon Connect's `DescribeInstance` ResourceNotFoundException message is per-resource and quotes the instance identifier the caller asked for; the emulator answers one fixed, resource-agnostic sentence for every 404 the Connect service raises.

AWS: `DescribeInstance` (Smithy: com.amazonaws.connect#DescribeInstance, GET /instance/{InstanceId}, InstanceId @httpLabel @required @length min 1 max 100; modeled errors InternalServiceException 500, InvalidRequestException 400, ResourceNotFoundException 404 — "The specified resource was not found."). The model does not carry message text, so this is recorded behavior only: on an instance id that existed, reached ACTIVE and was then deleted, DescribeInstance answers HTTP 404 with Error.Code `ResourceNotFoundException` and Error.Message `Instance <instance-id> not found`. Message template: the fixed text is `Instance ` + ` not found`; the single variable part is the instance identifier taken verbatim from the request's `InstanceId` path label. (The check's other assertion — connect-contact-lens ListRealtimeContactAnalysisSegments answering ResourceNotFoundException with `Real-time contact analysis not found.` for the same deleted instance — already matches, which is what isolates this to Connect's own instance-lookup wording.)

Emulator: the same 404/ResourceNotFoundException is returned, but Error.Message is the literal `Resource not found` — the instance id is never interpolated. The cause is a single service-wide constant: `RESOURCE_NOT_FOUND_MESSAGE = "Resource not found"` and the `resource_not_found()` constructor, which the DescribeInstance handler calls for the missing-instance branch. Its doc comment and unit test assert the sentence is deliberately fixed and quotes no identifier, so every Connect 404 (unknown instance, unknown queue, unknown resource inside an instance) shares one wording and none of them can name the resource AWS names.

Why: a client that distinguishes Connect 404s by message — asserting the instance it asked for is the one reported gone, logging which identifier was rejected, or routing a deleted-instance path differently from a deleted-queue path — cannot do so against the emulator. Deletion-confirmation polling and teardown assertions written against AWS's `Instance <id> not found` text fail even though the status and error code are right, and tooling that extracts the identifier out of the message gets nothing.

Fix: Connect's refusal wording layer — the not-found message must be parameterized per resource plane rather than being one fixed constant, and the instance plane's missing-instance branch must build `Instance <InstanceId> not found` from the request's path label. The instance-lookup branch of the DescribeInstance handler is what selects it. (The service-wide constructor is also used by the queue plane and other instance-scoped reads, so the fix is to give the instance plane its own sentence, not to re-word the shared one for all of them; those other planes keep whatever their own recordings show.)

Traces: connect-contact-lens.deleted-instance-lookup-rejected — the DescribeInstance control call after CreateInstance -> ACTIVE -> DeleteInstance asserts `Message == f"Instance {instance_id} not found"` and sees `'Resource not found'`.

## Evidence
Failing check: connect-contact-lens.deleted-instance-lookup-rejected (mismatch)

  check_connect_contact_lens_deleted_instance_lookup_rejected.py, line 78, in run
    assert describe_error["Message"] == f"Instance {instance_id} not found"
  AssertionError: unexpected DescribeInstance message 'Resource not found'

Expected (AWS, recorded): HTTP 404, Error.Code = ResourceNotFoundException, Error.Message = "Instance <instance-id> not found" where <instance-id> is the value sent in the InstanceId path label.
Actual (emulator):       HTTP 404, Error.Code = ResourceNotFoundException, Error.Message = "Resource not found".

Status and error code agree; only the message diverges. Source of the fixed text:
  services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs:44
    pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found";
  services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs:52-56
    pub fn resource_not_found() -> ResourceNotFoundException { ... RESOURCE_NOT_FOUND_MESSAGE ... }
  services/service-com-amazonaws-connect-amazonconnectservice/src/state/refusals.rs:198-206
    #[test] the_not_found_message_is_fixed_text — asserts the sentence is "Resource not found"
  services/service-com-amazonaws-connect-amazonconnectservice/src/ops/describe_instance.rs:174-178
    store.instances.get(&input.instance_id).ok_or_else(|| ...ResourceNotFoundException(crate::state::resource_not_found()))

Positive control in the same check passes: the instance really was created and reached InstanceStatus ACTIVE before DeleteInstance, and connect-contact-lens ListRealtimeContactAnalysisSegments on the deleted instance already answers ResourceNotFoundException with the correct "Real-time contact analysis not found." message — so the divergence is confined to Connect's instance-lookup message.

## Steps to Reproduce
1. connect.CreateInstance(IdentityManagementType="CONNECT_MANAGED", InstanceAlias=<unique>, InboundCallsEnabled=true, OutboundCallsEnabled=true); capture Id.
2. Poll connect.DescribeInstance(InstanceId=Id) until Instance.InstanceStatus == "ACTIVE" (positive control: the instance exists and works).
3. connect.DeleteInstance(InstanceId=Id).
4. Poll connect.DescribeInstance(InstanceId=Id) until it raises ResourceNotFoundException.
5. Inspect that error: HTTP status is 404 and Error.Code is ResourceNotFoundException (both correct), but Error.Message is "Resource not found" instead of "Instance <Id> not found".

Replay: connect-contact-lens.deleted-instance-lookup-rejected
