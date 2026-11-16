Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.emr-serverless-spark-job-run-reaches-success
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.emr-serverless-spark-job-run-reaches-success|run#26|AssertionError|assert final == "SUCCESS", (
Issue-Commit: e65b11926418f2246d75a3bf5dec882b7ebf85de

# EMR Serverless Spark job runs never execute the submitted workload: GetJobRun settles FAILED with an emulator-authored stateDetails instead of SUCCESS, and no output is written

> _Note: this issue was identified against the code base at commit `e65b11926418f2246d75a3bf5dec882b7ebf85de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartJobRun

## Description
Cause 1 of 1: a SPARK job run submitted to a STARTED EMR Serverless application is recorded but never executed, so it settles in terminal FAILED and the submitted entry point never runs.

Failing check: emr.emr-serverless-spark-job-run-reaches-success

AWS: EMR Serverless StartJobRun submits a unit of work to a SPARK application; the run then walks the modeled lifecycle and a successful Spark submit settles in SUCCESS. Smithy (com.amazonaws.emrserverless): JobRunState enum = SUBMITTED | PENDING | SCHEDULED | RUNNING | SUCCESS | FAILED | CANCELLING | CANCELLED | QUEUED; JobRun.state and JobRun.stateDetails are both @required, stateDetails is String256 (@length min 1 max 256, pattern ".*\\S.*"); JobRun.totalExecutionDurationSeconds is documented as "The job run total execution duration in seconds. This field is only available for job runs in a COMPLETED, FAILED, or CANCELLED state"; JobRun.startedAt is "The date and time when the job moved to the RUNNING state" and JobRun.endedAt "when the job was terminated". The run's jobDriver.sparkSubmit.entryPoint (an s3:// PySpark script) is executed by a real Spark driver and executors under the executionRole, so a driver that calls saveAsTextFile(out_uri) leaves part-* objects under that S3 prefix. A run that AWS does execute and that exits 0 reports state SUCCESS with a non-zero totalExecutionDurationSeconds; a FAILED state is reserved for a workload AWS actually ran and that actually failed.

Emulator: GetJobRun(applicationId, jobRunId) converges on state 'FAILED' with stateDetails 'This emulator records job runs but executes no Spark or Hive workload, so the job run ended without running.' — an emulator-authored sentence that is not any AWS state-details text. The run never passes through RUNNING, nothing under the entry point is executed, and no part-* object appears under the job's output prefix in S3. The suite's assertion fires at check line 266: "the Spark job run must reach SUCCESS, got 'FAILED' (stateDetails: 'This emulator records job runs but executes no Spark or Hive workload, so the job run ended without running.')". The application itself is created and reaches STARTED normally, and StartJobRun is accepted and returns a jobRunId — the divergence is entirely in the run's execution and terminal state.

Why: a client that submits a Spark job and waits for a terminal state can never observe success. Every success path built on EMR Serverless is unreachable: the submitted script's side effects (objects written to S3 through Spark's own writer) never happen, totalExecutionDurationSeconds stays 0/absent, and the echoed jobDriver/executionRole assertions downstream of the state check are never reached. Worse, the failure is reported with the same FAILED state AWS uses for a genuinely failing workload, so error handling and retry logic cannot distinguish "my job is broken" from "nothing ran" — a CI gate written against AWS inverts its verdict against the emulator.

Fix: the EMR Serverless job-run execution path and its state machine — StartJobRun's handler and the job-run state transitions backing GetJobRun/ListJobRuns in the emrserverless crate (services/service-com-amazonaws-emrserverless-awstoledowebservice). A submitted sparkSubmit run must advance SUBMITTED/PENDING -> SCHEDULED -> RUNNING -> SUCCESS when the entry point completes successfully, set startedAt/endedAt and a non-zero totalExecutionDurationSeconds, carry an AWS-shaped stateDetails (never an emulator self-description), and the executed driver's S3 writes must land under the requested output URI. Reserve FAILED for a workload that was executed and did fail.

Related open issues that describe the same family but do not carry this check's replay key: 20261022T154846Z-emr-serverless-job-runs-are-recorded-but-never-executed.md, 20261024T013257Z-emr-serverless-job-runs-never-execute-the-submitted.md, 20261030T093552Z-emr-serverless-job-run-lifecycle-diverges-runs-never.md, 20261113T102950Z-athena-emr-serverless-glue-s3-an-emr-serverless.md. None of them is marked as owning emr.emr-serverless-spark-job-run-reaches-success, so it is filed here; a fixer should close them together if one change covers all.

Traces: emr.emr-serverless-spark-job-run-reaches-success (assertion at check line 266, after polling GetJobRun for up to 900s).

## Evidence
Expected (AWS): GetJobRun(applicationId=<app>, jobRunId=<run>)["jobRun"]["state"] == "SUCCESS" for a sparkSubmit run whose entry point exits 0; jobRun.totalExecutionDurationSeconds > 0; jobRun.jobDriver.sparkSubmit.entryPoint echoes the submitted s3://<bucket>/<script-key>; jobRun.executionRole echoes the submitted role ARN; jobRun.stateDetails is AWS's own state-details text; S3 ListObjectsV2(Bucket=<bucket>, Prefix="<out-prefix>/part-") returns the part file(s) Spark's writer produced.

Actual (emulator): the poll converges on state 'FAILED' with stateDetails 'This emulator records job runs but executes no Spark or Hive workload, so the job run ended without running.' The check fails with:
  AssertionError: the Spark job run must reach SUCCESS, got 'FAILED' (stateDetails: 'This emulator records job runs but executes no Spark or Hive workload, so the job run ended without running.')
The run never reports RUNNING, totalExecutionDurationSeconds is not positive, and no part-* object exists under the output prefix. (Bucket names, role ARNs, application/job-run ids and the nonce are per-run values, not requirements; the requirement is the terminal state, the lifecycle it passes through, and the executed side effects.)

## Steps to Reproduce
1. emr-serverless CreateApplication(name=<unique>, releaseLabel="emr-6.15.0", type="SPARK", maximumCapacity={cpu:"8 vCPU", memory:"32 GB"}, autoStartConfiguration={enabled:true}, autoStopConfiguration={enabled:true, idleTimeoutMinutes:1}).
2. Poll GetApplication until state leaves CREATING; StartApplication if needed and poll to STARTED.
3. Put a PySpark entry point in S3 that writes argv[1] to argv[2] via spark.sparkContext.parallelize([...],1).saveAsTextFile(out_uri); create an execution role trusted by emr-serverless.amazonaws.com and scoped to the bucket.
4. StartJobRun(applicationId=<app>, executionRoleArn=<role>, jobDriver={sparkSubmit:{entryPoint:"s3://<bucket>/job/write_nonce.py", entryPointArguments:[<nonce>, "s3://<bucket>/job-output"], sparkSubmitParameters:"--conf spark.executor.cores=1 --conf spark.executor.memory=2g --conf spark.driver.cores=1 --conf spark.driver.memory=2g --conf spark.executor.instances=1"}}).
5. Poll GetJobRun(applicationId, jobRunId) until state is in {SUCCESS, FAILED, CANCELLED}.
6. Observe state == FAILED with the emulator-authored stateDetails above, instead of SUCCESS; then ListObjectsV2(Bucket=<bucket>, Prefix="job-output/part-") and observe no objects.
