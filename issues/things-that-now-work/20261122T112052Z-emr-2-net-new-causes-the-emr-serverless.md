Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.emr-serverless-failing-script-reports-failed-and-writes-no-output emr.parquet-written-by-one-spark-step-is-read-back-by-the-next
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.emr-serverless-failing-script-reports-failed-and-writes-no-output|_poll#44|Timeout|return last
Issue-Key: emr.parquet-written-by-one-spark-step-is-read-back-by-the-next|_poll#19|Timeout|time.sleep(interval_s)
Issue-Commit: ce4ee9e56b2f664f3ccf49966acace7716de13e8

# emr: 2 net-new causes — the EMR Serverless failing/control job-run pair and the two-step Parquet round trip never reach a verdict inside the replay budget

> _Note: this issue was identified against the code base at commit `ce4ee9e56b2f664f3ccf49966acace7716de13e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
Two net-new root causes in the `emr` suite. Both surface as the replay wall being reached with NO AWS assertion graded: the check's very first wait never satisfies its predicate, so the emulator is never asked the question the check exists to ask. Numbered below; each carries its own failing check id.

=== Cause 1 of 2: an EMR Serverless application + sparkSubmit job-run pair never settles, so neither the failing run's FAILED nor the control run's SUCCESS is ever observable ===

AWS:      Smithy (com.amazonaws.emrserverless): `ApplicationState` = CREATING | CREATED | STARTING | STARTED | STOPPING | STOPPED | TERMINATED, and `JobRunState` = SUBMITTED | PENDING | SCHEDULED | RUNNING | SUCCESS | FAILED | CANCELLING | CANCELLED | QUEUED. CreateApplication followed by StartApplication converges on STARTED, and a `sparkSubmit` run submitted with StartJobRun converges on one of the terminal JobRunStates, where GetJobRun reports `state`, a non-empty `stateDetails` and a non-zero `totalExecutionDurationSeconds` for a driver that really ran (`JobRun$totalExecutionDurationSeconds` is "the job run total execution duration in seconds"). Convergence timing is recorded behavior only — AWS does not model a span — but the states themselves are modelled, and every one of them must be REACHED for a client that polls to be able to grade a run.

Emulator: the check is cut off at the replay wall — `Error: Check timed out after 60.0s` — before any assertion on a job run is graded. The emulator's own transient spans are sub-second BY DESIGN (services/service-com-amazonaws-emrserverless-awstoledowebservice/src/state.rs: `LIFECYCLE_TRANSITION = 500ms`, `JOB_RUN_STAGE_TRANSITION = 750ms`), so the budget is not being spent on a span the emulator deliberately models; it is spent on a predicate that is still unmet at every probe. Two distinct contributors are visible in the checkout. (a) Because every lifecycle edge is non-zero but the clock is only materialised by the NEXT read, a caller polling at AWS-appropriate intervals pays a FULL interval per transient state: the application's leaves-CREATING poll (10s interval) and leaves-STARTING poll (10s interval) each burn one whole interval before the 500ms edge is reported, and each job-run poll (20s interval) burns one whole interval before the run's stages are reported — four unsettled first probes alone exceed the 60s wall. (b) The terminal JobRunState is explicitly NOT clock-owed: state.rs documents "a run that has not been executed yet stays RUNNING rather than being reported as finished", and the terminal state is produced only when the in-process Spark execution answers (src/state/spark_job_run.rs, `awaits_workload` / `perform_owed` / `WORKLOAD_FIRST_WAIT`). A run whose embedded PySpark execution does not answer inside the budget is therefore reported as RUNNING indefinitely, and the poll for a terminal state can never return. Hypothesis: the two real PySpark executions this check submits (a driver that builds a SparkSession, sums a 100-element RDD and then raises, plus a control driver that writes one part file) are what the budget is actually consumed by.

Why:      a client cannot grade an EMR Serverless run at all. Waiting for a terminal state is the only way to learn whether submitted work succeeded or failed; a run that stays non-terminal makes StartJobRun a fire-and-forget call with no observable outcome, so the whole failure-reporting contract (FAILED + stateDetails naming the failure + a non-zero totalExecutionDurationSeconds + no Spark `_SUCCESS` marker committed) is unreachable, and so is the positive control that proves the backend runs anything at all.

Fix:      the job-run lifecycle in the EMR Serverless state machine and the workload runner behind it — the execution handoff and its waits in src/state/spark_job_run.rs, and the clock-owed stage queue in src/state.rs that GetJobRun/ListJobRuns materialise. The settled state must become observable within a client's polling horizon, and a run whose workload cannot be executed must come to REST in a modelled terminal state rather than being left RUNNING.

Traces:   emr.emr-serverless-failing-script-reports-failed-and-writes-no-output. Handler path: services/service-com-amazonaws-emrserverless-awstoledowebservice/src/ops/get_application.rs (settled_mut), src/state.rs lines 195-212 (LIFECYCLE_TRANSITION, JOB_RUN_STAGE_TRANSITION), src/state.rs lines 352-370 (JOB_RUN_STAGES and the note that no terminal state is clock-owed), src/state/spark_job_run.rs lines 36-77 (when the workload runs, WORKLOAD_FIRST_WAIT / WORKLOAD_HOST_WAIT). Related but NOT covering this check (none carries its replay key): 20261116T101705Z-emr-serverless-spark-job-runs-never-execute-the.md, 20261030T093552Z-emr-serverless-job-run-lifecycle-diverges-runs-never.md, 20261024T013257Z-emr-serverless-job-runs-never-execute-the-submitted.md and 20261022T154846Z-emr-serverless-job-runs-are-recorded-but-never-executed.md all report runs that SETTLE on the wrong terminal state (FAILED instead of SUCCESS); this cause is a run that never settles at all and is therefore never graded.

=== Cause 2 of 2: a spark-submit step on an EMR-on-EC2 cluster never reaches a terminal StepState, so DescribeStep can never be polled to a verdict ===

AWS:      Smithy (com.amazonaws.emr): `StepState` = PENDING | CANCEL_PENDING | RUNNING | COMPLETED | CANCELLED | FAILED | INTERRUPTED; `StepStatus` carries State, StateChangeReason, FailureDetails and Timeline, and DescribeStep "Provides more detail about the cluster step." A step submitted with RunJobFlow leaves PENDING, runs, and comes to rest in COMPLETED or FAILED, which is what DescribeStep then reports. At StepConcurrencyLevel=1 steps are strictly sequential, so the last step reaching a terminal state implies the first already has. Convergence timing is recorded behavior only; the state transition itself is modelled.

Emulator: the check is cut off at the replay wall — `Error: Check timed out after 60.0s`. Its ONLY wait is a DescribeStep poll for the second step to reach a terminal StepState (1800s deadline, 20s interval); everything before it (STS GetCallerIdentity, bucket and script upload, IAM role + instance profile, RunJobFlow, ListSteps) is a single call each. So the budget is consumed inside that poll: DescribeStep keeps answering a non-terminal State (PENDING/RUNNING) for a `command-runner.jar spark-submit` step and never reports COMPLETED or FAILED. The step runner is reached from the DescribeStep handler on every call (services/service-com-amazonaws-emr-elasticmapreduce/src/ops/describe_step.rs invokes run_due_bootstrap_actions, run_due_step_applications, resolve_due_step_commands and run_due_steps), so the transition is not blocked on a caller — it is simply never produced for this step shape.

Why:      inter-step data flow through S3 is ungradeable, and worse, a client that waits for a step cannot proceed. Every EMR workflow is "submit a step, wait for its terminal state, read its output": a step that never terminates hangs the caller until its own deadline and never lets it distinguish a Parquet dataset that was written and read back from one that was never written. Spark's `_SUCCESS` marker, the part files and the step's FailureDetails are all unreachable behind that wait.

Fix:      the EMR cluster step runner and the step-application execution behind it in the EMR state machine (crate::state::run_due_steps / run_due_step_applications / resolve_due_step_commands in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs), as reached from the DescribeStep/ListSteps handlers. A step whose submitted application cannot be executed must still come to rest in a modelled terminal StepState with StateChangeReason/FailureDetails rather than being left non-terminal forever.

Traces:   emr.parquet-written-by-one-spark-step-is-read-back-by-the-next. Handler path: services/service-com-amazonaws-emr-elasticmapreduce/src/ops/describe_step.rs lines 160-224 (the four clock-materialisation calls and run_due_steps). Related but NOT covering this check (none carries its replay key): 20261119T185618Z-emr-6-net-new-root-causes-in-the.md and 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md describe steps that reach a WRONG terminal state or execute no command; this cause is a step that reaches no terminal state at all.

## Evidence
Replay output, verbatim, both under the `### emr` heading and neither marked ALREADY FILED nor OWNED:

  #### [mismatch] emr.emr-serverless-failing-script-reports-failed-and-writes-no-output
  Error: Check timed out after 60.0s

  #### [mismatch] emr.parquet-written-by-one-spark-step-is-read-back-by-the-next
  Error: Check timed out after 60.0s

Cause 1 — expected vs actual:
  Expected: GetApplication converges on `state` = "STARTED"; GetJobRun on the failing run converges on `state` = "FAILED" with a non-empty `stateDetails` and `totalExecutionDurationSeconds` > 0; GetJobRun on the control run converges on `state` = "SUCCESS"; ListJobRuns reports the same two states; HeadObject on <failPrefix>/_SUCCESS answers 404 and the failing prefix lists no keys.
  Actual: no assertion is ever graded. The lifecycle/job-run polls (10s, 10s, 20s, 20s intervals) are still unsatisfied when the 60s wall is hit, so no `state` value was ever accepted as terminal. The emulator's modelled transient spans are 500ms (LIFECYCLE_TRANSITION) and 750ms per stage (JOB_RUN_STAGE_TRANSITION), and no terminal JobRunState is clock-owed at all (src/state.rs: "a run that has not been executed yet stays RUNNING rather than being reported as finished").

Cause 2 — expected vs actual:
  Expected: DescribeStep(ClusterId=<j-…>, StepId=<s-…>) eventually answers Step.Status.State in {COMPLETED, CANCELLED, FAILED, INTERRUPTED} — here COMPLETED for both the write and the read step — after which the Parquet row count, SUM, column types and probed row are read out of S3.
  Actual: the DescribeStep poll (1800s deadline, 20s interval) never sees a terminal Step.Status.State; the check is cut off at the 60s wall with the step still non-terminal. No Step.Status.State assertion, no StateChangeReason/FailureDetails assertion and no S3 read is ever graded.

Note on values: cluster ids (j-…), step ids (s-…), application ids, job-run ids, bucket names, nonces and role names in these checks are per-run and are NOT part of the requirement; the requirement is that each of the modelled states is REACHED and reported.

## Steps to Reproduce
Cause 1 (emr.emr-serverless-failing-script-reports-failed-and-writes-no-output):
1. Create an S3 bucket; upload two PySpark entry points — one that builds a SparkSession, does real RDD work, prints, then raises RuntimeError; one that writes a single-partition text file containing a nonce.
2. Create an IAM execution role trusted by emr-serverless.amazonaws.com.
3. emr-serverless CreateApplication(type="SPARK", releaseLabel="emr-6.15.0", autoStartConfiguration.enabled=true, autoStopConfiguration={enabled:true, idleTimeoutMinutes:1}).
4. Poll GetApplication until `application.state` leaves CREATING, StartApplication if needed, then poll until it leaves STARTING. Assert it is STARTED.
5. StartJobRun with jobDriver.sparkSubmit.entryPoint = s3://<bucket>/<failing script>, then poll GetJobRun until `state` is in {SUCCESS, FAILED, CANCELLED}.
6. Observe: the poll never terminates — the run does not reach a terminal state inside the replay budget, so the FAILED / stateDetails / totalExecutionDurationSeconds assertions and the "_SUCCESS marker absent" assertions are never reached. The same holds for the control run and for ListJobRuns.

Cause 2 (emr.parquet-written-by-one-spark-step-is-read-back-by-the-next):
1. Create an S3 bucket and upload one PySpark script with a write mode and a read mode.
2. Create the EMR service role and the EC2 instance profile (EMR_DefaultRole / EMR_EC2_DefaultRole do not exist in a fresh account).
3. RunJobFlow with ReleaseLabel="emr-6.15.0", Applications=[Hadoop, Spark], one MASTER + one CORE m5.xlarge, KeepJobFlowAliveWhenNoSteps=true, StepConcurrencyLevel=1 and two `command-runner.jar` steps: `spark-submit --deploy-mode client s3://<bucket>/job/parquet_roundtrip.py write …` then the same script with `read`.
4. ListSteps — both steps are reported (this part passes).
5. Poll DescribeStep on the SECOND step id for Step.Status.State in {COMPLETED, CANCELLED, FAILED, INTERRUPTED}.
6. Observe: DescribeStep keeps answering a non-terminal State and the poll never completes, so neither step's terminal state nor any S3 round-trip assertion is ever graded.
