Issue-Source: trace-sweep-divergence
Issue-Trace: sfn-failing-lambda-task-execution-diagnosis
Issue-Step: task#10
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sfn-failing-lambda-task-execution-diagnosis sfn-legacy-lambda-arn-task-failure-events sfn-poller-test-state-branch-walk sfn-retry-catch-error-name-matching
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: states|GetExecutionHistory|2xx->2xx|$.events[*].executionFailedEventDetails.cause
Issue-Key: states|GetExecutionHistory|2xx->2xx|$.events[*].taskFailedEventDetails.cause
Issue-Key: states|TestState|4xx->4xx|$.message
Issue-Related: 20261109T234059Z-states-getexecutionhistory-omits-the-task-lambdafunction-started-event-shifting.md
Issue-Related: 20261112T174000Z-states-getexecutionhistory-failure-events-report-the-emulator-s-lambda.md
Issue-Commit: fc2f81e292198b4441d1fd70b1dba421b345d347

# states failure identity and refusal text are emulator-authored, not AWS's (6 causes)

> _Note: this issue was identified against the code base at commit `fc2f81e292198b4441d1fd70b1dba421b345d347`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExecutionHistory

## Description
Six net-new mismatch clusters on the states service share one fault: the text and the identity the emulator puts on a failure are its OWN, not the ones AWS records. On every history read of an execution whose Lambda Task failed, the `error` member of the TaskFailed / LambdaFunctionFailed / ExecutionFailed events is the Step Functions-side name `Lambda.AWSLambdaException` and the `cause` member is an IAM authorization sentence the emulator composes, instead of the error type and failure payload the invoked function produced; a redriven read additionally chains an event to the wrong `previousEventId`; a Lambda task timeout records a cause document whose shape is not the one AWS records; and TestState refuses an unknown stateName with a hand-written sentence instead of AWS's fixed message. HTTP statuses, event types and event ordering are otherwise correct. All six clusters are reported with no owner by the replay. 6 causes.

Cause 1 of 6: TaskFailed and ExecutionFailed on an optimized lambda:invoke Task carry the emulator's error name and an IAM-denial cause.
AWS: Smithy: GetExecutionHistory.output.events -> HistoryEventList of HistoryEvent; HistoryEvent.taskFailedEventDetails -> TaskFailedEventDetails{resourceType, resource, error -> SensitiveError @length 0..256 "The error code of the failure", cause -> SensitiveCause @length 0..32768 "A more detailed explanation of the cause of the failure"}; HistoryEvent.executionFailedEventDetails -> ExecutionFailedEventDetails{error, cause}. When a Task using the optimized arn:aws:states:::lambda:invoke integration invokes a function that raises an unhandled error, the error name recorded on BOTH the TaskFailed event and the terminal ExecutionFailed event is the function's own unhandled-error type (the recording shows the Python exception class `RuntimeError`) and the cause is the function's own failure document (the corpus asserts its shape, not a literal value). A Lambda-service error name such as Lambda.AWSLambdaException is what AWS records only when the Lambda API call itself was refused; a function that ran and raised is not that class.
Emulator: both events report error `Lambda.AWSLambdaException` and cause `User: <assumed-role arn> is not authorized to perform: lambda:InvokeFunction on resource: <function arn> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <uuid>; Proxy: null)` — a denial no part of the recorded run produced. In that string the fixed text is the "is not authorized to perform: lambda:InvokeFunction on resource: ... because no identity-based policy allows ..." template; the role ARN, the function ARN and the request id are per-run values from the request/state. The defect is not those values: it is that an authorization-refusal sentence is emitted at all, in place of the function's error type and payload.
Why: Retry/Catch `ErrorEquals` matching on the function's error name is the documented way to branch on a function failure, so error handling that works against AWS never matches against the emulator; and an operator reading the history is pointed at a non-existent IAM problem and loses the function's failure payload entirely.
Fix: the lambda:invoke Task integration's failure mapping in the states execution interpreter — the component that derives error/cause for the TaskFailed and ExecutionFailed history events must carry the invoked function's error type and payload through rather than substituting a Lambda-API authorization refusal.
Traces: sfn-failing-lambda-task-execution-diagnosis#task#10 (cluster also covers #task#12)

Cause 2 of 6: the tail/reverse read of the same history serves the same wrong error/cause, so no read path returns AWS's failure identity.
AWS: as cause 1 — the stored event reads back identically whichever page, order or filter the client asks for; executionFailedEventDetails.error is the function's error type (`RuntimeError` in the recording) and the cause is its failure document.
Emulator: on this read the terminal failure lands at $.events[0] and still reports error `Lambda.AWSLambdaException` with the same IAM-denial cause sentence.
Why: a client cannot work around the defect by re-reading the history a different way — the identity is wrong at the source, not on one page.
Fix: the same failure-mapping component as cause 1; it is the recorded event, not the read/paging path, that is wrong. (This step's $.nextToken is also the bare index '4' rather than an opaque PageToken @length 1..1024; that defect is owned by the open 20261109T234059Z-states-read-responses-echo-internal-values-non-opaque.md and is not asserted here.)
Traces: sfn-failing-lambda-task-execution-diagnosis#task#11

Cause 3 of 6: after a redrive, the re-read history repeats the wrong error/cause AND links an event to the wrong predecessor.
AWS: Smithy: HistoryEvent.id — "The id of the event. Events are numbered sequentially, starting at one"; HistoryEvent.previousEventId — "The id of the previous event", i.e. the id of the event that actually precedes it in the causal chain. On the recorded redriven execution the event at this position links back to event id 2, and its error/cause are the function's as in cause 1.
Emulator: the same `Lambda.AWSLambdaException` / IAM-denial pair on the ExecutionFailed and TaskFailed events, and previousEventId 7 where the recording links to 2 — the event is chained to the wrong predecessor.
Why: previousEventId is how a client or the console reconstructs the causal chain of a redriven execution; a wrong link attributes the failure to the wrong step, and tooling that walks back from the terminal event reaches the wrong state-entered event.
Fix: the history recorder's previousEventId assignment on the redrive path (event chaining in the execution's history store), plus the cause-1 failure mapping. ($.nextToken '5' on this step is the non-opaque-token defect owned elsewhere and is not asserted here.)
Traces: sfn-failing-lambda-task-execution-diagnosis#task#19

Cause 4 of 6: a legacy direct-ARN Lambda Task records the same wrong identity on LambdaFunctionFailed.
AWS: Smithy: HistoryEvent.lambdaFunctionFailedEventDetails -> LambdaFunctionFailedEventDetails{error -> SensitiveError, cause -> SensitiveCause}, "Contains details about a Lambda function that failed during an execution". For a Task whose Resource is a bare function ARN, AWS records the function's own unhandled-error type (`RuntimeError` in the recording) on the LambdaFunctionFailed event and on the terminal ExecutionFailed event, with the function's payload as the cause.
Emulator: both events report error `Lambda.AWSLambdaException` and the same emulator-authored lambda:InvokeFunction denial sentence, naming the legacy role and function ARNs.
Why: the legacy direct-ARN shape is what most older state machines use, so the same Catch/Retry breakage and the same loss of the function's failure payload apply there too.
Fix: the same failure-mapping component, on the legacy direct-ARN Lambda task path that emits LambdaFunctionFailed.
Traces: sfn-legacy-lambda-arn-task-failure-events#task#10

Cause 5 of 6: the cause document recorded for a Lambda task that timed out does not have the shape AWS records.
AWS: recorded behavior only — the corpus asserts the SHAPE of taskFailedEventDetails.cause for a Task whose Lambda invocation timed out (so the per-run request id inside it is deliberately not compared), and the emulator's document does not satisfy that shape. The error name on these events matches, so only the cause document diverges.
Emulator: cause `{"errorMessage":"RequestId: <uuid> Error: Task timed out after 2.00 seconds","errorType":"Sandbox.Timedout"}` — on both the first-attempt and the retried TaskFailed event. Fixed text here is "RequestId: " / " Error: Task timed out after " / " seconds" and the errorType `Sandbox.Timedout`; variable are the request id and the configured timeout. Hypothesis: the emulator hand-builds this document (member set/ordering) rather than passing through the timeout payload Lambda returns, which is what makes the shape assertion fail; the per-run request id is NOT the asserted difference.
Why: a Retry/Catch definition that inspects the cause document of a timed-out Lambda task, and any operator diagnosing a timeout, read a document with a different shape from AWS's for the same failure.
Fix: the Lambda task timeout path of the same failure-mapping component — build taskFailedEventDetails.cause from the invocation's returned timeout payload instead of composing one. Open issues 20261105T115709Z-states-retry-catch-and-legacy-lambda-task-modelling-diverge.md and 20261103T052008Z-states-execution-outcome-error-name-and-cause-text-diverge.md cover neighbouring Retry/Catch and DescribeExecution-level wording; the replay reports this cluster unowned by either.
Traces: sfn-retry-catch-error-name-matching#task#15

Cause 6 of 6: TestState's refusal for a stateName the definition does not declare uses emulator wording instead of AWS's message.
AWS: Smithy: TestState.input.stateName -> TestStateStateName @length 1..80; TestState models ValidationException (client error, HTTP 400, wire __type com.amazonaws.swf.service.v2.model#ValidationException). When stateName names no state in the submitted definition AWS answers HTTP 400 with the fixed message `State not found in definition` — the whole message is fixed text, with no variable part interpolated from the request.
Emulator: the same HTTP 400 and the same error type, but message `stateName 'Wait30seconds' names no state the definition declares` — emulator-authored wording that additionally interpolates the submitted state name (`Wait30seconds` in this recording is the per-request value, not part of any template).
Why: an error message is a requirement of the API: a client or test that asserts on AWS's refusal text, or an operator matching logs, sees a message AWS never produces for this error class.
Fix: TestState's stateName validation in its handler — emit AWS's fixed `State not found in definition` message for this refusal. The open 20261103T052008Z-states-request-validation-gaps-and-non-opaque-pagination-tokens.md owns other TestState validation gaps (including the mock result-schema refusal on task#7 of this same fixture) but does not carry this cluster's replay key.
Traces: sfn-poller-test-state-branch-walk#task#9

Note on already-filed issues: 20261112T174000Z-states-getexecutionhistory-failure-events-report-the-emulator-s-lambda.md describes the same substitution on GetExecutionHistory failure events, and 20261103T052008Z-states-execution-outcome-error-name-and-cause-text-diverge.md owns it as seen through DescribeExecution's execution-level error/cause. The replay reports all six clusters here with NO owner — none of those issues carries the replay keys for the cause members, the redrive previousEventId link, the timeout cause document or the TestState stateName message — so they are filed here and those issues are named for the fixer to reconcile.

## Evidence
All six causes are mismatch clusters from the replay of simparity-wire-aws-v1 (68 fixtures, 787 steps, 24 mismatched) against the emulator built from this checkout. Statuses were as expected on every cause except cause 6's text; only the bodies diverged.

Cause 1 — sfn-failing-lambda-task-execution-diagnosis#task#10, GetExecutionHistory, 200 -> 200 (cluster also covers #task#12):
  $.events[4].taskFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[4].taskFailedEventDetails.cause: expected a shaped (not literally asserted) value, got 'User: arn:aws:sts::<account>:assumed-role/<sfn role>/<session> is not authorized to perform: lambda:InvokeFunction on resource: arn:aws:lambda:<region>:<account>:function:<function> because no identity-based policy allows the lambda:InvokeFunction action (Service: AWSLambda; Status Code: 403; Error Code: AccessDeniedException; Request ID: <uuid>; Proxy: null)'
  $.events[5].executionFailedEventDetails.error / .cause: the same pair.

Cause 2 — sfn-failing-lambda-task-execution-diagnosis#task#11, GetExecutionHistory, 200 -> 200:
  $.events[0].executionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[0].executionFailedEventDetails.cause: expected a shaped value, got the same IAM-denial sentence.
  ($.nextToken: expected a shaped opaque token, got '4' — owned elsewhere, not asserted here.)

Cause 3 — sfn-failing-lambda-task-execution-diagnosis#task#19, GetExecutionHistory, 200 -> 200:
  $.events[0].executionFailedEventDetails.error / .cause and $.events[1].taskFailedEventDetails.error / .cause: same expected-vs-actual pairs as cause 1.
  $.events[3].previousEventId: expected 2, got 7.
  ($.nextToken: expected a shaped opaque token, got '5' — owned elsewhere.)

Cause 4 — sfn-legacy-lambda-arn-task-failure-events#task#10, GetExecutionHistory, 200 -> 200:
  $.events[4].lambdaFunctionFailedEventDetails.error: expected 'RuntimeError', got 'Lambda.AWSLambdaException'
  $.events[4].lambdaFunctionFailedEventDetails.cause: expected a shaped value, got the IAM-denial sentence naming the legacy role and function ARNs
  $.events[5].executionFailedEventDetails.error / .cause: the same pair.

Cause 5 — sfn-retry-catch-error-name-matching#task#15, GetExecutionHistory, 200 -> 200:
  $.events[4].taskFailedEventDetails.cause: expected a shaped value, got '{"errorMessage":"RequestId: <uuid> Error: Task timed out after 2.00 seconds","errorType":"Sandbox.Timedout"}'
  $.events[7].taskFailedEventDetails.cause: the same divergence on the retried attempt.

Cause 6 — sfn-poller-test-state-branch-walk#task#9, TestState, 400 -> 400:
  $.message: expected 'State not found in definition', got "stateName 'Wait30seconds' names no state the definition declares".

## Steps to Reproduce
1. Build the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Cause 1 (representative): fixture sfn-failing-lambda-task-execution-diagnosis, step task#10 — start an execution whose Task uses the optimized arn:aws:states:::lambda:invoke integration against a function that raises an unhandled error, wait for the terminal status, then call GetExecutionHistory and compare $.events[4].taskFailedEventDetails.error/.cause and $.events[5].executionFailedEventDetails.error/.cause: the recording carries the function's error type (RuntimeError) and its failure payload; the emulator carries 'Lambda.AWSLambdaException' and a lambda:InvokeFunction IAM-denial sentence. Step task#12 of the same fixture is the same cluster.
3. Cause 2: same fixture, step task#11 — re-read the same history on the tail/reverse page and compare $.events[0].executionFailedEventDetails.error/.cause.
4. Cause 3: same fixture, step task#19 — redrive the execution, re-read the history and compare $.events[0]/$.events[1] failure details plus $.events[3].previousEventId (recording links to event id 2, emulator to 7).
5. Cause 4: fixture sfn-legacy-lambda-arn-task-failure-events, step task#10 — same run shape with a Task whose Resource is the bare function ARN; compare $.events[4].lambdaFunctionFailedEventDetails.error/.cause and $.events[5].executionFailedEventDetails.error/.cause.
6. Cause 5: fixture sfn-retry-catch-error-name-matching, step task#15 — run a state machine whose Lambda Task times out and is retried, then call GetExecutionHistory and compare the taskFailedEventDetails.cause documents on $.events[4] and $.events[7] against the shape the recording asserts (ignore the per-run request id inside them).
7. Cause 6: fixture sfn-poller-test-state-branch-walk, step task#9 — call TestState with a stateName that the submitted definition does not declare; both answer HTTP 400 ValidationException, but compare $.message against AWS's fixed 'State not found in definition'.
