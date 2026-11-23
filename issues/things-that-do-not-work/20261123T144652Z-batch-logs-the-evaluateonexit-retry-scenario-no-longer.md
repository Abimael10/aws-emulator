Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__logs.batch-retry-attempt-number-visible-to-container-and-second-attempt-succeeds
Issue-Key: batch__logs.batch-retry-attempt-number-visible-to-container-and-second-attempt-succeeds|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Commit: 364d717fb0950a5d6ba16d8ba80d0b55cb268a4c

# batch__logs: the evaluateOnExit RETRY scenario no longer settles at all — a submitted Fargate container job reaches no terminal status inside the run budget, so the two-attempt retry and AWS_BATCH_JOB_ATTEMPT injection are ungradeable (1 cause)

> _Note: this issue was identified against the code base at commit `364d717fb0950a5d6ba16d8ba80d0b55cb268a4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Cause 1 of 1: a container job submitted with retryStrategy={attempts:2, evaluateOnExit:[{onExitCode:'42',action:'RETRY'},{onReason:'*',action:'EXIT'}]} never reaches a terminal status that DescribeJobs can report, so the check is killed at the harness budget before it can read the attempts array at all. The failure mode has changed: this check previously reached its assertion and reported a one-entry attempts array; it now produces no verdict.

AWS: Smithy: RetryStrategy.attempts — "The number of times to move a job to the RUNNABLE status. You can specify between 1 and 10 attempts. If the value of attempts is greater than one, the job is retried on failure the same number of attempts as the value." RetryStrategy.evaluateOnExit — "Array of up to 5 objects that specify the conditions where jobs are retried or failed." EvaluateOnExit.onExitCode — "Contains a glob pattern to match against the decimal representation of the ExitCode returned for a job"; EvaluateOnExit.action (RetryAction, required) — "Specifies the action to take if all of the specified conditions ... are met." JobStatus enumerates SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED, so a submitted job must progress to one of SUCCEEDED/FAILED. AttemptDetail.startedAt/stoppedAt are the STARTING->RUNNING and RUNNING->terminal transitions, and AttemptContainerDetail.exitCode is "The exit code for the job attempt. A non-zero exit code is considered failed", while AttemptContainerDetail.logStreamName is "The name of the CloudWatch Logs log stream that's associated with the container ... Each container attempt receives a log stream name when they reach the RUNNING status." Concretely, for a container that exits 42 on its first attempt: Batch matches onExitCode '42', re-queues the job to RUNNABLE, runs the container a second time with AWS_BATCH_JOB_ATTEMPT incremented to 2, the job ends SUCCEEDED, DescribeJobs reports exactly two AttemptDetail entries with container.exitCode 42 then 0, and each attempt's own awslogs stream holds exactly one 'ATTEMPT=<n>' line (n taken from the injected environment variable, not from any recorded run).

Emulator: the replay reports only 'Error: Check timed out after 60.0s' — no status, no attempts array, no log event. The scenario does not get as far as a DescribeJobs read reporting SUCCEEDED or FAILED: the submitted job is still non-terminal when the harness kills the check, so nothing about the retry decision, the per-attempt exit codes or the injected AWS_BATCH_JOB_ATTEMPT value can be compared.

Why: a client encoding its failure policy in retryStrategy/evaluateOnExit gets neither the retry nor a terminal verdict. Code that submits a job and polls DescribeJobs for a terminal status blocks indefinitely rather than seeing SUCCEEDED/FAILED, the attempts array it audits never materialises, and a container that relies on AWS_BATCH_JOB_ATTEMPT to make an attempt-dependent decision can never be observed making it.

Fix: the Batch job data-plane runner and its job state machine — placement of a RUNNABLE job onto its compute environment's backing cluster, actually running the container image, the status transition to a terminal state driven by the real container exit, and the retry-evaluation step that matches a finished attempt's exitCode/reason against evaluateOnExit, re-queues to RUNNABLE up to attempts, appends one AttemptDetail per attempt and increments AWS_BATCH_JOB_ATTEMPT in the container environment. The awslogs delivery path that writes each attempt's stdout into the named log group/stream is on the same path.

Related open issues that do NOT cover this check (the suite left it unowned, so none carries its current replay key): 20261119T003525Z-batch-logs-a-placed-job-s-container-never.md names this check under its Cause 3 but is keyed on a run#10 AssertionError on 'assert len(attempts) == 2' — the check no longer reaches that line; 20261119T120645Z-batch-logs-queued-jobs-are-never-dispatched-a.md, 20261107T153156Z-batch-logs-a-placed-batch-job-ends-failed.md and 20261122T113224Z-batch-ecs-a-fargate-batch-job-now-fails.md cover neighbouring symptoms of the same family but not this check.

Traces: batch__logs.batch-retry-attempt-number-visible-to-container-and-second-attempt-succeeds.

## Evidence
Verdict: [mismatch]. Detail, verbatim: 'Error: Check timed out after 60.0s'.

Expected (AWS): after SubmitJob of a job definition whose command is ['sh','-c','echo ATTEMPT=$AWS_BATCH_JOB_ATTEMPT; [ "$AWS_BATCH_JOB_ATTEMPT" = "1" ] && exit 42; exit 0'] with retryStrategy {attempts:2, evaluateOnExit:[{onExitCode:'42',action:'RETRY'},{onReason:'*',action:'EXIT'}]}, DescribeJobs settles on status 'SUCCEEDED'; job['attempts'] has length 2; the per-attempt container.exitCode values are [42, 0]; the two attempts name two distinct logStreamName values; and each of those streams holds exactly one line, 'ATTEMPT=1' and 'ATTEMPT=2' respectively. The two literal exit codes 42/0 and the ATTEMPT= values come from the request's own command and the attempt counter, not from any recorded run; the stream names, task ARNs and timestamps are per-run values and are not part of the requirement.

Actual (emulator): the job never reaches SUCCEEDED or FAILED within the harness budget — the check is killed at 60.0 s inside its terminal-status wait, so there is no status, no attempts array and no log event to compare. Note the regression in failure mode: the same check previously settled fast and failed on 'a RETRY on exit 42 with attempts=2 must produce exactly 2 attempts, got 1'.

## Steps to Reproduce
1. logs CreateLogGroup for a fresh group; iam CreateRole for a Fargate task execution role allowing ecr:GetAuthorizationToken, ecr:BatchCheckLayerAvailability, ecr:GetDownloadUrlForLayer, ecr:BatchGetImage, logs:CreateLogStream, logs:PutLogEvents.
2. batch CreateComputeEnvironment (MANAGED/FARGATE, maxvCpus=4) and CreateJobQueue bound to it; wait for both VALID.
3. batch RegisterJobDefinition (type container, Fargate) with executionRoleArn set, logConfiguration logDriver=awslogs pointed at that log group, image public.ecr.aws/docker/library/busybox:1.36, and command ['sh','-c','echo ATTEMPT=$AWS_BATCH_JOB_ATTEMPT; [ "$AWS_BATCH_JOB_ATTEMPT" = "1" ] && exit 42; exit 0'], plus retryStrategy={'attempts':2,'evaluateOnExit':[{'onExitCode':'42','action':'RETRY'},{'onReason':'*','action':'EXIT'}]}.
4. batch SubmitJob to that queue/definition.
5. Poll DescribeJobs for a terminal status. Observed: the job does not reach SUCCEEDED or FAILED and the run is killed at the 60 s cap.
6. When it does settle, assert status=='SUCCEEDED', len(attempts)==2, per-attempt container.exitCode==[42,0], two distinct logStreamName values, and one 'ATTEMPT=1' / 'ATTEMPT=2' line in the respective streams.
