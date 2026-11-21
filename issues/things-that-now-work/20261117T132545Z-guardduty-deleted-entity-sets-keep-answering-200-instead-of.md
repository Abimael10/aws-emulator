Issue-Source: trace-sweep-divergence
Issue-Trace: guardduty-all-set-types-coexist-on-detector
Issue-Step: task#19
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: guardduty-all-set-types-coexist-on-detector guardduty-threatentityset-lifecycle-with-metadata guardduty-trusted-entity-set-lifecycle guardduty-trustedentityset-lifecycle-with-metadata
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty|GetThreatEntitySet|4xx->2xx|$.__type
Issue-Key: guardduty|GetThreatEntitySet|4xx->2xx|$.message
Issue-Key: guardduty|GetThreatEntitySet|4xx->2xx|$.type
Issue-Key: guardduty|GetThreatEntitySet|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: guardduty|GetThreatEntitySet|4xx->2xx|status
Issue-Key: guardduty|GetTrustedEntitySet|4xx->2xx|$.__type
Issue-Key: guardduty|GetTrustedEntitySet|4xx->2xx|$.message
Issue-Key: guardduty|GetTrustedEntitySet|4xx->2xx|$.type
Issue-Key: guardduty|GetTrustedEntitySet|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: guardduty|GetTrustedEntitySet|4xx->2xx|status
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Deleted entity sets keep answering 200 instead of GuardDuty's 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetThreatEntitySet

## Description
This issue bundles 2 distinct net-new replay clusters, both in the guardduty crate and both on the entity-set read path (GetThreatEntitySet and GetTrustedEntitySet). Fix BOTH causes and re-replay every fixture#step named in Traces (and in trace_ids) before submitting. Neither cluster is marked owned by an open issue (owned_by []) and neither is blocked by a stub (stub_path "").

The shared fault: once a Delete*EntitySet has been accepted for an entity set, the read of that same id must report the set GONE, i.e. refuse it exactly as an id no create ever minted is refused. The emulator instead RETAINS the deleted record for a chosen retention window and keeps serving it as a success, so the whole absence refusal (status, error code, error message) never reaches the caller for a deleted set. The two causes are the two entity-set families, which have separate handlers and separate stores and therefore separate replay clusters.

No open guardduty issue covers this read path; the only guardduty issue that owns entity-set-adjacent read clusters in this run is 20261110T102233Z-guardduty-read-backs-serve-content-the-emulator-cannot.md (IPSet `location` and GetInvestigation content), which is a different fault and does not cover either cause here.

Cause 1 of 2: a threat entity set whose delete has been accepted is still read back with HTTP 200 instead of the family's 404 absence refusal. Fixture guardduty-all-set-types-coexist-on-detector, step task#19.
AWS: Smithy GetThreatEntitySet is GET /detector/{DetectorId}/threatentityset/{ThreatEntitySetId}, success 200, output Name/Format/Location/Status (@required) + ExpectedBucketOwner/Tags/CreatedAt/UpdatedAt/ErrorDetails; its own error list names only BadRequestException (400) and InternalServerErrorException (500). DeleteThreatEntitySet is DELETE on the same URI with an empty output. The 404 refusal is therefore RECORDED BEHAVIOR ONLY, and the recording is unambiguous: after the delete is accepted, a read of that threatEntitySetId answers HTTP 404 with error code `ResourceNotFoundException` (body `__type` and `type`, plus header `x-amzn-errortype`) and message `The specified entity set does not exist.` That message is ENTIRELY fixed text for this error class — it names neither the member that carried the id nor the id value nor the detector, so nothing in it is taken from the request or from state; the requirement is the exact sentence, the 404 status and the ResourceNotFoundException code.
Emulator: the read answers HTTP 200 with a full entity-set body. The replay reports `status: expected 404, got 200` and all four refusal keys missing: `$.message` (expected 'The specified entity set does not exist.', got None), `$.__type` and `$.type` (expected 'ResourceNotFoundException', got None) and `$headers.x-amzn-errortype` (expected 'ResourceNotFoundException', got None). This is deliberate in the handler: services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/get_threat_entity_set.rs documents and implements "a set whose delete has been accepted is STILL REPORTED here", filtering the lookup only on `set.is_reportable(now)`, where src/state/entity_sets.rs sets ENTITY_SET_DELETE_PENDING -> ENTITY_SET_DELETED after ENTITY_SET_TRANSITION_MILLIS = 500 and keeps the record readable for ENTITY_SET_DELETED_RETENTION_MILLIS = 10_000. The correct refusal already exists and is unreachable for a deleted set: crate::scaffolded::Scaffolded::not_found + ENTITY_SET_NOT_FOUND_MESSAGE produce precisely the 404 / ResourceNotFoundException / fixed-sentence response the recording expects.
Why: a client that deletes an entity set and then confirms the deletion by reading the id gets a success carrying a live-looking set, so its not-found branch never runs. Delete-then-verify loops conclude the delete failed (or spin until the emulator's private 10 s retention elapses, a timing the real service does not have), teardown/idempotent-cleanup code re-issues deletes or aborts, and any code that treats a successful read as "the resource exists" acts on a resource AWS has removed.
Fix: the read handler for this operation and the entity-set state projection it consults — services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/get_threat_entity_set.rs and the reportability/retention rule in src/state/entity_sets.rs: an entity set whose delete has been accepted must stop satisfying the read's lookup and take the existing not_found path, rather than being retained and served as a success.
Traces: guardduty-all-set-types-coexist-on-detector#task#19 (representative) and guardduty-threatentityset-lifecycle-with-metadata#task#6 (affected_count 2).

Cause 2 of 2: the same divergence on the trusted-entity-set family — a deleted trusted entity set is read back 200 instead of the 404 absence refusal. Fixture guardduty-all-set-types-coexist-on-detector, step task#21.
AWS: Smithy GetTrustedEntitySet is GET /detector/{DetectorId}/trustedentityset/{TrustedEntitySetId}, success 200, output Name/Format/Location/Status (@required) + ExpectedBucketOwner/Tags/CreatedAt/UpdatedAt/ErrorDetails, errors BadRequestException (400) and InternalServerErrorException (500) only; DeleteTrustedEntitySet is DELETE on the same URI, empty output. Recorded behavior only for the refusal: after the delete, the read of that trustedEntitySetId answers HTTP 404, error code `ResourceNotFoundException` in `__type`, `type` and the `x-amzn-errortype` header, message `The specified entity set does not exist.` — the identical fixed sentence the threat family uses, with no request- or state-derived parts.
Emulator: HTTP 200 with a full entity-set body; the replay reports `status: expected 404, got 200` plus `$.message`, `$.__type`, `$.type` and `$headers.x-amzn-errortype` all missing. Same mechanism as cause 1 (the sibling handler retains the deleted record and reports DELETE_PENDING/DELETED for its retention window instead of refusing the id).
Why: identical client consequence as cause 1, on the family where AWS allows only one set per detector: a caller that deletes the trusted entity set and reads it back is told it still exists, so "delete, confirm gone, create the replacement" never advances past the confirm step, and a caller that keys on the read succeeding operates on a set the service has removed.
Fix: the same component in the trusted-entity-set handler — services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/get_trusted_entity_set.rs plus the shared reportability/retention rule in src/state/entity_sets.rs. Listed as its own cause because it is a separate handler, a separate store map and its own replay cluster, and must be re-replayed on its own.
Traces: guardduty-all-set-types-coexist-on-detector#task#21 (representative), guardduty-trusted-entity-set-lifecycle#task#6 and guardduty-trustedentityset-lifecycle-with-metadata#task#6 (affected_count 3).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against the booted emulator: emulator_up=true ("/__admin/health answered 200 and the :35287 data listener accepts"), bootstrap_signal=false, 86 fixtures replayed, 559 steps total, 536 passed, 23 mismatched, 0 errored, 0 blocked, not_implemented=0, score 95.9%. 11 divergence clusters; 2 are owned by open guardduty issues, 0 are stub-blocked, and 7 further clusters are assertions on per-run minted association ids (not filed here). Expected = the recorded AWS response, got = the emulator.

Cause 1 - guardduty-all-set-types-coexist-on-detector#task#19, GetThreatEntitySet, expected status 404, got 200:
  status: expected 404, got 200
  $.message: missing (expected 'The specified entity set does not exist.', got None)
  $.__type: missing (expected 'ResourceNotFoundException', got None)
  $.type: missing (expected 'ResourceNotFoundException', got None)
  $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException', got None)
  divergence_key: service=guardduty|GetThreatEntitySet|status=404->200|keys=$.__type,$.message,$.type,$headers.x-amzn-errortype,status
  affected_count 2 (also guardduty-threatentityset-lifecycle-with-metadata#task#6); owned_by []; stub_path "".

Cause 2 - guardduty-all-set-types-coexist-on-detector#task#21, GetTrustedEntitySet, expected status 404, got 200:
  status: expected 404, got 200
  $.message: missing (expected 'The specified entity set does not exist.', got None)
  $.__type: missing (expected 'ResourceNotFoundException', got None)
  $.type: missing (expected 'ResourceNotFoundException', got None)
  $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException', got None)
  divergence_key: service=guardduty|GetTrustedEntitySet|status=404->200|keys=$.__type,$.message,$.type,$headers.x-amzn-errortype,status
  affected_count 3 (also guardduty-trusted-entity-set-lifecycle#task#6 and guardduty-trustedentityset-lifecycle-with-metadata#task#6); owned_by []; stub_path "".

Model evidence (smithy_query on services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json):
  GetThreatEntitySet: GET /detector/{DetectorId}/threatentityset/{ThreatEntitySetId}, code 200; errors BadRequestException 400, InternalServerErrorException 500. DeleteThreatEntitySet: DELETE on the same URI, code 200, empty output.
  GetTrustedEntitySet: GET /detector/{DetectorId}/trustedentityset/{TrustedEntitySetId}, code 200; errors BadRequestException 400, InternalServerErrorException 500. DeleteTrustedEntitySet: DELETE on the same URI, code 200, empty output.
  Neither Get models ResourceNotFoundException, so the 404/ResourceNotFoundException/'The specified entity set does not exist.' refusal is grounded in the recording (and in the emulator's own shared constants), not in the operation's error list.

Supporting code reading (for the fix, not a requirement): services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/get_threat_entity_set.rs lines 193-217 state the rule as implemented - "A set whose delete has been accepted is STILL REPORTED here" - looking the record up with `.filter(|set| set.is_reportable(now))` and only falling through to `crate::scaffolded::Scaffolded::not_found(crate::scaffolded::ENTITY_SET_NOT_FOUND_MESSAGE)` once the record stops being reportable. services/service-com-amazonaws-guardduty-guarddutyapiservice/src/state/entity_sets.rs lines 32-79 define ENTITY_SET_DELETE_PENDING / ENTITY_SET_DELETED, ENTITY_SET_TRANSITION_MILLIS = 500 and ENTITY_SET_DELETED_RETENTION_MILLIS = 10_000 (both documented as emulator-CHOSEN spans) plus DELETED_ENTITY_SETS_RETAINED, i.e. the retention that keeps the deleted id answering. services/service-com-amazonaws-guardduty-guarddutyapiservice/src/scaffolded.rs lines 304-325 and 545-562 already provide exactly the expected refusal: RESOURCE_NOT_FOUND_EXCEPTION = "ResourceNotFoundException", ENTITY_SET_NOT_FOUND_MESSAGE = "The specified entity set does not exist." and `Scaffolded::not_found` with status 404 - so the fix is reaching that path for a deleted set, not authoring new wording.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, then replay the simparity-wire-aws-v1 wire corpus. Confirm emulator_up=true and bootstrap_signal=false before reading any divergence.
2. Cause 1: replay fixture guardduty-all-set-types-coexist-on-detector through step task#19 (GET /detector/{detectorId}/threatentityset/{threatEntitySetId} after that set's DeleteThreatEntitySet was accepted). Assert HTTP 404, body `__type` and `type` = ResourceNotFoundException, header x-amzn-errortype = ResourceNotFoundException, body `message` = exactly "The specified entity set does not exist." Observed: HTTP 200 with the entity-set body.
3. Cause 1 (second cluster member): repeat with fixture guardduty-threatentityset-lifecycle-with-metadata at step task#6.
4. Cause 2: replay fixture guardduty-all-set-types-coexist-on-detector through step task#21 (GET /detector/{detectorId}/trustedentityset/{trustedEntitySetId} after its DeleteTrustedEntitySet) and make the same four assertions; then repeat for guardduty-trusted-entity-set-lifecycle#task#6 and guardduty-trustedentityset-lifecycle-with-metadata#task#6.
5. Harness-free repro: ensure a detector exists (ListDetectors, else CreateDetector(Enable=true)). CreateThreatEntitySet(DetectorId=<d>, Name=<n>, Format=TXT, Location=<s3 uri>, Activate=false) and keep the returned threatEntitySetId I. DeleteThreatEntitySet(DetectorId=<d>, ThreatEntitySetId=I). Immediately GetThreatEntitySet(DetectorId=<d>, ThreatEntitySetId=I): it must be refused 404 ResourceNotFoundException with the fixed sentence above, with no dependence on how long you wait. Any 200 - and in particular one reporting Status DELETE_PENDING or DELETED - is this defect. Repeat the same sequence with CreateTrustedEntitySet / DeleteTrustedEntitySet / GetTrustedEntitySet for cause 2.
6. Note while fixing: do not weaken the refusal for the other absence cases that already pass - an id of the right form that no create minted, and an id minted under a different detector, must keep answering the same 404 and the same sentence; and an id outside the member's alphabet must keep its existing constraint-violation refusal.
7. Re-replay all of guardduty-all-set-types-coexist-on-detector#task#19, guardduty-all-set-types-coexist-on-detector#task#21, guardduty-threatentityset-lifecycle-with-metadata#task#6, guardduty-trusted-entity-set-lifecycle#task#6 and guardduty-trustedentityset-lifecycle-with-metadata#task#6 before submitting; all must pass, and the full corpus must not regress.
