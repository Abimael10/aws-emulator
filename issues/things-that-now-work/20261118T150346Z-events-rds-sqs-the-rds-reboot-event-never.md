Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__rds__sqs.xsvc-rds-1-near-miss-rules-not-delivered-after-matching-rule-receives
Issue-Key: events__rds__sqs.xsvc-rds-1-near-miss-rules-not-delivered-after-matching-rule-receives|_receive#23|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 8e5675a008353ca996c02d5e79f804eabbe67887

# events__rds__sqs: the RDS reboot event never reaches a matching EventBridge rule, so the near-miss pattern check reaches no verdict

> _Note: this issue was identified against the code base at commit `8e5675a008353ca996c02d5e79f804eabbe67887`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RebootDBInstance

## Description
1 net-new root cause.

Cause 1 of 1: the RDS→EventBridge→SQS leg this check is calibrated on never produces an effect: after RebootDBInstance on an available DB instance, no "RDS DB Instance Event" carrying EventID RDS-EVENT-0006 for that instance's ARN reaches the exactly-matching rule's queue, so the check stalls in its positive-control wait and is cut off with `Error: Check timed out after 60.0s` before any near-miss absence can be attributed to a pattern. Failing check: events__rds__sqs.xsvc-rds-1-near-miss-rules-not-delivered-after-matching-rule-receives.

AWS: recorded behavior only — neither the RDS nor the EventBridge model describes event emission. What the recording requires is: a reboot of a DB instance publishes an event to the default event bus with detail-type "RDS DB Instance Event" whose detail carries SourceArn = `arn:<partition>:rds:<region>:<account>:db:<db id>` (a value from the request/state), EventID = "RDS-EVENT-0006" (fixed text — the DB instance restarted event) and a Date at or after the moment the RebootDBInstance response was served; an ENABLED rule whose EventPattern is {"source": ["aws.rds"], "detail-type": ["RDS DB Instance Event"], "detail": {"EventID": ["RDS-EVENT-0006"], "SourceArn": [<that ARN>]}} delivers it to its SQS target, while a rule one field away (a different EventID, a non-existent SourceArn, or detail-type "RDS DB Snapshot Event") receives nothing and keeps both ApproximateNumberOfMessages and ApproximateNumberOfMessagesNotVisible at "0". Repairing a near-miss rule's pattern to the matching one and rebooting again must deliver to the SAME queue, which is what proves the earlier zero came from the pattern and not from the queue.

Emulator: nothing is delivered and the check never reaches a verdict. The whole replay detail is `Error: Check timed out after 60.0s`; the check's own budgets are far larger (DELIVERY_BUDGET_S = 600 for the post-reboot wait, 1200 s for the instance to become available), so the harness cap is hit while one of those polls is still spinning. Two sibling checks in this suite group pass, so the stall is specific to this flow: either the instance never settles available for the reboot, or — the reading the check's structure points at — the reboot produces no RDS event on the default bus at all, leaving `_await_post(sqs, good_url, cutoff, 600)` with nothing to match. Hypothesis (neither the model nor the recording states which): the RDS control plane emits no lifecycle events to EventBridge, so every RDS-sourced rule is dead regardless of pattern.

Why: a client that routes RDS lifecycle events through EventBridge gets an accepted rule and target that never fire, so an operational signal (an instance restarted) is silently lost; and because the matching rule produces nothing, a pattern that SHOULD be a near miss is indistinguishable from one that correctly matches — every RDS rule looks equally correct and equally dead.

Fix: the RDS control-plane event emission path — RebootDBInstance (and the instance state machine around it) must publish the "RDS DB Instance Event" envelope with its detail SourceArn/EventID/Date onto the default event bus so EventBridge rule matching and SQS target delivery can run; and, if the stall is upstream, the CreateDBInstance/DescribeDBInstances state progression that must reach `available` so a reboot can be issued at all.

Related open issue, named for the fixer: 20261115T181413Z-rds-3-more-control-plane-lifecycle-flows-never.md reports other RDS control-plane lifecycles that never converge and whose checks are killed at the 60 s budget; it is a different service heading and does not name this event-delivery flow or this replay key.

Traces: events__rds__sqs.xsvc-rds-1-near-miss-rules-not-delivered-after-matching-rule-receives.

## Evidence
Expected (AWS): with one exactly-matching rule and three near-miss rules (EventID "RDS-EVENT-0087"; SourceArn of a non-existent instance; detail-type "RDS DB Snapshot Event"), each targeting its own SQS queue, a RebootDBInstance on the available instance delivers exactly one event to the matching rule's queue whose detail has SourceArn == the instance ARN and EventID == "RDS-EVENT-0006" and whose Date is at/after the reboot response's server time; each near-miss queue receives nothing and reports {"ApproximateNumberOfMessages": "0", "ApproximateNumberOfMessagesNotVisible": "0"}. After PutRule rewrites each near-miss pattern to the matching one and the instance is rebooted again, each of those same queues receives the post-reboot RDS-EVENT-0006.

Actual (emulator): no post-reboot event is ever observed on the matching rule's queue; the check is cut off by the harness with:
  Error: Check timed out after 60.0s
while the positive-control wait (`_await_post(sqs, good_url, cutoff, DELIVERY_BUDGET_S=600)`) is still polling, so none of the near-miss absence assertions or the repair leg were evaluated. The DB identifier, account, region, rule names and queue URLs are per-run generated values and are not requirements; the requirements are the "RDS DB Instance Event" detail-type, the fixed EventID "RDS-EVENT-0006", the SourceArn taken from the rebooted instance, and the Date ordering against the reboot response.

## Steps to Reproduce
1. events.PutRule (ENABLED) four times with EventPattern {"source": ["aws.rds"], "detail-type": ["RDS DB Instance Event"], "detail": {"EventID": [...], "SourceArn": [...]}}: one exactly matching the DB ARN with EventID RDS-EVENT-0006, and three one field away (EventID RDS-EVENT-0087; a non-existent `<db>-absent` SourceArn; detail-type "RDS DB Snapshot Event"). PutTargets each to its own SQS queue whose policy admits events.amazonaws.com for that rule ARN.
2. CreateDBInstance (db.t4g.micro) and poll DescribeDBInstances until DBInstanceStatus == "available".
3. RebootDBInstance(DBInstanceIdentifier=<db id>) and note the response's server Date as the cutoff.
4. Poll ReceiveMessage on the matching rule's queue.
   -> EXPECT one event with detail.EventID == "RDS-EVENT-0006", detail.SourceArn == the instance ARN and detail.Date >= cutoff.
   -> OBSERVE nothing; the check is killed at the 60 s harness cap with the positive control unmet.
5. (Never reached) assert each near-miss queue is empty, then PutRule each to the matching pattern, reboot again, and assert each of those queues now receives the event.
6. Or replay the check id in trace_ids.
