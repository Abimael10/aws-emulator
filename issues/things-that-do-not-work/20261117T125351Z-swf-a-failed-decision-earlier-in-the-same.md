Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: swf.cancel-timer-failed-then-cancel-workflow-execution swf.cancel-timer-failed-then-complete-workflow-execution swf.cancel-timer-failed-then-continue-as-new-workflow-execution swf.cancel-timer-failed-then-fail-workflow-execution
Issue-Shape-Id: com.amazonaws.swf#SimpleWorkflowService
Issue-Key: swf.cancel-timer-failed-then-cancel-workflow-execution|run#23|AssertionError|assert len(close_failed) == 1, (
Issue-Key: swf.cancel-timer-failed-then-complete-workflow-execution|run#23|AssertionError|assert len(close_failed) == 1, (
Issue-Key: swf.cancel-timer-failed-then-continue-as-new-workflow-execution|run#23|AssertionError|assert len(close_failed) == 1, (
Issue-Key: swf.cancel-timer-failed-then-fail-workflow-execution|run#23|AssertionError|assert len(close_failed) == 1, (
Issue-Commit: 09e6554b721404b87f89139bb0b4ce0938b372df

# swf: a failed decision earlier in the same decision list does not make a later closing decision UNHANDLED_DECISION — the run closes instead of staying open

> _Note: this issue was identified against the code base at commit `09e6554b721404b87f89139bb0b4ce0938b372df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RespondDecisionTaskCompleted

## Description
Cause 1 of 1: `RespondDecisionTaskCompleted` evaluates "has this decider seen everything?" ONCE, before it applies any decision, so a `*Failed` event that an EARLIER decision in the SAME decision list produced is not treated as an event the decider has not handled. A closing decision that follows it is therefore applied instead of being rejected, and the execution closes. Failing checks: swf.cancel-timer-failed-then-cancel-workflow-execution, swf.cancel-timer-failed-then-complete-workflow-execution, swf.cancel-timer-failed-then-continue-as-new-workflow-execution, swf.cancel-timer-failed-then-fail-workflow-execution.

AWS: a decision list that pairs a decision which fails with a workflow-closing decision records the failure first and then REJECTS the close. The recorded behavior for `CancelTimer`(unknown timerId) followed by a closing decision in one `RespondDecisionTaskCompleted` call is, in history order: `CancelTimerFailed` with `cause=TIMER_ID_UNKNOWN` and `decisionTaskCompletedEventId` = that call's own `DecisionTaskCompleted` eventId, then the matching `<Close>WorkflowExecutionFailed` event with `cause=UNHANDLED_DECISION` and the same `decisionTaskCompletedEventId`; the execution stays `OPEN` and SWF schedules a fresh decision task (`openCounts.openDecisionTasks == 1`) carrying the unhandled events. Smithy (com.amazonaws.swf): `CompleteWorkflowExecutionFailedCause`, `FailWorkflowExecutionFailedCause`, `CancelWorkflowExecutionFailedCause` and `ContinueAsNewWorkflowExecutionFailedCause` all model `UNHANDLED_DECISION`; `CancelTimerFailedCause` models `TIMER_ID_UNKNOWN`; each `*FailedEventAttributes` requires `cause` + `decisionTaskCompletedEventId` ("The ID of the DecisionTaskCompleted event corresponding to the decision task that resulted in the ... decision"). None of this is an HTTP error: `RespondDecisionTaskCompleted` itself answers 200 with the modeled empty body in both the AWS and the emulator case — the divergence is visible only in the history and in `DescribeWorkflowExecution`.

Emulator: the `CancelTimerFailed` event is recorded correctly, but the closing decision is then applied unconditionally. The history observed for all four closing decision types is [(1,'WorkflowExecutionStarted'),(2,'DecisionTaskScheduled'),(3,'DecisionTaskStarted'),(4,'DecisionTaskCompleted'),(5,'CancelTimerFailed'),(6,'WorkflowExecution{Canceled|Completed|ContinuedAsNew|Failed}')] — NO `CancelWorkflowExecutionFailed` / `CompleteWorkflowExecutionFailed` / `ContinueAsNewWorkflowExecutionFailed` / `FailWorkflowExecutionFailed` event at all, and the run is closed rather than left open with a new decision task. (The eventIds and event names above are the shape of the divergence, not a required value list; the requirement is the extra `*Failed` event before any close event and the run staying open.)

Why: a decider that pairs bookkeeping decisions with a close relies on SWF refusing the close when an earlier decision in the same batch failed — it expects the follow-up decision task so it can react to the failure before closing. Against the emulator the workflow is already closed and that decision task never arrives, so the failure is silently swallowed; subsequent polls, `DescribeWorkflowExecution` status and any compensating logic all diverge, and tests of the UNHANDLED_DECISION class pass against AWS but not here.

Fix: the decision-application loop in the `RespondDecisionTaskCompleted` handler must re-evaluate the "unhandled events" condition as it walks the decision list rather than computing it once up front — any `*Failed` event appended by a preceding decision of the same list counts as an event the decider has not handled, so each of the four closing decisions must take its `UNHANDLED_DECISION` branch and the run must stay open with a decision task scheduled.

Traces: services/service-com-amazonaws-swf-simpleworkflowservice/src/ops/respond_decision_task_completed.rs — `unseen_events` computed at line 426 from `execution.needs_decision_since(started_event_id)` and then passed unchanged into every `apply_decision` call (line 475); the branches that consume it are lines 750 (CompleteWorkflowExecution), 776 (FailWorkflowExecution), 803 (CancelWorkflowExecution) and 1525 (`continue_as_new`). `CancelTimerFailed` is appended at line 1138. No open issue covers this; the three open swf/lambda__swf issues are about ScheduleLambdaFunction and PollForDecisionTask on an unknown domain.

## Evidence
Check swf.cancel-timer-failed-then-cancel-workflow-execution:
  assert len(close_failed) == 1
  AssertionError: expected one CancelWorkflowExecutionFailed, got []; all events were [(1,'WorkflowExecutionStarted'),(2,'DecisionTaskScheduled'),(3,'DecisionTaskStarted'),(4,'DecisionTaskCompleted'),(5,'CancelTimerFailed'),(6,'WorkflowExecutionCanceled')]
Check swf.cancel-timer-failed-then-complete-workflow-execution:
  AssertionError: expected one CompleteWorkflowExecutionFailed, got []; ... (6,'WorkflowExecutionCompleted')
Check swf.cancel-timer-failed-then-continue-as-new-workflow-execution:
  AssertionError: expected one ContinueAsNewWorkflowExecutionFailed, got []; ... (6,'WorkflowExecutionContinuedAsNew')
Check swf.cancel-timer-failed-then-fail-workflow-execution:
  AssertionError: expected one FailWorkflowExecutionFailed, got []; ... (6,'WorkflowExecutionFailed')

Expected (AWS): CancelTimerFailed{cause=TIMER_ID_UNKNOWN, timerId=<the decision's timerId>, decisionTaskCompletedEventId=<this call's DecisionTaskCompleted eventId>} followed by <Close>WorkflowExecutionFailed{cause=UNHANDLED_DECISION, decisionTaskCompletedEventId=<same eventId>}, CancelTimerFailed.eventId < <Close>WorkflowExecutionFailed.eventId, DescribeWorkflowExecution.executionInfo.executionStatus == 'OPEN' and openCounts.openDecisionTasks == 1, and the next decision task's new events begin with the two *Failed events.
Actual (emulator): only CancelTimerFailed, then the terminal close event; no *Failed close event; execution CLOSED.
HTTP status/body of RespondDecisionTaskCompleted itself: 200 / empty in both cases (no error code involved).

## Steps to Reproduce
1. RegisterDomain + RegisterWorkflowType (defaultTaskList, defaultTaskStartToCloseTimeout=60, defaultExecutionStartToCloseTimeout=600, defaultChildPolicy=TERMINATE).
2. StartWorkflowExecution on that task list; PollForDecisionTask to get the first decision task.
3. RespondDecisionTaskCompleted with TWO decisions in one call: [{decisionType:'CancelTimer', cancelTimerDecisionAttributes:{timerId:'never-started'}}, {decisionType:'CancelWorkflowExecution', cancelWorkflowExecutionDecisionAttributes:{details:'bad-timer'}}].
4. GetWorkflowExecutionHistory: AWS shows CancelTimerFailed(TIMER_ID_UNKNOWN) then CancelWorkflowExecutionFailed(UNHANDLED_DECISION); the emulator shows CancelTimerFailed then WorkflowExecutionCanceled.
5. DescribeWorkflowExecution: AWS reports executionStatus OPEN with openDecisionTasks 1; the emulator reports the run closed.
6. Repeat step 3 substituting CompleteWorkflowExecution, FailWorkflowExecution and ContinueAsNewWorkflowExecution for the closing decision — same divergence each time.
