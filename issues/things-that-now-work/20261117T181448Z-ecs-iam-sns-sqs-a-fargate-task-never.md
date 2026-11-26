Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted
Issue-Key: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted|_await_stopped#23|Timeout|assert publisher.get("exitCode") == 0, (
Issue-Commit: c19026fddc546a37c1d4d3954f77370ec86334b1

# ecs__iam__sns__sqs: a Fargate task never settles to STOPPED with its container's output, so the sns:Publish denial and its repair are never observable

> _Note: this issue was identified against the code base at commit `c19026fddc546a37c1d4d3954f77370ec86334b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a Fargate task launched by RunTask never reaches an outcome the caller can read, so the whole task-role permission-chain scenario (a role without sns:Publish being denied, then the same role publishing after the one missing statement is granted) never produces an observable step. Failing check: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted.

NOTE: the open issue 20261117T135333Z-ecs-iam-sns-sqs-a-fargate-task-whose-role-lacks-sns-publish.md describes the same scenario, but the replay did NOT report this check as OWNED by it or as already filed, so it is filed here as a surviving failure and that issue is named for the fixer to deduplicate against.

AWS: recorded behavior only for the task runtime. In AWS, RunTask on FARGATE places the task, the container's entryPoint runs, and DescribeTasks (Smithy: ECS DescribeTasks, readonly, returns Tasks/Failures) eventually reports the task with lastStatus 'STOPPED' and the essential container carrying exitCode 0 for a script that exits 0; the awslogs log driver creates the stream <prefix>/<container>/<task-id> in the configured log group and CloudWatch Logs GetLogEvents returns the lines the container printed. For the denied run the recording shows the in-container AWS CLI refusal of SNS Publish with error code 'AuthorizationError' and the message whose fixed text is "User: <caller-arn> is not authorized to perform: SNS:Publish on resource: <topic-arn> because no identity-based policy allows the SNS:Publish action" — the fixed text is everything outside the three substitutions, and the variable parts are the assumed-role caller ARN (arn:aws:sts::<account>:assumed-role/<role-name>/<task-id>, taken from the task's role and task id), and the topic ARN from the request. For the granted run the SQS subscriber receives the SNS envelope whose Message is the published body.

Emulator: the replay reports 'Error: Check timed out after 60.0s'. Nothing in the chain settles: the task never reaches lastStatus STOPPED with a container exitCode, no awslogs stream content with the container's OUTCOME= line appears, and no message reaches the subscribed SQS queue — so neither the AuthorizationError refusal nor the post-grant delivery is ever produced. The check is killed at the harness budget while polling, long before its own 420s STOPPED deadline, i.e. the task makes no progress at all.

Why: a client that launches a one-shot Fargate task and reads DescribeTasks plus the task's awslogs stream to find out what the task did gets nothing — it cannot tell a permission failure from a task that never ran, and the diagnose-then-repair loop for an IAM task-role gap (grant the missing sns:Publish statement and re-run) can never be validated. Anything built on 'run a task, read its exit code and its logs' hangs instead of concluding.

Fix: the ECS task lifecycle/runtime state that must advance a RunTask-created Fargate task through PROVISIONING/PENDING/RUNNING to STOPPED and record the essential container's exitCode for DescribeTasks, together with the awslogs log-driver delivery that must create the task's log stream and append the container's stdout to CloudWatch Logs; and, once the container's calls are actually made, the SNS Publish authorization path that must refuse an un-granted task role with AuthorizationError and allow it after the grant.

Traces: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted.

## Evidence
Expected (AWS): DescribeTasks reports the task lastStatus 'STOPPED' with the essential container exitCode 0; GetLogEvents on <stream-prefix>/Publisher/<task-id> returns the container's 'OUTCOME=DENIED ... DETAIL=An error occurred (AuthorizationError) when calling the Publish operation: User: arn:aws:sts::<account>:assumed-role/<denied-role>/<task-id> is not authorized to perform: SNS:Publish on resource: <topic-arn> because no identity-based policy allows the SNS:Publish action' for the un-granted role, and 'OUTCOME=OK' plus an SNS envelope in the subscribed queue for the granted role and for the same role after the grant.

Actual (emulator): 'Error: Check timed out after 60.0s'. No STOPPED task, no container exitCode, no log-stream content and no queue message were observable at any point, so no error code or message was produced to compare.

## Steps to Reproduce
1. Create an SQS queue, an SNS topic with a queue policy allowing sns.amazonaws.com, and subscribe the queue to the topic (protocol 'sqs').
2. Create three IAM roles trusting ecs-tasks.amazonaws.com: an execution role with AmazonECSTaskExecutionRolePolicy, one task role with an inline Allow of sns:Publish on the topic, and one task role with no inline policy.
3. Create a CloudWatch Logs log group and an ECS cluster; RegisterTaskDefinition twice (FARGATE, awsvpc, cpu 256 / memory 512) with a container whose command runs `aws sns publish --topic-arn $TOPIC_ARN --message $BODY` and prints OUTCOME=OK or OUTCOME=DENIED, using the awslogs driver into that log group.
4. RunTask both definitions with launchType FARGATE and an awsvpc network configuration.
5. Poll DescribeTasks for lastStatus STOPPED and the container's exitCode, then GetLogEvents on <prefix>/Publisher/<task-id>.
Observed: nothing advances — the check is killed at its 60s budget with no STOPPED task, no log events and no SQS message. Expected: a STOPPED task per run, the denied run's log line carrying the AuthorizationError refusal, the granted run's message in the subscriber queue, and delivery again after put_role_policy grants sns:Publish to the previously denied role.
