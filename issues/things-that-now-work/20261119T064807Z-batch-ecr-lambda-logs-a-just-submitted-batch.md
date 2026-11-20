Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-submitted-job-has-no-execution-record
Issue-Key: batch__ecr__lambda__logs.refarch-batch-submitted-job-has-no-execution-record|run#20|AssertionError|assert absent not in job, (
Issue-Commit: 744a403bcd89e11c6cc0112b0c3d4fd5ce52e365

# batch__ecr__lambda__logs: a just-submitted Batch job already carries an execution record — DescribeJobs returns startedAt=0 on a job that has only been queued

> _Note: this issue was identified against the code base at commit `744a403bcd89e11c6cc0112b0c3d4fd5ce52e365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobs

## Description
One net-new root cause, grounded in the single failing check the suite lists under `### batch__ecr__lambda__logs` that is marked neither ALREADY FILED nor OWNED. (The other failures under that heading are OWNED by 20261105T082918Z-batch-ecr-lambda-logs-lambda-invoke-of-a.md, 20261116T201949Z-batch-ecr-lambda-logs-the-in-lambda-aws.md and 20261116T073408Z-batch-ecr-lambda-logs-batch-describecomputeenvironments-omits-servicerole-for-a-managed-compute.md, none of which describes this.) The same defect is reported for the `batch` service token in the companion issue filed in this sweep (DescribeJobs synthesising startedAt=0); it is filed separately here because this check is attributed to a different service heading and takes its own gate verdict.

Cause 1 of 1: DescribeJobs invents an execution record for a job that has only been accepted into the queue.
AWS: Smithy (com.amazonaws.batch) `JobDetail.startedAt` -> Long, "The Unix timestamp (in milliseconds) for when the job was started. More specifically, it's when the job transitioned from the STARTING state to the RUNNING state"; `JobDetail.stoppedAt` -> "when the job transitioned from the RUNNING state to a terminal state"; `JobDetail.createdAt` -> "when the job was created ... specifically at the time SubmitJob was called"; `JobDetail.attempts` -> "A list of job attempts that are associated with this job." Both timestamp members are `clientOptional`, i.e. legally absent. Recorded behaviour: DescribeJobs issued immediately after SubmitJob returns the queued job with `attempts: []`, `createdAt` set, and NO `startedAt` and NO `stoppedAt` key — acceptance into the queue is the only event that has happened, so it is the only timestamp present.
Emulator: the JobDetail for the freshly submitted job carries `startedAt` with the value `0`. The check fails with `the accepted job carries 'startedAt'=0; neither exists until a container is placed, and synthesising one answers the operator's question wrongly` (it asserts over both 'startedAt' and 'stoppedAt').
Why: an operator diagnosing "did my submission actually run?" reads a 200 with a jobId plus a start timestamp as "the job started", when nothing has been placed. Presence of `startedAt` is the canonical has-this-executed signal; consumers that read the value get the epoch (1970-01-01T00:00:00Z) as a start time, so queue wait (`startedAt - createdAt`) is hugely negative and runtime (`stoppedAt - startedAt`) is ~55 years.
Fix: the DescribeJobs serialiser/projection for JobDetail in the Batch service — `startedAt` and `stoppedAt` must be optional and omitted while the job state machine has recorded no STARTING->RUNNING (resp. RUNNING->terminal) transition, instead of being defaulted to 0.
Traces: batch__ecr__lambda__logs.refarch-batch-submitted-job-has-no-execution-record.

## Evidence
Suite: service `batch__ecr__lambda__logs` 11/15 (73%), 4 failed; three are OWNED, this is the surviving one.

  [mismatch] batch__ecr__lambda__logs.refarch-batch-submitted-job-has-no-execution-record
    AssertionError: the accepted job carries 'startedAt'=0; neither exists until a container is placed, and synthesising one answers the operator's question wrongly
    Request: SubmitJob(jobName=..., jobQueue=<VALID Fargate queue arn>, jobDefinition=<FARGATE container job definition arn>) immediately followed by DescribeJobs(jobs=[jobId]).
    Expected (recorded AWS): the single JobDetail has attempts == [], 'createdAt' present, and neither 'startedAt' nor 'stoppedAt' present.
    Actual: 'startedAt' present with value 0.

## Steps to Reproduce
1. Create a Fargate compute environment and poll it to VALID; create an ENABLED job queue over it and poll it to VALID.
2. RegisterJobDefinition(type='container', platformCapabilities=['FARGATE'], containerProperties={image: <ecr repositoryUri>, executionRoleArn: <task execution role>, resourceRequirements: [VCPU 0.25, MEMORY 512], networkConfiguration: {assignPublicIp: 'ENABLED'}}).
3. SubmitJob against that queue and job definition.
4. Immediately call DescribeJobs(jobs=[<jobId>]).
5. Observe the returned JobDetail contains 'startedAt': 0. AWS returns the queued job with attempts=[] and createdAt only — no 'startedAt' and no 'stoppedAt' key until a container is actually placed.
