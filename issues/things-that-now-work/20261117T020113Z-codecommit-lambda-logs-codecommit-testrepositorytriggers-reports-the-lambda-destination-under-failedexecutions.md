Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit__lambda__logs.repository-trigger-invokes-lambda-with-a-payload-naming-the-ref-update
Issue-Key: codecommit__lambda__logs.repository-trigger-invokes-lambda-with-a-payload-naming-the-ref-update|run#35|AssertionError|assert tested["successfulExecutions"] == [trigger_name], (
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# codecommit: TestRepositoryTriggers reports the Lambda destination under failedExecutions with 'The function is currently in the following state: Pending' — a just-created ZipFile function never becomes invocable

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestRepositoryTriggers

## Description
One net-new root cause.

Cause 1 of 1: CodeCommit's trigger test cannot notify a Lambda destination because the function is still reported in state `Pending` long after CreateFunction returned, so the trigger lands in failedExecutions instead of successfulExecutions.

AWS: Smithy — CodeCommit TestRepositoryTriggers output carries `successfulExecutions` (RepositoryTriggerNameList) and `failedExecutions` (RepositoryTriggerExecutionFailureList); Lambda's `State` enum (com.amazonaws.lambda#State) has values Pending, Active, Inactive, Failed, Deactivating, Deactivated, ActiveNonInvocable, Deleting, and `ResourceConflictException` is the client error (HTTP 409, doc 'The resource already exists, or another operation is in progress.') AWS raises for an invoke against a function that is not yet invocable. On AWS a function created from `Code={"ZipFile": ...}` leaves Pending and reaches State=Active within seconds — well before the several intervening control-plane calls this flow makes (AddPermission, CreateRepository, PutFile, CreateBranch, PutRepositoryTriggers, GetRepositoryTriggers) — so TestRepositoryTriggers invokes it and answers `successfulExecutions: [<triggerName>], failedExecutions: []`.

Emulator: TestRepositoryTriggers answers `successfulExecutions: []` and puts the trigger in failedExecutions with `failureMessage` = `Unable to send the notification to <lambda function ARN>: ResourceConflictException: The operation cannot be performed at this time. The function is currently in the following state: Pending` — fixed text apart from the destination ARN taken from the trigger and the state name taken from the function's stored State. The function is therefore never invocable, and no trigger payload is ever delivered, so the rest of the flow (the trigger record naming refs/heads/main and the commit id, and the absence of an invoke for the unlisted branch) cannot be distinguished from 'not invoked'.

Why: a client relying on AWS's behavior creates a function, wires it as a repository trigger and calls TestRepositoryTriggers to verify the wiring; on AWS this is the standard pre-flight and it succeeds. Against the emulator it always reports a failure attributed to a transient Lambda state that never clears, so neither TestRepositoryTriggers nor any real reference update can drive a CodeCommit->Lambda trigger.

Fix: whichever of the two holds — the Lambda control plane's function-state machine, which must advance a ZipFile-sourced function from Pending to Active (and publish that through GetFunctionConfiguration.State) rather than leaving it Pending indefinitely; and/or CodeCommit's trigger dispatcher, which must treat the Pending window the way AWS does rather than converting a 409 ResourceConflictException into a terminal trigger failure. The suite's evidence shows the function still Pending after six intervening control-plane calls, which points at the state machine first.

Traces: codecommit__lambda__logs.repository-trigger-invokes-lambda-with-a-payload-naming-the-ref-update.

## Evidence
Expected (AWS): TestRepositoryTriggers(repositoryName=<repo>, triggers=[{name, destinationArn=<fn arn>, branches=["main"], events=["updateReference"]}]) -> successfulExecutions == [<triggerName>], failedExecutions == [].

Actual (emulator): successfulExecutions == [] and failedExecutions == [{'trigger': '<triggerName>', 'failureMessage': 'Unable to send the notification to <lambda-function-arn>: ResourceConflictException: The operation cannot be performed at this time. The function is currently in the following state: Pending'}]. The destination ARN and trigger name are request values; 'The operation cannot be performed at this time. The function is currently in the following state: Pending' is the Lambda refusal text and 'Pending' is the function's stored State value.

## Steps to Reproduce
1. CreateRole trusting lambda.amazonaws.com and attach AWSLambdaBasicExecutionRole.
2. CreateFunction(Runtime=python3.12, Code={ZipFile: <small zip>}, Handler=handler.handler, Timeout=30).
3. CreateRepository; AddPermission(Principal=codecommit.amazonaws.com, SourceArn=<repo arn>, Action=lambda:InvokeFunction).
4. PutFile on branch main; CreateBranch(unlisted, commitId=<seed>).
5. PutRepositoryTriggers with one trigger whose destinationArn is the function ARN, branches=["main"], events=["updateReference"].
6. TestRepositoryTriggers with that same trigger.
Observed: the trigger is in failedExecutions with 'ResourceConflictException: ... state: Pending'. Expected: successfulExecutions == [<triggerName>]. Also check GetFunctionConfiguration(<fn>).State at step 6 — it is still Pending, where AWS reports Active.
