Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__stepfunctions.stop-execution-records-abort-and-truncates-state-history
Issue-Key: cloudformation__stepfunctions.stop-execution-records-abort-and-truncates-state-history|run#13|AssertionError|assert types == ["ExecutionStarted", "WaitStateEntered", "ExecutionAborted"], (
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# cloudformation__stepfunctions: StopExecution inside a Wait state appends an extra WaitStateAborted history event that AWS never records

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExecutionHistory

## Description
One net-new root cause.

Cause 1 of 1: GetExecutionHistory on an execution ended by StopExecution while it was inside a Wait state contains an extra per-state WaitStateAborted event. Failing check id: cloudformation__stepfunctions.stop-execution-records-abort-and-truncates-state-history.

AWS:      recorded behavior only. The recording shows that when StopExecution ends a running execution that is suspended inside a top-level Wait state, the execution's history is truncated at the interrupted state: it is exactly ExecutionStarted, WaitStateEntered, ExecutionAborted, in that order. The interrupted Wait state is Entered and then never left — there is no WaitStateExited, no per-state abort event, and the Wait state's Next transition to the following Succeed state never fires (no SucceedStateEntered). The only record of the abort is the execution-level ExecutionAborted event. Smithy (com.amazonaws.sfn): HistoryEventType does declare the value WaitStateAborted (alongside MapStateAborted, ParallelStateAborted, TaskStateAborted), so the value exists in the model — but the recording shows AWS does not emit it for a top-level Wait state ended by StopExecution. The DescribeExecution side of the same recording is matched by the emulator: status ABORTED, error and cause echoed back from the caller's StopExecution request, no output member, and a second StopExecution on the now-terminal execution is an idempotent no-op that neither rewrites error/cause nor moves stopDate.

Emulator: the history carries a fourth event. GetExecutionHistory (paginated, reverseOrder=false) returns the event types ['ExecutionStarted', 'WaitStateEntered', 'WaitStateAborted', 'ExecutionAborted'] — a WaitStateAborted inserted between WaitStateEntered and ExecutionAborted.

Why:      a client that reads execution history gets a state-level event AWS never recorded. Anything that replays or counts history — audit/export pipelines, per-state timing or step-level metrics derived from Entered/Exited pairs, logic that asks whether a state was left, and arithmetic over event ids / previousEventId chains — sees an extra event and shifted ids, so an execution that AWS reports as interrupted-inside-Wait is reported as having a distinct per-state abort record. Downstream comparisons of history length or of a fixed event index silently disagree with AWS.

Fix:      the Step Functions interpreter's StopExecution path and the execution-history writer it drives — the component that appends history events when a running execution is aborted must stop the interpreter at the entered state without emitting a per-state WaitStateAborted event, leaving ExecutionAborted as the only abort record.

Traces:   cloudformation__stepfunctions.stop-execution-records-abort-and-truncates-state-history (listed under the suite report's ### cloudformation__stepfunctions heading).

## Evidence
Check: cloudformation__stepfunctions.stop-execution-records-abort-and-truncates-state-history [mismatch]

State machine definition under test (ASL): StartAt "HoldForOperator"; States = { "HoldForOperator": {"Type": "Wait", "Seconds": 120, "Next": "TaskSucceeded"}, "TaskSucceeded": {"Type": "Succeed"} }. The 120 s wait cannot elapse within the check, so StopExecution is the only thing that can end the execution.

Sequence: StartExecution -> poll GetExecutionHistory until WaitStateEntered is present -> StopExecution(executionArn, error="OperatorAbort", cause="stopped by the reconciliation operator") -> poll DescribeExecution to terminal.

DescribeExecution (agrees with AWS): status ABORTED, error "OperatorAbort", cause "stopped by the reconciliation operator", no output member. A second StopExecution(error="SecondAbort", ...) is a no-op: error stays "OperatorAbort" and stopDate is unchanged.

GetExecutionHistory event types, paginated with reverseOrder=false:
  Expected (recorded AWS): ['ExecutionStarted', 'WaitStateEntered', 'ExecutionAborted']
  Actual (emulator):       ['ExecutionStarted', 'WaitStateEntered', 'WaitStateAborted', 'ExecutionAborted']

Failure text:
  File "check_cloudformation__stepfunctions_stop_execution_records_abort_and_truncates_state_history.py", line 60, in run
    assert types == ["ExecutionStarted", "WaitStateEntered", "ExecutionAborted"], (
  AssertionError: the aborted run's history is ['ExecutionStarted', 'WaitStateEntered', 'WaitStateAborted', 'ExecutionAborted']

The divergence is the presence of the WaitStateAborted event type; the execution arn, name, timestamps and event ids in the run are per-run values and are not part of the requirement.

Model evidence: com.amazonaws.sfn#HistoryEventType includes WaitStateEntered, WaitStateExited and WaitStateAborted, so the emitted type is modelled — the divergence is that AWS does not record it on this path.

## Steps to Reproduce
1. CreateStateMachine with definition {"StartAt":"HoldForOperator","States":{"HoldForOperator":{"Type":"Wait","Seconds":120,"Next":"TaskSucceeded"},"TaskSucceeded":{"Type":"Succeed"}}} under a Step Functions execution role.
2. StartExecution with any input.
3. Poll GetExecutionHistory (reverseOrder=false, all pages) until the event types contain "WaitStateEntered" — i.e. the interpreter is inside the Wait state.
4. StopExecution(executionArn, error="OperatorAbort", cause="stopped by the reconciliation operator").
5. Poll DescribeExecution until status != RUNNING; it is ABORTED with the caller's error/cause and no output (this matches AWS).
6. Read the full GetExecutionHistory event-type sequence.
   Expected: ['ExecutionStarted', 'WaitStateEntered', 'ExecutionAborted'] — no WaitStateExited, no per-state abort event, no SucceedStateEntered.
   Actual: ['ExecutionStarted', 'WaitStateEntered', 'WaitStateAborted', 'ExecutionAborted'].
7. StopExecution a second time on the terminal execution: it remains a no-op in both (error, stopDate and history unchanged), so only the extra WaitStateAborted event diverges.
