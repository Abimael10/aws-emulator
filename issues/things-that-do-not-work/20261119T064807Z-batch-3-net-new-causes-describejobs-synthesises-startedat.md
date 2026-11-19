Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.submit-job-progresses-to-runnable batch.job-dependency-holds-job-pending batch.job-dependency-releases-on-success-and-blocks-on-parent-failure batch.cancel-job-before-running-moves-to-failed batch.cancel-jobs-bulk-cancels-pending-job-and-leaves-terminal-job-frozen batch.compute-environment-disabled-withholds-placement-until-reenabled batch.job-attempt-duration-timeout-terminates-running-job batch.job-timeout-attribute-triggers-timeout-failure batch.delete-consumable-resource-in-use-rejected batch.concurrent-consumable-resource-allocation-enforces-capacity
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.submit-job-progresses-to-runnable|run#12|AssertionError|assert "startedAt" not in job, f"unplaced RUNNABLE job must not have startedAt: {job.get('startedAt')}"
Issue-Key: batch.job-dependency-holds-job-pending|run#10|AssertionError|assert "startedAt" not in dep, (
Issue-Key: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure|run#10|AssertionError|assert "startedAt" not in held, (
Issue-Key: batch.cancel-job-before-running-moves-to-failed|run#12|AssertionError|assert "startedAt" not in job, "a job cancelled before STARTING must not report startedAt"
Issue-Key: batch.cancel-jobs-bulk-cancels-pending-job-and-leaves-terminal-job-frozen|run#28|AssertionError|assert "startedAt" not in blocked_after, (
Issue-Key: batch.compute-environment-disabled-withholds-placement-until-reenabled|_assert_withheld#17|AssertionError|assert "startedAt" not in job, (
Issue-Key: batch.job-attempt-duration-timeout-terminates-running-job|_wait_job_status#20|AssertionError|raise AssertionError(f"job {job_id} stuck at status {status} after {timeout_s}s (last={last})")
Issue-Key: batch.job-timeout-attribute-triggers-timeout-failure|_poll_until#18|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last={last}")
Issue-Key: batch.delete-consumable-resource-in-use-rejected|run#14|AssertionError|assert batch.describe_consumable_resource(consumableResource=cr_arn)["inUseQuantity"] == 1
Issue-Key: batch.concurrent-consumable-resource-allocation-enforces-capacity|run#19|AssertionError|assert max(observed_in_use) == 1, "expected the winning job to actually hold the single unit"
Issue-Commit: 744a403bcd89e11c6cc0112b0c3d4fd5ce52e365

# batch: 3 net-new causes — DescribeJobs synthesises startedAt=0 on jobs that never started, timeout.attemptDurationSeconds leaves a job stuck RUNNING forever, and consumable-resource inUseQuantity is never charged by a running job

> _Note: this issue was identified against the code base at commit `744a403bcd89e11c6cc0112b0c3d4fd5ce52e365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobs

## Description
Three net-new root causes in the AWS Batch job plane, grounded in failing behavioral checks the suite lists under `### batch` that are marked neither ALREADY FILED nor OWNED.

Context on related open issues (they do NOT cover these checks — their recorded replay keys are different failures, and the emulator's behaviour has since moved on): 20261119T015949Z-batch-2-net-new-causes-timeout-attemptdurationseconds-is.md reports the attempt timeout being ignored with the job ending SUCCEEDED in ~2 s, and reports `batch.concurrent-consumable-resource-allocation-enforces-capacity` as never reaching a verdict because no job was ever observable RUNNING. Jobs now DO sit in RUNNING for the duration their command declares; what is reported below is the divergence that is now observable in that reached-RUNNING path (the job never leaves RUNNING at all, and the consumable-resource accounting it should charge stays at zero). 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md, 20261112T013843Z-batch-retrystrategy-is-not-applied-a-job-whose.md and 20261117T124549Z-batch-7-net-new-causes-bulk-job-and.md describe other symptoms (the old never-executes reason string, retryStrategy, refusal wording/page sizes) and do not describe any of the three causes below.

=== Cause 1 of 3: DescribeJobs reports startedAt=0 for a job that has not started — the member must be ABSENT until the job transitions STARTING -> RUNNING ===
AWS: Smithy (com.amazonaws.batch) `JobDetail.startedAt` -> Long, documented as "The Unix timestamp (in milliseconds) for when the job was started. More specifically, it's when the job transitioned from the STARTING state to the RUNNING state." It is `clientOptional`, i.e. legally absent on the wire, and `JobDetail.stoppedAt` is likewise "when the job transitioned from the RUNNING state to a terminal state". Recorded behaviour: for a job in SUBMITTED, PENDING or RUNNABLE — and for a job that was cancelled before it was ever placed — real Batch returns a JobDetail with NO `startedAt` key at all (and no `stoppedAt`), only `createdAt`. The timestamp appears exactly once the job has genuinely entered RUNNING, and its value is the real transition instant.
Emulator: every JobDetail carries `startedAt`, and it is `0` for jobs that have never been placed. Six checks fail on exactly this, all reading the literal value 0: an unplaced RUNNABLE job (`unplaced RUNNABLE job must not have startedAt: 0`), a dependency-blocked PENDING job (`a PENDING dependent has not run, so startedAt must be absent: 0`), a job cancelled before STARTING (`a job cancelled before STARTING must not report startedAt`), a dependency-blocked job that was then cancelled (`a dependency-blocked job must have no startedAt regardless of cancellation, got 0`), and a job withheld because its compute environment is DISABLED (`a withheld job must have no startedAt, got 0`).
Why: a client cannot distinguish "queued, never ran" from "ran". `startedAt` is the canonical has-this-job-executed signal: operators, waiters and dashboards test for the member's presence, and SDK consumers that read it get the epoch (1970-01-01T00:00:00Z) as a start time. Every elapsed-time computation (`stoppedAt - startedAt`, queue-wait = `startedAt - createdAt`) yields ~55 years of runtime or a negative queue wait, and a withheld/blocked/cancelled-before-start job is misreported as having executed — which is the opposite of the fact the operator is querying.
Fix: the DescribeJobs serialiser/projection for JobDetail in the Batch service (the job-detail view built from stored job state): `startedAt` and `stoppedAt` must be modelled as optional and omitted while unset, rather than defaulted to 0 when the job state machine has not recorded a STARTING->RUNNING (resp. RUNNING->terminal) transition. The same omission applies to `attempts[].startedAt`/`stoppedAt` for an attempt that has not started.
Traces: batch.submit-job-progresses-to-runnable, batch.job-dependency-holds-job-pending, batch.job-dependency-releases-on-success-and-blocks-on-parent-failure, batch.cancel-job-before-running-moves-to-failed, batch.cancel-jobs-bulk-cancels-pending-job-and-leaves-terminal-job-frozen, batch.compute-environment-disabled-withholds-placement-until-reenabled.

=== Cause 2 of 3: timeout.attemptDurationSeconds is still not enforced — the job now stays RUNNING forever instead of being terminated ===
AWS: Smithy `JobTimeout.attemptDurationSeconds` — "The job timeout time (in seconds) that's measured from the job attempt's startedAt timestamp. After this time passes, Batch terminates your jobs if they aren't finished. The minimum value for the timeout is 60 seconds." `JobDetail.status` targets JobStatus [SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED] and `JobDetail.statusReason` is "A short, human-readable string to provide more details for the current status of the job". So a job submitted with `timeout={attemptDurationSeconds: N}` whose command runs longer than N is terminated by Batch at `startedAt + N`, becomes terminal FAILED (never stays RUNNING, never SUCCEEDED), gets `stoppedAt` set, and carries a statusReason naming the attempt timeout — fixed text for that error class, with the duration/limit in it taken from the request's attemptDurationSeconds; both checks assert only the class (terminal FAILED, reason containing "timeout"/"timed out"). Hypothesis: AWS words it "Job attempt duration exceeded timeout".
Emulator: the job is placed, enters RUNNING, and never leaves it. `batch.job-attempt-duration-timeout-terminates-running-job`: `job <id> stuck at status RUNNING after 300.0s`, with the polled JobDetail showing `status: 'RUNNING'`, one attempt with a `startedAt`, and the submitted `timeout` still in place — the 300 s observation window is five times the 60 s attemptDurationSeconds. `batch.job-timeout-attribute-triggers-timeout-failure`: `condition not met within 300s`, last JobDetail again `status: 'RUNNING'` for a `['/bin/sh','-c','sleep 600']` command under its attempt timeout. No deadline is armed from `timeout.attemptDurationSeconds`, so the only thing that can end such a job is its command finishing on its own.
Why: a client has no way to bound a runaway job. The attempt timeout is the safety net for stuck/runaway workloads (cost control, timeout-driven retry, alerting keyed on the timeout statusReason); with it unenforced a job that AWS would kill after N seconds occupies queue and compute capacity indefinitely, every waiter on a terminal status hangs, and the timeout failure path cannot be tested at all.
Fix: the job attempt lifecycle in the Batch job state machine behind SubmitJob — arm a deadline at `attempt.startedAt + timeout.attemptDurationSeconds` when the attempt starts, and when it expires terminate the attempt, set the job terminal FAILED with `stoppedAt` and the attempt-timeout `statusReason` (and the matching `attempts[].statusReason`), releasing the job's capacity.
Traces: batch.job-attempt-duration-timeout-terminates-running-job, batch.job-timeout-attribute-triggers-timeout-failure.

=== Cause 3 of 3: a running job never charges the consumable resource it claimed — DescribeConsumableResource reports inUseQuantity 0 while the unit is held ===
AWS: Smithy `ConsumableResourceSummary.inUseQuantity` -> Long, "The amount of the consumable resource that is currently in use", alongside `totalQuantity` ("The total amount of the consumable resource that is available") and `resourceType` (REPLENISHABLE / NON_REPLENISHABLE); a job claims units through `consumableResourceProperties.consumableResourceList[].quantity`. So while a job that claims 1 unit of a totalQuantity=1 resource is RUNNING, DescribeConsumableResource reports `inUseQuantity=1` and `availableQuantity=0` (total - inUse), a second claimant of the same unit is held pending rather than admitted, and a REPLENISHABLE resource returns to `inUseQuantity=0` once the holder goes terminal. Recorded behaviour additionally: DeleteConsumableResource while `inUseQuantity > 0` is refused with error code `ClientException` and the fixed message `Consumable resource cannot be deleted when in use` (no variable part), and the resource is NOT removed.
Emulator: the claim is never recorded. `batch.delete-consumable-resource-in-use-rejected` polls the job to RUNNING successfully and then fails on `describe_consumable_resource(consumableResource=cr_arn)['inUseQuantity'] == 1` — the running holder of the single NON_REPLENISHABLE unit leaves inUseQuantity at its unheld value. `batch.concurrent-consumable-resource-allocation-enforces-capacity` samples the resource six times while a job is STARTING/RUNNING and fails with `expected the winning job to actually hold the single unit`: every sample reads inUseQuantity 0 (the availableQuantity == totalQuantity - inUseQuantity invariant holds, so availableQuantity is reported as the full total while the unit is in use). Because the accounting never leaves zero, the in-use delete refusal above is never reached by the check and the admission gate is inert — nothing holds the second claimant back.
Why: consumable-resource capacity is unenforced and unobservable. A client that uses a consumable resource to cap concurrency (licence seats, external-service throughput, a fixed pool) has every job admitted at once, sees an idle resource under load, and a DeleteConsumableResource issued while jobs are mid-flight is not protected by the in-use guard — the resource backing running jobs can be deleted.
Fix: consumable-resource accounting in the Batch job placement/state component — on admitting a job, charge each entry of its `consumableResourceProperties.consumableResourceList` against the named resource's `inUseQuantity` (and recompute `availableQuantity = totalQuantity - inUseQuantity`); hold a claimant whose claim does not fit; on a job going terminal, release the units for a REPLENISHABLE resource (and keep them consumed for NON_REPLENISHABLE); and gate DeleteConsumableResource on `inUseQuantity > 0` with the ClientException above.
Traces: batch.delete-consumable-resource-in-use-rejected, batch.concurrent-consumable-resource-allocation-enforces-capacity.

## Evidence
Suite: 217/295 passed (73.6%); service `batch` 201/251 (80%), 50 failed. All but the failures below are marked ALREADY FILED or OWNED.

Cause 1 — startedAt synthesised as 0 on jobs that never started:
  [mismatch] batch.submit-job-progresses-to-runnable
    AssertionError: unplaced RUNNABLE job must not have startedAt: 0
  [mismatch] batch.job-dependency-holds-job-pending
    AssertionError: a PENDING dependent has not run, so startedAt must be absent: 0
  [mismatch] batch.job-dependency-releases-on-success-and-blocks-on-parent-failure
    AssertionError: a PENDING dependent has not run, so startedAt must be absent: 0
  [mismatch] batch.cancel-job-before-running-moves-to-failed
    AssertionError: a job cancelled before STARTING must not report startedAt
  [mismatch] batch.cancel-jobs-bulk-cancels-pending-job-and-leaves-terminal-job-frozen
    AssertionError: a dependency-blocked job must have no startedAt regardless of cancellation, got 0
  [mismatch] batch.compute-environment-disabled-withholds-placement-until-reenabled
    AssertionError: during the 240s absence window (3x the control's measured 5s placement latency): a withheld job must have no startedAt, got 0
  Expected (recorded AWS): DescribeJobs returns the JobDetail with no 'startedAt' key (and no 'stoppedAt'), only 'createdAt', for any job still SUBMITTED/PENDING/RUNNABLE or cancelled before STARTING.
  Actual: the key is always present with value 0.

Cause 2 — attempt timeout never terminates the attempt:
  [mismatch] batch.job-attempt-duration-timeout-terminates-running-job
    AssertionError: job <jobId> stuck at status RUNNING after 300.0s (last JobDetail: status 'RUNNING', attempts=[{container:{taskArn, logStreamName, networkInterfaces:[]}, startedAt:<t>}], container.command ['sleep','600'], timeout present)
    Expected: terminal FAILED about attemptDurationSeconds after attempts[0].startedAt, with stoppedAt set and a statusReason naming the attempt timeout.
    Actual: RUNNING for the whole 300 s window (5x the 60 s attemptDurationSeconds).
  [mismatch] batch.job-timeout-attribute-triggers-timeout-failure
    AssertionError: condition not met within 300s; last JobDetail status 'RUNNING' for command ['/bin/sh','-c','sleep 600'] submitted with a timeout.
    Expected: FAILED + attempt-timeout statusReason.
    Actual: still RUNNING.

Cause 3 — consumable-resource in-use accounting stays at zero:
  [mismatch] batch.delete-consumable-resource-in-use-rejected
    AssertionError at: assert batch.describe_consumable_resource(consumableResource=cr_arn)['inUseQuantity'] == 1
    Setup: CreateConsumableResource(resourceType='NON_REPLENISHABLE', totalQuantity=1); job definition command ['sleep','30'] with consumableResourceProperties.consumableResourceList=[{consumableResource: <arn>, quantity: 1}]; the job was polled to RUNNING before the read.
    Expected: inUseQuantity=1 / availableQuantity=0 while the job runs; then DeleteConsumableResource refused with ClientException and the fixed message 'Consumable resource cannot be deleted when in use', the resource still describable and still in use; after the job goes terminal, inUseQuantity back to 0 and the delete succeeds.
    Actual: inUseQuantity is not 1 while the holder is RUNNING, so the in-use delete refusal is never even reached.
  [mismatch] batch.concurrent-consumable-resource-allocation-enforces-capacity
    AssertionError: expected the winning job to actually hold the single unit (max(observed_in_use) over six samples taken while a job was STARTING/RUNNING)
    Setup: REPLENISHABLE resource totalQuantity=1; two jobs submitted concurrently, each claiming quantity 1, command ['sleep','40'].
    Expected: inUseQuantity reaches 1 while one job holds the unit, availableQuantity 0, exactly one job past RUNNABLE and the other still SUBMITTED/PENDING/RUNNABLE, and the unit released on completion.
    Actual: every sample reads inUseQuantity 0 (availableQuantity reconciles to the full totalQuantity), so no job is gated behind the single unit.

## Steps to Reproduce
Preconditions (all causes): an IAM Fargate task-execution role; a compute environment polled to VALID; an ENABLED job queue polled to VALID; a FARGATE container job definition (VCPU 0.25 / MEMORY 512).

Cause 1 (startedAt on an unstarted job):
1. SubmitJob against a queue whose compute environment cannot place it yet — e.g. an UNMANAGED compute environment (job sits RUNNABLE), or a job with dependsOn=[{jobId: <a job still running>}] (job sits PENDING), or a queue whose compute environment has been set state=DISABLED.
2. Call DescribeJobs(jobs=[jobId]).
3. Observe the JobDetail contains 'startedAt': 0. AWS returns no 'startedAt' (and no 'stoppedAt') key at all for that job.
4. Also: SubmitJob, then CancelJob before it reaches STARTING, then DescribeJobs — the cancelled-before-start job still reports startedAt 0 instead of omitting it.

Cause 2 (attempt timeout):
1. RegisterJobDefinition with containerProperties.command = ['sleep','600'].
2. SubmitJob with timeout={'attemptDurationSeconds': 60}.
3. Poll DescribeJobs for 300 s. The job reports status 'RUNNING' for the whole window and never becomes terminal; AWS terminates the attempt ~60 s after attempts[0].startedAt, returning status FAILED with stoppedAt set and a statusReason naming the attempt timeout.

Cause 3 (consumable resource):
1. CreateConsumableResource(consumableResourceName=<name>, resourceType='NON_REPLENISHABLE', totalQuantity=1).
2. RegisterJobDefinition with command ['sleep','30'] and consumableResourceProperties={'consumableResourceList':[{'consumableResource': <arn>, 'quantity': 1}]}.
3. SubmitJob; poll DescribeJobs until status == 'RUNNING'.
4. Call DescribeConsumableResource(consumableResource=<arn>): inUseQuantity reads 0 and availableQuantity equals totalQuantity, where AWS reports inUseQuantity 1 / availableQuantity 0.
5. Call DeleteConsumableResource(consumableResource=<arn>) while that job runs: it is not refused with ClientException 'Consumable resource cannot be deleted when in use'.
6. With a REPLENISHABLE totalQuantity=1 resource, submit two jobs concurrently each claiming 1 unit: both are admitted past RUNNABLE instead of one being held until the other releases the unit.
