Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__logs.batch-maxvcpus-cap-holds-second-job-runnable-then-runs-it batch__logs.batch-sequential-array-children-run-one-at-a-time-in-index-order
Issue-Key: batch__logs.batch-maxvcpus-cap-holds-second-job-runnable-then-runs-it|run#6|Timeout|_release_estate(ctx, bat, queue, ce, net, jobs=[j for j in (first_id, second_id) if j])
Issue-Key: batch__logs.batch-sequential-array-children-run-one-at-a-time-in-index-order|_poll_until#6|Timeout|return last
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# batch__logs: queued jobs are never dispatched — a maxvCpus-capped queue never runs the held second job, and a SEQUENTIAL array dependency never runs its children, so neither serialisation nor its log ordering is ever observable (2 causes)

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Two net-new root causes grounded in failing behavioral checks the suite listed under `### batch__logs` and marked neither ALREADY FILED nor OWNED. Both checks consume their whole run budget without the job states AWS reaches ever appearing.

Related open issues exist for this service token but do NOT carry these checks' replay keys and do not cover these two scheduling behaviours: 20261119T003525Z-batch-logs-a-placed-job-s-container-never.md (a placed job's container never writes stdout to CloudWatch Logs), 20261107T153156Z-batch-logs-a-placed-batch-job-ends-failed.md (a placed job ends FAILED with an emulator-authored reason), 20261104T041055Z-batch-logs-array-job-submission-is-unimplemented-and.md and 20261114T101713Z-batch-logs-array-job-children-never-execute-so.md (array submission / per-child index injection).

=== Cause 1 of 2: a compute environment's maxvCpus does not gate placement, and the held job is never run afterwards ===
AWS: Smithy: ComputeResource.maxvCpus is "The maximum number of vCPUs that a compute environment can support", required on a MANAGED compute environment; JobStatus is the enum [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]. With maxvCpus=1 and two 1-vCPU Fargate jobs, the first job runs while the second is held in RUNNABLE (listed by ListJobs with jobStatus=RUNNABLE, never placed, writing no log stream); when the first reaches SUCCEEDED the second is placed, runs, reaches SUCCEEDED, and its own log stream holds only its own output, whose first event is no earlier than the first container's last event.
Emulator: neither job is ever observed running to completion; the check's polling for the first job's SUCCEEDED and then for the second job's placement runs to the end of the run budget (`Check timed out after 60.0s`). The capacity gate, the hand-off after the first job finishes, and the resulting per-job log stream ordering are all unobservable because no job is ever dispatched.
Why: a client sizing a compute environment with maxvCpus relies on Batch both to withhold placement beyond that capacity and to resume the backlog when capacity frees; here nothing runs at all, so queue drain never happens and the maxvCpus contract has no observable effect in either direction.
Fix: the capacity accounting in the job placement/scheduling path behind SubmitJob — sum the vCPU resourceRequirements of the compute environment's placed jobs against ComputeResource.maxvCpus, hold over-capacity jobs in RUNNABLE, and re-evaluate the queue when a job terminates so the held job is then placed and run.
Traces: batch__logs.batch-maxvcpus-cap-holds-second-job-runnable-then-runs-it.

=== Cause 2 of 2: a SEQUENTIAL array dependency does not serialise an array job's children in ascending index order ===
AWS: Smithy: ArrayJobDependency is the enum [N_TO_N, SEQUENTIAL], used as JobDependency.type on SubmitJob; ArrayPropertiesDetail.index is "The job index within the array that's associated with this job". With a SEQUENTIAL array dependency, the children of an array job run strictly one after another in ascending index order even on capacity that could run them all at once (here 4 vCPUs against 3 x 0.25-vCPU children), so child i only starts after child i-1 has finished — observable as child i's first log event following child i-1's last.
Emulator: no child is ever observed running or finishing; the check polls for the children's progression and exhausts the run budget (`Check timed out after 60.0s`). Neither the serialisation nor the index ordering is produced, and no per-child log stream ordering exists to compare.
Why: a client that uses a SEQUENTIAL array dependency to express an ordered pipeline over array indices gets neither ordering nor execution; stage i+1 can never be known to follow stage i, and the array never completes.
Fix: the array-job child scheduling in the SubmitJob/array path — a SEQUENTIAL dependency must gate each child on the completion of the preceding index (holding later children in PENDING) and release them in ascending index order as each predecessor terminates.
Traces: batch__logs.batch-sequential-array-children-run-one-at-a-time-in-index-order.

## Evidence
Suite: 217/295 passed; service `batch__logs` 2/10 (20%), 8 failed. The two failures below were the only ones marked neither ALREADY FILED nor OWNED.

Cause 1 — batch__logs.batch-maxvcpus-cap-holds-second-job-runnable-then-runs-it
  Expected (AWS): with maxvCpus=1, job A runs to SUCCEEDED while job B stays RUNNABLE and unplaced (no log stream); B then runs to SUCCEEDED and its log stream contains exactly its own BEGIN/DONE markers, its first event no earlier than A's last.
  Actual: no job ever runs or terminates; the polling loop ran out of budget — `Error: Check timed out after 60.0s`.

Cause 2 — batch__logs.batch-sequential-array-children-run-one-at-a-time-in-index-order
  Expected (AWS): a size-3 array job with a SEQUENTIAL array dependency on 4 vCPUs of capacity runs children 0,1,2 strictly one at a time in ascending index order; child i's first logged line follows child i-1's last.
  Actual: no child is ever observed executing; `Error: Check timed out after 60.0s`.

The 60s figure is this run's harness budget and the checks' 150s/15s container dwells are per-run values, not requirements; the requirement is that the placement gating, the hand-off and the index ordering occur at all.

## Steps to Reproduce
Preconditions for both: an IAM Fargate task execution role, a CloudWatch log group, a MANAGED/FARGATE compute environment polled to VALID and an ENABLED job queue polled to VALID, and a Fargate job definition with awslogs logging to that group.

Cause 1: Create the compute environment with computeResources.maxvCpus=1. Register a job definition requiring VCPU 1 / MEMORY 2048. SubmitJob A with a command that dwells, then SubmitJob B with the same requirements. Poll DescribeJobs/ListJobs: A never reaches RUNNING or SUCCEEDED, B is never placed afterwards, and neither job's log stream is ever created — so neither the hold nor the subsequent run can be observed.

Cause 2: Create the compute environment with computeResources.maxvCpus=4 and register a job definition requiring VCPU 0.25 / MEMORY 512. SubmitJob with arrayProperties.size=3 and dependsOn=[{jobId: <this array parent>, type: "SEQUENTIAL"}] (self-SEQUENTIAL array dependency) and a command that logs a start and end marker. Poll DescribeJobs on the parent and ListJobs with arrayJobId: the children never run, so no per-child log stream exists and ascending-index serialisation cannot be observed.
