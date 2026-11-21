Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: scheduler__sqs.scheduler-unauthorized-target-role-routes-invocation-to-the-dead-letter-queue
Issue-Key: scheduler__sqs.scheduler-unauthorized-target-role-routes-invocation-to-the-dead-letter-queue|run#23|AssertionError|assert payload["QueueUrl"] == target_url, (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# scheduler__sqs: the dead-letter record EventBridge Scheduler writes for a denied SQS target names a synthesized real-AWS-host QueueUrl instead of the target queue's URL

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSchedule

## Description
Cause 1 of 1: when a schedule's invocation of an SQS target is refused and Scheduler dead-letters it, the record it writes carries the SQS request it attempted with QueueUrl rendered as https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>, not the queue URL SQS serves for the Target.Arn the schedule names. Everything else in the record (the original MessageBody/Input) is correct, so only the ARN-to-URL translation diverges.

AWS: Smithy (sqs): the QueueUrl SendMessage requires is the URL of that queue as CreateQueue/GetQueueUrl return it; Scheduler resolves a Target.Arn of the form arn:aws:sqs:<region>:<account>:<name> to that queue's URL and the dead-letter record reports the request it attempted. The host is environment-specific and is not the requirement; the invariant is that the QueueUrl in the dead-letter record is the same string SQS serves for the target queue. Recorded behavior only for the concrete host (the record's ERROR_CODE=AccessDenied, TARGET_ARN and SCHEDULE_ARN fields are not in dispute here).

Emulator: the dead-letter event 'names the wrong target queue': QueueUrl 'https://sqs.us-west-2.amazonaws.com/<account>/<queue>' where the target queue is served at the emulator's own listener.

Why: a dead-letter record exists so an operator or a redrive tool can replay the failed invocation. Replaying the recorded request addresses a host nothing answers on, and correlating the record with the queue it was meant for (by URL) fails.

Fix: EventBridge Scheduler's target-invocation/dead-letter composition for SQS targets (resolve the target ARN to the QueueUrl the SQS service serves instead of formatting one from a fixed real-AWS pattern).

Traces: scheduler__sqs.scheduler-unauthorized-target-role-routes-invocation-to-the-dead-letter-queue.

## Evidence
scheduler-unauthorized-target-role-routes-invocation-to-the-dead-letter-queue [mismatch]:

    AssertionError: the dead-letter event names the wrong target queue: 'https://sqs.us-west-2.amazonaws.com/000000000000/<target-queue>', expected 'https://127.0.0.1:36181/000000000000/<target-queue>'

The permitted control schedule delivered and the dead-letter record's MessageBody matched the schedule's Input, so the only diverging field is QueueUrl.
Expected (AWS): the record's QueueUrl is the URL SQS serves for Target.Arn.
Actual (emulator): the fixed https://sqs.<region>.amazonaws.com/<account>/<name> form. (Host, port, account id and queue name are per-run values.)

## Steps to Reproduce
1. Create a target queue and a DLQ with SQS; create a role that allows sqs:SendMessage on the DLQ but NOT on the target queue.
2. scheduler CreateSchedule with a one-shot at(...) expression, Target={Arn: <target queue ARN>, RoleArn: <that role>, Input: <json>, RetryPolicy:{MaximumRetryAttempts:0}, DeadLetterConfig:{Arn: <DLQ ARN>}}.
3. Receive the dead-letter message from the DLQ and parse its body.
4. Compare body['QueueUrl'] with sqs GetQueueUrl for the target queue name. Expected equal; observed the record carries https://sqs.<region>.amazonaws.com/<account>/<name>.
