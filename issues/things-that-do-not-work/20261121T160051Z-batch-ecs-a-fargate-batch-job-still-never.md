Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|_poll_until#39|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# batch__ecs: a Fargate Batch job still never reaches a terminal status, so propagateTags never produces an ECS task carrying the merged job/job-definition tags

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new root cause, grounded in the only failing check the suite lists under `### batch__ecs` (marked neither ALREADY FILED nor OWNED).

IMPORTANT — this is the SAME symptom and the SAME replay key as the issue marked fixed, 20261119T120645Z-batch-ecs-a-batch-job-submitted-to-a.md (`Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|_poll_until#39|TimeoutError|...the run budget ended first; not graded`). That fix did not take effect: the job still never settles. Treat this as the regression of that work item, not a newly discovered behaviour. Related open issues that do NOT cover this check: 20261119T003525Z-batch-ecs-logs-a-fargate-batch-job-never.md, 20261120T110741Z-batch-2-net-new-causes-a-job-on.md, 20261120T150929Z-batch-s3-a-submitted-batch-job-never-reaches.md.

Cause 1 of 1: a job placed on a VALID MANAGED/FARGATE compute environment never reaches SUCCEEDED or FAILED, so no backing ECS task is ever created and the documented tag-propagation precedence is unobservable.
AWS: Smithy: SubmitJob.propagateTags — "Specifies whether to propagate the tags from the job or job definition to the corresponding Amazon ECS task. If no value is specified, the tags aren't propagated. Tags can only be propagated to the tasks during task creation. For tags with the same name, job tags are given priority over job definitions tags. If the total number of combined tags from the job and job definition is over 50, the job is moved to the FAILED state. When specified, this overrides the tag propagation setting in the job definition." Smithy JobStatus enum is [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED], and AttemptContainerDetail.taskArn is "The Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the job attempt" — so a placed Fargate job runs as an ECS task in the compute environment's backing cluster, that task is resolvable through the ECS APIs with the union of job-definition and job tags (the job's value winning on a duplicate key), and the job then settles in SUCCEEDED (or FAILED).
Emulator: DescribeJobs never reports a terminal status for the submitted job. The check's bounded poll for `SUCCEEDED or FAILED` consumes the whole run budget and aborts with `TimeoutError: job <jobId> reached SUCCEEDED or FAILED: the run budget ended first; not graded`. Because the attempt never completes, there is no ECS task to read tags from at all. Hypothesis: the Batch placement/attempt lifecycle behind SubmitJob does not drive a placed job past its pre-terminal status and does not register a backing ECS task.
Why: a client gets no completion signal for work it submitted — any poll-to-terminal workflow (CI gating, Step Functions `.sync` integration, retry/evaluateOnExit policies) hangs forever, and because no ECS task is created, tag-based cost allocation and ECS-side automation keyed on Batch-propagated tags see nothing. The documented "job tags are given priority over job definitions tags" rule cannot be observed.
Fix: the Batch job placement / attempt-lifecycle component behind SubmitJob — place a FARGATE job as a real ECS task registered in the ECS state the ECS APIs read, apply the merged job-definition + job tags to that task with the job's tags winning on duplicate keys when propagateTags is true, and advance the job through RUNNING to a terminal status.
Traces: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins.

## Evidence
Suite: service token `batch__ecs` 0/1 (0%), 1 failed — its only check.

Expected (AWS): SubmitJob with propagateTags=true against a job definition tagged {owner, shared} and a job tagged {shared, jobonly} yields an ECS task whose tag set is the union with the job's value for the overlapping key, and DescribeJobs then reports SUCCEEDED.
Actual (error verdict): the job never leaves a pre-terminal status —
  File "check_batch__ecs_batch_propagate_tags_copies_definition_and_job_tags_to_ecs_task_job_tag_wins.py", line 241, in _wait_job_terminal
  File "...", line 65, in _poll_until
  TimeoutError: job 6ef9b408-4996-4f49-b339-a7e1fe85105c reached SUCCEEDED or FAILED: the run budget ended first; not graded
The job id and the check's nominal 900 s job budget are per-run values and are not the requirement; the requirement is that a placed job settles and that the ECS task it ran as exists with the propagated tags.

## Steps to Reproduce
Preconditions: an IAM Fargate task execution role (ecs-tasks.amazonaws.com trust + AmazonECSTaskExecutionRolePolicy), a MANAGED/FARGATE compute environment polled to VALID, an ENABLED job queue polled to VALID.
1. RegisterJobDefinition (type=container, FARGATE platform capability) with tags {"owner":"jobdef","shared":"from-jobdef"} and a short-lived command.
2. SubmitJob to that queue with propagateTags=true and tags {"shared":"from-job","jobonly":"yes"}.
3. Poll DescribeJobs for a terminal status — it never reaches SUCCEEDED or FAILED.
4. List/describe ECS tasks on the compute environment's backing cluster: no task corresponds to the job, so the merged tag set (with shared=from-job) cannot be read.
5. Re-replay check batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins to confirm.
