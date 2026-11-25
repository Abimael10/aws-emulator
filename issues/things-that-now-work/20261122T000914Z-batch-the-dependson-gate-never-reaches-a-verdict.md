Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure|_trace_job_to_terminal#31|Timeout|return trace, job
Issue-Commit: a9288303feb8dead80b5d9b52ed92320ffa70966

# batch: the dependsOn gate never reaches a verdict — a submitted container job does not settle, so a dependent job is never observable in PENDING and never released by a SUCCEEDED parent (1 cause)

> _Note: this issue was identified against the code base at commit `a9288303feb8dead80b5d9b52ed92320ffa70966`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new root cause in the AWS Batch job data plane, grounded in the single failing behavioral check the suite listed under `### batch` that was marked neither ALREADY FILED nor OWNED by an open issue.

Related open issues exist but do NOT carry this check's replay key, so they do not cover it: 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md (it lists this check id, but under the replay key `run#10|AssertionError|assert held["status"] == "PENDING"` — the scenario no longer gets that far and now exhausts the runner budget instead, so the suite does not mark it owned), 20261120T110741Z-batch-2-net-new-causes-a-job-on.md (CE state never gates placement / dependsOn on a MANAGED-FARGATE CE), 20261119T120645Z-batch-logs-queued-jobs-are-never-dispatched-a.md (batch__logs), 20261121T160051Z-batch-ecs-a-fargate-batch-job-still-never.md (batch__ecs).

=== Cause 1 of 1: a submitted container job never settles, so the dependsOn hold/release contract is unobservable (check runs to the runner budget) ===
AWS: Smithy: `JobStatus` is an enum of [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED]; `JobDetail.status` is "The current status for the job" and its note points at the troubleshooting page for "Jobs stuck in RUNNABLE status", i.e. a placed job is expected to progress. `SubmitJob` accepts `dependsOn` (JobDependencyList of `JobDependency{jobId, type}`, `type` targeting `ArrayJobDependency` [N_TO_N, SEQUENTIAL]) and `JobDetail.dependsOn` is "A list of job IDs that this job depends on". AWS's documented behavior for a dependency: a job whose dependency has not yet SUCCEEDED is held in `PENDING` (and while held has no `attempts` and no `startedAt`); when the dependency reaches SUCCEEDED the dependent is released and advances SUBMITTED -> PENDING -> RUNNABLE -> STARTING -> RUNNING -> SUCCEEDED, with `startedAt` no earlier than the parent's `stoppedAt`; when the dependency FAILS the dependent reaches a terminal state having never been dispatched — no `attempts`, no `startedAt`, no `container.exitCode` — and `JobDetail.statusReason` ("A short, human-readable string to provide more details for the current status of the job") explains the dependency failure. Each of those is a separate, observable transition; the status values and the presence/absence of `attempts`/`startedAt`/`exitCode` are the requirement, while concrete job ids and epoch timestamps are per-run values and are not.
Emulator: the whole scenario never reaches any of those assertions. Replaying `batch.job-dependency-releases-on-success-and-blocks-on-parent-failure` ends with `Check timed out after 60.0s` (verdict: mismatch) — the check stands up its own VPC/subnet/IGW, an ECS task-execution role, a MANAGED/FARGATE compute environment polled to VALID, an ENABLED queue polled to VALID, two Fargate job definitions, and four jobs (good parent + dependent, failing parent + dependent), then polls DescribeJobs; no job in that graph settles, so the poll loops until the runner budget is exhausted and the emulator produces no verdict on PENDING-hold, on release-after-success, or on the blocked-dependent terminal shape at all. Against real AWS the same sequence settles and the check asserts.
Why: a client that models workflow ordering with `dependsOn` gets no usable signal from this emulator. Waiters and state machines polling for the dependent's `PENDING` hold, for its release after the parent SUCCEEDS, or for the dependency-failed terminal state (empty `attempts`, absent `startedAt`, absent `container.exitCode`, explanatory `statusReason`) never fire, and any orchestration that gates downstream work on a Batch dependency simply hangs rather than failing fast. Because the scenario never settles, the emulator is also silently untestable on the dependency contract: a regression here cannot be detected.
Fix: the job placement/attempt lifecycle behind SubmitJob and the dependency gate in the job scheduler — a job placed on a VALID MANAGED/FARGATE compute environment attached to an ENABLED queue must advance to a terminal status promptly, and `dependsOn` must hold the dependent in PENDING until its dependency resolves, then release it on SUCCEEDED or terminate it (undispatched, with a statusReason naming the dependency failure) on FAILED. The DescribeJobs serializer must reflect that state, omitting `startedAt`/`attempts`/`container.exitCode` for a job that never ran.
Traces: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure.

## Evidence
Suite: 239/295 passed (81.0%); service `batch` 221/251 (88%), 30 failed. 29 of those 30 are marked OWNED by an existing open issue; the one below is not.

[mismatch] batch.job-dependency-releases-on-success-and-blocks-on-parent-failure
  Error: Check timed out after 60.0s

Expected (AWS), per the check's assertions, none of which was ever reached:
  - a dependent whose dependsOn is unsatisfied leaves SUBMITTED into status 'PENDING', echoes [parent jobId] in dependsOn, and has no 'startedAt';
  - the good parent reaches 'SUCCEEDED' through a monotonic status trace (SUBMITTED -> ... -> SUCCEEDED), then its dependent reaches 'SUCCEEDED' with container.exitCode == 0 and startedAt >= parent stoppedAt;
  - the failing parent (container exits 9) reaches 'FAILED', and its dependent reaches 'FAILED' with no attempts, no startedAt, no container.exitCode, and a non-empty statusReason.
Actual (emulator): no job in the graph settles; DescribeJobs polling never satisfies any of the above and the check is cut off at the 60 s runner budget with no verdict on the dependency contract.

## Steps to Reproduce
Preconditions (all created by the check itself): a VPC with a public subnet and a default route via an internet gateway, an ECS task-execution IAM role, a MANAGED/FARGATE compute environment (maxvCpus 8) polled to VALID, an ENABLED job queue over it polled to VALID, and two FARGATE job definitions using image public.ecr.aws/docker/library/busybox:1.36 with commands ['/bin/sh','-c','sleep 20; exit 0'] and ['/bin/sh','-c','sleep 20; exit 9'].

1. SubmitJob good_parent using the exit-0 definition.
2. SubmitJob good_child using the exit-0 definition with dependsOn=[{'jobId': good_parent}].
3. SubmitJob bad_parent using the exit-9 definition.
4. SubmitJob bad_child using the exit-0 definition with dependsOn=[{'jobId': bad_parent}].
5. Poll DescribeJobs on good_child and bad_child until each leaves SUBMITTED: AWS reports 'PENDING' with dependsOn echoing the parent jobId and no startedAt.
6. Poll DescribeJobs on good_parent to a terminal status: AWS reports SUCCEEDED; then poll good_child: AWS reports SUCCEEDED with container.exitCode 0 and startedAt >= good_parent.stoppedAt.
7. Poll DescribeJobs on bad_parent: AWS reports FAILED; then poll bad_child: AWS reports FAILED with attempts empty, no startedAt, no container.exitCode and a non-empty statusReason.

Against the emulator the polling in steps 5-7 never converges and the scenario is cut off at the runner budget (60 s) without reaching any of those observations.
