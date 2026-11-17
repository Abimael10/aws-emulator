Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2.resource-policy-conflicting-revision-rejected
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2.resource-policy-conflicting-revision-rejected|run#13|AssertionError|assert message == (
Issue-Commit: d56f3e72fadd2bcc773c18998c38d59f68ab7320

# eventbridgev2: PutResourcePolicy ConflictException on a stale ExpectedRevisionId carries an emulator-authored message instead of AWS's revision-mismatch wording

> _Note: this issue was identified against the code base at commit `d56f3e72fadd2bcc773c18998c38d59f68ab7320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutResourcePolicy

## Description
One net-new root cause.

Cause 1 of 1: the ConflictException a revision-gated PutResourcePolicy answers when ExpectedRevisionId does not match the stored revision is worded by the emulator rather than as AWS words that error class. Failing check: eventbridgev2.resource-policy-conflicting-revision-rejected.

AWS: Smithy (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json) — PutResourcePolicy.ExpectedRevisionId (PolicyRevisionId, @length 1..256): "The write succeeds only if the named policy's current revision ID matches this value; a policy that does not exist yet matches only the sentinel \"NO_POLICY\" (create-only). On mismatch the operation fails with ConflictException." ConflictException is modeled on PutResourcePolicy with httpError 409, and its own documentation states the contract the message must convey: "A client-supplied precondition (e.g. ExpectedRevisionId on a resource-policy write) did not match the current state of the resource. Retrying the same request will fail again; re-read the resource and re-evaluate before retrying." The recorded AWS response for this error class is HTTP 409, Error.Code "ConflictException", and Error.Message — entirely FIXED text, with NO value taken from the request or from state (not the ExpectedRevisionId the caller sent, not the policy name, not the bus ARN):

  "The resource policy revision does not match ExpectedRevisionId, or the event bus was recreated since that revision was read. Re-read the policy and re-evaluate before retrying. A recent policy change can take up to a minute to be reflected on all endpoints, so a read taken immediately after one may still return the earlier revision."

Emulator: the status (409) and the code (ConflictException) are right, but the message is the emulator's own sentence, built by interpolating the request's ExpectedRevisionId and the resolved policy name: the replay observed Error.Message 'ExpectedRevisionId <expected-revision-id> does not match the current state of the <policy-name> resource policy' (in the recorded run, the caller's stale token and the "default" policy name filled those slots). AWS's error class has no such slots — those are per-request values the emulator injects into text AWS never emits.

Why: the whole message is a fixed, documented string, so clients and test fixtures match on it. A client that keys retry/conflict handling off the refusal text (or asserts the documented sentence, as this check does) sees a message that is not AWS's at all, and in particular loses the two operational facts AWS's wording carries and the emulator's drops: that the conflict can also mean the event bus was RECREATED since the revision was read, and that a recent policy change may take up to a minute to be reflected on all endpoints so an immediate re-read can still return the earlier revision. A caller told only "ExpectedRevisionId X does not match" will re-read once, get stale data, and loop or mis-report another writer.

Fix: the refusal's wording belongs in the eventbridgev2 resource-policy refusal constructor, not in the handler's formatting — `crate::state::revision_conflict(policy_name, expected)` (services/service-com-amazonaws-eventbridgev2-awseventsv2/src/state.rs), called from PutResourcePolicy's precondition branch in services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/put_resource_policy.rs. It must emit the error class's fixed text with no request- or state-derived substitution; because the text is wholly fixed, the constructor needs neither the expected id nor the policy name for the message. Check the same constructor's other callers (any other revision-gated resource-policy write on this service) so they do not keep the interpolated form.

Note: no open issue covers this; the eventbridgev2 resource-policy issues already filed are for UNIMPLEMENTED DeleteResourcePolicy (reported ALREADY FILED by the suite), which is a different cause.

Traces: eventbridgev2.resource-policy-conflicting-revision-rejected — CreateEventBus, PutResourcePolicy (unconditional, stores revision R), then PutResourcePolicy with ExpectedRevisionId set to a token that is not R; the check accepts the code and fails on the message. The same check's later assertions (the rejected write must not advance RevisionId and must not change the stored document) were not reached.

## Evidence
Check: [mismatch] eventbridgev2.resource-policy-conflicting-revision-rejected

Replay output:
  File "check_eventbridgev2_resource_policy_conflicting_revision_rejected.py", line 104, in run
    assert message == (
  AssertionError: unexpected message 'ExpectedRevisionId sim2real-behav-stale-revision-id does not match the current state of the default resource policy'

Expected (AWS, HTTP 409 / Error.Code ConflictException / Error.Message, all fixed text):
  "The resource policy revision does not match ExpectedRevisionId, or the event bus was recreated since that revision was read. Re-read the policy and re-evaluate before retrying. A recent policy change can take up to a minute to be reflected on all endpoints, so a read taken immediately after one may still return the earlier revision."

Actual (emulator, HTTP 409 / Error.Code ConflictException / Error.Message, template with per-request slots):
  "ExpectedRevisionId <ExpectedRevisionId from the request> does not match the current state of the <resolved PolicyName> resource policy"
  (recorded instance: 'ExpectedRevisionId sim2real-behav-stale-revision-id does not match the current state of the default resource policy' — the token and the policy name are values from the request, not part of AWS's message)

Model support: PutResourcePolicy errors include ConflictException (client, httpError 409); PutResourcePolicyRequest.ExpectedRevisionId doc states the mismatch fails with ConflictException; ConflictException doc states re-read-and-re-evaluate semantics.

Source: the message is assembled by crate::state::revision_conflict(policy_name, expected), invoked at services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/put_resource_policy.rs:291-298.

## Steps to Reproduce
1. CreateEventBus(Name=<unique>, ClientToken=<unique>) and wait for State == ACTIVE.
2. PutResourcePolicy(ResourceArn=<bus arn>, PolicyDocument=<valid non-public policy allowing events:PutEvents to the account root>) with no ExpectedRevisionId; note the returned RevisionId.
3. PutResourcePolicy(ResourceArn=<bus arn>, PolicyDocument=<a different valid policy>, ExpectedRevisionId="<any token that is not the RevisionId from step 2>").
4. Observe the refusal: HTTP 409 with Error.Code "ConflictException" (correct) but Error.Message equal to 'ExpectedRevisionId <token> does not match the current state of the default resource policy' instead of AWS's fixed revision-mismatch sentence quoted in the evidence.
5. (Not reached by the check, verify after the fix) GetResourcePolicy(ResourceArn=<bus arn>) must still report the step-2 RevisionId and the step-2 document.
