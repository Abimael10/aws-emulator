Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-merge-source-into-merged-api-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-merge-source-into-merged-api-lifecycle appsync-merged-api-source-association-management appsync-type-sdl-json-format-fidelity
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|StartSchemaMerge|2xx->4xx|$.sourceApiAssociationStatus
Issue-Key: appsync|StartSchemaMerge|2xx->4xx|status
Issue-Key: appsync|GetType|2xx->2xx|$.type.definition
Issue-Commit: 6f8cc16d39e6073bea26b894e8eeb7007d08e8b2

# appsync: 2 net-new causes — StartSchemaMerge refuses with 409 ConcurrentModificationException instead of scheduling the merge, and GetType SDL drops AWS's union line breaks

> _Note: this issue was identified against the code base at commit `6f8cc16d39e6073bea26b894e8eeb7007d08e8b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaMerge, GetType

## Description
Two independent net-new root causes in the appsync control plane, both surviving the replay's OWNED/ALREADY-FILED marks (both clusters report an empty owned_by). Both are service=appsync; each must be closed before this issue is submitted.

Related open issues that do NOT cover these: 20261029T200818Z-appsync-source-api-association-projection-and-merge-status.md covers the SourceApiAssociation PROJECTION (associationId/ARN form, status detail wording, MERGE_FAILED vs MERGE_SUCCESS) on the Associate/Get/Update/List operations — it does not touch StartSchemaMerge, which is a status-code refusal rather than a body projection. 20261119T233938Z-appsync-3-net-new-causes-createfunction-picks-the.md (behavioral) covers an AssociateMergedGraphqlApi-scheduled merge never settling to a terminal status; cause 1 below is the explicit merge-trigger operation being REFUSED outright. 20261108T213015Z-appsync-2-net-new-causes-updatetype-is-an.md covers UpdateType being an unimplemented stub, not GetType's SDL rendering of an existing type (cause 2).

Cause 1 of 2 — StartSchemaMerge answers 409 ConcurrentModificationException instead of accepting the merge and returning MERGE_SCHEDULED.
AWS: Smithy: StartSchemaMerge POST /v1/mergedApis/{mergedApiIdentifier}/sourceApiAssociations/{associationId}/merge, http code 200, output member sourceApiAssociationStatus (SourceApiAssociationStatus enum = MERGE_SCHEDULED, MERGE_FAILED, MERGE_SUCCESS, MERGE_IN_PROGRESS, AUTO_MERGE_SCHEDULE_FAILED, DELETION_SCHEDULED, DELETION_IN_PROGRESS, DELETION_FAILED); operation doc "Initiates a merge operation. Returns a status that shows the result of the merge operation.". Modeled errors are BadRequestException(400), NotFoundException(404), ConcurrentModificationException(409), UnauthorizedException(401), InternalFailureException(500); ConcurrentModificationException is documented "Another modification is in progress at this time and it must complete before you can make your change." — i.e. it is reserved for an association that currently has a modification in flight. Recorded behavior: for an existing, settled source-API association on a merged API, StartSchemaMerge is ACCEPTED with HTTP 200 and a body whose sourceApiAssociationStatus is "MERGE_SCHEDULED"; the merge then runs asynchronously and the association's status is polled through GetSourceApiAssociation.
Emulator: answers HTTP 409 with no sourceApiAssociationStatus member at all ($.sourceApiAssociationStatus missing, expected 'MERGE_SCHEDULED'). Hypothesis: the handler treats the association's own non-terminal state (it is left at MERGE_SCHEDULED by the association-time schedule that never executes) as a concurrent modification, so the explicit merge trigger can never be accepted.
Why: on-demand re-merge is unreachable. A client that changes a source API's schema and then calls StartSchemaMerge to republish it into the merged API is told a conflicting modification is in progress — a retryable error that never clears — so it either loops on 409 forever or gives up; the whole MERGE_SCHEDULED -> MERGE_IN_PROGRESS -> terminal progression the operation exists to start is never entered, and clients cannot distinguish a real concurrency conflict from this permanent refusal.
Fix: the StartSchemaMerge handler and the source-API-association state machine it guards — accept the request when no modification is actually in flight, move the association to MERGE_SCHEDULED and return that status with HTTP 200, and raise ConcurrentModificationException only while a merge/delete is genuinely in progress (and NotFoundException when the association or merged API does not resolve).
Traces: appsync-merge-source-into-merged-api-lifecycle#task#6, appsync-merged-api-source-association-management#task#11.

Cause 2 of 2 — GetType(format=SDL) renders a union type on one line, dropping the line structure AWS's SDL printer emits.
AWS: Smithy: GetType GET /v1/apis/{apiId}/types/{typeName} with required httpQuery format (TypeDefinitionFormat = SDL | JSON), output type: Type{name, description, arn, definition, format}; the model documents definition only as "The type definition" and says nothing about its layout, so this is recorded behavior only. The recording shows AWS re-printing the type from its parsed schema rather than echoing the submitted text, and for a union the printed definition breaks the declaration across lines: the keyword and type name, a blank line, a line holding only " = ", then the member list joined with " | ", and a trailing newline — "union <Name>\n\n = \n<Member1> | <Member2>\n". The fixed parts are the "union " keyword and that line/whitespace structure; the variable parts are the union's name and its member names, taken from the schema the request's API holds.
Emulator: returns the collapsed single-line form, 'union Thing = Widget | Gadget\n' (the names there are this fixture's values; the requirement is the layout rule, not the literal), for the same stored union type — the blank line and the standalone " = " line are missing.
Why: every client that round-trips SDL through GetType sees spurious drift: schema-diff and drift-detection tooling that compares the retrieved definition with the uploaded SDL (or with a previous GetType result, or with a checksum of it) reports a change on every union type, and code generators that re-parse the definition line-wise mis-read it.
Fix: the SDL printer behind GetType's `definition` projection (the type-definition serializer in the GraphQL schema state) — emit AWS's union layout; the same printer serves the other type kinds, so the union branch is the one to correct.
Traces: appsync-type-sdl-json-format-fidelity#task#12, appsync-type-sdl-json-format-fidelity#task#14, appsync-type-sdl-json-format-fidelity#task#15.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: 92 fixtures replayed, 743 steps, 702 passed, 41 mismatched (score 94.5%), 19 divergence clusters, emulator_up=true ("/__admin/health answered 200 and the :46215 data listener accepts"). 17 of the 19 clusters are OWNED by open issues; these are the 2 that are not.

Cause 1 — cluster service=appsync|StartSchemaMerge|status=200->409|keys=$.sourceApiAssociationStatus,status, owned_by: []. Representative appsync-merge-source-into-merged-api-lifecycle#task#6, also affecting appsync-merged-api-source-association-management#task#11 (affected_count 2).
  detail: "status: expected 200, got 409" and "$.sourceApiAssociationStatus: missing (expected 'MERGE_SCHEDULED', got None)".
  Expected: 200 with {"sourceApiAssociationStatus": "MERGE_SCHEDULED"}. Actual: 409 (ConcurrentModificationException is the only 409 modeled on this operation) with no sourceApiAssociationStatus.

Cause 2 — cluster service=appsync|GetType|status=200->200|keys=$.type.definition, owned_by: []. Representative appsync-type-sdl-json-format-fidelity#task#12, also affecting #task#14 and #task#15 (affected_count 3).
  detail: "$.type.definition: value_mismatch (expected 'union Thing\n\n = \nWidget | Gadget\n', got 'union Thing = Widget | Gadget\n')".
  Status matched (200); only the rendered SDL layout of the union diverged — the blank line after the type name and the standalone " = " line are absent from the emulator's output.

Model evidence (services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/smithy-model/com-amazonaws-appsync-awsdeepdishcontrolplaneservice.json): StartSchemaMerge POST /v1/mergedApis/{mergedApiIdentifier}/sourceApiAssociations/{associationId}/merge, code 200, output {sourceApiAssociationStatus: SourceApiAssociationStatus}, errors BadRequestException(400)/ConcurrentModificationException(409)/NotFoundException(404)/UnauthorizedException(401)/InternalFailureException(500); ConcurrentModificationException doc "Another modification is in progress at this time and it must complete before you can make your change."; SourceApiAssociationStatus enum values as listed above. GetType GET /v1/apis/{apiId}/types/{typeName} with required format query member (TypeDefinitionFormat = SDL|JSON) -> type: Type{name, description, arn, definition, format}.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator, then replay the wire corpus simparity-wire-aws-v1.
2. Cause 1: replay appsync-merge-source-into-merged-api-lifecycle step task#6 (and appsync-merged-api-source-association-management task#11). By hand: CreateGraphqlApi for a source API and run StartSchemaCreation on it; CreateGraphqlApi with apiType MERGED; AssociateSourceGraphqlApi/AssociateMergedGraphqlApi to create the association; then POST /v1/mergedApis/<mergedApiIdentifier>/sourceApiAssociations/<associationId>/merge (StartSchemaMerge). Expect HTTP 200 with {"sourceApiAssociationStatus":"MERGE_SCHEDULED"}; the emulator answers 409 with no sourceApiAssociationStatus.
3. Cause 2: replay appsync-type-sdl-json-format-fidelity steps task#12, task#14 and task#15. By hand: CreateGraphqlApi, CreateType/StartSchemaCreation with SDL declaring a union over two object types, then GET /v1/apis/<apiId>/types/<UnionName>?format=SDL. Expect type.definition to carry AWS's multi-line union layout "union <Name>\n\n = \n<A> | <B>\n"; the emulator returns the collapsed single-line "union <Name> = <A> | <B>\n".
4. Both causes must pass before this issue is closed.
