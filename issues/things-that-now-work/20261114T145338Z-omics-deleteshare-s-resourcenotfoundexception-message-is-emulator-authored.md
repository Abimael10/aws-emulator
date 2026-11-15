Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: omics.create-share-lifecycle-reaches-pending-then-deleted
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics|DeleteShare|ResourceNotFoundException
Issue-Commit: c17029b5143eaf540f8fe3ae3fd6caada5435149

# omics DeleteShare's ResourceNotFoundException message is emulator-authored ("Share <id> not found") instead of AWS's fixed "Requested share not found"

> _Note: this issue was identified against the code base at commit `c17029b5143eaf540f8fe3ae3fd6caada5435149`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteShare

## Description
Cause 1 of 1: the not-found refusal of a share-addressed HealthOmics call carries wording this emulator invented, interpolating the requested share id, where AWS answers one fixed sentence with no variable part.

AWS:      DeleteShare is DELETE /share/{shareId} and models ResourceNotFoundException (HTTP 404, errorKind client) — Smithy: DeleteShare errors include ResourceNotFoundException @httpError 404, documented "The target resource was not found in the current Region." The model does not state the message text, so this is recorded behavior only: the recording of a second DeleteShare on an already-deleted share returns 404 ResourceNotFoundException whose Message is exactly "Requested share not found". That message is ENTIRELY fixed text — AWS names no share id, no ARN and no account in it, so there is no variable part to fill from the request or from state.

Emulator: the same call returns the right status and the right error code, but the Message is built from a template that splices the requested share id in: observed "Share 218a0194-fbe0-4528-9256-a1ea1560a057 not found" (the uuid is the shareId from the request; the fixed parts of the emulator's template are the leading "Share " and the trailing " not found"). The text comes from the shared helper crate::state::share_not_found_message(&input.share_id), used by the DeleteShare handler for both of its not-found conditions (no such share, and a share whose offer no longer stands).

Why:     a client that distinguishes HealthOmics refusals by message — error-mapping tables, log assertions, retry/idempotency wrappers that treat "Requested share not found" as "the delete already happened" — never matches the emulator's text, because the emulator's message differs for every share id and never equals AWS's sentence. Tests written against AWS's wording fail against the emulator, and a message carrying a share id also leaks an identifier AWS deliberately keeps out of this refusal.

Fix:      the shared refusal-message helper that DeleteShare's not-found path calls (crate::state::share_not_found_message, reached from the ResourceNotFoundException construction in the DeleteShare handler). It must return the fixed sentence "Requested share not found" with no interpolation of the requested id; the handler's status (404) and error class (ResourceNotFoundException) are already correct and must not change.

Traces:   services/service-com-amazonaws-omics-omics/src/ops/delete_share.rs lines 200-205 construct ResourceNotFoundException from crate::state::share_not_found_message(&input.share_id). Failing check: omics.create-share-lifecycle-reaches-pending-then-deleted — the check creates an ACTIVE workflow, CreateShare (PENDING), GetShare (PENDING), DeleteShare, waits for DELETED, then re-issues DeleteShare and asserts the refusal is ResourceNotFoundException with Message == "Requested share not found".

## Evidence
Check omics.create-share-lifecycle-reaches-pending-then-deleted, assertion at check line 98:
  assert message == "Requested share not found", f"unexpected not-found message: {message!r}"
  AssertionError: unexpected not-found message: 'Share 218a0194-fbe0-4528-9256-a1ea1560a057 not found'

Expected (AWS, from the recording): HTTP 404, Error.Code = "ResourceNotFoundException", Error.Message = "Requested share not found" (entirely fixed text).
Actual (emulator): HTTP 404, Error.Code = "ResourceNotFoundException", Error.Message = "Share <shareId> not found", where <shareId> is the shareId path label from the request (the recorded run showed 218a0194-fbe0-4528-9256-a1ea1560a057; that value is per-run and is not part of the requirement).
Everything earlier in the lifecycle matched: CreateShare returned PENDING, GetShare echoed the workflow ARN and PENDING, DeleteShare reached DELETED — only the second-delete refusal message diverges.

## Steps to Reproduce
1. Boot the emulator and point a botocore omics client at it.
2. create_workflow(name=..., engine="WDL", definitionZip=<zip containing main.wdl>, requestId=...) and poll get_workflow until status == ACTIVE; keep the workflow arn.
3. create_share(resourceArn=<workflow arn>, principalSubscriber="111122223333", shareName=...) -> shareId, status PENDING.
4. delete_share(shareId=<shareId>) and poll get_share until status DELETED (or until it 404s).
5. Call delete_share(shareId=<shareId>) a second time and inspect the ClientError: Error.Code is ResourceNotFoundException (correct) but Error.Message is "Share <shareId> not found" instead of AWS's "Requested share not found".
