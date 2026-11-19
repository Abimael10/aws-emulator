Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.redrive-execution-at-history-limit-rejected
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.redrive-execution-at-history-limit-rejected|run#21|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: e91c89e9d6482fa7ae1cb641524eb980150715bc

# Step Functions: RedriveExecution's ExecutionNotRedrivable message for a history-limit-failed execution uses the wrong reason text ('Execution history event limit exceeded' instead of 'Execution has exceeded the maximum history event limit')

> _Note: this issue was identified against the code base at commit `e91c89e9d6482fa7ae1cb641524eb980150715bc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RedriveExecution

## Description
Cause 1 of 1: the reason clause of the ExecutionNotRedrivable message RedriveExecution returns for an execution that FAILED by exceeding the 25,000-event history limit is worded differently from AWS. Failing check ids for this cause: stepfunctions.redrive-execution-at-history-limit-rejected.

AWS: RedriveExecution models ExecutionNotRedrivable as a 400 client error (Smithy: sfn RedriveExecution errors = ExecutionDoesNotExist, ExecutionLimitExceeded, ExecutionNotRedrivable, InvalidArn, ValidationException; all httpError 400; ExecutionNotRedrivable doc: "The execution Amazon Resource Name (ARN) that you specified for executionArn cannot be redriven."). The model does not state the message text, so this is recorded behavior only: when the target execution is a STANDARD execution that AWS failed because it reached the maximum number of history events, the refusal carries code ExecutionNotRedrivable and the message template `Execution <executionArn> invalid for redrive as: Execution has exceeded the maximum history event limit`. In that template the fixed text is `Execution `, ` invalid for redrive as: ` and the reason `Execution has exceeded the maximum history event limit`; the only variable part is <executionArn>, taken verbatim from the request's executionArn. The execution ARN in the recording (an account id, a generated state-machine name and a generated execution name) is a per-run value that fills the template and is not part of it.

Emulator: the status and the error code are right — the call is refused with ExecutionNotRedrivable — but the reason clause is emitted as `Execution history event limit exceeded`, producing `Execution <executionArn> invalid for redrive as: Execution history event limit exceeded`. The prefix `Execution <arn> invalid for redrive as: ` matches; only the trailing reason phrase diverges, so the emulator is already classifying the execution as not-redrivable for the right cause and just spells that cause differently.

Why: a client that distinguishes the several ExecutionNotRedrivable reasons AWS can return (history-limit exhaustion versus a non-FAILED/non-TIMED_OUT status versus an execution older than the redrive retention window) can only do so from this reason clause, because the error code is the same for all of them. Any operator runbook, retry/escalation rule, log assertion or test that keys on AWS's phrase 'Execution has exceeded the maximum history event limit' sees an unrecognized reason against the emulator and takes the wrong branch — typically retrying a redrive that can never succeed, or reporting an unknown failure. Error messages are part of the contract this emulator exists to reproduce.

Fix: the RedriveExecution handler's redrivability check in the Step Functions service — specifically the place that builds the ExecutionNotRedrivable message's reason clause for an execution terminated by the 25,000-event history-event limit. Only that reason string needs to become AWS's wording; the ARN interpolation, the surrounding fixed text, the 400 status and the ExecutionNotRedrivable code are already correct, as are the surrounding assertions in the check (redriveStatus NOT_REDRIVABLE, redriveCount staying 0, status staying FAILED), which all passed.

Traces: stepfunctions.redrive-execution-at-history-limit-rejected — the check first redrives an ordinary FAILED execution successfully (positive control, passes), then runs a 3200-item INLINE Map that fails with States.Runtime at event #25000 ExecutionFailed, then calls RedriveExecution on it and compares the refusal message.

## Evidence
Check: stepfunctions.redrive-execution-at-history-limit-rejected (verdict: mismatch)

Assertion that failed (check_stepfunctions_redrive_execution_at_history_limit_rejected.py line 191):
  assert message == expected, f"expected message {expected!r}, got {message!r}"

AssertionError: expected message 'Execution arn:aws:states:us-west-2:000000000000:execution:sim2real-behav-sm-9fbf0ee151ef:sim2real-behav-exec-3924ff4c05a8 invalid for redrive as: Execution has exceeded the maximum history event limit', got 'Execution arn:aws:states:us-west-2:000000000000:execution:sim2real-behav-sm-9fbf0ee151ef:sim2real-behav-exec-3924ff4c05a8 invalid for redrive as: Execution history event limit exceeded'

Expected (AWS), as a template:
  HTTP 400, Error.Code = ExecutionNotRedrivable
  Error.Message = "Execution <executionArn> invalid for redrive as: Execution has exceeded the maximum history event limit"
    fixed text: "Execution ", " invalid for redrive as: ", "Execution has exceeded the maximum history event limit"
    variable:   <executionArn> = the request's executionArn

Actual (emulator), same template:
  HTTP 400, Error.Code = ExecutionNotRedrivable  (correct)
  Error.Message = "Execution <executionArn> invalid for redrive as: Execution history event limit exceeded"
    reason clause diverges: "Execution history event limit exceeded" instead of "Execution has exceeded the maximum history event limit"

Everything else the check asserts about this state matched: the execution reached FAILED with error States.Runtime and cause "The execution reached the maximum number of history events (25000).", its last history event was #25000 ExecutionFailed, describeExecution reported redriveStatus NOT_REDRIVABLE before and after, and redriveCount stayed 0 — so only the message wording is wrong.

## Steps to Reproduce
1. Create an IAM role trusted by states.amazonaws.com.
2. CreateStateMachine (type STANDARD) with a definition whose single state is an INLINE Map over $.items running three Pass states per item:
   {"StartAt":"Fan","States":{"Fan":{"Type":"Map","ItemsPath":"$.items","ItemProcessor":{"ProcessorConfig":{"Mode":"INLINE"},"StartAt":"A","States":{"A":{"Type":"Pass","Next":"B"},"B":{"Type":"Pass","Next":"C"},"C":{"Type":"Pass","End":true}}},"End":true}}}
3. StartExecution with input {"items": [0..3199]} — enough history events to cross the 25,000-event limit.
4. Poll DescribeExecution until terminal; it is FAILED with error States.Runtime and cause "The execution reached the maximum number of history events (25000).", and redriveStatus NOT_REDRIVABLE.
5. Call RedriveExecution(executionArn=<that execution>).
6. Observe the refusal: code is ExecutionNotRedrivable (correct) but Error.Message ends with "invalid for redrive as: Execution history event limit exceeded"; AWS ends it with "invalid for redrive as: Execution has exceeded the maximum history event limit".
