Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|_poll_until#39|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# batch__ecs: a Batch job submitted to a Fargate queue never reaches a terminal state, so propagateTags never creates an ECS task carrying the merged job-definition and job tags (1 cause)

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new root cause, grounded in the failing behavioral check the suite listed under `### batch__ecs` and marked neither ALREADY FILED nor OWNED.

=== Cause 1 of 1: a placed job never settles, so the ECS task Batch must create for it — and the tags propagated onto that task — never exist ===
AWS: Smithy: SubmitJob.propagateTags — "Specifies whether to propagate the tags from the job or job definition to the corresponding Amazon ECS task. If no value is specified, the tags aren't propagated. Tags can only be propagated to the tasks during task creation. For tags with the same name, job tags are given priority over job definitions tags. If the total number of combined tags from the job and job definition is over 50, the job is moved to the FAILED state." JobStatus is the enum [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED], so a job placed on a VALID FARGATE compute environment runs as an ECS task and then reaches SUCCEEDED (or FAILED); the task is visible through ECS (DescribeTasks/ListTagsForResource) carrying the union of job-definition and job tags with the job's value winning on a key collision.
Emulator: the job never reaches SUCCEEDED or FAILED. Polling DescribeJobs for a terminal status consumed the entire run budget and the check aborted with `TimeoutError: job <jobId> reached SUCCEEDED or FAILED: the run budget ended first; not graded`, so no corresponding ECS task is ever produced and the propagated tag set cannot be read back from ECS at all. Hypothesis: nothing in the Batch placement path creates a backing ECS task, so neither the task nor its tags nor the job's terminal status is ever produced.
Why: a client that tags Batch jobs for cost allocation or for ECS-side identification gets no ECS task to tag and no completion signal; tag-based cost reports and any ECS-side automation keyed on Batch-propagated tags see nothing, and the documented job-tag-wins precedence rule is unobservable.
Fix: the Batch job placement/attempt lifecycle behind SubmitJob — a placed Fargate job must create a backing ECS task (registered in the ECS state the ECS APIs read), apply the merged job-definition + job tags to it with the job's tags taking precedence on duplicate keys when propagateTags is true, and drive the job to a terminal status.
Traces: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins.

## Evidence
Suite: 217/295 passed; service `batch__ecs` 0/1 (0%), 1 failed — the only check for this service token, marked neither ALREADY FILED nor OWNED.

Expected (AWS): SubmitJob with propagateTags=true on a job definition carrying tags and a job carrying tags that overlap on one key produces an ECS task whose tags are the union, with the job's value for the overlapping key, and the job then reaches SUCCEEDED.
Actual (error verdict): the job never reached a terminal status within its poll budget —
  File "check_batch__ecs_batch_propagate_tags_copies_definition_and_job_tags_to_ecs_task_job_tag_wins.py", line 241, in _wait_job_terminal
  TimeoutError: job 23cfc04b-df0a-48ac-87d0-6ec0634ad2be reached SUCCEEDED or FAILED: the run budget ended first; not graded
The job id above and the check's 900s nominal job budget are per-run values, not requirements; the requirement is that a placed job settles and that its ECS task exists with the propagated tags.

## Steps to Reproduce
Preconditions: an IAM Fargate task execution role (ecs-tasks.amazonaws.com trust + AmazonECSTaskExecutionRolePolicy), a MANAGED/FARGATE compute environment polled to VALID, an ENABLED job queue polled to VALID.

1. RegisterJobDefinition (type=container, FARGATE platform capability) with tags {"owner":"jobdef","shared":"from-jobdef"} and a short-lived command.
2. SubmitJob to the queue with propagateTags=true and tags {"shared":"from-job","jobonly":"yes"}.
3. Poll DescribeJobs for a terminal status: the job never reaches SUCCEEDED or FAILED.
4. List ECS tasks on the compute environment's backing cluster and read the task's tags: no task corresponding to the job exists, so the union of tags (with "shared" = "from-job") cannot be observed.
