Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus
Issue-Key: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus|_drain#37|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=wait)
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# events__sqs__stepfunctions: a succeeded Step Functions execution never produces an aws.states 'Step Functions Execution Status Change' event on the default bus, so a rule-matched SQS target never receives it

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause: Step Functions does not emit its execution status-change notification to the EventBridge default bus, so a rule written against it never fires and the check waits out its whole liveness budget.

AWS: when a Step Functions execution reaches a terminal status, Step Functions publishes an event to the account's DEFAULT EventBridge bus with source 'aws.states' and detail-type 'Step Functions Execution Status Change' (AWS documents this delivery as best-effort). The detail carries the execution's identity and outcome — executionArn, stateMachineArn, name, status (e.g. SUCCEEDED), input/output and startDate/stopDate, the dates expressed as epoch MILLISECONDS — and the event's `resources` list names the execution ARN. A rule on the default bus whose event pattern filters on source 'aws.states', that detail-type and that state machine's ARN plus the status the execution reached matches and delivers to its SQS target; an otherwise identical rule filtered on a status the execution never reached does not match.
Emulator: no such event is ever observed. The target queue stays empty for the whole liveness window and the check is killed by the harness ('Check timed out after 60.0s') while polling for the delivery, so neither the positive match, the negative (wrong-status) rule, nor the startDate/stopDate unit-and-ordering bracket the check establishes can be evaluated.
Why: execution status-change events are how callers build workflow observability and fan-out — alarms on failed executions, a queue of completed runs, a chained workflow triggered by a parent finishing. Against the emulator every such consumer is silently starved: nothing errors, the message simply never arrives, so an integration test hangs or times out rather than reporting a defect.
Fix: the execution terminal-transition path in the states crate — publish the status-change event to the default bus (source 'aws.states', detail-type 'Step Functions Execution Status Change', detail with executionArn/stateMachineArn/name/status/input/output and epoch-millisecond startDate/stopDate, resources naming the execution ARN) so the EventBridge rule-matching and target-delivery path can carry it.
Traces: events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus.

Adjacent OPEN issue that does NOT cover this check (it carries no replay key for it; the suite marks it as owning only the events__stepfunctions variant of the scenario): 20261107T042814Z-events-stepfunctions-an-execution-reaching-a-terminal-status.md. A similar open issue does not cover a failing check, so this one is filed here and that issue is named — the two are very likely the same underlying emission gap and can be closed together, but this check's replay key is only on this issue.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 761/955 passed; service heading `events__sqs__stepfunctions` 12/14. The check is marked neither ALREADY FILED nor OWNED.

events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus:
  Error: Check timed out after 60.0s

The check creates a role trusted by states.amazonaws.com, a STANDARD state machine whose only state is {"Passthrough": {"Type": "Pass", "Result": {"ok": true, "n": 7}, "End": true}}, two SQS queues, and two default-bus rules — one matching source 'aws.states', detail-type 'Step Functions Execution Status Change', that state machine's ARN and the status the execution reaches, one matching a status it never reaches — then starts executions and polls for the delivery. Because the status-change event is never published, the queue for the matching rule stays empty and the check is cut off by the harness budget while still polling (its own liveness budget is 150s; it never gets there).

Expected (AWS, per the Step Functions EventBridge reference the check cites): an event on the default bus with source 'aws.states', detail-type 'Step Functions Execution Status Change', `resources` naming the execution ARN, and a detail whose status is the execution's terminal status and whose startDate/stopDate are epoch milliseconds bracketing the run; the matching rule delivers it to its SQS target and the wrong-status rule delivers nothing.
Actual (emulator): no event, no delivery, within the whole polling window.

Fixed vs. variable parts: fixed — the source string 'aws.states' and the detail-type string 'Step Functions Execution Status Change'. Variable — the executionArn, stateMachineArn, execution name, status, input/output and the start/stop timestamps, all taken from the execution the emulator itself ran. No per-run id or wall-clock value is asserted as a requirement here; the only timing assertion is the millisecond UNIT and the start<=stop ordering.

## Steps to Reproduce
Boot the emulator from this checkout and replay events__sqs__stepfunctions.step-functions-execution-status-change-reaches-the-default-bus. Manually: create a role trusted by states.amazonaws.com; create a STANDARD state machine with a single Pass state; create an SQS queue and a default-bus rule with event pattern {"source":["aws.states"],"detail-type":["Step Functions Execution Status Change"],"detail":{"stateMachineArn":["<sm arn>"],"status":["SUCCEEDED"]}} targeting that queue (plus a second, identical rule filtered on "FAILED" targeting a second queue). StartExecution, poll DescribeExecution to SUCCEEDED, then ReceiveMessage on both queues: expect exactly one aws.states status-change event on the SUCCEEDED queue (resources naming the execution ARN; detail.startDate/stopDate in epoch milliseconds) and nothing on the FAILED queue. The emulator delivers nothing to either.
