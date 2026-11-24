Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted
Issue-Key: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted|_await_stopped#23|Timeout|time.sleep(5)
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# A Fargate task whose role lacks sns:Publish never settles: the denial/grant cycle never completes

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the ECS task that should be denied sns:Publish and then succeed once granted never produces an observable outcome.

AWS: recorded behavior only — the recording shows a Fargate task whose task role has no sns:Publish running, logging the AccessDenied refusal and stopping, and the same task succeeding (the SQS subscriber receives the message) once the grant is added.

Emulator: the replay reports `Check timed out after 60.0s`. Neither the denied run nor the granted run reaches a state the check can read — no stopped task, no log stream content, no delivered message.

Why: an operator cannot observe why a task failed (or that it ran at all), which is the entire diagnostic loop for a task-role permission problem.

Fix: the ECS task runtime/lifecycle path that must settle a task to STOPPED with its container's exit and logs (and the SNS publish authorization result it is supposed to surface). Hypothesis: the task never leaves PROVISIONING/PENDING, so nothing downstream happens.

Traces: ecs__iam__sns__sqs.fargate-task-role-without-sns-publish-is-denied-until-granted.

## Evidence
Expected: the task runs, logs an AccessDenied for sns:Publish and stops; after the grant is added the same task publishes and the SQS subscriber receives the message.
Actual: `Error: Check timed out after 60.0s` — no task outcome, log content or message was observable.

## Steps to Reproduce
1. Create a cluster, an SNS topic with an SQS subscriber, a task definition whose task role has no sns:Publish, and RunTask on Fargate.
2. Poll DescribeTasks for STOPPED and read the task's log stream.
Observed: nothing settles within the budget (the check is killed at 60s). Expected: a STOPPED task whose logs name the sns:Publish denial, then success after the grant.
