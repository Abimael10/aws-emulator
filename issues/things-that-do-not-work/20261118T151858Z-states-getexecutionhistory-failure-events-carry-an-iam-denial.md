Issue-Source: trace-sweep-divergence
Issue-Trace: sfn-failing-lambda-task-execution-diagnosis
Issue-Step: task#10
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sfn-failing-lambda-task-execution-diagnosis sfn-legacy-lambda-arn-task-failure-events
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: states|GetExecutionHistory|2xx->2xx|$.events[*].executionFailedEventDetails.cause
Issue-Key: states|GetExecutionHistory|2xx->2xx|$.events[*].taskFailedEventDetails.cause
Issue-Related: 20261109T234059Z-states-getexecutionhistory-omits-the-task-lambdafunction-started-event-shifting.md
Issue-Related: 20261112T174000Z-states-getexecutionhistory-failure-events-report-the-emulator-s-lambda.md
Issue-Commit: 1e2ad6ae4a0f762adb30ae2d836d3e967d1eb8e6

# states GetExecutionHistory failure events carry an IAM-denial cause and Lambda.AWSLambdaException instead of the function's error

> _Note: this issue was identified against the code base at commit `1e2ad6ae4a0f762adb30ae2d836d3e967d1eb8e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExecutionHistory

## Description
GetExecutionHistory projects the wrong failure identity into every failure event of an execution whose Lambda-invoking Task failed: both the task-level failure event (taskFailedEventDetails for an optimized lambda:invoke Task, lambdaFunctionFailedEventDetails for a legacy function-ARN Task) and the terminal executionFailedEventDetails carry the emulator's own error name and an IAM-authorization sentence, instead of the error name and message the invoked function raised. The same four clusters also expose a non-opaque page token on this operation. 4 causes.

Related open issue 20261103T052008Z-states-execution-outcome-error-name-and-cause-text-diverge.md files the identical error/cause divergence on the DescribeExecution surface (its Cause 1) and is keyed on states|DescribeExecution|$.error/$.cause; it does NOT cover the GetExecutionHistory event-detail members asserted here, which are serialized from the history record by a different path. Related open issue 20261103T052008Z-states-request-validation-gaps-and-non-opaque-pagination-tokens.md owns the non-opaque-token divergence on other operations/clusters; the GetExecutionHistory clusters below are not owned by it.

Cause 1 of 4 — the task-failure and execution-failure events of a failed lambda:invoke Task carry the emulator's error name and an IAM-denial cause (fixture sfn-failing-lambda-task-execution-diagnosis, step task#10).
AWS: Smithy (com.amazonaws.sfn): HistoryEvent.taskFailedEventDetails -> TaskFailedEventDetails{resourceType, resource, error: SensitiveError @length 0..256, cause: SensitiveCause @length 0..32768}; HistoryEvent.executionFailedEventDetails -> ExecutionFailedEventDetails{error, cause}. When an optimized lambda:invoke Task's function raises an unhandled error, AWS puts the function's own error type in `error` — here RuntimeError, the exception class the recorded function raised — and the function's message/stack in `cause`; the terminal ExecutionFailed event repeats that same error/cause. The error name is the function's unhandled-error type, not a Step Functions or Lambda service error. Recorded response: HTTP 200 with error 'RuntimeError' on both events.
Emulator: HTTP 200, but $.events[4].taskFailedEventDetails.error and $.events[5].executionFailedEventDetails.error are both 'Lambda.AWSLambdaException', and both `cause` members carry an IAM sentence of the shape "User: <assumed-role ARN> is not authorized to perform: lambda:InvokeFunction on resource: <function ARN> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <uuid>; Proxy: null)". Fixed text of that emulator sentence is the "is not authorized to perform: ... because no identity-based policy allows ..." template; the role ARN, function ARN and request id are per-run values. No part of the recording produced an authorization failure at all — the recording's function was invoked and raised.
Why: Catch/Retry authoring and operator triage both read these members. A client matching ErrorEquals on the function's error name (the documented way to branch on a function failure) finds 'Lambda.AWSLambdaException' and never matches, and an operator reading the history is pointed at a non-existent IAM misconfiguration instead of the function's exception.
Fix: the lambda:invoke Task integration's invoke/failure mapping (it must dispatch the function rather than synthesize an AccessDeniedException) and the history-event construction that copies error/cause into TaskFailed and ExecutionFailed.
Traces: sfn-failing-lambda-task-execution-diagnosis#task#10 (cluster also covers #task#12).

Cause 2 of 4 — the same wrong error/cause appear on a paged read of the history, whose nextToken is a bare ordinal (fixture sfn-failing-lambda-task-execution-diagnosis, step task#11).
AWS: Smithy: GetExecutionHistory is @paginated{inputToken: nextToken, outputToken: nextToken, items: events, pageSize: maxResults}; output nextToken -> PageToken @length 1..1024, and an unparseable token is refused with InvalidToken (400). AWS's token is an opaque continuation string, never a record index; the recording asserts only that a token is present (shaped, not value-asserted). The event's executionFailedEventDetails.error is again the function's 'RuntimeError'.
Emulator: HTTP 200 with $.events[0].executionFailedEventDetails.error 'Lambda.AWSLambdaException' and the same IAM-denial cause, and $.nextToken '4' — the plain decimal offset of the next event.
Why: a client that stores or logs the token sees internal state, and nothing stops it from forging or arithmetically advancing a token that AWS would refuse as InvalidToken; combined with the wrong error name, a paged consumer of the history branches wrongly on failure.
Fix: the GetExecutionHistory handler's pagination-token encoding (opaque, decodable token subject to InvalidToken on garbage) plus the same failure-mapping component as Cause 1.
Traces: sfn-failing-lambda-task-execution-diagnosis#task#11.

Cause 3 of 4 — a second (post-redrive) run of the same execution repeats both faults across its TaskFailed and ExecutionFailed events (fixture sfn-failing-lambda-task-execution-diagnosis, step task#19).
AWS: as Cause 1 and 2: every failure event of the re-run carries the function's own error name ('RuntimeError' in the recording) and its message in `cause`, and the page token is opaque.
Emulator: HTTP 200 with $.events[0].executionFailedEventDetails.error and $.events[1].taskFailedEventDetails.error both 'Lambda.AWSLambdaException', both causes the IAM-denial sentence, and $.nextToken '5'.
Why: the redriven attempt is the one an operator inspects to confirm a retry behaved; it reports the same fabricated authorization failure, so the redrive outcome cannot be distinguished from the original.
Fix: same components as Causes 1 and 2 — the failure mapping must apply to re-run/redriven attempts as well.
Traces: sfn-failing-lambda-task-execution-diagnosis#task#19.

Cause 4 of 4 — a legacy function-ARN Task's lambdaFunctionFailedEventDetails carries the same wrong error and IAM cause (fixture sfn-legacy-lambda-arn-task-failure-events, step task#10).
AWS: Smithy: HistoryEvent.lambdaFunctionFailedEventDetails -> LambdaFunctionFailedEventDetails{error: SensitiveError @length 0..256, cause: SensitiveCause @length 0..32768}. For a Task whose Resource is a bare Lambda function ARN, the LambdaFunctionFailed event and the terminal ExecutionFailed event both carry the function's own error type ('RuntimeError' in the recording) and its message.
Emulator: HTTP 200 with $.events[4].lambdaFunctionFailedEventDetails.error and $.events[5].executionFailedEventDetails.error both 'Lambda.AWSLambdaException' and both causes the IAM-denial sentence.
Why: the legacy function-ARN integration is a separate code path from the optimized lambda:invoke one, so a fix to the optimized path alone leaves this event type still reporting a fabricated authorization failure; ErrorEquals matching on the function error fails here too.
Fix: the legacy function-ARN Task integration's invoke/failure mapping and the LambdaFunctionFailed history-event construction.
Traces: sfn-legacy-lambda-arn-task-failure-events#task#10.

## Evidence
All four causes are mismatch clusters from the replay of simparity-wire-aws-v1 (score 91.9%, 72 mismatched steps, 29 clusters); all returned HTTP 200 as expected, with diverging bodies, and none is owned by an open issue or blocked by a stub.

Cause 1 — sfn-failing-lambda-task-execution-diagnosis#task#10 (cluster also covers #task#12), GetExecutionHistory, expected 200 / got 200:
  $.events[4].taskFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[4].taskFailedEventDetails.cause: expected a shaped value, got 'User: arn:aws:sts::<account>:assumed-role/<sfn-role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: <function arn> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <uuid>; Proxy: null)'
  $.events[5].executionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[5].executionFailedEventDetails.cause: same IAM sentence

Cause 2 — sfn-failing-lambda-task-execution-diagnosis#task#11, GetExecutionHistory, expected 200 / got 200:
  $.events[0].executionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[0].executionFailedEventDetails.cause: expected a shaped value, got the IAM sentence
  $.nextToken: expected a shaped (opaque) token, got '4'

Cause 3 — sfn-failing-lambda-task-execution-diagnosis#task#19, GetExecutionHistory, expected 200 / got 200:
  $.events[0].executionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[1].taskFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  both corresponding .cause members: expected shaped values, got the IAM sentence
  $.nextToken: expected a shaped (opaque) token, got '5'

Cause 4 — sfn-legacy-lambda-arn-task-failure-events#task#10, GetExecutionHistory, expected 200 / got 200:
  $.events[4].lambdaFunctionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[5].executionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  both corresponding .cause members: expected shaped values, got the IAM sentence

The ARNs, role-session ids, request ids and event-id numbers above are per-run values from the recording and are not part of the requirement; the requirement is that `error` is the invoked function's own error type, that `cause` is the function's failure message, and that nextToken is opaque.

## Steps to Reproduce
1. Replay fixture sfn-failing-lambda-task-execution-diagnosis against the emulator. At step task#10 (the representative) the fixture calls GetExecutionHistory on the terminal execution; compare $.events[4].taskFailedEventDetails.{error,cause} and $.events[5].executionFailedEventDetails.{error,cause} with the recording — the recording carries the function's error type, the emulator carries 'Lambda.AWSLambdaException' plus an IAM-denial sentence.
2. Step task#11 of the same fixture reads the history with maxResults set; check both the failure event's error/cause and that $.nextToken is an opaque token rather than the decimal '4'.
3. Step task#19 of the same fixture reads the history of the redriven attempt; the same wrong error/cause appear on events[0]/events[1] and $.nextToken is '5'.
4. Replay sfn-legacy-lambda-arn-task-failure-events and compare step task#10: the legacy function-ARN Task's $.events[4].lambdaFunctionFailedEventDetails.{error,cause} and $.events[5].executionFailedEventDetails.{error,cause} show the same substitution.
5. Re-replay all four fixture steps after the fix; each must report the invoked function's own error type and message, and the paged reads must return an opaque nextToken (with a forged token refused as InvalidToken, 400).
