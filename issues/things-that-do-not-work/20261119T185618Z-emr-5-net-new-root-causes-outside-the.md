Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.installed-application-versions-agree-between-describe-cluster-and-the-node emr.scaling-emrfs-read-of-a-deleted-object-is-rejected-and-writes-no-output emr.emr-cancel-steps-on-a-running-step-prevents-its-output emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two emr.get-persistent-app-ui-presigned-url-unknown-ui-rejected
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.installed-application-versions-agree-between-describe-cluster-and-the-node|run#23|AssertionError|assert node[name] != _ABSENT, (
Issue-Key: emr.scaling-emrfs-read-of-a-deleted-object-is-rejected-and-writes-no-output|run#26|AssertionError|assert impl == _EMRFS_IMPL, (
Issue-Key: emr.emr-cancel-steps-on-a-running-step-prevents-its-output|run#29|AssertionError|assert marker_a == "404", (
Issue-Key: emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two|run#44|AssertionError|assert [diag["rows"], diag["bad"]] == [_BAD_ROWS, _BAD_UNPARSEABLE], (
Issue-Key: emr.get-persistent-app-ui-presigned-url-unknown-ui-rejected|run#12|AssertionError|assert message == "PersistentAppUIType is not valid.", message
Issue-Commit: 0da99206c40b9a0852cdbaa2ba3793f12a97497b

# emr: 5 net-new root causes outside the step runner — cluster nodes carry no Hadoop/Spark binaries or EMRFS config, CancelSteps(TERMINATE_PROCESS) never stops the running command, a typed CSV read repairs unparseable records, and an app-UI refusal names a non-existent member

> _Note: this issue was identified against the code base at commit `0da99206c40b9a0852cdbaa2ba3793f12a97497b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelSteps

## Description
This issue bundles 5 net-new root causes in the EMR crate (service token `emr`) that are NOT the "steps never run" family: in each one the step or call the check makes DOES execute and DOES answer, and what it reports is wrong. They are independent and each must be closed on its own.

In every cause the requirement is the general behavior; the bucket names, cluster ids (`j-...`), step ids (`s-...`) and nonces in the recordings are per-run values.

Cause 1 of 5: a cluster's nodes have none of the application binaries DescribeCluster claims are installed.
AWS: Smithy (com.amazonaws.emr#ElasticMapReduce): DescribeCluster -> Cluster.Applications is a list of Application{Name, Version, Args, AdditionalInfo}, documented as the applications installed on the cluster. On AWS a cluster launched with `Applications=[{Name: Hadoop}, {Name: Spark}]` reports a Version for each AND the primary node actually has those binaries: `hadoop version` prints a version whose leading dotted-numeric prefix is exactly the Version DescribeCluster reports (recorded: control plane `3.3.6`, node `3.3.6-amzn-1`), and `spark-submit --version` likewise, with the two versions differing from each other.
Emulator: DescribeCluster reports `Applications` with Hadoop Version `3.3.6` (and a Spark version), but a step running on the node finds neither binary: `hadoop version` and `spark-submit --version` both fail, so the probe records the `ABSENT` sentinel for both. The step's shell itself works — the same step's `aws s3 cp` uploads the three report objects, and the deliberate control probe of a non-existent binary correctly reports ABSENT — so this is the node image, not the step runner.
Why: the control plane advertises software that is not there. A client that reads Cluster.Applications to decide what it may submit (or an operator that pins a Spark/Hadoop version from it) is told the cluster can run Spark and Hadoop jobs when nothing on the node can, and the control plane's version numbers cannot be trusted against the node at all.
Fix: the cluster-node provisioning in the EMR crate (the node/step execution environment that must install the applications named in RunJobFlow's `Applications` and report versions consistent with Cluster.Applications), not the DescribeCluster serializer, whose shape is already correct.
Traces: emr.installed-application-versions-agree-between-describe-cluster-and-the-node.

Cause 2 of 5: the node's Hadoop configuration has no `fs.s3.impl`, so nothing identifies the s3:// connector as EMRFS.
AWS: recorded behavior only — on an `emr-6.15.0` cluster EMRFS is the default s3:// filesystem, so `hdfs getconf -confKey fs.s3.impl` on the node prints `com.amazon.ws.emr.hadoop.fs.EmrFileSystem` (from emr-7.10.0 onward this becomes the S3A class, which is why the checks pin the 6.x line).
Emulator: the same command, run inside a step that then uploads its stdout to S3, yields the EMPTY STRING — the key is unset in the node's Hadoop configuration (the upload itself succeeds, the object is just empty).
Why: any job or operator probing the cluster's Hadoop configuration to confirm which filesystem implementation carries its s3:// reads and writes gets nothing back, so the EMRFS leg of a write cannot be distinguished from some other S3 client, and configuration-driven code paths keyed on `fs.s3.impl` (committer selection, consistency settings) resolve to a default instead of EMRFS.
Fix: the node's Hadoop/core-site configuration in the EMR crate's cluster provisioning — the release-dependent `fs.s3.impl` (and the sibling `fs.s3n.impl`/`fs.s3a.impl` keys the same catalog provides) must be populated so `hdfs getconf -confKey` resolves them.
Traces: emr.scaling-emrfs-read-of-a-deleted-object-is-rejected-and-writes-no-output. (Related: the identical empty `fs.s3.impl` is also what breaks the OWNED check emr.emrfs-write-through-hadoop-fs-is-read-by-the-next-step, whose issue 20261118T213929Z-emr-cluster-steps-never-execute-their-command-every.md describes it as a step-execution defect rather than as missing node configuration.)

Cause 3 of 5: CancelSteps accepts a cancellation with StepCancellationOption TERMINATE_PROCESS but never terminates the running command, which runs to completion and lands its side effects.
AWS: Smithy: CancelSteps(ClusterId required, StepIds required, StepCancellationOption: enum SEND_INTERRUPT|TERMINATE_PROCESS) -> CancelStepsInfoList{StepId, Status, Reason}; StepState includes CANCEL_PENDING and CANCELLED. Documented and recorded behavior: cancelling a step that is RUNNING with TERMINATE_PROCESS kills the step's process, so the work the command had not yet done does NOT happen — the object the command would have uploaded after its sleep is absent from S3.
Emulator: CancelSteps answers correctly at the control plane (one CancelStepsInfo for the named step with Status `SUBMITTED`, and the step reaches a terminal state), but the step's process keeps running: the marker object the cancelled command uploads after its delay EXISTS in S3 afterwards (HeadObject 200 where AWS answers 404). The check's uncancelled control step on the same cluster with the same script also completes and writes its own marker, so the command execution path works — only the termination does not reach it.
Why: cancellation is unsound. A client that cancels a long-running step to stop it from writing (a wrong-input job, a runaway cost, a job writing to the wrong prefix) is told the cancellation was accepted while the side effects it was trying to prevent still land; nothing a client can observe distinguishes a cancellation that worked from one that did not.
Fix: the CancelSteps handler in the EMR crate and the step runner it must signal — a TERMINATE_PROCESS cancellation of a RUNNING step has to kill the step's process (and SEND_INTERRUPT has to interrupt it) before the step is settled, not merely record the cancellation in step state.
Traces: emr.emr-cancel-steps-on-a-running-step-prevents-its-output.

Cause 4 of 5: a schema-typed CSV read silently repairs an unparseable field instead of nulling it, so the driver sees zero bad records.
AWS: recorded behavior only — Spark's default PERMISSIVE read mode with an explicitly declared schema (`spark.read.schema("id INT, amount INT").csv(...)`) parses a record whose fields do not match the declared types by setting the unparseable columns to NULL. For the recorded 4-record input, of which exactly one record has non-numeric values in both integer columns, the driver observes 4 rows of which 1 has null columns.
Emulator: the driver — which really does run here and really does publish its own diagnostic to S3 — reports `{'rows': 4, 'bad': 0}`: all four records come back with non-null `id` and `amount`, i.e. the CSV reader ignored the declared integer schema (or coerced the malformed values to non-null) and the typed-read contract is lost.
Why: typed ingestion is the core of a CSV ETL step. A client relying on PERMISSIVE null-ing to count or quarantine bad records sees a clean dataset; data-quality gates and `_corrupt_record`-style handling never trigger, and a job that is supposed to fail on malformed input proceeds on silently wrong values.
Fix: the CSV reader / schema-coercion path in the step runtime's Spark DataFrame engine — a value that does not parse against the declared column type must become NULL (PERMISSIVE mode) rather than being passed through.
Traces: emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two.

Cause 5 of 5: GetPersistentAppUIPresignedURL's 400 refusal names `ProfilerId`, a member that is not part of the operation's input.
AWS: Smithy: GetPersistentAppUIPresignedURL(PersistentAppUIId required, PersistentAppUIType: enum PersistentAppUIType = SHS|TEZ|YTS, ApplicationId, AuthProxyCall, ExecutionRoleArn) -> errors InternalServerError (500), InvalidRequestException (400). The model gives no message text, so the sentence is recorded behavior only: HTTP 400 InvalidRequestException with message exactly `PersistentAppUIType is not valid.` — entirely fixed text naming the input member whose value was rejected, with no variable part.
Emulator: HTTP 400 InvalidRequestException with message `ProfilerId is not valid.` — the same `<Member> is not valid.` template filled with a name that is not a member of GetPersistentAppUIPresignedURLInput at all (the id member is `PersistentAppUIId`). Status and error type are correct; only the message diverges.
Why: the message is the only discriminator on InvalidRequestException for this operation, so a client or runbook that classifies the refusal by message does not recognise it, and an operator is pointed at a field that does not exist in the request they sent.
Fix: the input-validation / refusal branch for GetPersistentAppUIPresignedURL in the EMR crate's operation handlers — the rejected-enum branch must name `PersistentAppUIType`.
Traces: emr.get-persistent-app-ui-presigned-url-unknown-ui-rejected. NOTE: this cause is the same root cause already filed from the WIRE corpus as Cause 1 of open issue 20261119T140639Z-elasticmapreduce-emr-400-refusal-messages-name-the-wrong-member.md (service token `elasticmapreduce`, key `elasticmapreduce|GetPersistentAppUIPresignedURL|4xx->4xx|$.message`); that issue does not carry this behavioral check's replay key, so the check is recorded here and both can be closed by the same one-line fix.

## Evidence
Behavioral suite replay against the booted emulator (206/283 passed, 62 mismatched, 15 errored, bootstrap_signal false; service `emr` 170/220). All five checks are net-new: not marked ALREADY FILED and not OWNED by any open issue.

Cause 1 — emr.installed-application-versions-agree-between-describe-cluster-and-the-node: "a cluster launched with the Hadoop application must have the Hadoop binary installed; the node reported 'ABSENT' (DescribeCluster claims version '3.3.6')". The step's probe script (`hadoop version | head -1 | awk '{print $2}'`, `spark-submit --version`, plus a deliberate non-existent-binary control) uploaded all three reports with `aws s3 cp`, and the control correctly read back `ABSENT`, so the shell and the S3 leg worked.

Cause 2 — emr.scaling-emrfs-read-of-a-deleted-object-is-rejected-and-writes-no-output: "on emr-6.15.0 the cluster's fs.s3.impl must resolve to EMRFS: expected 'com.amazon.ws.emr.hadoop.fs.EmrFileSystem', the cluster reported ''" — produced by `hdfs getconf -confKey fs.s3.impl > /tmp/impl` followed by an upload of /tmp/impl, i.e. the object exists and is empty.

Cause 3 — emr.emr-cancel-steps-on-a-running-step-prevents-its-output: after the step was observed RUNNING, `cancel_steps(ClusterId=..., StepIds=[s-...], StepCancellationOption='TERMINATE_PROCESS')` returned one CancelStepsInfo with Status SUBMITTED (asserted and passing), yet "cancelling step A while it was RUNNING must stop its command before the upload, so s3://<bucket>/markers/step-A must not exist; HeadObject answered 'EXISTS'". The uncancelled control step B reached COMPLETED and wrote its own marker.

Cause 4 — emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two: input `1,10\n2,20\noops,notanumber\n4,40\n` read as `schema("id INT, amount INT")`; "the engine must really have read the CSV and really failed to parse one record against the declared integer schema: expected 4 records of which 1 unparseable, the driver reported {'bad': 0, 'nonce': 'ta-<nonce>', 'rows': 4}". The driver ran and published its diagnostic part file to S3; only the null-ing of the malformed record is missing.

Cause 5 — emr.get-persistent-app-ui-presigned-url-unknown-ui-rejected: expected 400 InvalidRequestException message `PersistentAppUIType is not valid.`, got `ProfilerId is not valid.`

## Steps to Reproduce
Boot the emulator and replay the five behavioral checks listed in trace_ids (service `emr`). Minimal shape of each cause:
1. Cause 1: RunJobFlow a cluster with `ReleaseLabel emr-6.15.0` and `Applications=[{Name:Hadoop},{Name:Spark}]` plus one command-runner.jar step that runs `hadoop version` and `spark-submit --version` and uploads the outputs; assert each reported version exists and that DescribeCluster's `Cluster.Applications[].Version` is the leading dotted-numeric prefix of the node's value, and that the two applications report different versions.
2. Cause 2: on the same cluster run a step doing `hdfs getconf -confKey fs.s3.impl` and upload stdout; assert it reads back `com.amazon.ws.emr.hadoop.fs.EmrFileSystem` on an emr-6.x release.
3. Cause 3: on a WAITING keep-alive cluster, AddJobFlowSteps a `bash -c "sleep N; <write marker to S3>"` step, wait until DescribeStep reports RUNNING, call CancelSteps with StepCancellationOption=TERMINATE_PROCESS, then after the step settles assert HeadObject on the marker key answers 404 while an uncancelled control step with the same script does write its marker.
4. Cause 4: put a 4-record CSV with exactly one record whose two columns are non-numeric, submit a spark-submit step that does `spark.read.schema("id INT, amount INT").csv(<prefix>)`, collects, and publishes `{rows: len(rows), bad: count of rows with a null column}`; assert rows=4 and bad=1.
5. Cause 5: call GetPersistentAppUIPresignedURL with a valid PersistentAppUIId and a PersistentAppUIType outside SHS|TEZ|YTS; assert HTTP 400, InvalidRequestException, message exactly `PersistentAppUIType is not valid.`
