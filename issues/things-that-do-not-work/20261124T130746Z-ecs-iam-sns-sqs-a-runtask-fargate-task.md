Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted
Issue-Key: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted|_await_stopped#41|Timeout|time.sleep(5)
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# ecs__iam__sns__sqs: a RunTask Fargate task never settles to STOPPED with its container's exitCode and awslogs output, so the task-role sns:Publish denial and its repair are never observable

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a one-shot Fargate task launched by RunTask never reaches an outcome a caller can read — it never settles to lastStatus STOPPED with the essential container's exitCode, its awslogs stream never carries the container's stdout, and the container's own SNS Publish call therefore never happens. The whole task-role permission chain (a task role WITHOUT sns:Publish must be denied, the same role must publish after the one missing statement is granted) produces no observable step. Failing check: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted.

NOTE on prior art: the open issues 20261117T181448Z-ecs-iam-sns-sqs-a-fargate-task-never.md and 20261117T135333Z-ecs-iam-sns-sqs-a-fargate-task-whose-role-lacks-sns-publish.md describe this same scenario, but this replay did NOT report the check as ALREADY FILED or OWNED by either of them, so it is filed here as a surviving failure and those issues are named so a fixer can deduplicate rather than treat it as new.

AWS: Smithy (ECS) — RunTask returns tasks/failures; DescribeTasks is readonly and returns Tasks/Failures. Beyond that the task runtime is recorded behavior only: in AWS RunTask with launchType FARGATE and an awsvpc networkConfiguration places the task, the container's entryPoint actually runs, and DescribeTasks eventually reports lastStatus 'STOPPED' with the essential container carrying exitCode 0 for a script that exits 0; the awslogs log driver creates the stream <awslogs-stream-prefix>/<container-name>/<task-id> in the configured group and CloudWatch Logs GetLogEvents returns the lines the container printed. For the un-granted task role the in-container AWS CLI Publish is refused with HTTP 403 and error code 'AuthorizationError' (Smithy: SNS Publish error AuthorizationErrorException, awsQueryError code 'AuthorizationError', httpResponseCode 403) and the message whose FIXED text is "User: <caller-arn> is not authorized to perform: SNS:Publish on resource: <topic-arn> because no identity-based policy allows the SNS:Publish action"; the VARIABLE parts are the assumed-role caller ARN (arn:aws:sts::<account>:assumed-role/<task-role-name>/<task-id>, taken from the task's role and the task id) and the topic ARN from the request — no value from any single run is part of the template. For the granted role the subscribed SQS queue receives the SNS envelope whose Message is the published body, and after put_role_policy adds the single Allow sns:Publish statement to the previously denied role, that role's next task delivers too.

Emulator: the replay reports 'Error: Check timed out after 60.0s'. Nothing in the chain advances: no task reaches lastStatus STOPPED with a container exitCode, no awslogs stream content containing the container's 'OUTCOME=' line appears, and no message reaches the subscribed queue. Because the container never runs, no status, error code or error message is produced at all — the AuthorizationError refusal and the post-grant delivery are both absent rather than wrong. The check is killed at the harness budget while polling, far short of its own 420s STOPPED deadline, i.e. there is no progress to be slow about.

Why: a client that launches a one-shot Fargate task and reads DescribeTasks plus the task's log stream to learn what the task did gets nothing — it cannot distinguish a permission failure from a task that never ran, and the standard diagnose-then-repair loop for an IAM task-role gap (observe the denial, grant the missing statement, re-run and confirm delivery) can never conclude. Anything built on 'run a task, read its exit code and its logs' hangs.

Fix: the ECS task lifecycle/runtime state that must advance a RunTask-created FARGATE task through PROVISIONING/PENDING/RUNNING to STOPPED and record the essential container's exitCode for DescribeTasks; the awslogs log-driver delivery that must create the task's log stream and append the container's stdout to CloudWatch Logs; and, once the container's own calls are actually issued, the SNS Publish authorization path that must refuse an un-granted task role with AuthorizationError/403 and allow it after the grant.

Traces: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted.

## Evidence
Expected (AWS): DescribeTasks reports each launched task with lastStatus 'STOPPED' and the essential container's exitCode 0; GetLogEvents on <stream-prefix>/<container>/<task-id> returns the container's single OUTCOME= line — for the un-granted role 'OUTCOME=DENIED ... DETAIL=An error occurred (AuthorizationError) when calling the Publish operation: User: arn:aws:sts::<account>:assumed-role/<denied-role-name>/<task-id> is not authorized to perform: SNS:Publish on resource: <topic-arn> because no identity-based policy allows the SNS:Publish action' (HTTP 403, code AuthorizationError; the ARNs and task id are substituted from the request/state), and 'OUTCOME=OK' for the granted role plus an SNS envelope carrying the published body in the subscribed SQS queue; after granting sns:Publish to the previously denied role, its next revision's task also reports OUTCOME=OK and its body arrives in the queue.

Actual (emulator): 'Error: Check timed out after 60.0s'. No task ever reached STOPPED, no container exitCode was reported, no log-stream events existed and no message reached the queue, so neither the refusal (status/code/message) nor the successful delivery was ever produced for comparison.

## Steps to Reproduce
1. Create an SQS queue, tag it, and read its QueueArn; create an SNS topic, set a queue policy allowing sns.amazonaws.com sqs:SendMessage with ArnEquals aws:SourceArn = the topic, and subscribe the queue (protocol 'sqs').
2. Create three IAM roles trusting ecs-tasks.amazonaws.com: an execution role with AmazonECSTaskExecutionRolePolicy attached, one task role with an inline Allow of sns:Publish on the topic ARN, and one task role with no policy.
3. Create a CloudWatch Logs log group and an ECS cluster. RegisterTaskDefinition twice (requiresCompatibilities FARGATE, networkMode awsvpc, cpu '256', memory '512', the execution role and the respective task role) with a container whose entryPoint runs a shell script calling `aws sns publish --topic-arn $TOPIC_ARN --message $BODY` and printing 'OUTCOME=OK ...' or 'OUTCOME=DENIED ... DETAIL=<last CLI line>', using logConfiguration logDriver 'awslogs' into that group with a stream prefix.
4. RunTask each definition with launchType FARGATE, count 1 and an awsvpcConfiguration (one subnet, one security group, assignPublicIp ENABLED).
5. Poll DescribeTasks until lastStatus == 'STOPPED' and assert the container's exitCode is 0; then GetLogEvents on <prefix>/<container>/<task-id> and read the OUTCOME= line; then drain the subscribed queue for the published body.
6. put_role_policy the single Allow sns:Publish statement onto the previously denied role, register a new revision of its family, RunTask it and repeat step 5.
Observed: nothing advances — the check is cut off at the 60s replay budget with no STOPPED task, no container exitCode, no log events and no SQS message. Expected: a STOPPED task per run, the denied run's log line carrying the AuthorizationError/403 refusal with AWS's message text, the granted run's envelope in the subscriber queue, and delivery again after the grant.
