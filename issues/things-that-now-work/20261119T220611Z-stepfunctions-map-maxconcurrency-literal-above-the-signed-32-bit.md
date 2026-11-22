Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.map-maxconcurrency-zero-and-above-int32-accepted-every-iteration-runs
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.map-maxconcurrency-zero-and-above-int32-accepted-every-iteration-runs|_run_one#5|AssertionError|assert result["status"] == "SUCCEEDED", (
Issue-Commit: 93174c59dc6eaf545661028161c23f9dd16ed610

# Map MaxConcurrency literal above the signed-32-bit maximum fails the execution with States.Runtime instead of running every iteration

> _Note: this issue was identified against the code base at commit `93174c59dc6eaf545661028161c23f9dd16ed610`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: a static ASL `MaxConcurrency` literal greater than 2147483647 on an inline Map is rejected at run time by the interpreter, so a state machine that AWS accepts and runs to completion ends FAILED. Failing check id: stepfunctions.map-maxconcurrency-zero-and-above-int32-accepted-every-iteration-runs.

AWS: a Map state whose `MaxConcurrency` is an integer literal one above the signed-32-bit maximum (2147483648) is accepted by CreateStateMachine and, when started, runs every iteration of its ItemProcessor; the execution reaches SUCCEEDED and DescribeExecution.output carries one result per input item, in item order. The same is true of an explicit `MaxConcurrency: 0` (which the emulator now handles). Smithy: CreateStateMachine.definition is an opaque `Definition` string (@length 1..1048576, @sensitive) with no per-field modeling of the ASL body, and the operation's only definition-shaped refusal is InvalidDefinition (400); there is no modeled constraint on MaxConcurrency at all, so the bound is an ASL rule and this is recorded behavior only. The recording shows the whole flow succeeding: CreateStateMachine 200, StartExecution 200, DescribeExecution status SUCCEEDED.

Emulator: CreateStateMachine accepts the definition (no InvalidDefinition), StartExecution returns 200, but the execution terminates with status 'FAILED', error 'States.Runtime' and cause "An error occurred while executing the state 'MapState' (entered at the event id #2). The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647, but the value was: 2147483648.0". In that cause, the fixed text is "An error occurred while executing the state '<StateName>' (entered at the event id #<id>). The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647, but the value was: <value>."; 'MapState' is the state name taken from the submitted definition, '#2' is the entering event id from that execution's own history, and the trailing value is the rejected number — the recorded run's particular name, id and number are template fill, not requirements. Note also that the emulator renders the integer literal 2147483648 as '2147483648.0', i.e. the literal is coerced to a float before the range test, which is a second symptom of the same parse path. The companion rule for the *dynamic* form (MaxConcurrencyPath/runtime-resolved values that overflow or go negative) is a different behavior and is already tracked by 20261110T010312Z-stepfunctions-6-remaining-causes-nested-suspension-credentials-map-concurrency.md (check stepfunctions.map-maxconcurrency-runtime-overflow-and-negative-path-fail-with-states-runtime); the defect here is that the emulator applies that path-value range rule to a STATIC literal, where AWS does not fail the execution. The earlier MaxConcurrency issues 20261107T081525Z-lambda-sqs-stepfunctions-3-net-new-causes-a.md and 20261107T233020Z-lambda-sqs-stepfunctions-a-map-state-with-maxconcurrency.md are marked fixed and do not cover this bound; the MaxConcurrency=0 half of this check passes, confirming those fixes landed.

Why: a client whose state machine declares a deliberately huge MaxConcurrency to mean "no practical limit" — a common idiom, and what CDK/templates emit when concurrency is meant to be unbounded — gets a successful CreateStateMachine and then every execution failing with States.Runtime. The failure is invisible until run time, is not catchable by the author's expectations of a valid definition, and no iteration output is produced, so downstream states and callers of DescribeExecution see FAILED where AWS produces SUCCEEDED with the full ordered result array.

Fix: the Map state's MaxConcurrency resolution and range validation in the ASL interpreter's Map-state execution path (the component that evaluates a state's concurrency before fan-out), plus the numeric parse that feeds it: a static integer literal must be parsed as an integer (not coerced to a float) and a value above the signed-32-bit maximum must be clamped to the effective concurrency ceiling rather than raising States.Runtime, leaving the 0..2147483647 range message for the dynamic/path-resolved form only.

Traces: stepfunctions.map-maxconcurrency-zero-and-above-int32-accepted-every-iteration-runs — a STANDARD state machine with an inline Map over 5 items, ItemsPath $.items, ItemProcessor a single Pass using States.MathAdd; MaxConcurrency=0 succeeds, MaxConcurrency=2147483648 fails.

## Evidence
Check: stepfunctions.map-maxconcurrency-zero-and-above-int32-accepted-every-iteration-runs (verdict: mismatch).

Expected (AWS): DescribeExecution status == 'SUCCEEDED' and output == [20, 40, 60, 80, 100] for input {"items": [{"n":10},{"n":20},{"n":30},{"n":40},{"n":50}]} with Map MaxConcurrency = 2147483648.

Actual (emulator): status='FAILED', error='States.Runtime', cause="An error occurred while executing the state 'MapState' (entered at the event id #2). The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647, but the value was: 2147483648.0".

Assertion raised by the check:
  File "check_stepfunctions_map_maxconcurrency_zero_and_above_int32_accepted_every_iteration_runs.py", line 155, in run
    overflow_output = _run_one(ctx, role_arn, _ABOVE_INT32)
  File "...", line 134, in _run_one
    assert result["status"] == "SUCCEEDED"
  AssertionError: a Map with MaxConcurrency=2147483648 over 5 items must SUCCEED; got status='FAILED' error='States.Runtime' cause="... must be a whole number from 0 to 2147483647, but the value was: 2147483648.0"

The MaxConcurrency=0 leg of the same check passed, so the divergence is specific to the above-int32 literal. Smithy (service-com-amazonaws-sfn-awsstepfunctions): CreateStateMachine.definition -> Definition (string, @length 1..1048576); modeled definition error is InvalidDefinition (400) only; no modeled MaxConcurrency constraint.

## Steps to Reproduce
1. Create an IAM role trusted by states.amazonaws.com.
2. CreateStateMachine (type STANDARD) with definition:
   {"StartAt":"MapState","States":{"MapState":{"Type":"Map","ItemsPath":"$.items","MaxConcurrency":2147483648,"ItemProcessor":{"ProcessorConfig":{"Mode":"INLINE"},"StartAt":"Double","States":{"Double":{"Type":"Pass","Parameters":{"doubled.$":"States.MathAdd($.n, $.n)"},"End":true}}},"End":true}}}
   -> accepted (200), as on AWS.
3. StartExecution with input {"items":[{"n":10},{"n":20},{"n":30},{"n":40},{"n":50}]}.
4. Poll DescribeExecution until terminal.
   Expected: status SUCCEEDED, output [20,40,60,80,100].
   Observed: status FAILED, error 'States.Runtime', cause "... The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647, but the value was: 2147483648.0".
5. Repeat step 2-4 with MaxConcurrency 0 to confirm that leg succeeds, isolating the above-int32 literal as the diverging input.
