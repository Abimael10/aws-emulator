Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.deploy-emr-cluster-cfn-then-step-executes emr.s3-dist-cp-group-by-aggregates-through-a-real-mapreduce-job emr.security-configuration-sse-kms-applies-to-emrfs-writes emr.parquet-written-by-one-spark-step-is-read-back-by-the-next emr.spark-step-partitioned-write-lands-hive-layout-in-s3 emr.spark-partitioned-input-pruned-then-written-back-partitioned emr.spark-defaults-classification-governs-the-shuffle-partition-count emr.emr-step-args-are-passed-verbatim-to-the-application emr.persistent-app-ui-creation-then-describe-eventually-ready emr.persistent-app-ui-describe-survives-cluster-termination-window
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.deploy-emr-cluster-cfn-then-step-executes|run#13|AssertionError|assert final_step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.s3-dist-cp-group-by-aggregates-through-a-real-mapreduce-job|run#18|AssertionError|assert step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.security-configuration-sse-kms-applies-to-emrfs-writes|run#27|AssertionError|assert final["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.parquet-written-by-one-spark-step-is-read-back-by-the-next|run#21|AssertionError|assert write_step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.spark-step-partitioned-write-lands-hive-layout-in-s3|run#23|AssertionError|assert described["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.spark-partitioned-input-pruned-then-written-back-partitioned|run#21|AssertionError|assert write_step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.spark-defaults-classification-governs-the-shuffle-partition-count|run#24|AssertionError|assert step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.emr-step-args-are-passed-verbatim-to-the-application|run#17|AssertionError|assert final == "COMPLETED", (
Issue-Key: emr.persistent-app-ui-creation-then-describe-eventually-ready|_run_trivial_spark_step#4|AssertionError|assert final_state == "COMPLETED", f"prerequisite SparkPi step {step_id} ended {final_state!r}, not COMPLETED"
Issue-Key: emr.persistent-app-ui-describe-survives-cluster-termination-window|run#7|AssertionError|assert step_state == "COMPLETED", f"prerequisite SparkPi step did not complete: {step_state}"
Issue-Commit: 0da99206c40b9a0852cdbaa2ba3793f12a97497b

# emr: 6 net-new root causes in the step data plane — submitted steps either run no application at all or die inside an incomplete Spark/Python runtime, so no step reaches COMPLETED

> _Note: this issue was identified against the code base at commit `0da99206c40b9a0852cdbaa2ba3793f12a97497b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddJobFlowSteps

## Description
This issue bundles 6 net-new root causes in the EMR crate's step data plane (service token `emr`). They all end with the same client-visible symptom — DescribeStep settles the step in `FAILED` instead of `COMPLETED` and the step's side effects are missing — but they fail at six DIFFERENT places, each of which has to be closed on its own before any of the ten checks below passes. The control plane is fine in every cause: RunJobFlow/AddJobFlowSteps accept the step, DescribeStep echoes `Config.Args` verbatim, and the step walks PENDING -> RUNNING -> terminal.

In every cause the requirement is the general behavior described, not the recorded bucket names, cluster ids (`j-...`), step ids (`s-...`) or nonces, which are per-run values.

Cause 1 of 6: a step whose HadoopJarStep names command-runner.jar or a Hadoop jar is never executed — the emulator settles it FAILED with a reason it authors itself.
AWS: Smithy (com.amazonaws.emr#ElasticMapReduce): AddJobFlowSteps/RunJobFlow take HadoopJarStepConfig{Jar, Args, MainClass, Properties}; DescribeStep returns StepStatus{State: StepState = PENDING|CANCEL_PENDING|RUNNING|COMPLETED|CANCELLED|FAILED|INTERRUPTED, StateChangeReason, FailureDetails{Reason, Message, LogFile}}. The model does not dictate message text, so the behavior here is recorded behavior only: on AWS a step whose command succeeds reaches `COMPLETED`, `FailureDetails` is absent, and the command's side effects (the s3-dist-cp copy/aggregate, the EMRFS write under the SSE-KMS security configuration, the CFN-submitted step's output) are present in S3 afterwards.
Emulator: State `FAILED` with StateChangeReason.Message and FailureDetails.Reason = `This emulator runs the step lifecycle but executes no Hadoop jar, so the step ended without running its application.` and FailureDetails.Message = `No application was started for this step: this emulator holds the cluster's step state and runs no Hadoop, Spark or MapReduce runtime, so the step's own side effects did not happen.` Both sentences are emulator-authored text that real EMR never emits in any field; no step output is written.
Why: every client that submits work to EMR (the CLI, CloudFormation's AWS::EMR::Step, Airflow/Step Functions EMR integrations) polls DescribeStep for COMPLETED and then reads the step's output. Against this emulator the happy path is unreachable: the poll always ends FAILED, the reason string is unknown to any client that classifies failures, and the output object is absent, so pipelines cannot be exercised at all.
Fix: the step execution engine behind AddJobFlowSteps/RunJobFlow in the EMR crate (the component that advances a step and must run its jar/command), and the state it writes into StepStatus.StateChangeReason/FailureDetails. Steps whose command succeeds must settle COMPLETED with no FailureDetails; the emulator's own explanation text must never be served as EMR's failure reason.
Traces: emr.deploy-emr-cluster-cfn-then-step-executes (step on a CFN-deployed cluster), emr.s3-dist-cp-group-by-aggregates-through-a-real-mapreduce-job (s3-dist-cp step), emr.security-configuration-sse-kms-applies-to-emrfs-writes (EMRFS write step under an SSE-KMS security configuration).

Cause 2 of 6: PySpark jobs abort because the node's pyspark package has no `pyspark.sql` submodules.
AWS: recorded behavior only — a cluster launched with `ReleaseLabel emr-6.15.0` and the Spark application ships a complete PySpark distribution, so `from pyspark.sql.types import ...` / `from pyspark.sql.functions import ...` import successfully and the spark-submit step reaches COMPLETED, writing its parquet/partitioned output to S3.
Emulator: the step settles FAILED with FailureDetails.Reason `The application the step submitted exited with a non-zero status.` and FailureDetails.Message `ModuleNotFoundError: No module named 'pyspark.sql.types'` (parquet check) and `ModuleNotFoundError: No module named 'pyspark.sql.functions'` (partitioned-write check). So a Spark runtime IS reached — `pyspark.sql` itself resolves — but its submodules do not exist.
Why: the two most common PySpark imports in production jobs fail, so no DataFrame job can run; a client sees a Spark step fail with a Python import error that no real EMR AMI produces, and the written-then-read-back data contract cannot be observed.
Fix: the Spark/Python runtime the step executor exposes to a spark-submit step in the EMR crate — `pyspark.sql.types` and `pyspark.sql.functions` must be importable (the module surface must match the pyspark that `pyspark.sql` already comes from).
Traces: emr.parquet-written-by-one-spark-step-is-read-back-by-the-next, emr.spark-step-partitioned-write-lands-hive-layout-in-s3.

Cause 3 of 6: the Spark SQL expression engine cannot resolve `concat(...)`/`cast(...)` inside selectExpr, so a partitioned write aborts with AnalysisException.
AWS: recorded behavior only — on emr-6.15.0 Spark SQL resolves the built-ins `concat` and `cast`, so `selectExpr("concat('<prefix>-', cast(id as string)) as <col>")` produces a column and the partitioned WRITE step reaches COMPLETED, landing a Hive-style partition layout in S3.
Emulator: the step settles FAILED with FailureDetails.Reason `The application the step submitted exited with a non-zero status.` and FailureDetails.Message `AnalysisException: cannot resolve the column concat('<literal-prefix>-', cast(id as string))`. The literal prefix in the recording is a per-run nonce; the requirement is that a `concat`/`cast` expression over an existing column resolves at all.
Why: a job that builds a partition key from a literal and a cast column — the normal way partitioned writes are produced — is rejected as if the user's SQL were invalid, so a client debugging against the emulator chases a non-existent query defect.
Fix: the Spark SQL expression resolver in the step runtime's query engine (the component that resolves selectExpr strings) — the standard function catalog must include `concat` and the `cast(<col> as <type>)` form.
Traces: emr.spark-partitioned-input-pruned-then-written-back-partitioned.

Cause 4 of 6: a PySpark job that reads `spark.conf` and performs a groupBy shuffle exits non-zero with no diagnosis.
AWS: recorded behavior only — a cluster whose `spark-defaults` configuration classification sets `spark.sql.shuffle.partitions` runs a job that calls `spark.conf.get("spark.sql.shuffle.partitions")`, `spark.range(...).selectExpr(...)`, `groupBy().sum()` and `saveAsTextFile`, reaches COMPLETED, and the aggregate's partition count equals the configured value.
Emulator: the step settles FAILED with StateChangeReason.Message `The application the step submitted exited with a non-zero status.` and no further detail — no FailureDetails.Message naming what failed, so the client gets a bare non-zero exit.
Why: the configuration classification plane is unobservable (a client cannot verify that a `spark-defaults` setting reached the engine), and the bare exit status gives an operator nothing to act on, where real EMR's FailureDetails/stderr identifies the failing stage.
Fix: the step runtime's PySpark support for `SparkSession.conf.get`, `spark.range(...).selectExpr`, `groupBy().sum()` and `saveAsTextFile`, plus the failure-reporting path that must populate FailureDetails.Message for a non-zero application exit instead of leaving only the generic sentence.
Traces: emr.spark-defaults-classification-governs-the-shuffle-partition-count.

Cause 5 of 6: a command-runner.jar `bash -c` step that invokes `python3` exits status 1.
AWS: recorded behavior only — a command-runner.jar step with Args `["bash", "-c", <script>, "<name>", <args...>]` runs the script on the primary node; the script copies a helper from S3 with the AWS CLI, runs `python3 /tmp/dump_argv.py "$@"` and uploads the result, and the step reaches COMPLETED. (DescribeStep's verbatim echo of `Config.Args` already passes in this check, so only the execution leg is at fault.)
Emulator: the step settles FAILED with FailureDetails.Reason `The command the step submitted exited with a non-zero status.` and FailureDetails.Message `The command the step submitted exited with status 1.` The script's output object is never written, so the argv-delivery contract (EMR hands the step's Args to the process byte-for-byte, dropping only the zero-length argument) cannot be observed.
Why: shell steps that shell out to python3 — the most common command-runner.jar pattern — always fail, and the status-1 sentence names no failing command, so a client cannot tell whether its script or the platform is wrong.
Fix: the command-runner.jar execution path in the EMR crate's step runtime (python3 invocation and argv delivery to the child process), and its failure reporting, which must identify the command that exited non-zero.
Traces: emr.emr-step-args-are-passed-verbatim-to-the-application.

Cause 6 of 6: a trivial SparkPi spark-submit step ends FAILED, so the persistent-app-UI plane behind it is never reached.
AWS: recorded behavior only — a trivial SparkPi step on a running cluster reaches COMPLETED, which is what makes a Spark history (persistent app UI) available for CreatePersistentAppUI/DescribePersistentAppUI afterwards.
Emulator: the prerequisite step settles `FAILED` in both checks (`prerequisite SparkPi step s-... ended 'FAILED', not COMPLETED`), so neither the creation-then-describe-becomes-ready path nor the describe-survives-cluster-termination window is exercised at all.
Why: a client cannot obtain a persistent application UI for a completed Spark application, because no Spark application ever completes; two whole persistent-app-UI behaviors are unobservable behind this one failure.
Fix: the same step execution engine as Cause 1/Cause 4 for spark-submit steps, and whatever records a completed Spark application for the persistent-app-UI store.
Traces: emr.persistent-app-ui-creation-then-describe-eventually-ready, emr.persistent-app-ui-describe-survives-cluster-termination-window.

Related but NOT covering these checks: open issues 20261118T213929Z-emr-cluster-steps-never-execute-their-command-every.md, 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md and 20261108T093128Z-ec2-emr-emr-job-flow-steps-always-end-failed-a.md describe the same family (steps that never run their command / spark-submit steps that never COMPLETE) for other scenarios, but none of them carries a replay key for any of the ten checks listed above, and none of them covers the four distinct in-runtime failures (Causes 2-5) that this emulator now produces after a Spark/Python runtime was partially stood up.

## Evidence
Behavioral suite replay against the booted emulator (206/283 passed, 62 mismatched, 15 errored, bootstrap_signal false; service `emr` 170/220). All ten checks below are net-new: not marked ALREADY FILED and not OWNED by any open issue.

Cause 1 — DescribeStep State `FAILED`, StateChangeReason.Message and FailureDetails.Reason = "This emulator runs the step lifecycle but executes no Hadoop jar, so the step ended without running its application.", FailureDetails.Message = "No application was started for this step: this emulator holds the cluster's step state and runs no Hadoop, Spark or MapReduce runtime, so the step's own side effects did not happen." Expected: State `COMPLETED`, no FailureDetails.
  - emr.deploy-emr-cluster-cfn-then-step-executes: "step submitted to CFN-deployed cluster did not complete: {'State': 'FAILED', ...}"
  - emr.s3-dist-cp-group-by-aggregates-through-a-real-mapreduce-job: "the s3-dist-cp step must COMPLETE ... Got 'FAILED'"
  - emr.security-configuration-sse-kms-applies-to-emrfs-writes: "the EMRFS write step must COMPLETE; got 'FAILED'"; the step's debug transcript object is absent (GetObject -> NoSuchKey).

Cause 2 — FailureDetails.Reason "The application the step submitted exited with a non-zero status." with FailureDetails.Message:
  - emr.parquet-written-by-one-spark-step-is-read-back-by-the-next: "ModuleNotFoundError: No module named 'pyspark.sql.types'"
  - emr.spark-step-partitioned-write-lands-hive-layout-in-s3: "ModuleNotFoundError: No module named 'pyspark.sql.functions'"

Cause 3 — emr.spark-partitioned-input-pruned-then-written-back-partitioned: FailureDetails.Message "AnalysisException: cannot resolve the column concat('pp-<nonce>-', cast(id as string))" (the `pp-<nonce>-` literal is this run's value, not part of the requirement).

Cause 4 — emr.spark-defaults-classification-governs-the-shuffle-partition-count: "the shuffle-config step must COMPLETE; got 'FAILED' (reason {'Message': 'The application the step submitted exited with a non-zero status.'})" — no FailureDetails.Message at all.

Cause 5 — emr.emr-step-args-are-passed-verbatim-to-the-application: "the argv-reporting step must reach COMPLETED, got 'FAILED' ({'Reason': 'The command the step submitted exited with a non-zero status.', 'Message': 'The command the step submitted exited with status 1.', 'LogFile': 's3://.../steps/s-.../stderr.gz'})". The preceding DescribeStep `Config.Args` echo assertion passed.

Cause 6 — emr.persistent-app-ui-creation-then-describe-eventually-ready: "prerequisite SparkPi step s-... ended 'FAILED', not COMPLETED"; emr.persistent-app-ui-describe-survives-cluster-termination-window: "prerequisite SparkPi step did not complete: FAILED".

## Steps to Reproduce
Boot the emulator and replay the ten behavioral checks listed in trace_ids (service `emr`). Minimal shape of each cause:
1. Cause 1: RunJobFlow a keep-alive cluster (ReleaseLabel emr-6.15.0, Applications Hadoop/Spark, LogUri set), AddJobFlowSteps a step whose HadoopJarStep.Jar is `command-runner.jar` (or the s3-dist-cp jar) with a command that succeeds, poll DescribeStep: assert State COMPLETED, no FailureDetails, and the command's S3 output object present. Assert no response field ever contains the text "This emulator runs the step lifecycle but executes no Hadoop jar".
2. Cause 2: submit a spark-submit step whose script does `from pyspark.sql.types import StructType` (and, separately, `from pyspark.sql.functions import col`); assert State COMPLETED and no ModuleNotFoundError in FailureDetails.
3. Cause 3: submit a spark-submit step that calls `selectExpr("concat('p-', cast(id as string)) as k")` on a DataFrame and writes partitioned output; assert State COMPLETED and no AnalysisException.
4. Cause 4: launch a cluster with a `spark-defaults` Configurations classification setting `spark.sql.shuffle.partitions`, submit a job that reads it via `spark.conf.get`, performs a groupBy shuffle and saves a report; assert State COMPLETED, and on any non-zero exit assert FailureDetails.Message names the failing stage rather than being absent.
5. Cause 5: submit `command-runner.jar` with Args `["bash", "-c", "set -eu; aws s3 cp s3://<b>/app.py /tmp/app.py; python3 /tmp/app.py \"$@\" > /tmp/out; aws s3 cp /tmp/out s3://<b>/out", "name", "plain", "two words", "", "$NOT_EXPANDED"]`; assert State COMPLETED and that the uploaded report shows the argv delivered byte-for-byte with only the zero-length argument dropped.
6. Cause 6: submit a trivial SparkPi spark-submit step on a WAITING cluster; assert COMPLETED, then CreatePersistentAppUI/DescribePersistentAppUI for that application.
