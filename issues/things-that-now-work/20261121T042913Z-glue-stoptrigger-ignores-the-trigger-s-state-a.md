Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.stop-trigger-on-created-trigger-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.stop-trigger-on-created-trigger-rejected|_error#7|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: ff2a85f16a37fa0b688f1b1dee68f28a0eaac3c8

# Glue StopTrigger ignores the trigger's state: a CREATED trigger is stopped with 200 instead of InvalidInputException 'The operation is not allowed when trigger is in state: CREATED'

> _Note: this issue was identified against the code base at commit `ff2a85f16a37fa0b688f1b1dee68f28a0eaac3c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopTrigger

## Description
Cause 1 of 1: StopTrigger performs the stop unconditionally, so a trigger that has never been started (TriggerState CREATED) is accepted instead of refused.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json) models StopTrigger with input Name (NameString, @required, @length 1..255) and output Name, and declares InvalidInputException (HTTP 400) among its errors alongside EntityNotFoundException, ConcurrentModificationException, OperationTimeoutException and InternalServiceException. The model does not state the state machine, so the transition rule is recorded behavior only: a trigger is CREATED immediately after CreateTrigger, StartTrigger moves it to ACTIVATED, and only an ACTIVATED trigger may be stopped — it then settles DEACTIVATED. A StopTrigger issued while the trigger is still CREATED is refused HTTP 400 with error code InvalidInputException and the message 'The operation is not allowed when trigger is in state: CREATED'. In that message the fixed text is 'The operation is not allowed when trigger is in state: '; the trailing token is a variable part taken from the emulator's own state — the TriggerState the addressed trigger is currently in (here CREATED) — not a literal from one run. The trigger name and the request id are NOT part of the message.

Emulator: StopTrigger answers HTTP 200 with the Name echo ({'Name': '<trigger-name>'}, content-type application/x-amz-json-1.1) for a trigger GetTrigger had just reported as State 'CREATED', and silently rewrites the stored state to DEACTIVATED. The handler says so in its own contract comment — 'Stopping takes no account of the state the trigger was in and is idempotent' — and unconditionally replaces trigger.state with TRIGGER_STATE_DEACTIVATED after only the not-found check.

Why: a client cannot tell a real stop from a no-op. Code that drives the documented lifecycle (create → start → stop) and treats the InvalidInputException as 'not started yet' never sees it, so an orchestration bug (stopping before starting) passes against the emulator and is refused in AWS. Worse, the emulator leaves a never-started trigger in DEACTIVATED, a state AWS can only reach from ACTIVATED, so every subsequent read of that trigger reports a lifecycle position AWS would never show, and the check's follow-on assertions (StartTrigger → ACTIVATED, StopTrigger → DEACTIVATED) are graded against a corrupted starting point.

Fix: StopTrigger's handler (services/service-com-amazonaws-glue-awsglue/src/ops/stop_trigger.rs) must gate the stop on the stored TriggerState before mutating it — admitting only the states AWS allows a stop from and otherwise returning InvalidInputException with AWS's wording, naming the current state — and must leave the trigger's state unchanged on that refusal. The state-name constants and the refusal message belong with the other trigger messages in crate::state::messages, next to TRIGGER_NOT_FOUND.

Traces: glue.stop-trigger-on-created-trigger-rejected (the check creates a SCHEDULED trigger with cron(0 12 * * ? *), confirms GetTrigger reports State 'CREATED', then calls StopTrigger).

Related open issue that does NOT carry this check's replay key: 20261117T133709Z-glue-accepts-requests-aws-refuses-and-answers-200.md describes the same class (200 where AWS answers 400) for other operations, and 20261117T133709Z-glue-createtrigger-refuses-a-scheduled-trigger-so-the.md is the opposite symptom (CreateTrigger refusing a scheduled trigger) — here CreateTrigger and GetTrigger both succeeded and the divergence is StopTrigger's missing state gate.

## Evidence
glue.stop-trigger-on-created-trigger-rejected, check line 61 (`code, message = _error(lambda: glue.stop_trigger(Name=trigger_name))`):

Expected (AWS): the call is refused — ClientError with Error.Code == 'InvalidInputException' and Error.Message == 'The operation is not allowed when trigger is in state: CREATED' (HTTP 400). The state token in the message is read from the trigger's stored TriggerState at the time of the call.

Actual (emulator): HTTP 200, body `{'Name': '<trigger-name>'}`. Replay raised `AssertionError: expected the call to be refused, but it succeeded: {'Name': 'sim2real-behav-trigger-0cef45ef00e2', 'ResponseMetadata': {'RequestId': ..., 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', ...}}}` (the generated trigger name and request id are per-run values, not part of the requirement).

Immediately before the failing call, GetTrigger(Name=<trigger>)['Trigger']['State'] == 'CREATED' asserted successfully, so the trigger genuinely was in the state AWS refuses a stop from. The handler source confirms the omission: services/service-com-amazonaws-glue-awsglue/src/ops/stop_trigger.rs lines 14-19 (module contract: 'Stopping takes no account of the state the trigger was in and is idempotent') and lines 191-194 (`let was = ::std::mem::replace(&mut trigger.state, crate::state::TRIGGER_STATE_DEACTIVATED.to_string());` — no state check between the not-found guard at line 176 and this write).

## Steps to Reproduce
1. glue CreateTrigger(Name=<t>, Type='SCHEDULED', Schedule='cron(0 12 * * ? *)', Actions=[{'JobName': <any-name>}], Tags={'sim2real': 'stop-trigger-on-created-trigger-rejected'}).
2. glue GetTrigger(Name=<t>) and confirm Trigger.State == 'CREATED'.
3. glue StopTrigger(Name=<t>) — AWS refuses 400 InvalidInputException 'The operation is not allowed when trigger is in state: CREATED'; the emulator answers 200 with the Name echo.
4. glue GetTrigger(Name=<t>) again — the emulator now reports State 'DEACTIVATED' for a trigger that was never started.
5. Re-replay check glue.stop-trigger-on-created-trigger-rejected; after the fix the check must also observe StartTrigger moving the trigger to ACTIVATED and StopTrigger then settling it to DEACTIVATED.
