Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.choice-state-exited-output-and-map-choice-history-count stepfunctions.choice-timestampgreaterthanpath-non-string-target-rejected stepfunctions.deep-sfn-language-choice-operand-path-null-vs-absent-rejected stepfunctions.jsonata-task-arguments-evaluating-to-non-object-fails-with-states-runtime stepfunctions.deep-sfn-3-parallel-retry-reruns-completed-sibling stepfunctions.deep-sfn-2-sfn-jsonata-context-retry-count-increments-per-attempt stepfunctions.deep-sfn-1-sfn-sync-express-machine-sync-task-rejected-at-runtime stepfunctions.jsonpath-assign-reads-task-catch-map-and-choice-rule
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.choice-state-exited-output-and-map-choice-history-count|run#10|AssertionError|assert types == [
Issue-Key: stepfunctions.choice-timestampgreaterthanpath-non-string-target-rejected|run#10|AssertionError|assert bad.get("cause") == (
Issue-Key: stepfunctions.deep-sfn-language-choice-operand-path-null-vs-absent-rejected|run#9|AssertionError|assert null_run.get("cause") == (
Issue-Key: stepfunctions.jsonata-task-arguments-evaluating-to-non-object-fails-with-states-runtime|run#11|AssertionError|assert exe.get("cause") == (
Issue-Key: stepfunctions.deep-sfn-3-parallel-retry-reruns-completed-sibling|run#7|AssertionError|assert desc["status"] == "SUCCEEDED", (
Issue-Key: stepfunctions.deep-sfn-2-sfn-jsonata-context-retry-count-increments-per-attempt|run#7|AssertionError|assert exe["status"] == "SUCCEEDED", (
Issue-Key: sfn|CreateStateMachine|InvalidDefinition
Issue-Commit: 1945f04af551ab9122ba85c8ecd7b899fcb2daf4

# stepfunctions: 7 net-new causes — Choice/Arguments runtime cause taxonomy, Map iteration history order, Parallel Retry timing, context RetryCount, EXPRESS .sync over-rejection and Catch-Assign variable analysis

> _Note: this issue was identified against the code base at commit `1945f04af551ab9122ba85c8ecd7b899fcb2daf4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Seven independent, net-new root causes in the Step Functions interpreter and its create-time definition analysis. None is covered by an open issue; where an open issue is adjacent it is named in the cause.

Cause 1 of 7: an INLINE Map's iteration history events are interleaved per iteration instead of all iterations being started first.
AWS: recorded behavior only (GetExecutionHistory is not modelled down to event order; Smithy: GetExecutionHistory -> HistoryEventList). For a Map with ItemsPath over 2 items, no MaxConcurrency (i.e. unlimited) and a single Pass ItemProcessor, the recorded history is exactly, in order: ExecutionStarted, MapStateEntered, MapStateStarted, MapIterationStarted, MapIterationStarted, PassStateEntered, PassStateExited, MapIterationSucceeded, PassStateEntered, PassStateExited, MapIterationSucceeded, MapStateSucceeded, MapStateExited, ChoiceStateEntered, ChoiceStateExited, SucceedStateEntered, SucceedStateExited, ExecutionSucceeded — one MapIterationStarted per item emitted BEFORE any iteration body runs, because an unlimited-concurrency Map starts every iteration at once. The event count (18) and the id of the final event (18) are part of the requirement; the number of MapIterationStarted events equals the number of items.
Emulator: the same 18 event types but serialized per iteration — MapIterationStarted, PassStateEntered, PassStateExited, MapIterationSucceeded, MapIterationStarted, PassStateEntered, PassStateExited, MapIterationSucceeded — so the second iteration is only started after the first one finished.
Why: a client that reads the history to reconstruct fan-out (how many iterations were dispatched, and when) sees a sequential walk and concludes the Map ran with MaxConcurrency 1; event ids no longer line up with the recorded positions, so id-based correlation of iteration events breaks.
Fix: the Map state's execution/history emission for INLINE mode — start (and record MapIterationStarted for) every iteration admitted by MaxConcurrency before running any iteration body.
Traces: stepfunctions.choice-state-exited-output-and-map-choice-history-count

Cause 2 of 7: every Choice comparator *Path runtime failure collapses into one generic cause; AWS words three distinct ones.
AWS: recorded behavior only for the exact sentences (the ASL Choice-state rules are documented at amazon-states-language-choice-state.html; the execution carries error States.Runtime, Smithy: DescribeExecution.error/cause). The fixed text differs per failure class, and only the path and state name are taken from the request/definition:
  - comparator target resolves to a value of the wrong type (TimestampGreaterThanPath pointing at the number 42): cause = "An error occurred while executing the state '<StateName>' (entered at the event id #<id>). Invalid value provided at path '<path>'"
  - comparator operand resolves to an explicit null (StringEqualsPath pointing at null): cause = "An error occurred while executing the state '<StateName>' (entered at the event id #<id>). Null value provided at path '<path>': Unable to compare with a null input."
  - comparator operand does not resolve at all (absent member): cause = "An error occurred while executing the state '<StateName>' (entered at the event id #<id>). Invalid path '<path>': The choice state's condition path references an invalid value."
Fixed text is the sentence after the state preamble; '<StateName>', '<id>' and '<path>' come from the definition and the request's input. The null case and the absent case must not be byte-identical.
Emulator: all three produce the third (absent-operand) sentence. Observed for a number-valued TimestampGreaterThanPath target: "An error occurred while executing the state 'Route' (entered at the event id #2). Invalid path '$.threshold': The choice state's condition path references an invalid value.", and for an explicit null StringEqualsPath operand: "An error occurred while executing the state 'C' (entered at the event id #2). Invalid path '$.bar': The choice state's condition path references an invalid value." Status/error (FAILED / States.Runtime) are right; only the cause text is wrong.
Why: a client that branches on the cause text (or surfaces it to an operator) cannot tell a mistyped value from a null from a missing member — three different authoring bugs read as one.
Fix: the Choice state's comparator evaluation, where the States.Runtime cause is composed — classify wrong-type, explicit-null and unresolvable separately. Adjacent but different: open issue 20261108T033044Z-stepfunctions-choice-timestamp-comparators-do-not-compare-instants covers Timestamp comparators not comparing instants at all; this cause is the failure wording.
Traces: stepfunctions.choice-timestampgreaterthanpath-non-string-target-rejected, stepfunctions.deep-sfn-language-choice-operand-path-null-vs-absent-rejected

Cause 3 of 7: a JSONata Task whose Arguments evaluates to a non-object fails with the emulator's own sentence instead of AWS's.
AWS: recorded behavior only (Task state documented at amazon-states-language-task-state.html). error = States.Runtime and cause = "An error occurred while executing the state '<StateName>' (entered at the event id #<id>). The Parameters '<json of the evaluated value>' could not be used to start the Task: [The value for the 'Arguments' field cannot be a string]" when the value is a string, and the same sentence ending "[The value for the 'Arguments' field must be an object]" when it is an array. Fixed text is everything but '<StateName>', '<id>' and the rendered value; the rendered value is the compact JSON of what the expression evaluated to. The history of the uncaught case is exactly [ExecutionStarted, TaskStateEntered, ExecutionFailed] with no EvaluationFailed event, and a States.ALL Catch must NOT intercept it.
Emulator: cause = "An error occurred while executing the state 'T' (entered at the event id #2). The value referenced by Arguments must be a JSON object, but the value was: \"hello\"" — emulator-authored wording, no "could not be used to start the Task" clause and no bracketed per-type reason.
Why: a client matching the documented sentence (or distinguishing the string case from the array case by the bracketed reason) does not match at all.
Fix: the JSONata Task state's Arguments validation, where the States.Runtime cause is composed.
Traces: stepfunctions.jsonata-task-arguments-evaluating-to-non-object-fails-with-states-runtime

Cause 4 of 7: a Retry on a Parallel state does not wait IntervalSeconds between attempts, so a time-gated branch fails every attempt.
AWS: recorded behavior only (Retry semantics documented at concepts-error-handling.html). A Parallel-level Retry re-runs the whole Parallel including the sibling branch that already succeeded, and the re-run happens at least IntervalSeconds after the first attempt: for Retry [{ErrorEquals:[<err>], IntervalSeconds:5, MaxAttempts:2, BackoffRate:1.0}] the recorded run enters Sibling twice and Gate twice, Flake once and Steady once, with the second Sibling entry >= IntervalSeconds after the first, and the execution SUCCEEDS with output ["sibling-done", "flaky-done"].
Emulator: the Parallel is retried the full MaxAttempts (three ParallelStateStarted events) but every attempt re-enters Flake, so the execution ends FAILED with error 'Sim2Real.Transient' / cause 'first attempt only'. The branch's gate is a JSONata Choice comparing ($toMillis($now()) - $toMillis($states.context.Execution.StartTime)) against 3000 ms, so on AWS the 5 s retry wait takes the second attempt past the gate. Hypothesis: the retry is re-dispatched immediately — the Retry's IntervalSeconds/BackoffRate wait is not observed for a Parallel state — or, equivalently, $now() does not advance across attempts (a per-execution frozen clock).
Why: every retry ladder a client relies on for back-off becomes instantaneous: a transient condition that clears with time never clears, retries are exhausted in milliseconds, and executions that succeed on AWS fail here.
Fix: the retrier that re-dispatches a Parallel (and the Retry wait generally) — honour IntervalSeconds * BackoffRate^(attempt-1) before the re-run; and the clock behind $now() must advance during an execution.
Traces: stepfunctions.deep-sfn-3-parallel-retry-reruns-completed-sibling

Cause 5 of 7: the execution context object's State omits RetryCount, so any state reading $$.State.RetryCount fails the execution.
AWS: recorded behavior only (context object documented at input-output-contextobject.html). $$.State carries Name, EnteredTime and RetryCount; a Parallel state whose Parameters contain {"rc.$": "$$.State.RetryCount"} resolves to 0 on the first attempt and increments per retry attempt, and the execution SUCCEEDS.
Emulator: the execution FAILS with error States.Runtime and cause "An error occurred while executing the state 'Attempt' (entered at the event id #2). The JSONPath '$$.State.RetryCount' specified for the field 'rc.$' could not be found in the input '{...\"State\":{\"Name\":\"Attempt\",\"EnteredTime\":\"...\"}}'" — the serialized context object shows State with only Name and EnteredTime.
Why: any definition that reads its own retry attempt (to log it, to key an idempotency record, or to branch on it) fails outright instead of running.
Fix: the context-object construction for a state — populate State.RetryCount (and keep it incrementing across Retry attempts). Note: open issue 20261121T033848Z-lambda-stepfunctions-xray-the-execution-context-object-s describes the same omission under the lambda__stepfunctions__xray heading; this check is listed under stepfunctions and is not marked owned, so it is filed here and should be closed together with it.
Traces: stepfunctions.deep-sfn-2-sfn-jsonata-context-retry-count-increments-per-attempt

Cause 6 of 7: CreateStateMachine refuses an EXPRESS definition using states:startExecution.sync:2, which AWS accepts at create and fails only at run time.
AWS: recorded behavior only. CreateStateMachine (Smithy: CreateStateMachine errors include InvalidDefinition, 400) ACCEPTS an EXPRESS state machine whose Task Resource is arn:aws:states:::states:startExecution.sync:2; every execution of it then fails at that state with States.Runtime and starts no child, while the byte-identical STANDARD machine runs its child to completion — the refusal comes from the workflow type at run time, not from create-time schema validation.
Emulator: CreateStateMachine is refused 400 InvalidDefinition with message "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.sync' service integration at /States/CallChild/Resource'", so the machine never exists and the run-time behaviour can never be observed.
Why: a client that deploys such an EXPRESS machine (legitimately on AWS, failing per-execution) cannot create it at all; CloudFormation/CDK deployments of it roll back.
Fix: the create-time ASL schema validation for EXPRESS type — do not reject the states:startExecution.sync:2 Resource at create; raise States.Runtime at the state when the execution reaches it. (The '.waitForTaskToken' create-time refusal, covered by open issue 20261121T143000Z-stepfunctions-definition-validation-and-list-clause-remainder cause 1, is a different rule and must stay.)
Traces: stepfunctions.deep-sfn-1-sfn-sync-express-machine-sync-task-rejected-at-runtime

Cause 7 of 7: a variable assigned by a Catch clause's Assign is not seen by the create-time undefined-variable analysis.
AWS: recorded behavior only (variables documented at workflow-variables.html). A JSONPath definition whose Parallel state carries Catch[{ErrorEquals:[States.ALL], Assign:{"errVar.$":"$.Error"}, Next:...}] and whose later Pass state reads {"errVar.$":"$errVar"} is ACCEPTED by CreateStateMachine and runs: the variable is in scope downstream of the Catch transition.
Emulator: CreateStateMachine is refused 400 InvalidDefinition with "Invalid State Machine Definition: 'UNDEFINED_VARIABLE: The variable '$errVar' is not defined. at /States/Emit/Parameters/errVar.$'" — the static analysis collects Assign from state-level and Choice-rule positions but not from a Catcher.
Why: the whole documented pattern of capturing a caught error into a variable is unusable; the machine cannot be created.
Fix: the create-time variable-scope analysis — treat a Catcher's Assign (like a Choice rule's Assign) as defining its variables on that catcher's Next path.
Traces: stepfunctions.jsonpath-assign-reads-task-catch-map-and-choice-rule

## Evidence
Cause 1: AssertionError: a Map over 2 items followed by a Choice must produce exactly this 18-event history, got 18 events: ['ExecutionStarted', 'MapStateEntered', 'MapStateStarted', 'MapIterationStarted', 'PassStateEntered', 'PassStateExited', 'MapIterationSucceeded', 'MapIterationStarted', 'PassStateEntered', 'PassStateExited', 'MapIterationSucceeded', 'MapStateSucceeded', 'MapStateExited', 'ChoiceStateEntered', 'ChoiceStateExited', 'SucceedStateEntered', 'SucceedStateExited', 'ExecutionSucceeded'] (expected the two MapIterationStarted events adjacent, before the first PassStateEntered).

Cause 2: expected cause "An error occurred while executing the state 'Route' (entered at the event id #2). Invalid value provided at path '$.threshold'", got "... Invalid path '$.threshold': The choice state's condition path references an invalid value."; expected "An error occurred while executing the state 'C' (entered at the event id #2). Null value provided at path '$.bar': Unable to compare with a null input.", got "... Invalid path '$.bar': The choice state's condition path references an invalid value."

Cause 3: expected cause "An error occurred while executing the state 'T' (entered at the event id #2). The Parameters '\"hello\"' could not be used to start the Task: [The value for the 'Arguments' field cannot be a string]", got "An error occurred while executing the state 'T' (entered at the event id #2). The value referenced by Arguments must be a JSON object, but the value was: \"hello\"".

Cause 4: AssertionError: the retried Parallel must SUCCEED on its second attempt; got 'FAILED' error='Sim2Real.Transient' cause='first attempt only' history=[... ParallelStateStarted, PassStateEntered, PassStateExited, ChoiceStateEntered, ChoiceStateExited, FailStateEntered, ParallelStateFailed, (x3) ..., ExecutionFailed] — Flake entered on all three attempts; expected state entry counts {'Sibling': 2, 'Gate': 2, 'Flake': 1, 'Steady': 1} and the second Sibling entry >= 5 s after the first.

Cause 5: AssertionError: execution 'FAILED': error='States.Runtime' cause="... The JSONPath '$$.State.RetryCount' specified for the field 'rc.$' could not be found in the input '{...,\"State\":{\"Name\":\"Attempt\",\"EnteredTime\":\"...\"}}'" — expected SUCCEEDED with rc = 0,1,2 across attempts.

Cause 6: botocore.errorfactory.InvalidDefinition: An error occurred (InvalidDefinition) when calling the CreateStateMachine operation: Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: Express state machine does not support '.sync' service integration at /States/CallChild/Resource' — expected the create to succeed and the execution to fail States.Runtime.

Cause 7: botocore.errorfactory.InvalidDefinition: An error occurred (InvalidDefinition) when calling the CreateStateMachine operation: Invalid State Machine Definition: 'UNDEFINED_VARIABLE: The variable '$errVar' is not defined. at /States/Emit/Parameters/errVar.$' — expected the create to succeed and the execution to emit errVar = the caught error name.

## Steps to Reproduce
1. (Cause 1) CreateStateMachine with a Map (ItemsPath $.items, INLINE ItemProcessor of one Pass, no MaxConcurrency) followed by a Choice and a Succeed; StartExecution with input {"items":[{"value":1},{"value":2}]}; page GetExecutionHistory and compare the ordered event-type list and the final event id.
2. (Cause 2) CreateStateMachine with a Choice whose rule is {"Variable":"$$.State.EnteredTime","TimestampGreaterThanPath":"$.threshold"}; run it with input {"threshold":42} and read DescribeExecution.cause. Then a Choice rule {"Variable":"$.foo","StringEqualsPath":"$.bar"} run with {"foo":"x","bar":null} and with {"foo":"x"}; the two causes must differ.
3. (Cause 3) CreateStateMachine with QueryLanguage JSONata and a Task (Resource arn:aws:states:::aws-sdk:sts:getCallerIdentity) whose Arguments is "{% $states.input.v %}"; run with {"v":"hello"} and with {"v":[1,2,3]}; read DescribeExecution.error/cause and GetExecutionHistory event types.
4. (Cause 4) CreateStateMachine with a Parallel of two branches (one a Pass, one a JSONata Choice gating on elapsed time since $states.context.Execution.StartTime into a Fail) and Retry [{ErrorEquals:[that error], IntervalSeconds:5, MaxAttempts:2, BackoffRate:1.0}]; StartExecution and compare per-state entry counts and the gap between the two Sibling entries.
5. (Cause 5) CreateStateMachine with a Parallel whose Parameters are {"run.$":"$.run","rc.$":"$$.State.RetryCount"}; StartExecution and read DescribeExecution.
6. (Cause 6) CreateStateMachine type=EXPRESS with a Task Resource arn:aws:states:::states:startExecution.sync:2 — it must be created; then StartSyncExecution/StartExecution and observe States.Runtime at that state, and the same definition as STANDARD running the child to completion.
7. (Cause 7) CreateStateMachine (JSONPath) with a Parallel carrying Catch[{ErrorEquals:["States.ALL"],Assign:{"errVar.$":"$.Error"},Next:"..."}] and a later Pass reading {"errVar.$":"$errVar"} — it must be created.
