Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked
Issue-Key: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# batch__logs: in a SEQUENTIAL array job no child is ever executed, so a failing child's own exit code is never carried back and the blocking of the children after it is unobservable (1 cause)

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new root cause, grounded in the one failing behavioral check the suite lists under `### batch__logs` that it marks neither ALREADY FILED nor OWNED.

The open issue 20261119T120645Z-batch-logs-queued-jobs-are-never-dispatched-a.md covers the POSITIVE SEQUENTIAL ordering check (batch__logs.batch-sequential-array-children-run-one-at-a-time-in-index-order) and the maxvCpus hold, and does NOT carry this check's replay key; the negative face reported here — the failing child's exit-code propagation and the must-be-absent execution of the children after it — is not among its causes. Also related but not covering this check: 20261107T153156Z-batch-logs-a-placed-batch-job-ends-failed.md.

=== Cause 1 of 1: array-job children are never dispatched, so neither the failing child's FAILED/exitCode nor the blocked state of its successors is ever produced ===

AWS: Smithy: `SubmitJob` takes `arrayProperties` and `dependsOn`, where `JobDependency.type` is an `ArrayJobDependency` of the enum [N_TO_N, SEQUENTIAL]; `ArrayPropertiesDetail.index` is "The job index within the array that's associated with this job" (returned for array job children) and `ArrayPropertiesDetail.statusSummary` summarises the children per `JobStatus`, the enum [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]. In AWS a size-3 array job carrying a SEQUENTIAL array dependency, submitted onto capacity that could run all children at once (4 vCPUs against 0.25-vCPU children, so capacity is never the limiting factor), runs its children strictly one at a time in ascending index order. Child `<arrayJobId>:0`, whose container exits non-zero, is addressable by DescribeJobs, ends `FAILED`, and Batch carries the container's own exit code back unchanged in `container.exitCode` — the exact non-zero value the container exited with, not a Batch-authored substitute — while its awslogs stream (reported on the job's container) holds the line the container printed before exiting. Because that child failed, the remaining children `:1` and `:2` are never attempted: each ends `FAILED` with an empty `attempts` list, no awslogs stream at all, and no `container.exitCode` (it is absent, because no container ran). A non-array control job on the same definition, queue and role takes the success branch and runs to `SUCCEEDED` with `container.exitCode` 0 and its own log stream.

Emulator: nothing in the array ever runs. The check consumes its entire run budget polling (`Check timed out after 60.0s`) without the control job or any array child reaching a terminal JobStatus. So the failing child's `FAILED` status and the unchanged propagation of its container exit code never occur; the per-child awslogs stream for the failing child never exists; and the must-be-absent effects for children `:1` and `:2` (empty `attempts`, no stream, absent `container.exitCode`) cannot be distinguished from 'has not run yet', which is exactly what makes the blocking unverifiable.

Why: a client that models an ordered, fail-fast pipeline as a SEQUENTIAL array job depends on two guarantees — that the exit code of the step that failed comes back verbatim so the failure can be classified, and that no later step runs after a failure. Here neither is observable: the array never terminates, the failing step's exit code is never reported, and a client cannot tell a blocked successor from a pending one, so fail-fast semantics and any retry/alerting logic keyed on `container.exitCode` are entirely absent.

Fix: the array-job child scheduling and attempt-result propagation behind SubmitJob — dispatch array children (gating each index on its predecessor's termination under a SEQUENTIAL dependency), carry the container's own exit code into `container.exitCode` and the job to `FAILED` on a non-zero exit, and on a predecessor failure terminate the remaining children as `FAILED` without an attempt, leaving `attempts` empty, no awslogs stream and `container.exitCode` absent.

Traces: batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked.

## Evidence
Suite: 919/1114 passed (82.5%); service `batch__logs` 6/10 (60%), 4 failed. Three of those were OWNED by existing issues; the one below was marked neither ALREADY FILED nor OWNED.

[mismatch] batch__logs.batch-sequential-array-child-failure-leaves-remaining-children-blocked
  Expected (AWS), from the check body:
    - the non-array control job on the same definition reaches SUCCEEDED with container.exitCode == 0 and an awslogs stream containing its END= line;
    - child `<arrayJobId>:0` reaches FAILED and `container.exitCode` equals the exact non-zero code its container exited with (`Batch must carry the container's own exit code back unchanged`), with an awslogs stream containing its BEGIN=0 line;
    - children `<arrayJobId>:1` and `:2` each reach FAILED with `attempts` empty, no awslogs stream, and `container.exitCode` absent (None).
  Actual: `Error: Check timed out after 60.0s` — no job in the submission (control job or any array child) ever reaches a terminal JobStatus, so none of the above is produced: no exit code is carried back and the blocked successors are indistinguishable from unstarted ones.

The 60s figure is this run's harness budget and the array size / exit code / vCPU figures are the check's inputs; the requirement is that array children are dispatched at all, that the failing child's own exit code is returned unchanged, and that its successors are terminated without an attempt.

## Steps to Reproduce
1. Create a CloudWatch log group and an IAM Fargate task execution role.
2. Create a MANAGED/FARGATE compute environment with computeResources.maxvCpus=4 and poll it to VALID; create an ENABLED job queue on it and poll to VALID. (4 vCPUs against 0.25-vCPU jobs means capacity is never the reason anything fails to run.)
3. RegisterJobDefinition requiring VCPU 0.25 / MEMORY 512 with awslogs logging to that log group and the command `sh -c 'echo BEGIN=$AWS_BATCH_JOB_ARRAY_INDEX; if [ "$AWS_BATCH_JOB_ARRAY_INDEX" = "0" ]; then exit <N>; fi; echo END=$AWS_BATCH_JOB_ARRAY_INDEX'` for some non-zero N.
4. SubmitJob once with no arrayProperties (the control job), and once with arrayProperties={size:3} and dependsOn=[{type:'SEQUENTIAL'}].
5. Poll DescribeJobs on the control job id and on `<arrayJobId>:0`, `:1`, `:2` until terminal.
6. Observe: nothing becomes terminal within the run budget. The control job never reaches SUCCEEDED, child `:0` never reaches FAILED and never reports container.exitCode == N, and children `:1`/`:2` never reach FAILED — so the 'never attempted' guarantees (empty attempts, no log stream, absent exitCode) cannot be asserted. In AWS all four jobs settle, `:0` returns exitCode N, and `:1`/`:2` are FAILED with no attempt.
