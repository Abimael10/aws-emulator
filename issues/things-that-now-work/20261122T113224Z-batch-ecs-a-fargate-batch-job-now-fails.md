Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|run#39|AssertionError|assert final["status"] == "SUCCEEDED", (
Issue-Commit: ce4ee9e56b2f664f3ccf49966acace7716de13e8

# batch__ecs: a Fargate Batch job now FAILS at container start — the default /aws/batch/job awslogs group is created under the job's task execution role, which AmazonECSTaskExecutionRolePolicy does not permit

> _Note: this issue was identified against the code base at commit `ce4ee9e56b2f664f3ccf49966acace7716de13e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
REOPENS 20261119T120645Z-batch-ecs-a-batch-job-submitted-to-a.md: its fix left the call refused. That fixed issue covered this same check (a Fargate Batch job never reaching a terminal state, so propagateTags never produced an ECS task carrying the merged tags). The job now does settle — but it settles FAILED on a refused logs:CreateLogGroup, so the scenario is still unobservable; this is the remainder of that work item, not a newly discovered behaviour. The open issue 20261121T160051Z-batch-ecs-a-fargate-batch-job-still-never.md describes the older never-settles symptom, which is no longer what happens, and is keyed on a TimeoutError this run no longer produces. Related open issues that do NOT cover this cause: 20261120T110741Z-batch-2-net-new-causes-a-job-on.md, 20261120T150929Z-batch-s3-a-submitted-batch-job-never-reaches.md.

Cause 1 of 1: a job placed on a VALID MANAGED/FARGATE compute environment is failed during container initialization because the emulator tries to CREATE the default Batch awslogs log group using the job's ECS task execution role, and that role — carrying only AmazonECSTaskExecutionRolePolicy, exactly as the scenario and AWS's own documentation prescribe — is not authorized for logs:CreateLogGroup.
AWS: Smithy: SubmitJob.propagateTags — "Specifies whether to propagate the tags from the job or job definition to the corresponding Amazon ECS task. If no value is specified, the tags aren't propagated. Tags can only be propagated to the tasks during task creation. For tags with the same name, job tags are given priority over job definitions tags."; JobStatus enum is [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]. The logging side is recorded behavior only: a container job whose containerProperties declare no logConfiguration uses Batch's default awslogs configuration writing to the log group /aws/batch/job, a group the AWS Batch service itself owns and provisions; the task execution role needs only logs:CreateLogStream and logs:PutLogEvents — the actions the AWS managed policy AmazonECSTaskExecutionRolePolicy grants. That managed policy does NOT include logs:CreateLogGroup, so on AWS no log-group creation is attempted under the caller's role, the container starts, and the job reaches SUCCEEDED with a backing ECS task carrying the union of job-definition and job tags (the job's value winning on a shared key).
Emulator: DescribeJobs reports status 'FAILED' with statusReason 'ResourceInitializationError: failed to validate logger args: create stream has been retried 1 times: failed to create Cloudwatch log group: AccessDeniedException: User: arn:aws:sts::<account>:assumed-role/<execution-role-name>/<task-session> is not authorized to perform: logs:CreateLogGroup on resource: arn:aws:logs:<region>:<account>:log-group:/aws/batch/job because no identity-based policy allows the logs:CreateLogGroup action: exceeded maximum number of attempts, 1'. In that message the fixed text is the ResourceInitializationError/failed-to-validate-logger-args wrapper and the AccessDeniedException sentence template; the account id, region, the assumed-role ARN with its session name, and the log-group ARN are values taken from the request and the emulator's state. Because the attempt never runs, there is no ECS task whose tags can be read, so the documented job-tag-wins precedence remains unverifiable.
Why: every Batch job that uses the default (unspecified) log configuration fails on the emulator, no matter how correctly the customer followed AWS's own guidance for the execution role. A client gets FAILED where AWS gets SUCCEEDED, the failure text accuses the customer's role of missing a permission AWS never requires of it, and anything downstream of a completed job — propagated tags for cost allocation, exit codes, dependsOn chains, Step Functions .sync integrations — never happens.
Fix: the Batch job container-start / log-configuration path (and the awslogs setup it drives) — the default /aws/batch/job group must be provisioned by the service rather than created under the job's task execution role, and the execution role must only be required to hold logs:CreateLogStream + logs:PutLogEvents; then apply the merged job-definition + job tags to the backing ECS task when propagateTags is true and advance the job to SUCCEEDED.
Traces: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins.

## Evidence
Suite: `batch__ecs` 0/1 (0%), 1 failed — its only check.

Expected (AWS): SubmitJob(propagateTags=true) on a FARGATE job definition tagged {jdonly, shared:from-jobdef} with job tags {jobonly, shared:from-job}, executionRoleArn = a role trusting ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy attached, reaches status SUCCEEDED, and the backing ECS task carries jdonly + jobonly + shared=from-job.
Actual (mismatch verdict):
  File "check_batch__ecs_batch_propagate_tags_copies_definition_and_job_tags_to_ecs_task_job_tag_wins.py", line 600, in run
    assert final["status"] == "SUCCEEDED"
  AssertionError: the propagating leg's job must SUCCEED, got 'FAILED' (statusReason='ResourceInitializationError: failed to validate logger args: create stream has been retried 1 times: failed to create Cloudwatch log group: AccessDeniedException: User: arn:aws:sts::000000000000:assumed-role/sim2real-behav-batch-exec-bf3e40cd9700/00000000000000000000000000000001 is not authorized to perform: logs:CreateLogGroup on resource: arn:aws:logs:us-west-2:000000000000:log-group:/aws/batch/job because no identity-based policy allows the logs:CreateLogGroup action: exceeded maximum number of attempts, 1')

The account id 000000000000, the region, the generated role name and the session id are per-run values; the requirement is that no logs:CreateLogGroup is demanded of the execution role for the default /aws/batch/job group and that the job reaches SUCCEEDED.

## Steps to Reproduce
1. CreateRole with a trust policy for ecs-tasks.amazonaws.com; AttachRolePolicy arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy (and nothing else).
2. CreateComputeEnvironment (MANAGED, FARGATE), poll to VALID; CreateJobQueue over it, poll to VALID/ENABLED.
3. RegisterJobDefinition (type=container, platformCapabilities=[FARGATE]) with containerProperties.executionRoleArn = that role, image public.ecr.aws/docker/library/busybox:1.36, command ["/bin/sh","-c","sleep 150"], resourceRequirements VCPU 0.25 / MEMORY 512, networkConfiguration.assignPublicIp=ENABLED, propagateTags=true and tags {"sim2realTagFromDefinition":"…","sim2realTagShared":"…"} — and NO containerProperties.logConfiguration.
4. SubmitJob to that queue with tags {"sim2realTagFromJob":"…","sim2realTagShared":"…"}.
5. Poll DescribeJobs: AWS reports SUCCEEDED; the emulator reports FAILED with the ResourceInitializationError / logs:CreateLogGroup AccessDeniedException statusReason above.
6. Re-replay check batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins to confirm.
