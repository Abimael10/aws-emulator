Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.schema-merge-requires-reconciled-source-association
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.schema-merge-requires-reconciled-source-association|_error#15|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded with {result!r}")
Issue-Commit: 4ac133c8413d664d8b895e7589c70f52baf3cd85

# appsync: StartSchemaMerge has no concurrent-modification guard — it accepts a merge while the association's merge is still in progress instead of refusing with ConcurrentModificationException

> _Note: this issue was identified against the code base at commit `4ac133c8413d664d8b895e7589c70f52baf3cd85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaMerge

## Description
1 net-new cause. The emulator's StartSchemaMerge is unconditional: it never consults the source-API association's current merge state, so a merge can be started while the association is still non-terminal.

Cause 1 of 1: StartSchemaMerge on a source-API association whose merge has not reached a terminal status is accepted (HTTP 200) instead of being refused with ConcurrentModificationException.

AWS: StartSchemaMerge (POST /v1/mergedApis/{mergedApiIdentifier}/sourceApiAssociations/{associationId}/merge) models ConcurrentModificationException with httpError 409 — Smithy: StartSchemaMerge errors = BadRequestException(400), ConcurrentModificationException(409), InternalFailureException(500), NotFoundException(404), UnauthorizedException(401); ConcurrentModificationException doc: "Another modification is in progress at this time and it must complete before you can make your change." While the association returned by GetSourceApiAssociation is in a non-terminal sourceApiAssociationStatus (anything other than MERGE_SUCCESS / MERGE_FAILED / AUTO_MERGE_SCHEDULE_FAILED — i.e. MERGE_SCHEDULED or MERGE_IN_PROGRESS, from Smithy enum SourceApiAssociationStatus), AWS refuses a new StartSchemaMerge for the whole non-terminal window with error code ConcurrentModificationException, HTTP 409, and message (recorded behavior; fixed text with no variable parts) "This association currently has an operation in progress, please wait until that is complete". Only once the association is terminal does StartSchemaMerge succeed with 200 and sourceApiAssociationStatus in {MERGE_SCHEDULED, MERGE_IN_PROGRESS, MERGE_SUCCESS}.

Emulator: with the association reported non-terminal by GetSourceApiAssociation immediately after AssociateSourceGraphqlApi, StartSchemaMerge returns HTTP 200 with body {"sourceApiAssociationStatus": "MERGE_SCHEDULED"} — no refusal at all. The check asserts the rejection and fails with "expected the call to be rejected, but it succeeded with {... 'HTTPStatusCode': 200 ... 'sourceApiAssociationStatus': 'MERGE_SCHEDULED'}". ConcurrentModificationException is apparently never raised by this operation for any association state.

Why: a client that serializes merges by relying on AWS's 409 (retry/back-off on ConcurrentModificationException, or treating a 200 as "my merge, and only my merge, is now queued") gets the opposite answer from the emulator. Overlapping merges are silently accepted, so the merge-conflict path of a client (and any IaC/orchestration retry loop keyed on ConcurrentModificationException) is unreachable and untestable, and the reported MERGE_SCHEDULED is not a truthful statement about which merge is pending.

Fix: the StartSchemaMerge handler/validation for the AppSync control plane — before scheduling, read the named association from association state and refuse with ConcurrentModificationException (409) and the fixed message above whenever its sourceApiAssociationStatus is non-terminal; the association state machine must also actually drive a started merge to a terminal status so the guard clears. Related but not covering this: open issue 20261029T200818Z-appsync-source-api-association-projection-and-merge-status.md covers the association projection and merge-status *values* (MERGE_FAILED vs MERGE_SUCCESS, status detail, associationId form); it says nothing about StartSchemaMerge's concurrency refusal.

Traces: appsync.schema-merge-requires-reconciled-source-association.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check appsync.schema-merge-requires-reconciled-source-association (verdict: mismatch).

Setup performed by the check: create an IAM role trusted by appsync.amazonaws.com; CreateGraphqlApi apiType=MERGED with mergedApiExecutionRoleArn; CreateGraphqlApi (API_KEY) as the source; StartSchemaCreation on the source with a real schema and wait for SUCCESS; AssociateSourceGraphqlApi(mergedApiIdentifier, sourceApiIdentifier); GetSourceApiAssociation reports a NON-terminal sourceApiAssociationStatus (not MERGE_SUCCESS/MERGE_FAILED/AUTO_MERGE_SCHEDULE_FAILED), so the concurrency branch is entered.

Expected (AWS): StartSchemaMerge(associationId, mergedApiIdentifier) -> HTTP 409, Error.Code = "ConcurrentModificationException", Error.Message = "This association currently has an operation in progress, please wait until that is complete" (entirely fixed text).

Actual (emulator): HTTP 200 with body {"sourceApiAssociationStatus": "MERGE_SCHEDULED"}. Harness failure:
  File "check_appsync_schema_merge_requires_reconciled_source_association.py", line 55, in run
    code, message = _error(lambda: appsync.start_schema_merge(...))
  AssertionError: expected the call to be rejected, but it succeeded with {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'sourceApiAssociationStatus': 'MERGE_SCHEDULED'}

Smithy evidence: StartSchemaMerge declares ConcurrentModificationException with httpError 409; SourceApiAssociationStatus enum = [MERGE_SCHEDULED, MERGE_FAILED, MERGE_SUCCESS, MERGE_IN_PROGRESS, AUTO_MERGE_SCHEDULE_FAILED, DELETION_SCHEDULED, DELETION_IN_PROGRESS, DELETION_FAILED].

## Steps to Reproduce
1. Clone the repo and boot the emulator; run the behavioral suite (simparity-behavioral-aws-v1).
2. Observe check appsync.schema-merge-requires-reconciled-source-association fail with "expected the call to be rejected, but it succeeded".
3. By hand against the emulator:
   a. iam CreateRole with a trust policy for appsync.amazonaws.com.
   b. appsync CreateGraphqlApi --api-type MERGED --authentication-type API_KEY --merged-api-execution-role-arn <role arn>  -> mergedApiId.
   c. appsync CreateGraphqlApi --authentication-type API_KEY -> sourceApiId; StartSchemaCreation on it with "schema { query: Query } type Query { ping: String }" and poll GetSchemaCreationStatus to SUCCESS.
   d. appsync AssociateSourceGraphqlApi --merged-api-identifier <mergedApiId> --source-api-identifier <sourceApiId> -> associationId.
   e. appsync GetSourceApiAssociation --merged-api-identifier <mergedApiId> --association-id <associationId>; note sourceApiAssociationStatus is non-terminal (MERGE_SCHEDULED / MERGE_IN_PROGRESS).
   f. While it is non-terminal, appsync StartSchemaMerge --merged-api-identifier <mergedApiId> --association-id <associationId>.
      Expected: HTTP 409 ConcurrentModificationException "This association currently has an operation in progress, please wait until that is complete".
      Actual: HTTP 200 {"sourceApiAssociationStatus": "MERGE_SCHEDULED"}.
