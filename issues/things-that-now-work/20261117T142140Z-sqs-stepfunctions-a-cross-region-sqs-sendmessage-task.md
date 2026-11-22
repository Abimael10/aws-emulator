Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied
Issue-Key: sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied|run#16|AssertionError|assert bad["status"] == "FAILED", (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# sqs__stepfunctions: a cross-region sqs:sendMessage task succeeds although the state machine's role has no sqs:SendMessage grant on the far-region queue

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
One net-new root cause.

Cause 1 of 1: a Step Functions sqs:sendMessage task targeting a queue in another region is performed without authorising it against the state machine's role, so an execution whose role carries no grant on the far-region queue terminates SUCCEEDED and the message is delivered.
AWS: a state machine's service-integration tasks are performed with its roleArn's credentials and authorised against that role's identity policy, and the resource ARN authorised is the TARGET queue's ARN — including its region. A Task of Resource 'arn:aws:states:::sqs:sendMessage' whose Parameters.QueueUrl names a queue in a different region, with no sqs:SendMessage grant covering that queue's ARN, fails with the AccessDenied class ('User: <role session arn> is not authorized to perform: sqs:SendMessage on resource: <far queue arn> because no identity-based policy allows the sqs:SendMessage action') and, uncaught, the execution terminates FAILED with nothing enqueued on the far queue. The model carries no authorisation rules, so that text is recorded behavior: the sentence is fixed for the class, with the role session ARN, the action and the far queue's ARN taken from the execution's role and the task's target.
Emulator: the execution terminates SUCCEEDED — 'a role with no sqs:SendMessage grant on the far queue must FAIL the task; got SUCCEEDED'. The grant is never checked, in this region or any other.
Why: cross-region least privilege is unverifiable. A role granted sqs:SendMessage only on same-region queues appears to work against a far-region queue, so a policy that AWS would deny ships, and a negative test that asserts the cross-region boundary cannot be written. The message is also delivered to a queue AWS would have protected.
Fix: the Step Functions task-execution path for the SQS sendMessage integration — authorise sqs:SendMessage against the identity derived from the state machine's roleArn, using the TARGET queue's full ARN (region included) as the resource, and fail the task with the AccessDenied error class when no statement allows it.
Traces: sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied. The same missing authorisation of task integrations is filed for the lambda__sqs__stepfunctions and sns__sqs__stepfunctions headings in this run; this check additionally grades that the far-region queue's ARN is the resource authorised, which neither of those covers.

## Evidence
Check sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied (verdict: mismatch), group 'cross-region'.

Expected (AWS): the execution whose role carries no sqs:SendMessage on the other region's queue terminates FAILED, with the task's cause naming sqs:SendMessage and the far queue's ARN, and the far queue stays empty. The positive control (the same task with the grant in place) succeeds and the message arrives.

Actual (emulator): `AssertionError: a role with no sqs:SendMessage grant on the far queue must FAIL the task; got 'SUCCEEDED'` (check file line 155).

## Steps to Reproduce
Against the emulator (two regions, same account):
1. CreateQueue in region B (the 'far' queue).
2. Create an IAM role trusted by states.amazonaws.com whose policy grants sqs:SendMessage only on queues in region A (or nothing at all) — no statement covering the region-B queue ARN.
3. In region A, CreateStateMachine with {"StartAt":"S","States":{"S":{"Type":"Task","Resource":"arn:aws:states:::sqs:sendMessage","Parameters":{"QueueUrl":"<region-B queue url>","MessageBody":"x"},"End":true}}} and that roleArn.
4. StartExecution and poll DescribeExecution: the emulator reports SUCCEEDED. AWS reports FAILED with an AccessDenied cause naming the region-B queue ARN.
5. ReceiveMessage on the region-B queue: the emulator delivered the message; AWS delivered nothing.
6. Add a statement allowing sqs:SendMessage on the region-B queue ARN and re-run: the execution must SUCCEED and the message must arrive.
