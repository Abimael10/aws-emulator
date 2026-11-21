Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__scheduler__sqs.scheduler-lambda-universal-target-invalid-input-dead-letters
Issue-Key: iam__lambda__logs__scheduler__sqs.scheduler-lambda-universal-target-invalid-input-dead-letters|run#37|AssertionError|assert whitespace_attrs.get("ERROR_CODE") == "ValidationException", (
Issue-Commit: 25ec19db617af3039a65e7f007e5f3e3f2921f74

# Lambda Invoke does not enforce the FunctionName pattern: a whitespace-only FunctionName is reported as InternalFailure instead of ValidationException

> _Note: this issue was identified against the code base at commit `25ec19db617af3039a65e7f007e5f3e3f2921f74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
ONE root cause (1 of 1), reached through this service group's EventBridge Scheduler universal-target invalid-input dead-letter check.

Cause 1 of 1: Lambda's Invoke does not enforce the `FunctionName` member's modeled pattern, so a FunctionName that cannot possibly name a function (here a whitespace-only string) is not refused with the protocol-level validation error; the failure surfaces as a server-class `InternalFailure` instead, and that is the code EventBridge Scheduler then puts on the dead-letter event.

AWS: Smithy `lambda#Invoke` input member `FunctionName` targets `NamespacedFunctionName`, is `@required`, is bound as an `@httpLabel` on `POST /2015-03-31/functions/{FunctionName}/invocations`, and carries `@length(min:1,max:256)` plus `@pattern("^(arn:(aws[a-zA-Z-]*)?:lambda:)?((eusc-)?[a-z]{2}((-gov)|(-iso([a-z]?)))?-[a-z]+-\\d{1}:)?(\\d{12}:)?(function:)?([a-zA-Z0-9-_\\.]+)(:(\\$LATEST(\\.PUBLISHED)?|[a-zA-Z0-9-_]+))?$")`. A value that fails a modeled constraint is refused by the framework, before any modeled error of the operation applies, with HTTP 400 and error code `ValidationException`, whose message is AWS's fixed validation text: `"1 validation error detected: Value '<the submitted value>' at 'functionName' failed to satisfy constraint: Member must satisfy regular expression pattern: <the member's pattern>"` — the fixed parts are "N validation error(s) detected: Value", "at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern:", and the variable parts are the rejected value taken from the request, the member name (`functionName`) and the pattern itself. Scheduler's universal target `arn:aws:scheduler:::aws-sdk:lambda:invoke` performs that Invoke on the schedule's behalf and, when it fails permanently, writes the target's own error onto the DeadLetterConfig queue, carrying the downstream code in the `ERROR_CODE` message attribute and its message in `ERROR_MESSAGE` (recorded behavior; the emulator already does this correctly for the sibling case below).

Emulator: a schedule whose Target.Input is `{"FunctionName": "     ", "InvocationType": "Event"}` does dead-letter, but with `ERROR_CODE = 'InternalFailure'`. The sibling schedule in the same check, whose Input has a FunctionName/Qualifier version mismatch, dead-lettered with the correct `ERROR_CODE = 'InvalidParameterValueException'` and the exact AWS message "The derived qualifier from the function name does not match the specified qualifier.", and its SCHEDULE_ARN matched — so the dead-letter envelope and the propagation of a *classified* Lambda error both work. Hypothesis: the whitespace FunctionName is never matched against the modeled pattern, so it reaches handler/state lookup, raises there unclassified, and is mapped to a generic server-class failure, which Scheduler faithfully reports as `InternalFailure`. The required `ERROR_MESSAGE` (the "1 validation error detected: ..." text above) was not graded, because the run stopped at the ERROR_CODE assertion.

Why: a client cannot distinguish its own malformed request from a service outage. `ValidationException`/400 is terminal and tells the caller to fix the schedule's Input; `InternalFailure` is server-class, so SDK retry policies and operators treat it as retryable and transient — a permanently broken schedule looks like a flaky emulator. Any consumer that routes dead-letter events by ERROR_CODE (alarm on 5xx-class, open a ticket on validation-class) routes this one wrongly, and anything asserting on the validation message gets nothing resembling it.

Fix: the Invoke handler's request validation in the Lambda service — the `FunctionName` httpLabel must be checked against the modeled length and pattern before any function lookup, and a violation must be serialized as HTTP 400 `ValidationException` with AWS's "N validation error(s) detected: ..." message text; correspondingly, the Scheduler universal-target dispatcher must keep propagating the downstream code verbatim (it already does) rather than substituting a generic one.

Related but NOT covering this failure (the replay marked this check neither ALREADY FILED nor OWNED): 20261109T141116Z-iam-lambda-logs-scheduler-sqs-scheduler-universal-target-arn-aws-scheduler-aws-sdk.md says the lambda:invoke universal target never invokes the function at all — in this run it does invoke and does dead-letter, so that issue's cause is not what this check now fails on. 20261112T140641Z-scheduler-4-net-new-behavioral-causes-in-the.md and 20261112T104021Z-scheduler-sns-a-dead-letter-event-for-an.md concern ERROR_CODE values for SNS/other targets, not Lambda Invoke's missing pattern validation.

Traces: iam__lambda__logs__scheduler__sqs.scheduler-lambda-universal-target-invalid-input-dead-letters.

## Evidence
Suite simparity-behavioral-aws-v1, replayed against this checkout (1884/2850 passed). The check is listed under `### iam__lambda__logs__scheduler__sqs` and is marked neither ALREADY FILED nor OWNED.

Cause 1 — iam__lambda__logs__scheduler__sqs.scheduler-lambda-universal-target-invalid-input-dead-letters [mismatch]:
  AssertionError: ERROR_CODE is 'InternalFailure', expected 'ValidationException'

What passed first, and what diverged:
  - The control schedule (Target.Input FunctionName = the function ARN) invoked the function and the sink received the control marker — PASSED.
  - The qualifier-mismatch schedule (Input FunctionName '<fn>:2' with Qualifier '1') dead-lettered with ERROR_CODE 'InvalidParameterValueException', ERROR_MESSAGE "The derived qualifier from the function name does not match the specified qualifier." and SCHEDULE_ARN equal to that schedule's ARN — PASSED. So both the Invoke error path and the dead-letter attribute propagation work for a classified error.
  - The whitespace schedule (Input `{"FunctionName": "     ", "InvocationType": "Event"}`) dead-lettered with:
      Expected ERROR_CODE: 'ValidationException'  (HTTP 400, framework constraint violation on the Invoke httpLabel)
      Actual   ERROR_CODE: 'InternalFailure'      (server-class)
      Expected ERROR_MESSAGE (not graded, the run stopped on ERROR_CODE): "1 validation error detected: Value '     ' at 'functionName' failed to satisfy constraint: Member must satisfy regular expression pattern: (arn:(aws[a-zA-Z-]*)?:lambda:)?((eusc-)?[a-z]{2}((-gov)|(-iso([a-z]?)))?-[a-z]+-\\d{1}:)?(\\d{12}:)?(function:)?([a-zA-Z0-9-_\\.]+)(:(\\$LATEST(\\.PUBLISHED)?|[a-zA-Z0-9-_]+))?" — the rejected value is the one from the request and the pattern is the member's own; only the schedule name/ARN and the function name are per-run values.
  The pattern quoted in the expectation is byte-identical to the `@pattern` on `lambda#NamespacedFunctionName` in this checkout's Smithy model, so the expectation is the model's own constraint.

## Steps to Reproduce
Boot the emulator from this checkout.
Direct path (shows the cause on its own):
1. Invoke(FunctionName='     ', InvocationType='Event') against the Lambda endpoint (e.g. POST /2015-03-31/functions/%20%20%20%20%20/invocations).
2. Observe a server-class failure instead of HTTP 400 ValidationException with the "1 validation error detected: Value '     ' at 'functionName' failed to satisfy constraint: Member must satisfy regular expression pattern: ..." message.
Through Scheduler (as replayed):
3. Create an SQS queue as a DLQ and a role Scheduler can assume that allows lambda:InvokeFunction and sqs:SendMessage on it.
4. CreateSchedule with a near-future at() expression, FlexibleTimeWindow Mode=OFF, RetryPolicy {MaximumRetryAttempts:0}, DeadLetterConfig.Arn = that queue, Target.Arn='arn:aws:scheduler:::aws-sdk:lambda:invoke' and Target.Input='{"FunctionName": "     ", "InvocationType": "Event"}'.
5. After the occurrence fires, ReceiveMessage on the DLQ with MessageAttributeNames=['All'] and read ERROR_CODE/ERROR_MESSAGE.
6. Observe ERROR_CODE='InternalFailure' instead of 'ValidationException'.
   Replay: iam__lambda__logs__scheduler__sqs.scheduler-lambda-universal-target-invalid-input-dead-letters
