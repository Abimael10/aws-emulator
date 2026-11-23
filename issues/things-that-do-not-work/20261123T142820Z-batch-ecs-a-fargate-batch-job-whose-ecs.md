Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|_poll_until#39|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# batch__ecs: a Fargate Batch job whose ECS task has already run and been inspected never reaches SUCCEEDED or FAILED — DescribeJobs serves a non-terminal status indefinitely

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobs

## Description
Cause 1 of 1: a Batch job submitted to an ENABLED MANAGED/FARGATE compute environment is dispatched far enough that it owns a real backing ECS task — the check resolves the task ARN from the job and reads the task's tags out of ECS — but DescribeJobs never advances that job's status to a terminal value. The job is still non-terminal when the runner's whole time budget is exhausted. Failing check: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins.

AWS: Smithy — batch JobStatus is an enum with values [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]; DescribeJobs returns JobDetail.status from that enum, together with JobDetail.attempts[].container.exitCode and JobDetail.statusReason. The model states the enum but not the progression, so the lifecycle here is recorded behavior only: SUCCEEDED and FAILED are the two terminal states, every submitted job that is placed eventually reaches one of them, and a job whose container exited 0 reports status SUCCEEDED with attempts[0].container.exitCode == 0. There is no AWS state in which a job that has a running-and-then-stopped ECS task stays non-terminal forever.

Emulator: the job progresses to the point of owning an ECS task (the check successfully read the task ARN from the job and then read that task's ECS tags, proving propagateTags was applied to a task that existed), and then DescribeJobs keeps returning a status that is neither SUCCEEDED nor FAILED for the rest of the run. The check polls DescribeJobs once a second with a 900 s allowance and the runner's budget ran out first, so the job was still non-terminal after every second of time the run had left. Hypothesis: nothing watches the backing ECS task's transition to STOPPED and writes the job's terminal status back, so the job's state machine stalls at STARTING/RUNNING.

Why: every client that submits work to Batch waits on a terminal job status — `aws batch wait jobs-complete`, Step Functions' Batch SubmitJob.sync integration, any CI step or orchestrator that blocks on DescribeJobs — and all of them hang indefinitely instead of completing. A client can also never read the job's exit code or statusReason, so success and failure are indistinguishable: a failing job looks exactly like a running one, and no retry, dependsOn gate or downstream step ever fires.

Fix: the Batch job state machine / job-status reconciliation in the Batch job data plane — the component that observes the backing ECS task's lifecycle must write the job's terminal status (SUCCEEDED when the container exited 0, FAILED otherwise, with statusReason and attempts[].container.exitCode populated). The tag-propagation path this check also exercises is already correct and must not be disturbed: leg 1 (propagated definition+job tags with the job's value winning the shared key) and leg 2 (the no-flag twin carrying none of them) both PASSED.

Traces: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins — TimeoutError raised by the check's own `_wait_job_terminal` helper at the final leg, after both tag-propagation legs passed.

Related but distinct open issues that do NOT carry this check's replay key: 20261122T113224Z-batch-ecs-a-fargate-batch-job-now-fails.md (a Fargate job FAILING at container start on the default awslogs group — a terminal FAILED, not a stall), 20261109T161717Z-batch-3-net-new-causes-tag-untag-204.md and 20261119T015949Z-batch-2-net-new-causes-timeout-attemptdurationseconds-is.md.

## Evidence
Expected (AWS): after SubmitJob on a FARGATE queue whose job definition runs a short container, DescribeJobs(jobs=[jobId]) eventually reports status 'SUCCEEDED' with attempts[0].container.exitCode == 0 (or 'FAILED' with a statusReason). Both are terminal; the poll ends.
Actual (emulator): DescribeJobs keeps returning a non-terminal status (never 'SUCCEEDED' and never 'FAILED') for a job that demonstrably already had a backing ECS task — the check had just resolved that task's ARN from the job and read its tags through ECS.
Error: TimeoutError: "job <jobId> reached SUCCEEDED or FAILED: the run budget ended first; not graded" — raised from the check's `_wait_job_terminal` (`lambda j: j is not None and j.get("status") in ("SUCCEEDED", "FAILED")`), polling at 1 s intervals with a 900 s allowance, after the runner's remaining time fell to <= 6 s. The jobId in the message is this run's generated id and is not part of the requirement.

Corroborating, from the same check, all PASSING before the stall: DescribeJobDefinitions reported propagateTags == True for the propagating definition and no propagateTags for the twin; the propagating job's ECS task carried the job-definition tags plus the job tags with the job's value winning the shared key; the twin's task carried none of the probe keys. So placement, task creation and tag propagation all work — only the terminal status is missing.

Model citation used: batch JobStatus enum = [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED].

## Steps to Reproduce
1. Create an IAM role trusted by ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy attached (the Fargate task execution role).
2. CreateComputeEnvironment(type='MANAGED', computeResources.type='FARGATE') and wait for it to be VALID; CreateJobQueue over it and wait for VALID.
3. RegisterJobDefinition(type='container', platformCapabilities=['FARGATE'], a short-running command, executionRoleArn=the role above, tags={...}, propagateTags=True).
4. SubmitJob(jobQueue=<queue>, jobDefinition=<def>, tags={...}).
5. Poll DescribeJobs(jobs=[jobId]) until status is 'SUCCEEDED' or 'FAILED'. AWS reaches 'SUCCEEDED' with attempts[0].container.exitCode == 0; the emulator serves a non-terminal status indefinitely, even though the job already has a backing ECS task (readable via the job's container.taskArn, whose ECS tags can be listed).
