Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.cancel-running-job-requires-terminate batch.concurrent-consumable-resource-allocation-enforces-capacity batch.job-queue-priority-scheduling-order-enforced batch.job-attempt-duration-timeout-terminates-running-job batch.job-timeout-attribute-triggers-timeout-failure batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown batch.managed-compute-environment-auto-scales-server-side batch.op-contract-update-compute-environment
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.cancel-running-job-requires-terminate|run#11|AssertionError|assert status == "RUNNING", f"CancelJob must not affect a RUNNING job, but status became {status}"
Issue-Key: batch.concurrent-consumable-resource-allocation-enforces-capacity|run#24|AssertionError|assert (status_a in running_statuses) != (status_b in running_statuses), (
Issue-Key: batch.job-queue-priority-scheduling-order-enforced|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: batch.job-attempt-duration-timeout-terminates-running-job|run#21|AssertionError|assert final["status"] == "FAILED", (
Issue-Key: batch.job-timeout-attribute-triggers-timeout-failure|run#19|AssertionError|assert final["status"] == "FAILED", f"job did not fail on timeout, ended as {final['status']}"
Issue-Key: batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown|run#43|AssertionError|assert high_after["status"] == "RUNNING", (
Issue-Key: batch.managed-compute-environment-auto-scales-server-side|_poll_until#18|Timeout|last = probe()
Issue-Key: batch.op-contract-update-compute-environment|run#28|AssertionError|assert message == expected, f"expected {expected!r}, got {message!r}"
Issue-Commit: bf205090aef11dede75ce528b7b57e0b8feca92b

# batch: 5 net-new causes — a RUNNING job's duration collapses (so Cancel-is-a-no-op, consumable-resource admission and cross-queue priority are unobservable), attemptDurationSeconds is still unenforced, a SAGEMAKER_TRAINING service job never holds its quota share, a MANAGED EC2 compute environment never auto-scales desiredvCpus, and UpdateComputeEnvironment's Fargate-unsupported-parameters message drops AWS's escaped apostrophe

> _Note: this issue was identified against the code base at commit `bf205090aef11dede75ce528b7b57e0b8feca92b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Five net-new root causes in AWS Batch, all grounded in failing behavioral checks the suite lists under `### batch` that are marked neither ALREADY FILED nor OWNED.

Context on related OPEN issues (they do NOT own these checks; the replay keys below are not carried by any of them): 20261119T015949Z-batch-2-net-new-causes-timeout-attemptdurationseconds-is.md describes the same FAMILY as Causes 1-2 but is grounded in different check ids (job-attempt-duration-timeout-forces-failed / -terminates-job / job-runs-to-succeeded-with-exit-code-and-status-sequence) and in the older symptom where no job was observable in RUNNING at all; the emulator has since moved on — a job IS now observable in RUNNING, it simply leaves RUNNING far too early, which is what Cause 1 reports, and the two attempt-timeout checks here are new ids. 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md, 20261120T141136Z-batch-2-net-new-causes-a-job-requiring.md and 20261121T005755Z-batch-2-net-new-causes-getjobqueuesnapshot-reports-no.md describe the older "never dispatched / never settles" symptoms, not these.

=== Cause 1 of 5: a RUNNING job does not stay RUNNING for the duration its container command declares, so it holds capacity for no measurable interval ===
AWS: Smithy (com.amazonaws.batch): `JobDetail.startedAt` is "when the job transitioned from the STARTING state to the RUNNING state" and `stoppedAt` "when the job transitioned from the RUNNING state to a terminal state"; `CancelJob` documents "Jobs that progressed to the STARTING or RUNNING state aren't cancelled. However, the API operation still succeeds, even if no job is cancelled. These jobs must be terminated with the TerminateJob or TerminateJobs operation." A job whose container command is `sleep 60` therefore remains RUNNING for about 60 s: CancelJob against it changes nothing (status stays RUNNING, and only TerminateJob ends it as FAILED), it occupies the compute environment's single vCPU slot and the single unit of a REPLENISHABLE consumable resource (`DescribeConsumableResource.inUseQuantity` = "The amount of the consumable resource that is currently in use") for that whole interval, and a second claimant stays in SUBMITTED/PENDING/RUNNABLE until the unit is released.
Emulator: the job reaches RUNNING but leaves it within seconds, long before the command's declared duration elapses, and ends SUCCEEDED. After a successful CancelJob on a `sleep 60` job, polling DescribeJobs every 3 s for 18 s observes `SUCCEEDED` (`CancelJob must not affect a RUNNING job, but status became SUCCEEDED`). Two jobs submitted concurrently, each declaring `sleep 40` and claiming the sole unit (totalQuantity=1, REPLENISHABLE) of a consumable resource, are BOTH already `SUCCEEDED` when read (`exactly one job should have progressed past RUNNABLE, got a=SUCCEEDED b=SUCCEEDED`), so inUseQuantity is never observed at 1 and no claimant is ever gated. With one 1-vCPU Fargate slot, the filler `sleep 30` job on a low-priority queue never stays RUNNING long enough for two further jobs to be observed contending for the freed slot, so the cross-queue priority comparison never reaches a verdict and the check exhausts the runner budget (`Check timed out after 60.0s`).
Why: a client cannot observe or rely on job occupancy. CancelJob appears to stop a RUNNING job (the opposite of the documented no-op), consumable-resource and maxvCpus admission limits are effectively unenforced because no two jobs are ever in flight at the same instant, job-queue priority has no observable effect, and any waiter/dashboard that samples RUNNING under load reads an idle system.
Fix: the clock-driven job attempt lifecycle and placement component behind SubmitJob in the Batch state machine — a performed container command must occupy the duration it declares so the RUNNING span is real, placement must hold a second claimant pending while capacity or a consumable unit is in use and release it on completion, and CancelJob must be a no-op for a job already in STARTING/RUNNING.
Traces: batch.cancel-running-job-requires-terminate, batch.concurrent-consumable-resource-allocation-enforces-capacity, batch.job-queue-priority-scheduling-order-enforced.

=== Cause 2 of 5: timeout.attemptDurationSeconds is not enforced — an over-running attempt ends SUCCEEDED instead of FAILED ===
AWS: Smithy: `JobTimeout.attemptDurationSeconds` — "The job timeout time (in seconds) that's measured from the job attempt's startedAt timestamp. After this time passes, Batch terminates your jobs if they aren't finished. The minimum value for the timeout is 60 seconds." So a job submitted with `timeout={attemptDurationSeconds: 60}` whose command sleeps 600 s is terminated by Batch itself at startedAt + attemptDurationSeconds, ends `FAILED` (never SUCCEEDED), and carries a `statusReason` ("A short, human-readable string to provide more details for the current status of the job") naming the attempt timeout — the requirement asserted by the checks is the class: terminal FAILED plus a statusReason naming the timeout, whose only variable part is the configured attemptDurationSeconds taken from the request. Hypothesis: AWS words it "Job attempt duration exceeded timeout".
Emulator: the job ends `SUCCEEDED` with the normal-exit reason: `expected the 60s attemptDurationSeconds to force the job to FAILED, got SUCCEEDED: Essential container in task exited` and `job did not fail on timeout, ended as SUCCEEDED`. No attempt deadline is armed from `timeout.attemptDurationSeconds`, and (per Cause 1) the attempt never occupies enough time to cross one.
Why: the attempt timeout is a client's only server-side safety net against a runaway job. Cost control, stuck-job detection and timeout-keyed retry/alerting all see a clean SUCCEEDED where AWS reports FAILED with a timeout reason, so a job AWS would kill is reported as having completed its work.
Fix: the job attempt lifecycle in the state machine behind SubmitJob — arm a deadline at `startedAt + attemptDurationSeconds`, terminate the attempt when it is exceeded, set the job (and `attempts[].statusReason`) to FAILED with the attempt-timeout reason.
Traces: batch.job-attempt-duration-timeout-terminates-running-job, batch.job-timeout-attribute-triggers-timeout-failure.

=== Cause 3 of 5: a SAGEMAKER_TRAINING service job never holds its quota share's capacity — it reaches SUCCEEDED immediately, so no other share's job is ever queued behind it ===
AWS: recorded behavior only for the service-job plane's timing (the Smithy model states no duration rule). AWS behavior the check records: with a service environment of maxCapacity 1 for one capacity unit and two quota shares on one SAGEMAKER_TRAINING job queue, the first share's submitted training job (whose entry point sleeps ~600 s, under a 900 s MaxRuntimeInSeconds/attemptDurationSeconds) stays `RUNNING` and holds the environment's sole unit; the other share's job on the same queue stays queued until the holder is terminated, and is then dispatched with no re-submission.
Emulator: the first share's job is already `SUCCEEDED` when the second share's job is read — `teamHigh's job left RUNNING before teamLow was read, so the queued read proves nothing: 'SUCCEEDED'` — so the sole capacity unit is never held, nothing is ever queued behind it, and the terminate-then-dispatch leg cannot be exercised.
Why: fair-share/quota-share scheduling is unobservable and effectively absent: a client cannot see capacity being reserved, lent or preempted, and over-subscription of a service environment's capacityLimits cannot be detected.
Fix: the service-job (SAGEMAKER_TRAINING) execution component behind SubmitServiceJob in the Batch state machine — a dispatched service job must occupy its declared run duration while holding the quota share's capacity, and queueing/dispatch across shares must be driven off that held capacity.
Traces: batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown.

=== Cause 4 of 5: a MANAGED EC2 compute environment at desiredvCpus=0 never scales up for a queued job ===
AWS: Smithy: `ComputeResource.desiredvCpus` — "The desired number of vCPUS in the compute environment. Batch modifies this value between the minimum and maximum values based on job queue demand." So a MANAGED EC2 compute environment created with desiredvCpus=0 (minvCpus 0, maxvCpus > 0) raises desiredvCpus above 0 on its own once a job requiring 1 VCPU is queued on a job queue over it — with no client scaling call — and the job then reaches RUNNING/SUCCEEDED on the provisioned capacity.
Emulator: the scenario never reaches a verdict inside the runner budget (`Check timed out after 60.0s`). The check creates and validates the MANAGED EC2 compute environment, asserts the baseline desiredvCpus==0, creates the queue and job definition, submits the job and then polls `DescribeComputeEnvironments` for `computeResources.desiredvCpus > 0`; that condition (or the preceding wait for the MANAGED EC2 environment to settle VALID) never becomes true, so server-side scaling is not observable at all.
Why: a client that provisions a scale-from-zero MANAGED EC2 environment sees capacity that never appears: queue-demand-driven scaling, desiredvCpus as a reported signal, and any job that depends on EC2 (non-Fargate) capacity are all unavailable.
Fix: the MANAGED EC2 compute-environment scaling component of the Batch state (the placement/scaling side of the compute-environment state machine) — queued demand on a job queue over a MANAGED EC2 environment must raise `computeResources.desiredvCpus` between minvCpus and maxvCpus and make the capacity available for placement.
Traces: batch.managed-compute-environment-auto-scales-server-side.

=== Cause 5 of 5: UpdateComputeEnvironment's Fargate-unsupported-parameters ClientException message drops the backslash AWS puts before the apostrophe ===
AWS: Smithy: `UpdateComputeEnvironment` has `computeResources: ComputeResourceUpdate` and errors `ClientException` (400) / `ServerException` (500); setting the EC2/SPOT-only `instanceRole` on a FARGATE compute environment is refused `ClientException`. AWS's recorded message text is, verbatim and in full (the escape before the apostrophe is literally present in the wire text):
  The following parameters can\'t be updated for Fargate compute environment: allocationStrategy, bidPercentage, ec2Configuration, ec2KeyPair, imageId, instanceRole, instanceTypes, launchTemplate, placementGroup, tags, type, updatePolicy, updateToLatestImageVersion.
All of that is FIXED text — the sentence and the alphabetical list of EC2-only parameter names are constant for a Fargate compute environment; no part of it comes from the request or from state.
Emulator: the status (400) and code (`ClientException`) match and the CE is correctly left unchanged, but the message is served without the backslash: `The following parameters can't be updated for Fargate compute environment: allocationStrategy, bidPercentage, ec2Configuration, ec2KeyPair, imageId, instanceRole, instanceTypes, launchTemplate, placementGroup, tags, type, updatePolicy, updateToLatestImageVersion.`
Why: a client (or test) matching on AWS's exact refusal text does not match the emulator's, so the one-character divergence turns a correctly-classified refusal into an unrecognised one.
Fix: the UpdateComputeEnvironment validation component that composes this ClientException message — it must emit AWS's literal text, including the `can\'t` escape, rather than a re-typed apostrophe.
Traces: batch.op-contract-update-compute-environment.

## Evidence
Suite: 261/295 passed (88.5%); service `batch` 235/251 (94%), 16 failed. Eleven of those are marked OWNED/ALREADY FILED; the five causes below cluster the eight surviving failures.

Cause 1 — RUNNING window collapses:
  [mismatch] batch.cancel-running-job-requires-terminate
    check_batch_cancel_running_job_requires_terminate.py:47 — AssertionError: CancelJob must not affect a RUNNING job, but status became SUCCEEDED
    Setup: 1-vCPU FARGATE CE, job definition command ['sleep','60']; the job IS polled to RUNNING, CancelJob(reason=...) succeeds, then DescribeJobs is sampled 6x every 3 s.
    Expected: status stays RUNNING for the whole 18 s window (CancelJob is a documented no-op for STARTING/RUNNING); only the subsequent TerminateJob ends it FAILED.
    Actual: SUCCEEDED inside the window — the `sleep 60` job left RUNNING in under 18 s.
  [mismatch] batch.concurrent-consumable-resource-allocation-enforces-capacity
    check_batch_concurrent_consumable_resource_allocation_enforces_capacity.py:108 — AssertionError: exactly one job should have progressed past RUNNABLE, got a=SUCCEEDED b=SUCCEEDED
    Setup: maxvCpus=4 FARGATE CE; CreateConsumableResource(resourceType=REPLENISHABLE, totalQuantity=1); job definition command ['sleep','40'] claiming quantity 1; two SubmitJob calls concurrently.
    Expected: exactly one job past RUNNABLE and the other in SUBMITTED/PENDING/RUNNABLE; DescribeConsumableResource observed at inUseQuantity=1 / availableQuantity=0 while the unit is held; the loser dispatched after release.
    Actual: both jobs SUCCEEDED before the first read; inUseQuantity never observed at 1.
  [mismatch] batch.job-queue-priority-scheduling-order-enforced
    Error: Check timed out after 60.0s — the check holds the single 1-vCPU slot with a ['sleep','30'] filler job on a priority=1 queue, then submits one probe job to that queue and one to a priority=100 queue over the same CE and polls which leaves RUNNABLE first.
    Expected: the filler occupies the slot for ~30 s; while it does, both probes sit RUNNABLE; when it finishes, the high-priority queue's job leaves RUNNABLE first and the low-priority one is still waiting.
    Actual: no interval in which the filler holds the slot and both probes are queued, so the differential never materialises and the scenario reaches no verdict.

Cause 2 — attemptDurationSeconds unenforced:
  [mismatch] batch.job-attempt-duration-timeout-terminates-running-job
    check_batch_job_attempt_duration_timeout_terminates_running_job.py:90 — AssertionError: expected the 60s attemptDurationSeconds to force the job to FAILED, got SUCCEEDED: Essential container in task exited
  [mismatch] batch.job-timeout-attribute-triggers-timeout-failure
    check_batch_job_timeout_attribute_triggers_timeout_failure.py:74 — AssertionError: job did not fail on timeout, ended as SUCCEEDED
    Setup (both): FARGATE CE + ENABLED queue; container command sleeping 600 s; SubmitJob with timeout={'attemptDurationSeconds': 60} (the modelled minimum).
    Expected: terminal status FAILED with a statusReason naming the attempt timeout, roughly 60 s after startedAt (stoppedAt-startedAt tracking the configured value), isTerminated true.
    Actual: SUCCEEDED, statusReason 'Essential container in task exited'.

Cause 3 — service job never holds quota-share capacity:
  [mismatch] batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown
    check_batch_e2e_fair_share_quota_lifecycle_affects_dispatch_and_teardown.py:187 — AssertionError: teamHigh's job left RUNNING before teamLow was read, so the queued read proves nothing: 'SUCCEEDED'
    Setup: CreateSchedulingPolicy(quotaSharePolicy.idleResourceAssignmentStrategy=FIFO); CreateServiceEnvironment(SAGEMAKER_TRAINING, capacityLimits maxCapacity=1 for one capacity unit); SAGEMAKER_TRAINING job queue over it; quota shares teamHigh (RESERVE) and teamLow (LEND), each maxCapacity 1; teamHigh's training job holds the unit with a ~600 s sleep under a 900 s MaxRuntimeInSeconds.
    Expected: teamHigh's job still RUNNING when teamLow's job is read, teamLow's job queued; terminating teamHigh's job dispatches teamLow's without re-submission.
    Actual: teamHigh's job already SUCCEEDED.

Cause 4 — MANAGED EC2 CE never scales:
  [mismatch] batch.managed-compute-environment-auto-scales-server-side
    Error: Check timed out after 60.0s — after validating a MANAGED EC2 CE at desiredvCpus=0 (with an EC2 instance profile), creating a queue over it and submitting a 1-VCPU/1024-MEMORY job, the check polls DescribeComputeEnvironments (interval 8 s, budget 300 s) for computeResources.desiredvCpus > 0 and then DescribeJobs for RUNNING/SUCCEEDED/FAILED.
    Expected: desiredvCpus rises above 0 with no client scaling call, and the job reaches RUNNING or SUCCEEDED on that capacity.
    Actual: the scenario never settles — no reading with desiredvCpus > 0 (nor a settled MANAGED EC2 environment) is ever produced.

Cause 5 — refusal message text:
  [mismatch] batch.op-contract-update-compute-environment
    check_batch_op_contract_update_compute_environment.py:80 — AssertionError: expected "The following parameters can\\'t be updated for Fargate compute environment: allocationStrategy, bidPercentage, ec2Configuration, ec2KeyPair, imageId, instanceRole, instanceTypes, launchTemplate, placementGroup, tags, type, updatePolicy, updateToLatestImageVersion.", got "The following parameters can't be updated for Fargate compute environment: allocationStrategy, bidPercentage, ec2Configuration, ec2KeyPair, imageId, instanceRole, instanceTypes, launchTemplate, placementGroup, tags, type, updatePolicy, updateToLatestImageVersion."
    The rest of the check passes: maxvCpus 4->8 update settles VALID, code is ClientException, and the CE is left with maxvCpus 8 / type FARGATE and no instanceRole. The sole divergence is the missing backslash before the apostrophe in AWS's fixed sentence.

## Steps to Reproduce
Common preconditions: an IAM Fargate task-execution role; a MANAGED/FARGATE compute environment polled to VALID; an ENABLED job queue polled to VALID; a FARGATE job definition using image public.ecr.aws/amazonlinux/amazonlinux:2023 with VCPU/MEMORY resourceRequirements.

Cause 1:
1. Create a FARGATE CE with maxvCpus=1 and a queue over it; RegisterJobDefinition with command ['sleep','60'].
2. SubmitJob, poll DescribeJobs to RUNNING, then CancelJob(jobId, reason='x').
3. Poll DescribeJobs every 3 s for ~18 s: the status becomes SUCCEEDED. AWS keeps it RUNNING for ~60 s and only TerminateJob ends it (FAILED).
4. Variant: CreateConsumableResource(REPLENISHABLE, totalQuantity=1); RegisterJobDefinition with command ['sleep','40'] and consumableResourceProperties claiming quantity 1; SubmitJob twice concurrently. Both are SUCCEEDED within seconds; DescribeConsumableResource never reports inUseQuantity=1 and neither job is ever held pending.
5. Variant: hold the single 1-vCPU slot with a ['sleep','30'] filler on a priority=1 queue, then submit probe jobs to a priority=1 and a priority=100 queue over the same CE: there is no window in which both probes are RUNNABLE behind the filler.

Cause 2:
1. RegisterJobDefinition with a command that sleeps 600 s.
2. SubmitJob with timeout={'attemptDurationSeconds': 60}.
3. Poll DescribeJobs until terminal: status is SUCCEEDED with statusReason 'Essential container in task exited'. AWS ends it FAILED ~60 s after startedAt with a statusReason naming the attempt timeout.

Cause 3:
1. CreateSchedulingPolicy with quotaSharePolicy={'idleResourceAssignmentStrategy':'FIFO'}; CreateServiceEnvironment(serviceEnvironmentType='SAGEMAKER_TRAINING', capacityLimits=[{maxCapacity:1, capacityUnit:'ml.c5.xlarge'}]); CreateJobQueue(jobQueueType='SAGEMAKER_TRAINING', schedulingPolicyArn=..., serviceEnvironmentOrder=[...]); CreateQuotaShare 'teamHigh' (RESERVE) and 'teamLow' (LEND), each maxCapacity 1.
2. Submit a SAGEMAKER_TRAINING service job under teamHigh whose entry point sleeps ~600 s (MaxRuntimeInSeconds 900); poll it to RUNNING.
3. Submit a second service job under teamLow on the same queue, then read both: teamHigh's job is already SUCCEEDED, so teamLow's job is never queued behind the held capacity unit.

Cause 4:
1. CreateComputeEnvironment type=MANAGED, state=ENABLED, computeResources.type=EC2 with minvCpus=0, desiredvCpus=0, maxvCpus>0 and an EC2 instance profile; poll to VALID and confirm desiredvCpus==0.
2. CreateJobQueue over it; RegisterJobDefinition requiring VCPU 1 / MEMORY 1024 with command ['/bin/sh','-c','sleep 30']; SubmitJob.
3. Poll DescribeComputeEnvironments: computeResources.desiredvCpus never rises above 0 (and the job never reaches RUNNING/SUCCEEDED on provisioned capacity).

Cause 5:
1. CreateComputeEnvironment type=MANAGED with computeResources.type=FARGATE, maxvCpus=4, subnets/securityGroupIds; poll to VALID.
2. UpdateComputeEnvironment(computeEnvironment=name, computeResources={'instanceRole': 'arn:aws:iam::<account>:instance-profile/bogus'}).
3. Inspect the 400 ClientException message: it reads "...can't be updated..." where AWS's text is "...can\\'t be updated..." (literal backslash before the apostrophe); the remainder of the sentence and the parameter list are identical.
