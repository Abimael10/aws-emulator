Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked
Issue-Key: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked|_poll_until#6|Timeout|return last
Issue-Commit: 6df4780ed6263f6fd5cc79bc833a1ad99a020747

# batch__logs: a SEQUENTIAL array job whose child exits non-zero never executes at all, so neither the failing child's exit code nor the blocking of the remaining children is ever observable

> _Note: this issue was identified against the code base at commit `6df4780ed6263f6fd5cc79bc833a1ad99a020747`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new root cause, grounded in the one failing check the suite lists under `### batch__logs` that it marks neither ALREADY FILED nor OWNED. The run consumes its whole budget without any job state AWS reaches ever appearing.

A related open issue exists for this service token but does NOT carry this check's replay key and does not cover the failure-propagation half of the SEQUENTIAL contract: 20261119T120645Z-batch-logs-queued-jobs-are-never-dispatched-a.md (maxvCpus gating, and SEQUENTIAL children running one at a time in index order). Also related, and likewise not covering it: 20261107T153156Z-batch-logs-a-placed-batch-job-ends-failed.md and 20261119T003525Z-batch-logs-a-placed-job-s-container-never.md.

=== Cause 1 of 1: a SEQUENTIAL array job's child failure neither carries back its exit code nor blocks the remaining children, because no job on the queue is ever run ===
AWS: Smithy: ArrayJobDependency is the enum [N_TO_N, SEQUENTIAL] used as JobDependency.type on SubmitJob; ArrayPropertiesDetail.index is "The job index within the array that's associated with this job"; ContainerDetail.exitCode is "The exit code returned upon completion" and JobStatus is the enum [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]. With a SEQUENTIAL array dependency, children run strictly one after another in ascending index order; a child that exits non-zero ends FAILED carrying that exact exit code in container.exitCode, and the array stops there — the children after it never run, so they acquire no container, no attempt and no awslogs stream, and the parent array job ends FAILED. A non-array control job submitted on the same definition, role, queue and image (where AWS_BATCH_JOB_ARRAY_INDEX is unset) takes the success branch, ends SUCCEEDED with container.exitCode 0, and its awslogs stream holds both of its printed markers.
Emulator: nothing on the queue is ever observed running or terminating. The check's polling for the control job's terminal status (and then for the array's children) runs out of the harness budget — `Error: Check timed out after 60.0s` — so no exit code is carried back, no child is blocked, no log stream exists, and neither the failure propagation nor the SEQUENTIAL gate has any observable effect. Because even the single non-array control job never reaches a terminal state, the data plane never dispatches work at all on this path.
Why: a client that models a staged pipeline as a SEQUENTIAL array job depends on Batch to (a) surface the failing stage's own exit code and (b) stop the remaining stages. Here the job never completes in either direction: a failure is never reported, downstream stages are neither run nor provably withheld, and any caller waiting on DescribeJobs for a terminal status waits forever.
Fix: the job placement/dispatch path behind SubmitJob and its array-child scheduling — place a submitted job on a VALID ENABLED queue with capacity, run the container and record its terminal status and container.exitCode; then, for an array job with a SEQUENTIAL dependency, gate each child on the preceding index and, when a child ends FAILED, leave the remaining children unplaced and end the parent FAILED.
Traces: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked.

## Evidence
Suite: 252/295 passed; service `batch__logs` 6/10 (60%), 4 failed. Three of those four are OWNED by already-filed issues; the one below was marked neither ALREADY FILED nor OWNED.

batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked
  Expected (AWS): on a MANAGED/FARGATE compute environment with 4 vCPUs against 0.25-vCPU jobs (capacity is never the limiting factor), the non-array control job on the shared definition ends SUCCEEDED with container.exitCode 0 and its awslogs stream carries both markers; the size-3 array job submitted with dependsOn type SEQUENTIAL has child index 0 exit non-zero — that child ends FAILED with its own exit code in container.exitCode — and children 1 and 2 never run (no container, no attempt, no log stream).
  Actual: `Error: Check timed out after 60.0s`. No job on the queue reaches a terminal status: the control job never ends, no child ever executes, and no log stream is ever created, so neither the exit-code propagation nor the blocking of later children can be observed.

The 60 s figure is this run's harness budget and the array size, exit code 7 and 0.25-vCPU sizing are the check's own values; the requirement is that a submitted job is dispatched and reaches a terminal state carrying its container exit code, and that a SEQUENTIAL array stops at the first failing index.

## Steps to Reproduce
Preconditions: an IAM Fargate task execution role, a CloudWatch log group, a MANAGED/FARGATE compute environment polled to VALID with maxVcpus 4, an ENABLED job queue polled to VALID, and a Fargate job definition (VCPU 0.25 / MEMORY 512) with awslogs logging to that group whose command is `sh -c 'echo BEGIN=$AWS_BATCH_JOB_ARRAY_INDEX; if [ "$AWS_BATCH_JOB_ARRAY_INDEX" = "0" ]; then exit 7; fi; echo END=$AWS_BATCH_JOB_ARRAY_INDEX'`.

1. SubmitJob on that definition with no arrayProperties (the control) and poll DescribeJobs for a terminal status — it never reaches SUCCEEDED (or any terminal state).
2. SubmitJob on the same definition with arrayProperties.size=3 and dependsOn=[{type: "SEQUENTIAL"}]; poll DescribeJobs on the parent and ListJobs with arrayJobId=<parent> — no child ever reaches RUNNING or a terminal status, child 0 never reports container.exitCode 7, and no child's awslogs stream is ever created, so the blocking of children 1 and 2 cannot be distinguished from nothing running.
