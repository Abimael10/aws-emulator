Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.get-statement-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.get-statement-nonexistent-rejected|_retry_get_statement_error#11|Timeout|time.sleep(interval_s)
Issue-Commit: e41fe9228f437447e7375797d494df1ae3df829c

# glue: the GetStatement refusal check on an interactive session never reaches a verdict — the CreateSession → READY → RunStatement → GetStatement sequence does not complete inside the budget

> _Note: this issue was identified against the code base at commit `e41fe9228f437447e7375797d494df1ae3df829c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStatement

## Description
Cause 1 of 1: the Glue interactive-session statement-refusal flow never completes, so the modeled GetStatement refusal for a statement id the session never issued is never graded. Failing check: glue.get-statement-nonexistent-rejected (verdict: mismatch, detail `Error: Check timed out after 60.0s`).

AWS: the flow this check drives is four plain Glue control-plane/session calls, each of which answers promptly on real AWS (the recording completes the whole sequence). Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json):
  * CreateSession(Id, Role, Command, GlueVersion, Timeout, IdleTimeout, Tags) -> 200 with `Session`; a Role Glue can assume is accepted (InvalidInputException is for a role Glue cannot assume), and the session then leaves PROVISIONING for READY so GetSession reports `Session.Status == "READY"`.
  * RunStatement(SessionId, Code) -> 200 with `Id`, and the statement then reaches a terminal `StatementState` (AVAILABLE, else ERROR) readable through GetStatement.
  * GetStatement(SessionId, Id) for an Id the session minted -> 200 with `Statement`, `Statement.Id` equal to the requested id.
  * GetStatement(SessionId, Id) for an Id this session never issued but that lies inside the session's own id space -> HTTP 400 `IllegalSessionStateException` (NOT EntityNotFoundException). Its message is this refusal's fixed text and has no variable part: `Illegal Session Status` — no session id and no statement id appear in it.
  * DeleteSession(Id) -> 200, after which GetSession(Id) is refused 400 `EntityNotFoundException`.

Emulator: the replay reports `Error: Check timed out after 60.0s`; the check never reached a verdict, so none of the four answers above was observed. The handler logic for the final refusal is present (services/service-com-amazonaws-glue-awsglue/src/ops/get_statement.rs classifies an id inside `SessionEntry::statement_id_in_live_space` as IllegalSessionStateException), so what diverges is that the sequence does not progress to that call inside the budget — the whole check is spent before a verdict.

Hypothesis (neither the model nor the recording states where the stall is; the replay detail carries no frame): the budget is consumed by one of the bounded waits this flow needs, in order of likelihood — (a) CreateSession being refused InvalidInputException for the freshly created glue.amazonaws.com-trusted role, which the check retries every 5 s for up to 90 s (`crate::state::glue_role_is_unassumable` is asked over the emulator's own IAM and never converges to assumable), (b) the session never leaving PROVISIONING for READY so the 5 s-interval GetSession poll never ends, or (c) the statement RunStatement minted never settling into AVAILABLE/ERROR so the 3 s-interval GetStatement poll never ends. A fixer should confirm which by replaying the check and watching which call is being re-issued when the budget expires.

Why: a client using Glue interactive sessions — notebooks, the Glue sessions CLI/SDK, any driver that creates a session, waits for READY, runs a cell and reads the statement back — hangs instead of progressing, and never observes the refusal that tells it a statement id belongs to no statement of the session. Code that retries or waits cannot recover, because nothing it waits for ever arrives.

Fix: the interactive-session flow handlers and the state that advances them — CreateSession's role-assumability gate (services/service-com-amazonaws-glue-awsglue/src/ops/create_session.rs), the session status settle used by GetSession, and the statement execution/settle path behind RunStatement/GetStatement (the session and statement records in the service's state module). Each call in the sequence must answer promptly, the session must reach READY, the statement must reach a terminal state, and GetStatement must then serve the 200 echo and the 400 IllegalSessionStateException `Illegal Session Status` refusal.

Traces: glue.get-statement-nonexistent-rejected.

## Evidence
Expected (real AWS, per the recording and the Smithy model): CreateSession 200 -> GetSession reports Status READY -> RunStatement 200 with Id -> GetStatement(Id) 200 with Statement.Id == Id -> GetStatement(Id+1) refused HTTP 400 IllegalSessionStateException with message exactly `Illegal Session Status` (fixed text, no variable parts) -> DeleteSession 200 and GetSession then 400 EntityNotFoundException. The whole sequence completes well inside the suite's per-check budget.
Actual (emulator): `Error: Check timed out after 60.0s` — the check never reached a verdict, so no status, error code or message was produced for any step of the sequence. Nothing in the flow waits on an external resource; only Glue's own session/statement progress is awaited.

## Steps to Reproduce
1. CreateRole with a trust policy allowing glue.amazonaws.com to sts:AssumeRole; attach AWSGlueServiceRole and AmazonS3FullAccess.
2. glue CreateSession(Id=<new id>, Role=<that role ARN>, Command={Name: glueetl, PythonVersion: 3}, GlueVersion=5.0, Timeout=15, IdleTimeout=15).
3. Poll glue GetSession(Id) until Session.Status == READY.
4. glue RunStatement(SessionId=Id, Code="1 + 1") and poll glue GetStatement(SessionId, Id=<minted id>) until Statement.State is AVAILABLE or ERROR; assert the 200 echoes the requested Id.
5. glue GetStatement(SessionId, Id=<minted id> + 1).
Observed: the sequence does not complete — the check is killed at 60 s with no verdict. Expected: step 5 is refused HTTP 400 IllegalSessionStateException with message `Illegal Session Status`, and DeleteSession then makes GetSession answer 400 EntityNotFoundException.
