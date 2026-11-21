Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-merge-source-into-merged-api-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-merge-source-into-merged-api-lifecycle appsync-merged-api-source-association-management
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|StartSchemaMerge|2xx->4xx|$.sourceApiAssociationStatus
Issue-Key: appsync|StartSchemaMerge|2xx->4xx|status
Issue-Commit: 075f5affd9be323818b79ff1d792bbcf981ada1a

# AppSync StartSchemaMerge refuses a re-initiated merge with 409 ConcurrentModificationException instead of 200 MERGE_SCHEDULED

> _Note: this issue was identified against the code base at commit `075f5affd9be323818b79ff1d792bbcf981ada1a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaMerge

## Description
One net-new cause in the emulator's AppSync Merged-API schema-merge state machine, found by replaying the wire corpus simparity-wire-aws-v1. StartSchemaMerge is implemented in this checkout (it is not an unimplemented scaffold stub), and no open issue carries this step's replay key.

Cause 1 of 1 — StartSchemaMerge issued on an association that already has a merge owed (the normal state immediately after an association is created) is refused 409 ConcurrentModificationException instead of being accepted as the initiation of that merge.

AWS: Smithy (services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/smithy-model): StartSchemaMerge POST /v1/mergedApis/{mergedApiIdentifier}/sourceApiAssociations/{associationId}/merge, success code 200, required httpLabel members associationId + mergedApiIdentifier, output member sourceApiAssociationStatus targeting the SourceApiAssociationStatus enum (AUTO_MERGE_SCHEDULE_FAILED | DELETION_FAILED | DELETION_IN_PROGRESS | DELETION_SCHEDULED | MERGE_FAILED | MERGE_IN_PROGRESS | MERGE_SCHEDULED | MERGE_SUCCESS); modeled errors BadRequestException (400), ConcurrentModificationException (409, documented "Another modification is in progress at this time and it must complete before you can make your change."), NotFoundException (404), UnauthorizedException (401), InternalFailureException (500). The operation is documented as "Initiates a merge operation. Returns a status that shows the result of the merge operation." Recorded behavior: in both fixtures a source API is associated to a MERGED API and StartSchemaMerge is then called on the fresh association — i.e. while the association is still in a non-terminal merge state, which the association-create's own response in the same fixture reports. AWS answers 200 with body member sourceApiAssociationStatus = "MERGE_SCHEDULED". So a merge the service already owes for an association does NOT close that association to a merge initiation: AWS coalesces the initiation into the pending merge and reports the association's non-terminal MERGE_SCHEDULED status. ConcurrentModificationException is the answer for a modification that genuinely conflicts (the association being deleted), not for the merge the call is itself asking for.

Emulator: the call is refused with HTTP 409 and the modeled ConcurrentModificationException (x-amzn-errortype ConcurrentModificationException), so the success body — and with it the sourceApiAssociationStatus member — is absent entirely (replay: "status: expected 200, got 409" and "$.sourceApiAssociationStatus: missing (expected 'MERGE_SCHEDULED', got None)"). The refusal is unconditional on a pending merge: SourceApiAssociationRecord::start_merge returns AssociationRefusal::ConcurrentModification as soon as merge_pending() (i.e. merge_due_at is Some) holds, and an association create schedules a merge, so every StartSchemaMerge arriving before the scheduled merge settles is refused. The error message the emulator attaches (the association-operation-in-progress text) is itself not a requirement here, because AWS raises no error for this call at all — the requirement is the 200 and the MERGE_SCHEDULED status value, whose fixed text is the enum symbol and which carries no request- or state-derived parts.

Why: a client following the documented Merged-API flow — associate the source API, then StartSchemaMerge, then poll GetSourceApiAssociation for a terminal status — gets a hard 409 from the very call that starts the merge. Because the refusal depends on whether the emulator's internally scheduled merge has settled yet, the same program succeeds or fails depending on how fast it issues the second call, so the failure is timing-dependent and unreproducible from the client's side. An SDK/CDK caller that treats ConcurrentModificationException as retryable spins; one that does not aborts the merge workflow outright. Per-run values (api ids, association ids, merge timestamps, request ids) are not requirements; the 200 status and the MERGE_SCHEDULED status value are.

Fix: the merge-initiation rule in the service's state — SourceApiAssociationRecord::start_merge in services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/state.rs — must not treat a merge the clock already owes as a conflicting modification. An initiation arriving while a merge is pending should coalesce into that pending merge (neither scheduling a second one nor pushing the owed due instant out) and answer with the association's MERGE_SCHEDULED status; only the deletion-pending case stays a ConcurrentModificationException. The handler (src/ops/start_schema_merge.rs) needs no change beyond passing the accepted status through.

Traces: appsync-merge-source-into-merged-api-lifecycle#task#6 and appsync-merged-api-source-association-management#task#11 (one cluster, 2 affected steps).

Related open issues that do NOT carry this step's replay key, so they do not cover this cause: 20261112T024108Z-appsync-schema-merge-2-net-new-causes-a.md (multi-source type composition and type withdrawal on disassociation — both about what a SUCCESSFUL merge does to the merged schema, not about StartSchemaMerge's admission check), 20261029T200818Z-appsync-source-api-association-projection-and-merge-status.md (the association projection's associationId/ARN/statusDetail and the MERGE_FAILED-vs-MERGE_SUCCESS outcome) and 20261119T233938Z-appsync-3-net-new-causes-createfunction-picks-the.md (an AssociateMergedGraphqlApi merge never leaving its scheduled state).

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 743 steps, 662 passed). Divergence cluster service=appsync|StartSchemaMerge|status=200->409|keys=$.sourceApiAssociationStatus,status — verdict mismatch, 2 affected steps:
  - appsync-merge-source-into-merged-api-lifecycle#task#6 (representative)
  - appsync-merged-api-source-association-management#task#11

Expected (recorded AWS response): HTTP 200, body {"sourceApiAssociationStatus": "MERGE_SCHEDULED"}.
Actual (emulator): HTTP 409 ConcurrentModificationException; no success body, so $.sourceApiAssociationStatus is absent. Replay detail verbatim:
    status: expected 200, got 409
    $.sourceApiAssociationStatus: missing (expected 'MERGE_SCHEDULED', got None)

In both fixtures the preceding step is an association create on the same pair (AssociateMergedGraphqlApi at appsync-merge-source-into-merged-api-lifecycle#task#4 / appsync-merged-api-source-association-management#task#10), whose own emulator response already reports the association with sourceApiAssociationStatusDetail "The source API merge is scheduled and has not been performed yet." — i.e. the association has a merge owed when task#6/task#11 arrives.

Emulator code evidence (services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice):
- src/state.rs:1726-1745 SourceApiAssociationRecord::start_merge — `if self.deletion_pending() { ... ConcurrentModification }` then `if self.merge_pending() { return Err(AssociationRefusal::ConcurrentModification(ASSOCIATION_OPERATION_IN_PROGRESS_MESSAGE.to_string())); }`. Only after both guards does it set merge_due_at / MERGE_SCHEDULED and return the status. The doc comment states the intent explicitly: "a call that arrives while the association still owes one is refused".
- src/state.rs:1645-1647 `merge_pending()` is simply `self.merge_due_at.is_some()`; the association create records a due instant, so the flag is set from creation until the clock settles it (src/state.rs:1686-1697 `settle`).
- src/ops/start_schema_merge.rs:208-217 lifts that refusal through `refuse(...)`, and :250-256 maps AssociationRefusal::ConcurrentModification onto the modeled ConcurrentModificationException (HTTP 409 per the model), which is the 409 the replay observed.

Model evidence: StartSchemaMerge SUCCESS_STATUS 200, output shape StartSchemaMergeResponse with the single member sourceApiAssociationStatus (SourceApiAssociationStatus enum including MERGE_SCHEDULED); ConcurrentModificationException httpError 409.

## Steps to Reproduce
Boot the emulator from this checkout and point an appsync client (plus iam/sts for the merge execution role) at it.

1. CreateGraphqlApi(name=<source>, authenticationType="API_KEY") and StartSchemaCreation on it with a small valid SDL; wait for GetSchemaCreationStatus == SUCCESS.
2. CreateRole trusted by appsync.amazonaws.com with an inline policy allowing appsync:SourceGraphQL on the source API ARN (and "/*").
3. CreateGraphqlApi(name=<merged>, authenticationType="API_KEY", apiType="MERGED", mergedApiExecutionRoleArn=<that role>).
4. AssociateMergedGraphqlApi(sourceApiIdentifier=<source apiId>, mergedApiIdentifier=<merged apiId>) (or AssociateSourceGraphqlApi from the other side). Note the associationId.
5. IMMEDIATELY (without waiting for the association to reach a terminal status) call StartSchemaMerge(mergedApiIdentifier=<merged apiId>, associationId=<associationId>).
   Expected: HTTP 200 with {"sourceApiAssociationStatus": "MERGE_SCHEDULED"}.
   Observed: HTTP 409 ConcurrentModificationException with no sourceApiAssociationStatus in the response.
   (Waiting ~1s first makes the same call succeed, which is what makes the defect timing-dependent.)

Equivalent replay: re-replay wire fixture appsync-merge-source-into-merged-api-lifecycle step task#6 and appsync-merged-api-source-association-management step task#11 from corpus simparity-wire-aws-v1.
