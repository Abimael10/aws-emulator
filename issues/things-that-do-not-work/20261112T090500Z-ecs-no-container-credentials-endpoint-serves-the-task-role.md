Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.task-role-credentials-endpoint-serves-task-role ecs.run-task-task-role-override-replaces-task-definition-role
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.task-role-credentials-endpoint-serves-task-role|run#38|AssertionError|assert exit_code == 0, (
Issue-Key: ecs.run-task-task-role-override-replaces-task-definition-role|_assert_exit#22|AssertionError|assert got == expected, (

# ECS task data plane: no container-credentials endpoint serves the task role to the container's own process

> _Filed while fixing `20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md` (Cause 5 of 7). That issue's other causes are resolved; this one is NOT, and it is tracked here on its own rather than closed with them._

## Operation
RunTask

## Description
AWS: a task whose definition (or whose `RunTask` `overrides`) names a `taskRoleArn` gets
`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` set in every container's environment, and `169.254.170.2`
serves temporary credentials for that role at that path, so an SDK or a `curl` inside the container
acts as the task role. `TaskOverride$taskRoleArn` replaces `TaskDefinition$taskRoleArn` for that one
task, so the role the endpoint serves follows the override. (The endpoint itself is recorded
behaviour, not modelled: the model carries only the two `taskRoleArn` members.)

Emulator: the task's task-role SESSION is already vended before any container starts
(`src/state/task_role_credentials.rs`) and is already what the AWS CLI inside the container signs
with (`src/state/container_aws_cli.rs`) — but it is deliberately never handed to the container's
own process: that module states "The credential never reaches the container's program ... and in no
response, log, environment variable or file of the task". So `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`
is unset, no address inside the container answers, and a probe that reads its credentials the way
every real containerised AWS workload does observes nothing. Because the container cannot read the
task-definition role either, the override-vs-definition distinction is unobservable too.

## Why it matters
This is how a containerised workload authenticates. A client whose container calls S3, DynamoDB or
SSM under its task role gets an unauthenticated container, and an override of the task role — the
standard way to run one task definition under different permissions — has no observable effect, so
least-privilege regressions pass silently.

## What a fix needs
1. Inject `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` into every container's environment for a task
   that resolved a task role (`resolved_task_role_arn`), beside the `HOSTNAME` the runtime already
   injects (`src/state/container_runtime.rs`).
2. Serve that one path inside the container's session: the container's program reaches it with an
   HTTP client (`curl`/`wget`), which the session has none of, and the answer is the vended
   session's `AccessKeyId`/`SecretAccessKey`/`Token`/`Expiration` with the `RoleArn` of the
   EFFECTIVE role. Two constraints: the client must answer that one address in process and no other
   (the session reaches no network), and only a real minted role session may be served — the
   emulator's own fallback credential (`TaskRoleCredential::Emulator`) must never be, because a
   program that reads it writes it to its `awslogs` stream, where any client can read it.
3. Keep the session's expiry with the vend, so the `Expiration` the endpoint reports is the one the
   credential really has rather than a chosen duration.

## Steps to Reproduce
1. Create a role trusted by `ecs-tasks.amazonaws.com` through IAM's own API, and a second one.
2. `RegisterTaskDefinition` with `taskRoleArn` = the first role and a container whose command reads
   `$AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` and fetches `169.254.170.2$AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`.
3. `RunTask` it once as registered and once with `overrides.taskRoleArn` = the second role.
4. Poll `DescribeTasks` for `containers[0].exitCode` -> in both cases the variable is unset and
   nothing answers the address, instead of the first run reporting the task definition's role and
   the second the overridden one.
