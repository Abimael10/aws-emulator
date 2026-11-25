Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns__sqs__stepfunctions.sfn-sns-publish-missing-permission-catch-denied-negative
Issue-Key: sns__sqs__stepfunctions.sfn-sns-publish-missing-permission-catch-denied-negative|_run_until_publish_denied#27|Timeout|time.sleep(5 + attempt * 5)
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# sns__sqs__stepfunctions: a Step Functions sns:publish task is performed although the state machine's role carries no sns:Publish grant, so the Catch branch for the denial is never taken

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
One net-new root cause.

Cause 1 of 1: the SNS publish service integration is performed without authorising it against the state machine's role, so a role with no sns:Publish grant publishes successfully and the Catch branch written for the denial never runs.
AWS: a state machine's tasks are performed with its roleArn's credentials and authorised against that role's identity policy. A Task of Resource 'arn:aws:states:::sns:publish' whose role has no sns:Publish grant on the target topic fails with the AccessDenied error class — 'User: <role session arn> is not authorized to perform: SNS:Publish on resource: <topic arn> because no identity-based policy allows the SNS:Publish action' — which a Catch on States.TaskFailed (or the AccessDenied error name) handles, routing the execution into the catch branch; with no Catch the execution terminates FAILED and the topic publishes nothing. The model carries no authorisation rules, so that text is recorded behavior: the sentence is fixed for this class, with the role session ARN, the action and the topic ARN taken from the execution's role and the task's target.
Emulator: the check never reaches a verdict — 'Error: Check timed out after 60.0s'. Its first blocking wait is the denied execution reaching a terminal state with the catch branch taken, which never happens; this is the same defect the sibling heading lambda__sqs__stepfunctions shows definitively (two checks there report the denied execution terminating 'SUCCEEDED' instead of FAILED for the lambda:invoke and sqs:sendMessage integrations). Hypothesis: the task integration is performed unconditionally, so no denial is ever produced for the Catch to match.
Why: a client cannot test the least-privilege boundary of its state machine role, and a Catch/fallback path written for an authorisation failure is dead code against the emulator. A workflow that AWS would deny appears to work, so an under-granted (or over-granted) role ships unnoticed, and the SNS topic receives publishes it would never receive in AWS.
Fix: the Step Functions task-execution path for the SNS publish integration — resolve the state machine's roleArn into an identity, authorise sns:Publish on the target topic before publishing, and fail the task with the AccessDenied error class so Catch/Retry and the FAILED execution status are reachable.
Traces: sns__sqs__stepfunctions.sfn-sns-publish-missing-permission-catch-denied-negative.

## Evidence
Check sns__sqs__stepfunctions.sfn-sns-publish-missing-permission-catch-denied-negative (verdict: mismatch), group 'overly-permissive'.

Expected (AWS): an execution of a state machine whose role lacks sns:Publish on the target topic takes its Catch branch on the AccessDenied task failure (and, uncaught, terminates FAILED); the topic publishes nothing and the subscribed queue receives nothing.

Actual (emulator): `Error: Check timed out after 60.0s` — the denied terminal state the check waits for never arrives.

Corroborating, under the sibling heading lambda__sqs__stepfunctions in the same run: `AssertionError: expected the denied execution to be FAILED, got 'SUCCEEDED'` and `AssertionError: execution with a role lacking sqs:SendMessage must terminate FAILED; got 'SUCCEEDED'` — the same missing authorisation of task integrations against the state machine role, filed separately for that service heading.

## Steps to Reproduce
Against the emulator (any region/account):
1. CreateTopic; CreateQueue and subscribe it to the topic.
2. Create an IAM role trusted by states.amazonaws.com with NO sns:Publish grant.
3. CreateStateMachine with definition {"StartAt":"P","States":{"P":{"Type":"Task","Resource":"arn:aws:states:::sns:publish","Parameters":{"TopicArn":"<topic>","Message":"x"},"Catch":[{"ErrorEquals":["States.ALL"],"Next":"Denied"}],"End":true},"Denied":{"Type":"Pass","End":true}}} and that roleArn.
4. StartExecution and poll DescribeExecution/GetExecutionHistory: AWS records a TaskFailed whose cause is the SNS AccessDenied sentence and routes into 'Denied'; the emulator performs the publish, so no TaskFailed event is produced.
5. ReceiveMessage on the subscribed queue: AWS delivers nothing; the emulator delivers the publish.
