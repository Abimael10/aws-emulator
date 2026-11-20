Issue-Source: trace-sweep-divergence
Issue-Trace: arc-zonal-shift-practice-run-error-paths
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: arc-zonal-shift-practice-run-error-paths
Issue-Shape-Id: com.amazonaws.arczonalshift#PercDataPlane
Issue-Key: arc-zonal-shift|StartPracticeRun|4xx->4xx|$.message
Issue-Key: arc-zonal-shift|StartPracticeRun|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: arc-zonal-shift|StartPracticeRun|4xx->4xx|status
Issue-Commit: 34d6906f61273ec57dac6da65b2d7d977ecf1950

# arc-zonal-shift StartPracticeRun answers 403 AccessDeniedException instead of 404 ResourceNotFoundException for an unregistered managed resource

> _Note: this issue was identified against the code base at commit `34d6906f61273ec57dac6da65b2d7d977ecf1950`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartPracticeRun

## Description
One net-new root cause in the arc-zonal-shift emulator, found by replaying the wire corpus simparity-wire-aws-v1. This bundle has 1 cause; it must be closed before submitting.

RELATION TO OPEN ISSUES (none of them covers this step): 20261029T170702Z-arc-zonal-shift-accessdeniedexception-403-body-does-not.md is about the BODY SHAPE of a 403 AccessDeniedException that the emulator is right to emit (CancelZonalShift/UpdateZonalShift on an unknown zonalShiftId); 20261111T135546Z-arc-zonal-shift-createpracticerunconfiguration-403-omits-the-not.md is about the MESSAGE of the 403 CreatePracticeRunConfiguration correctly emits for a non-managed resource; 20261104T064145Z-arc-zonal-shift-an-elbv2-load-balancer-opted.md is about ELBv2-to-ARC managed-resource REGISTRATION. This cause is different: for StartPracticeRun the emulator picks the WRONG MODELED ERROR CLASS and the wrong HTTP status for a resourceIdentifier that ARC holds no managed resource for. This divergence came back unowned in this replay.

Cause 1 of 1: StartPracticeRun against a resourceIdentifier that is not an ARC managed resource must be refused as NOT FOUND (404 ResourceNotFoundException), not as access denied (403 AccessDeniedException).

AWS: Smithy (com.amazonaws.arczonalshift#PercDataPlane): StartPracticeRun = POST /practiceruns, success 200, input resourceIdentifier (@required, @length 8..1024), awayFrom (@required, @length ..20), comment (@required, @length ..128); modeled errors AccessDeniedException (client, 403, member message optional, doc "You do not have sufficient access to perform this action."), ConflictException (409), ResourceNotFoundException (client, 404, member `message` @required, doc "The input requested a resource that was not found."), ValidationException (400), ThrottlingException (429), InternalServerException (500). Recorded behavior for this error class: when the named resourceIdentifier is not a managed resource of the caller's account, StartPracticeRun answers HTTP 404 with the awsJson/restJson error document carrying the lowercase member `message` whose text is the fixed sentence "The managed resource was not found." (entirely fixed text — it interpolates nothing from the request or from state), and the header x-amzn-errortype naming the ResourceNotFoundException shape with this service's Coral shape-id suffix, i.e. "ResourceNotFoundException:http://internal.amazon.com/coral/com.amazon.percdataplane/" (the shape name followed by the service's fixed Coral namespace URI). Note that the error class ARC picks here is PER OPERATION and must not be unified: for CreatePracticeRunConfiguration the recording shows 403 AccessDeniedException with "Resource <resource-arn> is not a managed resource for account ID <account-id>.", and for a shift-id-keyed mutation on an unknown zonalShiftId it shows a 403 with no body members at all — StartPracticeRun is the one that discloses not-found.

Emulator: the handler takes the authorization decision before it resolves the target and refuses every unresolvable identifier as denied. services/service-com-amazonaws-arczonalshift-percdataplane/src/ops/start_practice_run.rs:210-212 calls state.rs `authorized_resource(...)` and maps its failure to StartPracticeRunErrorModeled::AccessDeniedException; state.rs:492-499 resolves the identifier against managed_resources plus the published-resource registry and, on a miss, returns `target_unavailable()` — an AccessDeniedException whose message is the emulator's own fixed sentence "Requested resource is not available.". The replayed step therefore got HTTP 403, body member `message` = "Requested resource is not available.", and x-amzn-errortype = "AccessDeniedException" (also without the Coral shape-id suffix AWS appends), where the recording requires 404, `message` = "The managed resource was not found." and x-amzn-errortype = "ResourceNotFoundException:http://internal.amazon.com/coral/com.amazon.percdataplane/".

Why: a client cannot tell a missing resource from a permissions problem. SDK retry/handling logic and operator scripts branch on the error shape and the 4xx status: a caller that would create or register the resource (or report "no such managed resource") on ResourceNotFoundException instead sees AccessDeniedException and concludes its credentials or IAM policy are at fault, so it escalates a permissions incident that does not exist and never performs the recovery AWS's 404 tells it to. Code that catches ResourceNotFoundException around StartPracticeRun — the documented way to probe whether a practice run can be started for a resource — never catches anything and the exception escapes.

Fix: the StartPracticeRun handler's target-resolution branch (services/service-com-amazonaws-arczonalshift-percdataplane/src/ops/start_practice_run.rs) must, when the resourceIdentifier resolves to no managed resource, return the modeled ResourceNotFoundException with the fixed message above instead of mapping the state helper's AccessDeniedException; that requires a not-found-shaped refusal on the state seam (state.rs `authorized_resource` / `target_unavailable`) that this operation selects, without changing the operations the recordings show answering 403, and the error serializer must emit x-amzn-errortype for this service's errors with the Coral shape-id suffix as recorded.

Traces: arc-zonal-shift-practice-run-error-paths#task#1 (expected 404 ResourceNotFoundException, got 403 AccessDeniedException).

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: 34 steps, 24 passed, 10 mismatched, 0 errored, 0 blocked, score 70.6%, emulator_up=true (health 200, data listener accepts), bootstrap_signal=false. 8 divergence clusters; 7 are marked OWNED by open issues and excluded. The remaining, unowned cluster is this cause.

Cluster divergence_key: service=arc-zonal-shift|StartPracticeRun|status=404->403|keys=$.message,$headers.x-amzn-errortype,status
Fixture: arc-zonal-shift-practice-run-error-paths, step task#1, operation StartPracticeRun, verdict mismatch, affected_count 1.

Detail as reported by the replay:
  status: expected 404, got 403
  $.message: value_mismatch (expected 'The managed resource was not found.', got 'Requested resource is not available.')
  $headers.x-amzn-errortype: value_mismatch (expected 'ResourceNotFoundException:http://internal.amazon.com/coral/com.amazon.percdataplane/', got 'AccessDeniedException')

Expected (AWS, from the recording + model): HTTP 404; error shape ResourceNotFoundException (com.amazonaws.arczonalshift#ResourceNotFoundException, errorKind client, httpError 404, member `message` @required); message fixed text "The managed resource was not found." with no interpolated parts; x-amzn-errortype "ResourceNotFoundException:http://internal.amazon.com/coral/com.amazon.percdataplane/" (shape name + the service's fixed Coral namespace URI).
Actual (emulator): HTTP 403; AccessDeniedException; message "Requested resource is not available."; x-amzn-errortype "AccessDeniedException".

Code: services/service-com-amazonaws-arczonalshift-percdataplane/src/ops/start_practice_run.rs:210-212 (`authorized_resource(...).map_err(StartPracticeRunErrorModeled::AccessDeniedException)`), and services/service-com-amazonaws-arczonalshift-percdataplane/src/state.rs:492-499 (`authorized_resource` -> `target_unavailable()`), whose doc comment states the intent explicitly: "a resourceIdentifier ARC holds no resource for is refused with target_unavailable rather than reported as not found" — which is the divergence.

Per-run values in the recording (generated ARNs/names, account id, ids, timestamps, request ids) are not part of the requirement; the requirement is the 404 status, the ResourceNotFoundException shape/errortype header, and the fixed message text.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator.
2. Replay the wire fixture arc-zonal-shift-practice-run-error-paths from corpus simparity-wire-aws-v1 and look at step task#1.
3. Equivalently by hand: call arc-zonal-shift StartPracticeRun with resourceIdentifier set to an ARN that is NOT a registered ARC managed resource for the account (e.g. an ELBv2 load balancer ARN that never had zonal-shift integration enabled), awayFrom any AZ id, comment any string <=128 chars.
4. Expected: HTTP 404, body {"message": "The managed resource was not found."}, header x-amzn-errortype: ResourceNotFoundException:http://internal.amazon.com/coral/com.amazon.percdataplane/
5. Actual: HTTP 403, body with message "Requested resource is not available.", header x-amzn-errortype: AccessDeniedException
6. Re-replay arc-zonal-shift-practice-run-error-paths#task#1 to confirm the fix; also re-replay arc-zonal-shift-zonal-shift-error-paths and arc-zonal-shift-practice-run-configuration-validation-errors to confirm the operations that legitimately answer 403 were not changed.
