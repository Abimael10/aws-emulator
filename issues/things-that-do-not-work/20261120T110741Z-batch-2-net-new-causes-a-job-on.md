Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.compute-environment-disabled-withholds-placement-until-reenabled batch.job-dependency-releases-on-success-and-blocks-on-parent-failure
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.compute-environment-disabled-withholds-placement-until-reenabled|run#17|Timeout|_assert_withheld(
Issue-Key: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure|_trace_job_to_terminal#18|Timeout|return trace, job
Issue-Commit: 7098827f7d7522d4a6107e35de6129ed6df1e0dd

# batch: 2 net-new causes — a job on an ENABLED MANAGED/FARGATE compute environment is never placed (CE state never gates placement), and dependsOn never holds/releases a dependent, so both lifecycle checks never reach a verdict

> _Note: this issue was identified against the code base at commit `7098827f7d7522d4a6107e35de6129ed6df1e0dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Two net-new behavioral causes in the Batch job lifecycle, both grounded in failing checks that are NOT keyed to any open issue. Both surface as the check never reaching a verdict ("Check timed out after 60.0s"): the emulator answers the control-plane calls, accepts SubmitJob, and then never advances the job through the modelled state machine, so every poll loop in the check runs to its deadline. The neighbouring open issues 20261107T210820Z-batch-the-job-data-plane-never-executes-a.md and 20261119T120645Z-batch-a-submitted-job-never-reaches-running-or.md describe adjacent symptoms (a placed job going SUBMITTED->FAILED with an emulator-authored reason; a job never reaching RUNNING); neither carries these two checks' replay keys, and neither covers compute-environment-state gating of placement or dependsOn gating, which are the two behaviors asserted here.

Cause 1 of 2: a compute environment's state does not gate placement, and a job on an ENABLED MANAGED/FARGATE compute environment is never placed.
AWS: Smithy UpdateComputeEnvironmentRequest.state (CEState = ENABLED|DISABLED) — "If the state is ENABLED, then the Batch scheduler can attempt to place jobs from an associated job queue on the compute resources within the environment"; "If the state is DISABLED, then the Batch scheduler doesn't attempt to place jobs within the environment. Jobs in a STARTING or RUNNING state continue to progress normally." So a job submitted to an ENABLED queue backed by an ENABLED MANAGED/FARGATE CE is placed: JobDetail.status (required enum JobStatus) advances SUBMITTED -> ... -> STARTING/RUNNING, at which point JobDetail.container.taskArn names the real ECS task, and the job ends SUCCEEDED with JobDetail.startedAt and stoppedAt set (JobDetail.startedAt doc: "the Unix timestamp (in milliseconds) for when the job transitioned from the STARTING state to the RUNNING state"). A job whose only compute environment is DISABLED is withheld in RUNNABLE with JobDetail.attempts empty and startedAt absent, and re-ENABLING that compute environment releases the ALREADY-submitted job — no resubmission — so it then records exactly one attempt carrying container.taskArn, a startedAt, and reaches SUCCEEDED. State and status are independent: a DISABLED CE still reports status VALID.
Emulator: the check never gets past its positive control. It creates two MANAGED/FARGATE CEs (one ENABLED, one updated to DISABLED) and an ENABLED queue over each, submits the withheld job, then polls DescribeJobs on the control job for any of STARTING/RUNNING/SUCCEEDED/FAILED. No such status (and no container.taskArn) ever appears, so the check consumes its whole window and is cut off: "Error: Check timed out after 60.0s". The DISABLED-withholding leg, the re-enable leg and the released job's single attempt are therefore all unobservable.
Why: a client that submits a job and waits for it to be placed or to finish hangs forever against the emulator instead of getting a terminal status; schedulers that drain a queue by disabling a compute environment, or that re-enable one to release held work, get no signal at all — and because nothing is ever placed, a passing read cannot be distinguished from a genuinely withheld job.
Fix: the job scheduler/placement path of the Batch job state machine (the component that moves a SUBMITTED job through PENDING/RUNNABLE to STARTING/RUNNING and a terminal status, records attempts/container.taskArn/startedAt/stoppedAt) plus its consultation of the compute environment's state when deciding placement.
Traces: batch.compute-environment-disabled-withholds-placement-until-reenabled.

Cause 2 of 2: SubmitJob.dependsOn is never honoured — a dependent is neither held in PENDING nor released by its parent, and a dependent of a FAILED parent is never terminated with a dependency statusReason.
AWS: Smithy SubmitJobRequest.dependsOn — "A list of dependencies for the job. A job can depend upon a maximum of 20 jobs"; JobDetail.dependsOn echoes those job IDs. A job with an unsatisfied dependsOn leaves SUBMITTED and is held in PENDING (JobDetail.status enum), with startedAt absent and no attempts. When the parent reaches SUCCEEDED, the dependent is released and runs to SUCCEEDED with container.exitCode 0 and a startedAt no earlier than the parent's stoppedAt. When the parent reaches FAILED, the dependent is never dispatched: it reaches FAILED with attempts empty, no startedAt, no container.exitCode, and a non-empty JobDetail.statusReason ("a short, human-readable string to provide more details for the current status of the job") naming the dependency failure. Observed status sequences must be monotonic through SUBMITTED/PENDING/RUNNABLE/STARTING/RUNNING/terminal — the model's note on JobDetail.status ("If your jobs don't progress to STARTING ...") presumes that progression happens at all.
Emulator: the check registers two FARGATE job definitions (exit 0 and exit 9), submits two parents and two dependents with dependsOn, and polls each dependent to leave SUBMITTED and each parent to a terminal status. Neither transition is ever observed and the check is cut off: "Error: Check timed out after 60.0s". So dependsOn gating (PENDING hold, release on parent success, block on parent failure with a statusReason) has no observable behavior.
Why: fan-out/fan-in pipelines built on dependsOn cannot be emulated: a client cannot tell a held dependent from a lost one, a released child never runs, and a child whose parent failed never terminates — so the orchestration loop blocks rather than failing fast with a reason.
Fix: the SubmitJob dependency handling in the job scheduler/state machine (hold a dependent in PENDING until its dependencies are terminal, release on SUCCEEDED, terminate as FAILED with a dependency statusReason and no attempts on parent failure), and the same placement path named in Cause 1 that must carry the parents to a terminal status in the first place.
Traces: batch.job-dependency-releases-on-success-and-blocks-on-parent-failure.

## Evidence
Check batch.compute-environment-disabled-withholds-placement-until-reenabled — expected: DescribeJobs on the control job submitted to the ENABLED MANAGED/FARGATE queue reports status STARTING/RUNNING/SUCCEEDED with container.taskArn set; the twin job on the DISABLED CE stays RUNNABLE with no attempts and no startedAt; after UpdateComputeEnvironment(state=ENABLED) that same job reaches SUCCEEDED with exactly 1 attempt, attempt.container.taskArn set, an integer startedAt and startedAt >= control.stoppedAt. Actual: no job status ever leaves its initial value and no taskArn is ever reported; the check is terminated — "Error: Check timed out after 60.0s". Nothing in the DISABLED/re-ENABLED sequence is reached.

Check batch.job-dependency-releases-on-success-and-blocks-on-parent-failure — expected: each dependent leaves SUBMITTED into PENDING with dependsOn echoing the parent jobId and no startedAt; the parent of the exit-0 definition reaches SUCCEEDED and its dependent is released to SUCCEEDED with container.exitCode 0 and startedAt >= parent.stoppedAt; the parent of the exit-9 definition reaches FAILED and its dependent reaches FAILED with attempts == [], no startedAt, no container.exitCode and a non-empty statusReason. Actual: neither dependent leaves SUBMITTED and neither parent reaches a terminal status; the check is terminated — "Error: Check timed out after 60.0s".

In both cases the emulator accepted every control-plane call (CreateComputeEnvironment, CreateJobQueue, RegisterJobDefinition) and accepted SubmitJob, so the divergence is entirely in the job lifecycle/placement path, not in request validation. The 60 s cut-off is the harness cap; the check's own poll windows (up to 600 s for placement, 900 s for a terminal status) were never satisfied.

## Steps to Reproduce
Cause 1:
1. iam: resolve/create the Batch service-linked role and a Fargate task execution role.
2. batch CreateComputeEnvironment: two MANAGED/FARGATE CEs (type=MANAGED, state=ENABLED, computeResources.type=FARGATE, maxvCpus=4, real subnets + security group); poll DescribeComputeEnvironments until status=VALID. UpdateComputeEnvironment(state=DISABLED) on the second and poll until state=DISABLED, status=VALID.
3. batch CreateJobQueue: one ENABLED queue (priority 10) over each CE; poll to status=VALID.
4. batch RegisterJobDefinition: type=container, platformCapabilities=[FARGATE], a busybox image with command exit 0, VCPU 0.25 / MEMORY 512.
5. batch SubmitJob to the queue over the DISABLED CE, then SubmitJob to the queue over the ENABLED CE.
6. Poll DescribeJobs on the second job for status in {STARTING, RUNNING, SUCCEEDED, FAILED} and for container.taskArn. Observe it never advances (the poll runs to its deadline) — AWS places it and reports a taskArn.
7. Then UpdateComputeEnvironment(state=ENABLED) on the first CE and poll its already-submitted job: AWS carries it to SUCCEEDED with one attempt and a startedAt; the emulator reports no progress.

Cause 2:
1. Stand up a MANAGED/FARGATE CE (maxvCpus=8) and an ENABLED job queue over it, both polled to VALID.
2. batch RegisterJobDefinition twice: busybox commands `sleep 20; exit 0` and `sleep 20; exit 9`, platformCapabilities=[FARGATE].
3. batch SubmitJob: parentA (exit 0), childA with dependsOn=[{jobId: parentA}], parentB (exit 9), childB with dependsOn=[{jobId: parentB}].
4. Poll DescribeJobs on childA/childB until status != SUBMITTED: AWS reports PENDING with dependsOn echoed and no startedAt; the emulator never leaves SUBMITTED.
5. Poll parentA/parentB to a terminal status: AWS reports SUCCEEDED/FAILED; the emulator never reaches one, so the release of childA and the dependency-failure termination of childB (FAILED, no attempts, no startedAt, no exitCode, non-empty statusReason) are never observable.
