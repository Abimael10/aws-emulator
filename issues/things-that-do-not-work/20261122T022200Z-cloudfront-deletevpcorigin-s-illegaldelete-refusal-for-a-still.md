Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.op-contract-getVpcOrigin
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|DeleteVpcOrigin|IllegalDelete
Issue-Commit: cccfc998abc0f5f4b9d2e887762af47512c8b479

# cloudfront: DeleteVpcOrigin's IllegalDelete refusal for a still-deploying VPC origin is emulator-worded, so a caller's retry-until-Deployed loop never retries

> _Note: this issue was identified against the code base at commit `cccfc998abc0f5f4b9d2e887762af47512c8b479`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteVpcOrigin

## Description
REOPENS 20261112T003830Z-cloudfront-3-net-new-vpc-origin-causes-deletevpcorigin.md: its fix left the call refused. That issue's Cause 1 implemented DeleteVpcOrigin (it previously answered 501 NotImplemented) and its Fix text included "refuse a still-deploying origin with IllegalDelete (400)". The operation is now implemented, but the refusal it answers a still-deploying VPC origin with carries an emulator-authored sentence instead of the one the service answers, so a delete that AWS lets a caller retry into success stays refused for any caller that keys on the message. This is not a new defect; it is the un-finished part of that fix.

Cause 1 of 1: the IllegalDelete message DeleteVpcOrigin answers a VPC origin whose create is still propagating with is the emulator's own wording, not the service's.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json), smithy_query query=operation operation=DeleteVpcOrigin,GetVpcOrigin,CreateVpcOrigin: DeleteVpcOrigin input Id (@required, @httpLabel) and IfMatch (@required, @httpHeader If-Match); http {DELETE /2020-05-31/vpc-origin/{Id}, success code 202}; modeled errors include IllegalDelete (400, shape doc "Deletion is not allowed for this entity."), InvalidIfMatchVersion (400), PreconditionFailed (412), EntityNotFound (404), CannotDeleteEntityWhileInUse (409). CreateVpcOrigin answers 202 with VpcOrigin.Status "Deploying"; the status reaches "Deployed" on its own. Recorded behavior: a DeleteVpcOrigin issued while the VPC origin is still deploying is refused with HTTP 400, error Code 'IllegalDelete' and Message "The request failed because delete is not allowed for this vpc origin" — the whole sentence is FIXED text for this condition; it carries no value from the request and no identifier from state (no id, no name, no ARN, no status word). Once the origin reports Deployed, the same delete (with the origin's current ETag) succeeds with 202, after which GetVpcOrigin answers EntityNotFound (404) "The specified vpc origin does not exist". The model documents no message text, so the exact sentence above is recorded behavior only.

Emulator: the code and status are right, but the sentence is different: HTTP 400, Code 'IllegalDelete', Message "The request failed because delete is not allowed while the VPC origin is being deployed." — i.e. the fixed text is replaced by an emulator-authored sentence that also names the reason ("while the VPC origin is being deployed") that AWS's sentence does not state.

Why: this sentence is the only part of the refusal that distinguishes the TRANSIENT condition (still deploying — retry after the deployment settles) from the other 400-class IllegalDelete conditions of the same code, so clients and provisioning teardown loops branch on it. A caller that retries only on AWS's sentence sees an unrecognised message, treats the 400 as permanent and aborts: the VPC origin is then never deleted and leaks, and every later create that reuses its Name collides (EntityAlreadyExists). That is exactly what the failing check shows — it aborted on the first attempt with "expected DeleteVpcOrigin to eventually succeed once Deployed" and never reached the Deployed delete, the post-delete 202, or the EntityNotFound read that follows it, so the rest of the delete contract is left unverified behind this one sentence.

Fix: the VPC origin state slice that owns this refusal's wording — services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/state/vpc_origins.rs, the VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE constant that vpc_origin_illegal_delete() fills IllegalDelete.message from; the handler that raises it (src/ops/delete_vpc_origin.rs, the VPC_ORIGIN_STATUS_DEPLOYING branch) already decides the condition correctly and needs no change. Serve the service's fixed sentence for this condition instead of the emulator-authored one, and keep it free of request- and state-derived values. Then confirm the rest of the path the wrong sentence hid: once the status settles to Deployed, the delete must answer 202 with the VpcOrigin and its ETag, and GetVpcOrigin must then answer EntityNotFound.

Traces: cloudfront.op-contract-getVpcOrigin (the only failing check in this suite that reaches the still-deploying delete; it is listed under the '### cloudfront' heading and is marked neither ALREADY FILED nor OWNED).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 605/702 checks passed (86.2%), 84 mismatched, 13 errored, bootstrap_signal false. Of the 96 listed failures, 95 are marked OWNED by an open issue or ALREADY FILED (blocked by an unimplemented stub); cloudfront.op-contract-getVpcOrigin is the only one that is not.

[mismatch] cloudfront.op-contract-getVpcOrigin:
  File "check_cloudfront_op_contract_getVpcOrigin.py", line 54, in run
    _delete_with_retry(cf, vo_id, created["ETag"], deadline=time.monotonic() + 600.0, interval_s=10.0)
  File "check_cloudfront_op_contract_getVpcOrigin.py", line 135, in _delete_with_retry
  AssertionError: expected DeleteVpcOrigin to eventually succeed once Deployed, got IllegalDelete: 'The request failed because delete is not allowed while the VPC origin is being deployed.'

The check creates an internal ALB, calls CreateVpcOrigin over its ARN, and then retries DeleteVpcOrigin until it succeeds, retrying ONLY while the answer is code IllegalDelete AND message equal to the service's sentence, which the check carries as a literal:
  # DeleteVpcOrigin's answer while the VPC origin is still deploying.
  _ILLEGAL_DELETE_MESSAGE = "The request failed because delete is not allowed for this vpc origin"
  ...
  deploying = code == "IllegalDelete" and message == _ILLEGAL_DELETE_MESSAGE
  if not deploying or time.monotonic() >= deadline: raise AssertionError(...)
Because the emulator's message is a different string, `deploying` is false on the very first answer and the loop raises immediately — well inside its 600 s budget, so this is a wording divergence, not a timeout.

Expected vs actual for the still-deploying delete:
  expected: HTTP 400, Code 'IllegalDelete', Message "The request failed because delete is not allowed for this vpc origin"
  actual:   HTTP 400, Code 'IllegalDelete', Message "The request failed because delete is not allowed while the VPC origin is being deployed."
The status (400) and the code ('IllegalDelete') already match; only the message diverges.

Fixed vs variable parts: this message is fixed text in its entirety — AWS states the condition and interpolates nothing, so there is no request- or state-derived part to fill in. The HTTP status, the error code and this sentence are the requirements. The generated VPC origin id (vo_...), the ALB ARN, the ETag values, the account id, the region, request ids and timestamps that appear in a run are per-run values and are not part of any requirement.

Emulator source: services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/state/vpc_origins.rs lines 108-117 —
  pub const VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE: &str = "The request failed because delete is not allowed while the VPC origin is being deployed.";
  pub fn vpc_origin_illegal_delete() -> crate::ops::types::IllegalDelete { ... message: Some(VPC_ORIGIN_ILLEGAL_DELETE_MESSAGE.to_string()) }
raised from services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/delete_vpc_origin.rs lines 230-235 —
  if stored.status == crate::state::VPC_ORIGIN_STATUS_DEPLOYING { return Err(DeleteVpcOriginErrorModeled::IllegalDelete(crate::state::vpc_origin_illegal_delete()).into()); }
The neighbouring VPC-origin sentences in that same file are already recorded wire text (VPC_ORIGIN_NOT_FOUND_MESSAGE = "The specified vpc origin does not exist", VPC_ORIGIN_ALREADY_EXISTS_MESSAGE), so only the IllegalDelete one is emulator-authored.

Model grounding: smithy_query query=operation operation=DeleteVpcOrigin on services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json — success 202, IllegalDelete (400) modeled; smithy_query query=shape shape=IllegalDelete — structure with a single Message member, doc "Deletion is not allowed for this entity.", so the served sentence is not model-dictated and comes from the recording.

## Steps to Reproduce
1. git_clone the repository and run the behavioral suite (run_behavioral_suite), which builds and boots the emulator from this checkout.
2. Create a VPC origin: CreateVpcOrigin with a VpcOriginEndpointConfig naming a real backing resource (the check uses an internal application load balancer in two subnets of two AZs), HTTPPort 80, HTTPSPort 443, OriginProtocolPolicy "https-only". The call answers 202 with VpcOrigin.Status "Deploying" and an ETag.
3. Immediately call DELETE /2020-05-31/vpc-origin/{Id} with If-Match set to that ETag, while the origin still reports "Deploying". The refusal must be HTTP 400, Code 'IllegalDelete', Message "The request failed because delete is not allowed for this vpc origin" — the whole sentence fixed, interpolating nothing. The emulator currently answers the same status and code with "The request failed because delete is not allowed while the VPC origin is being deployed."
4. Confirm the refusal wrote nothing: GetVpcOrigin still answers 200 with the same ETag and the origin's configuration unchanged.
5. Confirm the retry path the wrong sentence hid: poll until GetVpcOrigin reports Status "Deployed", then re-issue DeleteVpcOrigin with the ETag that read reports. It must succeed with 202 and return the VpcOrigin plus its ETag, after which GetVpcOrigin answers HTTP 404 EntityNotFound "The specified vpc origin does not exist" and a second delete answers the same.
6. Regression guard: the other DeleteVpcOrigin refusals must keep their current answers — a malformed Id is InvalidArgument (400), an unknown but well-formed id EntityNotFound (404), a non-version If-Match InvalidIfMatchVersion (400), a stale but well-formed one PreconditionFailed (412), and an origin a stored distribution fronts CannotDeleteEntityWhileInUse (409).
7. Re-replay cloudfront.op-contract-getVpcOrigin; it must pass end to end, including its GetVpcOrigin malformed-Id assertion (InvalidArgument / "Id format is invalid") and its post-delete EntityNotFound assertion.
