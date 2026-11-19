Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.batch-consumable-resource-quantity-caps-concurrent-running-jobs batch.fair-share-quota-weight-orders-job-dispatch batch.job-queue-priority-scheduling-order-enforced
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.batch-consumable-resource-quantity-caps-concurrent-running-jobs|run#20|Timeout|assert max(concurrent_running) <= 1, (
Issue-Key: batch.fair-share-quota-weight-orders-job-dispatch|_poll_until#19|Timeout|last = probe()
Issue-Key: batch.job-queue-priority-scheduling-order-enforced|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# batch: a submitted job never reaches RUNNING or a terminal state, so consumable-resource capacity capping, fair-share weightFactor dispatch ordering and job-queue priority dispatch are all unobservable (3 causes)

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Three net-new root causes in the AWS Batch scheduling/data plane, grounded in failing behavioral checks the suite listed under `### batch` and marked neither ALREADY FILED nor OWNED. All three checks exhaust their whole run budget without ever observing the job state AWS reaches, because a job submitted to an ENABLED job queue backed by a VALID MANAGED/FARGATE compute environment is never advanced to RUNNING and never settles in a terminal state; every scheduling behaviour layered on top of placement is therefore unverifiable.

Related open issues exist but do NOT cover these checks (their replay keys are not carried by any of them, and the suite did not mark these checks OWNED): 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md (placed jobs settling SUBMITTED->FAILED with an emulator-authored reason, plus dependsOn/timeout/retry/array/GetJobQueueSnapshot), 20261110T230035Z-batch-3-net-new-causes-createjobqueue-mis-words.md (a submitted Fargate job never becomes RUNNABLE), 20261119T064807Z-batch-3-net-new-causes-describejobs-synthesises-startedat.md (startedAt=0 / stuck RUNNING), 20261119T015949Z-batch-2-net-new-causes-timeout-attemptdurationseconds-is.md (attempt timeout). The three causes below are about the scheduling decisions the emulator must make once a job is placed, and each needs its own fix and its own re-replay.

=== Cause 1 of 3: a consumable resource's totalQuantity does not cap the number of concurrently RUNNING jobs, and jobs requiring it never run at all ===
AWS: Smithy: ConsumableResourceRequirement{consumableResource, quantity} is "Information about a consumable resource required to run a job"; quantity is "The quantity of the consumable resource that is needed". A REPLENISHABLE consumable resource with totalQuantity=1 admits at most one job that requires quantity 1 to JobStatus RUNNING at any instant (JobStatus enum: SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED); the other job waits in RUNNABLE and runs after the first releases the resource, so both jobs eventually reach SUCCEEDED.
Emulator: neither job is ever observed RUNNING and neither reaches SUCCEEDED; the check's polling loop for a RUNNING/terminal transition runs until the harness budget ends (reported as `Check timed out after 60.0s`). No concurrency observation is ever produced, so the cap is not merely mis-enforced — the state it governs never occurs. Hypothesis: placement never moves a job out of its initial status, so the consumable-resource accounting is never consulted.
Why: a client that models a scarce external resource (licence seats, database connections) with a consumable resource gets neither the serialisation it paid for nor completion: waiters on RUNNING or SUCCEEDED never fire, and the documented admission control cannot be observed or relied on.
Fix: the job placement/scheduling component behind SubmitJob and the consumable-resource allocation it must consult — a job must be admitted to RUNNING only while its ConsumableResourceRequirement quantities fit inside the resource's totalQuantity, held in RUNNABLE otherwise, and released on completion so the waiting job runs.
Traces: batch.batch-consumable-resource-quantity-caps-concurrent-running-jobs.

=== Cause 2 of 3: a fair-share policy's weightFactor does not order dispatch — no job under an idle share is ranked ahead of one under a busy share ===
AWS: Smithy: ShareAttributes.weightFactor is "The weight factor for the share identifier. The default value is 1.0. A lower value has a higher priority for compute resources"; shareIdentifier selects the share. On a fair-share-scheduling queue whose sole compute slot is occupied by a job under one share, a newly submitted job under an idle share is ranked ahead of an equally-prioritised job under the busy share, and that ordering is observable in GetJobQueueSnapshot's frontOfQueue (ordered "based on their job priority and share usage" for FSS queues) and in which job is dispatched when the slot frees.
Emulator: the ordering can never be reached: the job that must occupy the queue's single Fargate slot is never observed RUNNING, so the share-usage state that weightFactor acts on is never established and the check runs to the end of its budget (`Check timed out after 60.0s`) without a dispatch-order observation.
Why: a client using a fair-share scheduling policy to stop one tenant starving another gets no share accounting and no ordering at all; weightFactor is accepted on CreateSchedulingPolicy and then has no effect on any observable dispatch decision.
Fix: the fair-share scheduler in the job dispatch path — per-share usage must be accumulated from placed jobs and used, together with weightFactor, to order the queue's RUNNABLE jobs (and the ordering surfaced through GetJobQueueSnapshot.frontOfQueue / frontOfQuotaShares).
Traces: batch.fair-share-quota-weight-orders-job-dispatch.

=== Cause 3 of 3: job-queue priority does not order dispatch across two queues sharing one capacity-constrained compute environment ===
AWS: Smithy: CreateJobQueue.priority — "Job queues with a higher priority (or a higher integer value for the priority parameter) are evaluated first when associated with the same compute environment. Priority is determined in descending order. For example, a job queue with a priority value of 10 is given scheduling preference over a job queue with a priority value of 1." With two queues (priority 1 and priority 100) on one FARGATE compute environment whose ComputeResource.maxvCpus ("The maximum number of vCPUs that a compute environment can support") allows exactly one 1-vCPU job, the job on the priority-100 queue is dispatched ahead of an equally eligible job that has been waiting on the priority-1 queue.
Emulator: no job on either queue is ever observed being dispatched (never RUNNING, never terminal), so the across-queue preference never materialises and the check exhausts its budget (`Check timed out after 60.0s`).
Why: a client that separates urgent from background work by queue priority on shared capacity loses the guarantee entirely — nothing is dispatched, and when dispatch is implemented the priority ordering has no code path behind it.
Fix: the cross-queue scheduling loop for a shared compute environment — queues associated with the same compute environment must be evaluated in descending priority order when capacity frees up.
Traces: batch.job-queue-priority-scheduling-order-enforced.

## Evidence
Suite: 217/295 passed; service `batch` 201/251 (80%), 50 failed. The three failures below were listed under `### batch` and marked neither ALREADY FILED nor OWNED.

Cause 1 — batch.batch-consumable-resource-quantity-caps-concurrent-running-jobs
  Expected (AWS): with a REPLENISHABLE consumable resource of totalQuantity 1, at most one of two quantity-1 jobs is RUNNING at any observed instant and both jobs reach SUCCEEDED.
  Actual: no RUNNING observation and no terminal status ever recorded; the poll loop ran to the end of the run budget — `Error: Check timed out after 60.0s`.

Cause 2 — batch.fair-share-quota-weight-orders-job-dispatch
  Expected (AWS): with weights {busy: 10.0, idle: 0.1} and the queue's sole 1-vCPU Fargate slot held by a job under share "busy", a new job under share "idle" ranks ahead of a new job under "busy" in GetJobQueueSnapshot.frontOfQueue.
  Actual: the holding job is never observed RUNNING, so no share usage and no ordering is ever produced — `Error: Check timed out after 60.0s`.

Cause 3 — batch.job-queue-priority-scheduling-order-enforced
  Expected (AWS): on one FARGATE compute environment with maxvCpus=1 shared by a priority-1 and a priority-100 queue, the job submitted to the priority-100 queue is dispatched before the job already waiting on the priority-1 queue.
  Actual: neither job is ever dispatched (no RUNNING/terminal observation) — `Error: Check timed out after 60.0s`.

The 60s figure is this run's harness budget, not a requirement; the requirement is that the state transitions occur at all. Job ids, queue/CE names, subnet and role ids in these runs are per-run values.

## Steps to Reproduce
Preconditions for all three: an IAM Fargate task execution role, a MANAGED/FARGATE compute environment polled to VALID, ENABLED job queue(s) polled to VALID, and a registered Fargate job definition.

Cause 1: CreateConsumableResource(resourceType=REPLENISHABLE, totalQuantity=1). SubmitJob twice with consumableResourcePropertiesOverride requiring quantity 1 of that resource and a command that dwells (e.g. `sleep 45; exit 0`). Poll DescribeJobs on both jobs: neither is ever RUNNING and neither reaches SUCCEEDED, so the "at most one RUNNING at a time, both SUCCEEDED" invariant can never be evaluated.

Cause 2: CreateSchedulingPolicy with fairsharePolicy.shareDistribution = [{shareIdentifier: "busy", weightFactor: 10.0}, {shareIdentifier: "idle", weightFactor: 0.1}]; create a FARGATE compute environment with maxvCpus=1 and a job queue bound to that scheduling policy. SubmitJob a long-running job with shareIdentifier "busy" and wait for it to occupy the slot (it never does), then SubmitJob one job per share with equal schedulingPriorityOverride and call GetJobQueueSnapshot: no frontOfQueue ordering by share usage is ever observable.

Cause 3: Create one FARGATE compute environment with maxvCpus=1; create two job queues on it, priority=1 and priority=100. SubmitJob a 1-vCPU job to the low-priority queue, let it wait, then SubmitJob an identical job to the high-priority queue, then poll DescribeJobs for which job is dispatched first: neither is ever dispatched.
