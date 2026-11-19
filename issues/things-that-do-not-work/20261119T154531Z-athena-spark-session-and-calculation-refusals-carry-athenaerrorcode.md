Issue-Source: trace-sweep-divergence
Issue-Trace: athena-spark-session-and-calculation-reference-errors
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-spark-session-and-calculation-reference-errors athena-notebook-reference-and-workgroup-type-errors
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|ListSessions|4xx->2xx|$.AthenaErrorCode
Issue-Key: athena|ListSessions|4xx->2xx|$.ErrorCode
Issue-Key: athena|ListSessions|4xx->2xx|$.Message
Issue-Key: athena|ListSessions|4xx->2xx|$.__type
Issue-Key: athena|ListSessions|4xx->2xx|status
Issue-Key: athena|GetSession|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|GetSession|4xx->4xx|$.ErrorCode
Issue-Key: athena|GetSessionStatus|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|GetSessionStatus|4xx->4xx|$.ErrorCode
Issue-Key: athena|TerminateSession|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|TerminateSession|4xx->4xx|$.ErrorCode
Issue-Key: athena|ListExecutors|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|ListExecutors|4xx->4xx|$.ErrorCode
Issue-Key: athena|StartCalculationExecution|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|StartCalculationExecution|4xx->4xx|$.ErrorCode
Issue-Key: athena|GetCalculationExecutionCode|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|GetCalculationExecutionCode|4xx->4xx|$.ErrorCode
Issue-Key: athena|CreatePresignedNotebookUrl|4xx->4xx|$.AthenaErrorCode
Issue-Key: athena|CreatePresignedNotebookUrl|4xx->4xx|$.ErrorCode
Issue-Commit: f97a2791479cf3e78acbdef1e7d516a9feec11d8

# Athena Spark session and calculation refusals carry AthenaErrorCode INVALID_INPUT, and ListSessions accepts a non-Spark workgroup

> _Note: this issue was identified against the code base at commit `f97a2791479cf3e78acbdef1e7d516a9feec11d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListSessions

## Description
Eight net-new root causes on the Athena interactive-session / calculation / notebook surface, all from the wire replay of trace set simparity-wire-aws-v1. Every cluster below carries an empty stub_path and an empty owned_by, so no filed issue carries its replay key. Each cause must be closed separately; there are 8 of them.

Related open issues that do NOT cover these causes: 20261117T053359Z-athena-7-net-new-causes-the-built-in.md and 20261117T160651Z-athena-9-net-new-causes-the-provisioned-dpu.md (both behavioral; they cover GetSessionEndpoint/GetSessionStatus answering NotImplemented and ListExecutors refusing a LIVE session — here the operations do answer and the refusal MESSAGE is already correct, only the AthenaErrorCode value is wrong), 20261119T114552Z-athena-4-net-new-causes-startcalculationexecution-refuses-every.md (StartCalculationExecution into a live session), and 20261104T163600Z-athena-interactive-session-calculation-notebook-and-tagging-operations.md (NotImplemented scaffolds).

=== Cause 1 of 8: ListSessions accepts a workgroup whose EngineVersion is not Spark instead of refusing it ===
AWS: Smithy ListSessions(WorkGroup @required @pattern ^[a-zA-Z0-9._-]{1,128}$, StateFilter, MaxResults @range 1..100, NextToken) -> {Sessions, NextToken}; modeled errors InvalidRequestException (400), ResourceNotFoundException (400), InternalServerException (500). Interactive sessions exist only in a Spark workgroup, so ListSessions addressed to a workgroup whose EngineVersion is a Presto/Athena engine version is refused synchronously: HTTP 400, __type InvalidRequestException, AthenaErrorCode/ErrorCode INVALID_INPUT, Message 'Workgroup EngineVersion is not Spark.' — the message is entirely fixed text and interpolates nothing from the request or state.
Emulator: the call SUCCEEDS with HTTP 200 and a session-list body; none of __type, AthenaErrorCode, ErrorCode or Message is emitted.
Why: a client that uses ListSessions to discover whether a workgroup can host Spark sessions is told a Presto workgroup has (an empty set of) sessions rather than that it is the wrong workgroup type, so the negative branch — pick/create a Spark workgroup — is never taken and the failure surfaces later and less intelligibly.
Fix: the ListSessions handler's request validation in the athena service crate — the workgroup's EngineVersion must be resolved and a non-Spark workgroup refused with InvalidRequestException before any listing is serialized.
Traces: athena-spark-session-and-calculation-reference-errors#task#2.

=== Causes 2-8: the unknown-session / unknown-calculation refusal carries AthenaErrorCode INVALID_INPUT instead of UNAUTHORIZED ===
AWS (shared by causes 2-8): each of these operations takes a @required SessionId (or CalculationExecutionId) and models InvalidRequestException (400), ResourceNotFoundException (400) and InternalServerException (500) — see Smithy GetSession, GetSessionStatus, TerminateSession, ListExecutors, StartCalculationExecution, GetCalculationExecutionCode, CreatePresignedNotebookUrl. For a well-formed id that the account does not hold, Athena refuses with HTTP 400 and InvalidRequestException whose InvalidRequestException.AthenaErrorCode member (@length 1..256, surfaced by botocore as both AthenaErrorCode and ErrorCode) is the fixed string 'UNAUTHORIZED', and whose Message is the fixed text 'Not authorized to make this request.' — the message interpolates nothing, and AWS deliberately does not disclose whether the id exists. Recorded behavior for the code string; the model only constrains it to be a string.
Emulator (shared by causes 2-8): status 400, __type InvalidRequestException and the Message all match, but AthenaErrorCode and ErrorCode are 'INVALID_INPUT'.
Why (shared): AthenaErrorCode is the only machine-readable discriminator inside InvalidRequestException, which Athena overloads for many unrelated conditions. INVALID_INPUT tells a client its request was malformed and should be corrected locally; UNAUTHORIZED tells it the reference is not one this principal may address, which is what AWS reports for a session or calculation that does not belong to the caller. Retry/alert/branching logic keyed on AthenaErrorCode takes the wrong branch for every unknown session or calculation reference, while the human-readable message says the opposite of the code.
Fix (shared): the error-classification path these handlers share in the athena service crate — the unknown-session / unknown-calculation InvalidRequestException must set AthenaErrorCode to UNAUTHORIZED. The message construction is already correct, so this is the code field only. It must be fixed per handler listed below, since each cluster is a distinct operation.

Cause 2 of 8: GetSession — AthenaErrorCode/ErrorCode 'INVALID_INPUT', expected 'UNAUTHORIZED'. Traces: athena-spark-session-and-calculation-reference-errors#task#4.
Cause 3 of 8: GetSessionStatus — same divergence. Traces: athena-spark-session-and-calculation-reference-errors#task#5.
Cause 4 of 8: TerminateSession — same divergence. Traces: athena-spark-session-and-calculation-reference-errors#task#7.
Cause 5 of 8: ListExecutors — same divergence. Traces: athena-spark-session-and-calculation-reference-errors#task#8.
Cause 6 of 8: StartCalculationExecution — same divergence. Traces: athena-spark-session-and-calculation-reference-errors#task#9.
Cause 7 of 8: GetCalculationExecutionCode — same divergence, on an unknown CalculationExecutionId. Traces: athena-spark-session-and-calculation-reference-errors#task#13.
Cause 8 of 8: CreatePresignedNotebookUrl — same divergence, on a session id that does not exist. Traces: athena-notebook-reference-and-workgroup-type-errors#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (42 fixtures, 433 steps, 372 passed, score 85.9%, emulator_up true, bootstrap_signal false). All eight clusters below report stub_path '' and owned_by [].

Cause 1 — ListSessions, athena-spark-session-and-calculation-reference-errors#task#2, expected status 400 / actual 200:
  status: expected 400, got 200
  $.__type: missing (expected 'InvalidRequestException', got None)
  $.AthenaErrorCode: missing (expected 'INVALID_INPUT', got None)
  $.ErrorCode: missing (expected 'INVALID_INPUT', got None)
  $.Message: missing (expected 'Workgroup EngineVersion is not Spark.', got None)

Causes 2-8 — expected status 400 / actual 400, Message and __type matching, each reporting exactly:
  $.AthenaErrorCode: value_mismatch (expected 'UNAUTHORIZED', got 'INVALID_INPUT')
  $.ErrorCode: value_mismatch (expected 'UNAUTHORIZED', got 'INVALID_INPUT')
  Cause 2 GetSession — athena-spark-session-and-calculation-reference-errors#task#4
  Cause 3 GetSessionStatus — athena-spark-session-and-calculation-reference-errors#task#5
  Cause 4 TerminateSession — athena-spark-session-and-calculation-reference-errors#task#7
  Cause 5 ListExecutors — athena-spark-session-and-calculation-reference-errors#task#8
  Cause 6 StartCalculationExecution — athena-spark-session-and-calculation-reference-errors#task#9
  Cause 7 GetCalculationExecutionCode — athena-spark-session-and-calculation-reference-errors#task#13
  Cause 8 CreatePresignedNotebookUrl — athena-notebook-reference-and-workgroup-type-errors#task#10

Smithy consulted (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): ListSessions (WorkGroup @required, errors InvalidRequestException/ResourceNotFoundException/InternalServerException), GetSession / GetSessionStatus / TerminateSession / ListExecutors / StartCalculationExecution / GetCalculationExecutionCode / CreatePresignedNotebookUrl (SessionId or CalculationExecutionId @required, same three modeled errors), and InvalidRequestException{AthenaErrorCode: ErrorCode @length 1..256, Message: ErrorMessage}, httpError 400.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it; replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture athena-spark-session-and-calculation-reference-errors to step task#2 — call ListSessions with the fixture's WorkGroup, a workgroup created with a Presto/Athena EngineVersion. The emulator answers 200 with a Sessions body; AWS refuses 400 InvalidRequestException / AthenaErrorCode INVALID_INPUT / 'Workgroup EngineVersion is not Spark.'.
3. Causes 2-7: continue the same fixture to steps task#4, task#5, task#7, task#8, task#9 and task#13 — call GetSession, GetSessionStatus, TerminateSession, ListExecutors and StartCalculationExecution with a SessionId the account never held, and GetCalculationExecutionCode with an unknown CalculationExecutionId. Each is refused 400 InvalidRequestException with the correct message 'Not authorized to make this request.' but AthenaErrorCode/ErrorCode 'INVALID_INPUT' instead of 'UNAUTHORIZED'.
4. Cause 8: replay fixture athena-notebook-reference-and-workgroup-type-errors to step task#10 — CreatePresignedNotebookUrl with a non-existent SessionId shows the same AthenaErrorCode divergence.
5. Re-replay all eight fixture#step bindings after fixing; all eight must pass before closing.
