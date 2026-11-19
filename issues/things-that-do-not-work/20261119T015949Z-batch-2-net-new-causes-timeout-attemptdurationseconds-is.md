Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.job-attempt-duration-timeout-forces-failed batch.job-attempt-duration-timeout-terminates-job batch.job-runs-to-succeeded-with-exit-code-and-status-sequence batch.concurrent-consumable-resource-allocation-enforces-capacity
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.job-attempt-duration-timeout-forces-failed|run#10|AssertionError|assert timed["status"] == "FAILED", (
Issue-Key: batch.job-attempt-duration-timeout-terminates-job|run#12|AssertionError|assert final["status"] == "FAILED", (
Issue-Key: batch.job-runs-to-succeeded-with-exit-code-and-status-sequence|run#10|AssertionError|assert set(trace) & {"RUNNABLE", "STARTING", "RUNNING"}, (
Issue-Key: batch.concurrent-consumable-resource-allocation-enforces-capacity|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Commit: 1dc698be9e80e0112562bfdd5ab001c28f556c88

# batch: 2 net-new causes — timeout.attemptDurationSeconds is never enforced (an over-running job ends SUCCEEDED), and a job is never observable in RUNNABLE/STARTING/RUNNING because its container command consumes no time

> _Note: this issue was identified against the code base at commit `1dc698be9e80e0112562bfdd5ab001c28f556c88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Two net-new root causes in the AWS Batch container-job data plane, grounded in failing behavioral checks the suite lists under `### batch` that are marked neither ALREADY FILED nor OWNED.

Context on related open issues (they do NOT cover these checks): 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md lists some of these check ids, but every cause it describes is the OLD symptom — a placed job settling SUBMITTED->FAILED with the emulator-authored reason "This emulator does not run containers, so the job's container was never started". The emulator's behavior has since changed: jobs now run and reach SUCCEEDED. What this issue reports is the NEW divergence in that now-working path (an over-running job SUCCEEDS instead of being timed out, and the whole walk is over before any client poll can see it). 20261112T013843Z-batch-retrystrategy-is-not-applied-...md (retryStrategy), 20261119T003525Z-batch-logs-a-placed-job-s-container-never...md (batch__logs) and 20261117T124549Z-batch-7-net-new-causes-bulk-job-and.md (refusal messages/page sizes) cover other symptoms and other service tokens.

=== Cause 1 of 2: timeout.attemptDurationSeconds is never enforced — a job whose command out-runs its attempt timeout ends SUCCEEDED ===
AWS: Smithy (com.amazonaws.batch): `JobTimeout.attemptDurationSeconds` — "The job timeout time (in seconds) that's measured from the job attempt's startedAt timestamp. After this time passes, Batch terminates your jobs if they aren't finished. The minimum value for the timeout is 60 seconds." `JobDetail.status` targets `JobStatus` [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED] and `JobDetail.statusReason` is "A short, human-readable string to provide more details for the current status of the job". So a job submitted with `timeout={attemptDurationSeconds: 60}` whose container command would take ~300 s is SIGTERM'd by Batch at startedAt + 60 s, ends `FAILED` (never SUCCEEDED), and its `statusReason` states that the attempt exceeded its timeout — fixed text naming the attempt-timeout condition; the duration/limit in it comes from the request's attemptDurationSeconds, and nothing else in it is per-run. Hypothesis: AWS words it "Job attempt duration exceeded timeout". The requirement that is not a hypothesis is the class: terminal status FAILED plus a statusReason naming the attempt timeout (both checks assert only that the reason contains "timeout"/"timed out").
Emulator: the job reaches `SUCCEEDED`. One check reports `a 300s command under a 60s attemptDurationSeconds must end FAILED, got 'SUCCEEDED' (SUCCEEDED means the timeout was ignored)`; the other reports `expected the job to be killed by its 60s timeout, got status='SUCCEEDED' reason='Essential container in task exited'`. The attempt therefore never exceeds any deadline, because the declared command (`sleep 300`) is performed instantaneously rather than for the duration it declares (`crate::state` performs a command out of a fixed catalog with no elapsed time — services/service-com-amazonaws-batch-awsbatchv20160810/src/state.rs, the `container_run` module), and no attempt-timeout deadline is armed from `timeout.attemptDurationSeconds` at all.
Why: a client cannot bound a runaway job. Every workload that relies on the attempt timeout as its safety net (cost control, stuck-job detection, timeout-driven retry/alerting keyed on the timeout statusReason) sees a clean SUCCEEDED instead of the FAILED+timeout-reason AWS produces, so the failure path is untestable and a job that AWS would kill is reported as having completed its work.
Fix: the job attempt lifecycle in the clock-driven state machine behind SubmitJob — it must arm a deadline at `startedAt + attemptDurationSeconds` for the attempt, terminate the attempt when a job's performed command has not finished within it, mark the job FAILED and set the attempt-timeout `statusReason` (and the matching `attempts[].statusReason`). That requires the performed command to occupy the duration it declares, so a timeout is reachable at all.
Traces: batch.job-attempt-duration-timeout-forces-failed, batch.job-attempt-duration-timeout-terminates-job.

=== Cause 2 of 2: a job is never observable in RUNNABLE/STARTING/RUNNING — the whole walk completes in ~2 s and no job holds capacity for a measurable interval ===
AWS: Smithy: `JobStatus` enumerates SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED; `JobDetail.status` carries the note "If your jobs don't progress to STARTING, see Jobs stuck in RUNNABLE status"; `JobDetail.startedAt` is "when the job transitioned from the STARTING state to the RUNNING state" and `stoppedAt` "when the job transitioned from the RUNNING state to a terminal state". A job therefore spends real time in RUNNABLE/STARTING/RUNNING: a client polling DescribeJobs every few seconds observes at least one of them, and a job executing a `sleep 40` command stays RUNNING for about that long, holding the capacity and the consumable-resource units it claimed for that interval (`ConsumableResourceSummary.inUseQuantity` — "The amount of the consumable resource that is currently in use" — reads 1 of a totalQuantity of 1 while one job runs, and a second job claiming the same single unit stays pending until the first releases it; a REPLENISHABLE resource returns the unit on completion).
Emulator: each clock-owed phase is 500 ms (`JOB_PHASE_MILLIS` in services/service-com-amazonaws-batch-awsbatchv20160810/src/state.rs) and the container command is performed with no elapsed time, so a job walks SUBMITTED -> ... -> SUCCEEDED in about two seconds regardless of what its command declares. A polling client sees only the status trace ['SUBMITTED', 'SUCCEEDED'] (`job reached SUCCEEDED without ever being observed queued or running`) — RUNNABLE, STARTING and RUNNING are never returned by any DescribeJobs call. Consequently a pair of jobs that each claim the single unit of a totalQuantity=1 REPLENISHABLE consumable resource are both finished before either is observable as STARTING/RUNNING: polling for "either job in STARTING or RUNNING" never succeeds and that check never reaches a verdict (it exhausts the suite's per-check deadline — reported as `Check timed out after 60.0s`), so inUseQuantity is never observed at 1, no job is ever gated behind the held unit, and the winner/loser startedAt-after-stoppedAt ordering cannot exist.
Why: a client cannot observe its own jobs. Waiters and dashboards polling DescribeJobs/ListJobs for RUNNABLE (backlog), STARTING or RUNNING see nothing between submission and completion; `batch wait` style polling, queue-depth metrics and "is my job running yet?" logic all read an idle system under load. Anything whose semantics depend on a job OCCUPYING capacity — consumable-resource admission, maxVcpus caps, fair-share dispatch ordering, concurrency limits — is unobservable and effectively unenforced, because no two jobs are ever in flight at the same instant.
Fix: the clock-driven job lifecycle and placement component in the Batch state (state.rs, its `placement` and `container_run` modules): a performed command must occupy the duration it declares so that a job genuinely sits in RUNNING, and the transition spans must leave each non-terminal status observable to an ordinary polling client rather than collapsing the walk into a sub-poll-interval burst; consumable-resource admission must then hold a second claimant pending while the unit is in use and release it on completion.
Traces: batch.job-runs-to-succeeded-with-exit-code-and-status-sequence, batch.concurrent-consumable-resource-allocation-enforces-capacity.

## Evidence
Suite: 218/295 passed; service `batch` 203/251 (81%), 48 failed. All but five of those failures are marked ALREADY FILED or OWNED; the failures below are the surviving ones this issue clusters.

Cause 1 — attempt timeout ignored:
  [mismatch] batch.job-attempt-duration-timeout-forces-failed
    AssertionError: a 300s command under a 60s attemptDurationSeconds must end FAILED, got 'SUCCEEDED' (SUCCEEDED means the timeout was ignored)
    Expected: terminal status FAILED, statusReason naming the attempt timeout (reason contains "timeout"/"timed out").
    Actual: status 'SUCCEEDED'.
  [mismatch] batch.job-attempt-duration-timeout-terminates-job
    AssertionError: expected the job to be killed by its 60s timeout, got status='SUCCEEDED' reason='Essential container in task exited'
    Expected: FAILED + attempt-timeout statusReason, with stoppedAt-startedAt tracking the configured 60 s.
    Actual: SUCCEEDED with the normal-exit reason 'Essential container in task exited'.
  Request shape in both cases: SubmitJob with timeout={'attemptDurationSeconds': 60} (the modelled minimum) over a job definition whose command sleeps far longer.

Cause 2 — no observable RUNNABLE/STARTING/RUNNING, no held capacity:
  [mismatch] batch.job-runs-to-succeeded-with-exit-code-and-status-sequence
    AssertionError: job reached SUCCEEDED without ever being observed queued or running: ['SUBMITTED', 'SUCCEEDED']
    Expected: the polled status trace intersects {RUNNABLE, STARTING, RUNNING}.
    Actual: ['SUBMITTED', 'SUCCEEDED'].
  [mismatch] batch.concurrent-consumable-resource-allocation-enforces-capacity
    Error: Check timed out after 60.0s — the check stalls in `_poll_until(_one_job_running, timeout_s=240, interval_s=5)`, i.e. polling DescribeJobs for either of two submitted jobs to report STARTING or RUNNING; both jobs are already SUCCEEDED (command ['sleep','40'], consumableResourceProperties claiming 1 unit of a totalQuantity=1 REPLENISHABLE resource) before the first poll, so the condition can never become true.
    Expected while one job runs: DescribeConsumableResource reports inUseQuantity=1, availableQuantity=0, totalQuantity=1; exactly one job past RUNNABLE and the other still SUBMITTED/PENDING/RUNNABLE; the loser's startedAt >= the winner's stoppedAt; inUseQuantity back to 0 once both finish.
    Actual: no job is ever reported STARTING or RUNNING, so none of those observations is reachable.

Supporting code in the checkout (citations only): services/service-com-amazonaws-batch-awsbatchv20160810/src/state.rs line 151 — `pub const JOB_PHASE_MILLIS: i64 = 500;` (every clock-owed job transition, so the full walk is ~2 s) and lines 30-42 of the same file's module doc — a job's container command is "PERFORMED" by a fixed catalog with "NO process is started", hence no elapsed duration for a `sleep`.

## Steps to Reproduce
Preconditions (both causes): an IAM Fargate task-execution role; a MANAGED/FARGATE compute environment polled to VALID; an ENABLED job queue polled to VALID; a FARGATE job definition (image public.ecr.aws/amazonlinux/amazonlinux:2023, VCPU 0.25 / MEMORY 512).

Cause 1:
1. RegisterJobDefinition with containerProperties.command = ['sleep','300'] (or submit with that containerOverrides command).
2. SubmitJob with timeout={'attemptDurationSeconds': 60}.
3. Poll DescribeJobs until the job is terminal. Observe status 'SUCCEEDED' (and container/status reason 'Essential container in task exited'); AWS ends it FAILED roughly 60 s after startedAt with a statusReason naming the attempt timeout, and stoppedAt-startedAt ~= 60 s.

Cause 2:
1. RegisterJobDefinition with command ['sleep','40'] and consumableResourceProperties={'consumableResourceList':[{'consumableResource': <arn of a REPLENISHABLE CreateConsumableResource with totalQuantity=1>, 'quantity': 1}]}.
2. SubmitJob twice against the same queue, concurrently.
3. Poll DescribeJobs on both ids every 1-5 s and record every status seen: the trace is ['SUBMITTED','SUCCEEDED'] for each — RUNNABLE/STARTING/RUNNING never appear — and both jobs are terminal within ~2 s even though each declares a 40 s command.
4. While polling, call DescribeConsumableResource on that resource: inUseQuantity is never observed at 1 and no job is ever held pending behind the single unit, so the admission gate and the loser-starts-after-winner-stops ordering can never be observed.
