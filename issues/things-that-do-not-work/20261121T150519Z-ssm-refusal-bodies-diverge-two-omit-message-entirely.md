Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-inventory-and-compliance-tracking
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-inventory-and-compliance-tracking ssm-vscan-traj-patch-and-compliance-read-mistakes ssm-patch-baseline-and-compliance-workflow ssm-jit-access-request-invalid-target-error
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|PutComplianceItems|4xx->4xx|$.Message
Issue-Key: ssm|ListComplianceItems|4xx->4xx|$.Message
Issue-Key: ssm|DeletePatchBaseline|4xx->4xx|$.Message
Issue-Key: ssm|StartAccessRequest|4xx->4xx|$.message
Issue-Commit: b61d91764c5b64113b88eda248364bbad90afe06

# SSM refusal bodies diverge: two omit Message entirely and two carry the wrong sentence

> _Note: this issue was identified against the code base at commit `b61d91764c5b64113b88eda248364bbad90afe06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutComplianceItems

## Description
Four net-new causes under one heading: the emulator answers these four refusals with the right HTTP status and the right error class, and gets the BODY wrong — two emit no message member at all, one re-words AWS's sentence, and one answers a different refusal than the one AWS makes for that input. Each cause is a separate recorded step a fixer must re-replay.

Cause 1 of 4: PutComplianceItems' InvalidResourceId refusal carries no message.
AWS: Smithy (ssm): PutComplianceItems input {ResourceId: ComplianceResourceId @length 1..100 (required), ResourceType: ComplianceResourceType (required), ComplianceType, ExecutionSummary, Items}; errors include InvalidResourceId (client, httpError 400, documented "The resource ID isn't valid. Verify that you entered the correct ID and try again."). The modeled InvalidResourceId shape declares NO members, so the message is recorded behavior only: AWS answers HTTP 400 InvalidResourceId with a message whose fixed text is "Resource id " ... " is not a valid managed instance id." and whose one variable part is the ResourceId value taken verbatim from the request. The instance-id-shaped value in the recording is the request's value, not part of the template.
Emulator: HTTP 400 with the correct error class but NO `Message` member in the body at all ($.Message `missing (expected '...', got None)`).
Whit a client gets wrong: a caller that surfaces or logs the service's message has nothing to show, and SDK error objects report an empty message — the caller cannot tell WHICH id was rejected or why, and message-matching remediation never fires. This is the entire content of the refusal.
Fix: the PutComplianceItems handler's InvalidResourceId refusal path — the error value must be built with AWS's sentence, interpolating the request's ResourceId, and the serializer must emit it as `Message`.
Traces: ssm-inventory-and-compliance-tracking#task#8.

Cause 2 of 4: ListComplianceItems' InvalidResourceId refusal carries no message.
AWS: Smithy (ssm): ListComplianceItems input {Filters, ResourceIds: ComplianceResourceIdList @length min 1, ResourceTypes, NextToken, MaxResults}; errors include InvalidResourceId (client, httpError 400, same documented text, no modeled members). Recorded behavior: a ResourceIds entry that is not a managed-instance id is refused HTTP 400 InvalidResourceId with the same template as cause 1 — fixed text "Resource id " ... " is not a valid managed instance id.", variable part the offending id from the request.
Emulator: HTTP 400, correct error class, no `Message` member ($.Message `missing ... got None`).
Why: identical to cause 1 on a read path — the caller is told its id is invalid but not which one or by what rule, and nothing can be surfaced to a user.
Fix: the ListComplianceItems handler's InvalidResourceId refusal path, plus whatever shared refusal builder it and PutComplianceItems both call — the two should produce the same sentence from one place.
Traces: ssm-vscan-traj-patch-and-compliance-read-mistakes#task#9.

Cause 3 of 4: DeletePatchBaseline's ResourceInUseException message is emulator-authored.
AWS: Smithy (ssm): DeletePatchBaseline input {BaselineId @length 20..128 @pattern ^[a-zA-Z0-9_\\-:/]{20,128}$ (required)}; errors InternalServerError and ResourceInUseException (client, httpError 400, documented "Error returned if an attempt is made to delete a patch baseline that is registered for a patch group."); the ResourceInUseException shape carries a `Message` member. Recorded behavior: deleting a baseline still registered to a patch group is refused HTTP 400 ResourceInUseException with the message "PatchBaseline to be deleted is used for Patch Group: <patch group>" — everything up to and including the ": " is fixed text, and the single variable part is the name of the patch group the baseline is registered to, read from the emulator's state. The baseline and its registration are left intact.
Emulator: HTTP 400 ResourceInUseException (status and class right) with the message "Patch baseline is registered with patch group <patch group>" — different fixed text throughout; only the patch-group value agrees.
Why: the message is the only informative part of this refusal. A client that matches or displays the service's sentence sees wording AWS never emits, so message-keyed remediation (deregister the group, then retry the delete) does not trigger.
Fix: the DeletePatchBaseline handler's in-use refusal message builder — emit AWS's sentence with the registered patch group interpolated.
Related open issue, which does NOT carry this fixture step's replay key: 20261120T214510Z-ssm-5-net-new-causes-patchbaseline-tag-reads.md states the same wording defect as its Cause 3, grounded in BEHAVIORAL checks only; this wire step is not replayed by it.
Traces: ssm-patch-baseline-and-compliance-workflow#task#14.

Cause 4 of 4: StartAccessRequest answers the account-enablement refusal where AWS refuses the target value's shape.
AWS: Smithy (ssm): StartAccessRequest input {Reason: String1to256 (required), Targets: Targets @length 0..5 (required), Tags}; errors include ValidationException (client, httpError 400) and AccessDeniedException. Recorded behavior: a request whose Targets entry carries a node value outside the expression AWS constrains a just-in-time target value with is refused HTTP 400 ValidationException with a lowercase `message` of AWS's standard validation form — "1 validation error detected: Value at 'targets.<index>.member.values' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: <expression>]". The fixed text is the "1 validation error detected: Value at '...' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: ...]" frame; the variable parts are the member path (with the collection index from the request) and the expression. This clause is reached BEFORE the account-level feature gate, so the shape refusal wins whether or not the account is enrolled.
Emulator: HTTP 400 ValidationException (status and class right) with `message` = "Just-In-Time Node Access service is not enabled in this account." — the account-gate sentence, for a request AWS never gets as far as the gate with. The whole divergence is `$.message`.
Why: a client validating its target syntax against the service is told the FEATURE is off rather than that its value is malformed, so it fixes the wrong thing; and once the account is enrolled the malformed target would be accepted instead of refused.
Fix: state the constraint where this crate already states the live-service constraints the on-disk model omits (the stated-constraints table the refusal layer renders `Member must satisfy constraint: [ ... ]` from), so the boundary walk refuses the value before the handler consults the account gate. Note for the fixer: the expression itself is not in this checkout's Smithy model — read it from the recorded response for this step rather than inventing one.
IMPORTANT dedupe note: open issue 20261120T210000Z-ssm-startaccessrequest-target-value-pattern-not-stated.md already carries exactly this fixture/step pair in its front matter (Issue-Trace: ssm-jit-access-request-invalid-target-error, Issue-Step: task#1) and the same analysis, but the replay reported this cluster as unowned. If that issue is still open, treat this cause as the same cause and close it once, there.
Traces: ssm-jit-access-request-invalid-target-error#task#1.

## Evidence
Cause 1 — fixture `ssm-inventory-and-compliance-tracking`, step `task#8`, PutComplianceItems [mismatch, status 400 -> 400]: `$.Message: missing (expected 'Resource id <request ResourceId> is not a valid managed instance id.', got None)`. In the recording the variable part was the instance-id-shaped value supplied in the request.

Cause 2 — fixture `ssm-vscan-traj-patch-and-compliance-read-mistakes`, step `task#9`, ListComplianceItems [mismatch, 400 -> 400]: `$.Message: missing (expected 'Resource id <request ResourceIds entry> is not a valid managed instance id.', got None)`.

Cause 3 — fixture `ssm-patch-baseline-and-compliance-workflow`, step `task#14`, DeletePatchBaseline [mismatch, 400 -> 400]: `$.Message: value_mismatch (expected 'PatchBaseline to be deleted is used for Patch Group: <patch group>', got 'Patch baseline is registered with patch group <patch group>')`. Error class ResourceInUseException and status 400 both match; only the fixed text diverges.

Cause 4 — fixture `ssm-jit-access-request-invalid-target-error`, step `task#1`, StartAccessRequest [mismatch, 400 -> 400]: expected `$.message` = "1 validation error detected: Value at 'targets.1.member.values' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: ^(?:...]" (the replay truncates the expression); actual `$.message` = "Just-In-Time Node Access service is not enabled in this account."

## Steps to Reproduce
1. Clone the repo, boot the emulator built from it, and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1 — fixture `ssm-inventory-and-compliance-tracking`, step `task#8`: call `ssm.PutComplianceItems` with a ResourceId that is not a managed-instance id (ResourceType='ManagedInstance', a ComplianceType and ExecutionSummary that validate). Observe HTTP 400 InvalidResourceId with no `Message` member.
3. Cause 2 — fixture `ssm-vscan-traj-patch-and-compliance-read-mistakes`, step `task#9`: call `ssm.ListComplianceItems` with a ResourceIds entry that is not a managed-instance id. Observe HTTP 400 InvalidResourceId with no `Message` member.
4. Cause 3 — fixture `ssm-patch-baseline-and-compliance-workflow`, step `task#14`: CreatePatchBaseline, RegisterPatchBaselineForPatchGroup for a patch group, then DeletePatchBaseline on that baseline. Compare the ResourceInUseException message with 'PatchBaseline to be deleted is used for Patch Group: <patch group>'.
5. Cause 4 — fixture `ssm-jit-access-request-invalid-target-error`, step `task#1`: call `ssm.StartAccessRequest` with a valid Reason and a Targets entry whose Values hold a node value outside the just-in-time target expression, against an account with no access-request approval policy. Observe the account-gate sentence instead of the `targets.<index>.member.values` validation clause.
