Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus
Issue-Key: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus|_drain#37|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=wait)
Issue-Commit: f7078ec9d24b4839050651ec5b14c25693a01eca

# events__sqs__stepfunctions: a succeeded execution publishes no aws.states Execution Status Change event to the default bus, so an SQS-targeted rule never receives it (1 cause)

> _Note: this issue was identified against the code base at commit `f7078ec9d24b4839050651ec5b14c25693a01eca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: an execution that reaches a terminal status publishes nothing to the DEFAULT EventBridge bus, so a rule filtered on that state machine's ARN and status never delivers to its SQS target and the check exhausts its budget. Failing check: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus.

AWS: Step Functions publishes execution status transitions to the account's DEFAULT event bus (documented at https://docs.aws.amazon.com/eventbridge/latest/ref/events-ref-states.html; delivery is best-effort but observable well inside a normal poll window). The envelope carries `source` = `aws.states`, `detail-type` = `Step Functions Execution Status Change`, `account` = the executing account, `region` = the execution's region, and the execution ARN inside `resources`. Its `detail` carries `executionArn`, `stateMachineArn`, `name`, `status` (one of the modelled ExecutionStatus values, SUCCEEDED for a machine whose only state is a Pass), `startDate` and `stopDate` as epoch MILLISECONDS with startDate <= stopDate, `input`, `output` (the Pass state's result) and `outputDetails.included` = true for an output small enough to inline. recorded behavior only for the exact detail body; the ARNs, names, account, region and dates are values taken from the request and the emulator's own state, never literals from one run.

Emulator: two rules are created on the default bus, identical except for the admitted status (SUCCEEDED and FAILED), each with an SQS target whose policy admits events.amazonaws.com. Fresh executions of a Pass-only state machine are started repeatedly; nothing is ever written to the SUCCEEDED queue and the check ends with `Error: Check timed out after 60.0s`. Because the positive control never lands, the calibrated negative in the same check (that the FAILED-filtered twin, on identical wiring, admits nothing) is also ungradable — an absence proves nothing when no event is ever published.

Why: any consumer wired to react to a workflow reaching a terminal status (fan-out to a queue, alarm, or downstream step) never fires, and a client cannot distinguish "the workflow did not run" from "the status event was never published". The status filter in the rule pattern is likewise never exercised, so a wrong-status delivery would go unnoticed.

Fix: the Step Functions execution status-transition path (the component that completes an execution and records its terminal status) must publish the Execution Status Change envelope to the default EventBridge bus, with the envelope and detail members above, so the default bus's rule-matching and SQS target delivery can evaluate it.

Related filing: open issue 20261107T042814Z-events-stepfunctions-an-execution-reaching-a-terminal-status.md describes the same missing publication from the events__stepfunctions check, and 20261029T182228Z-events-sfn-step-functions-publishes-no-execution-status-change-event.md is also open against it; neither was reported as owning THIS check's replay key, so the failure is filed here. A fixer should treat them as one defect and close them together.

Traces: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus.

## Evidence
Expected (AWS): after StartExecution on a single-Pass-state machine reaches SUCCEEDED, a default-bus rule whose pattern names {"source":["aws.states"],"detail-type":["Step Functions Execution Status Change"], detail.stateMachineArn = <the created machine>, detail.status = ["SUCCEEDED"]} delivers at least one message to its SQS target, whose body carries source `aws.states`, detail-type `Step Functions Execution Status Change`, the executing account and region, the execution ARN in `resources`, and detail {executionArn, stateMachineArn, name, status: SUCCEEDED, startDate/stopDate as epoch milliseconds in order, output = the Pass result, outputDetails.included = true}.
Actual (emulator): nothing is ever delivered to the queue; the check is cut off with `Error: Check timed out after 60.0s` while re-starting fresh executions to poll for the event. DescribeExecution-based assertions and the FAILED-twin absence window are never reached.

## Steps to Reproduce
1. CreateStateMachine with a single Pass state returning {"ok": true, "n": 7}, using a states-trusting role.
2. Create two SQS queues, each with a policy admitting events.amazonaws.com.
3. PutRule twice on the DEFAULT bus with patterns identical except detail.status: ["SUCCEEDED"] vs ["FAILED"], both scoped to the state machine ARN; PutTargets each rule at its own queue.
4. StartExecution and poll DescribeExecution until SUCCEEDED.
5. Poll the SUCCEEDED queue with ReceiveMessage. Nothing arrives (the FAILED queue is also empty, but that is unexercised, not a verdict).
6. Re-replay the check in trace_ids to confirm.
