Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.bootstrap-action-completes-before-the-first-step-runs emr.emr-cancel-and-wait-on-a-failed-step-cancels-the-following-step emr.emr-cancel-steps-on-a-running-step-prevents-its-output emr.emr-step-concurrency-level-two-runs-two-steps-with-overlapping-intervals emr.emrfs-write-through-hadoop-fs-is-read-by-the-next-step emr.hdfs-write-survives-the-step-boundary emr.installed-application-versions-agree-between-describe-cluster-and-the-node emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two emr.step-added-while-cluster-starting-queues-then-runs
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.bootstrap-action-completes-before-the-first-step-runs|run#24|AssertionError|assert observed == token, (
Issue-Key: emr.emr-cancel-and-wait-on-a-failed-step-cancels-the-following-step|run#22|AssertionError|assert body1 == nonce1, (
Issue-Key: emr.emr-cancel-steps-on-a-running-step-prevents-its-output|run#27|AssertionError|assert body_b == nonce_b, (
Issue-Key: emr.emr-step-concurrency-level-two-runs-two-steps-with-overlapping-intervals|_run_pair#15|AssertionError|assert body == nonce, (
Issue-Key: emr.emrfs-write-through-hadoop-fs-is-read-by-the-next-step|run#28|AssertionError|assert impl == _EMRFS_IMPL, (
Issue-Key: emr.hdfs-write-survives-the-step-boundary|run#30|AssertionError|assert observed == payload, (
Issue-Key: emr.installed-application-versions-agree-between-describe-cluster-and-the-node|run#21|AssertionError|assert control == _ABSENT, (
Issue-Key: emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two|run#20|AssertionError|assert _get_text(s3, bucket, _CONTROL_KEY) == nonce, (
Issue-Key: emr.step-added-while-cluster-starting-queues-then-runs|run#26|AssertionError|assert body == nonce.encode("utf-8"), (
Issue-Commit: 69851043ac49cf9e64b4660f983f2f135bf8a7d9

# EMR cluster steps never execute their command: every step's data-plane side effect is absent, so 9 step/bootstrap/concurrency checks read back an empty S3 object

> _Note: this issue was identified against the code base at commit `69851043ac49cf9e64b4660f983f2f135bf8a7d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddJobFlowSteps

## Description
One root cause (Cause 1 of 1), grounded in 9 failing behavioral checks.

Cause 1 of 1: a step submitted through RunJobFlow.Steps or AddJobFlowSteps is accepted and tracked as a control-plane record, but the HadoopJarStep command it carries is never executed on the cluster, so the step produces no observable side effect. Every one of the nine checks below drives a cluster, submits a step whose command writes a value the check controls (an S3 marker, a value read back out of HDFS or EMRFS, the output of a shell probe on the node), then reads that value back — and reads back the empty string in every case.

AWS: Smithy com.amazonaws.emr#StepConfig requires HadoopJarStep (HadoopJarStepConfig) and AddJobFlowSteps/RunJobFlow accept it; com.amazonaws.emr#StepState is PENDING | CANCEL_PENDING | RUNNING | COMPLETED | CANCELLED | FAILED | INTERRUPTED. AWS runs the named jar/command on the cluster's primary node under the configured instance profile: a step that reaches COMPLETED has run its command to exit status 0 and its side effects are durable. com.amazonaws.emr#BootstrapActionConfig requires ScriptBootstrapAction, which AWS runs on every node before the cluster leaves BOOTSTRAPPING and therefore before the first step runs. The model does not describe command execution itself, so beyond the state enum this is recorded behavior only: the recordings show the step's command running and the written object being readable afterwards, with the exact bytes the command was told to write.

Emulator: every step's command is a no-op. The emulator serves the object the command was supposed to create as a zero-length body (or does not create it, and the check's read helper yields ''), so:
- a step told to copy a bootstrap-written token off the node's local disk into S3 writes '' instead of the token;
- a step told to write its own nonce to an S3 marker key writes '';
- a step told to echo `hadoop fs -D fs.s3.impl` / the cluster's resolved Hadoop configuration reports '' rather than the EMRFS class name;
- a step told to read back what a previous step wrote into HDFS reports '';
- a step told to probe a binary that does not exist on the AMI reports '' rather than the check's 'ABSENT' sentinel, which shows the step's shell is not running at all rather than fabricating output.
Because the very first data-plane assertion fails, the behaviors these checks exist to pin — bootstrap-before-first-step ordering, CancelSteps on a RUNNING step suppressing its output, CANCEL_AND_WAIT cancelling the following step, StepConcurrencyLevel=2 producing two overlapping run intervals, the TERMINATE_CLUSTER/CANCEL_AND_WAIT ActionOnFailure refusal at concurrency 2, and a step queued while the cluster is still STARTING later running — are all unobservable. None of them can be judged until a step command actually runs.

Why: a client that submits work to EMR gets a cluster and a step record that look healthy while no computation happens. Any program that submits a step and then consumes its output — an ETL step writing Parquet, a bootstrap action installing a dependency a later step needs, an s3-dist-cp or hadoop fs stage feeding the next step — reads empty or missing data and either silently produces wrong results or fails far from the cause. Step ordering, cancellation and concurrency semantics are equally untestable against the emulator, so code that depends on them cannot be validated at all.

Fix: the EMR step execution path — whatever component advances a step record from PENDING through RUNNING to COMPLETED must actually invoke the step's HadoopJarStep (Jar/Args/MainClass, command-runner.jar included) on the cluster's node environment with the cluster's instance-profile credentials, and the bootstrap-action path must run each ScriptBootstrapAction on the nodes before the cluster leaves BOOTSTRAPPING. The step state machine and the node/command execution backing it are the components at fault, not the request parsing: the control-plane records are accepted and shaped correctly.

Related open issues that look similar but do not own these checks (the replay report marks none of the nine as OWNED or ALREADY FILED): 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md, 20261108T093128Z-ec2-emr-emr-job-flow-steps-always-end-failed-a.md, 20261118T001504Z-emr-glue-s3-an-emr-step-reported-completed.md. If the same execution path is repaired those may close together, but these nine checks carry replay keys none of them record.

Traces: the nine failing checks listed under trace_ids; representative assertions are quoted in the evidence.

## Evidence
All nine failures are the same shape: the value the step's command was told to produce is read back as the empty string. Fixed text is the check's own assertion wording; the bucket names, nonces and tokens are per-run values generated by the harness and are NOT part of the requirement.

1. emr.bootstrap-action-completes-before-the-first-step-runs — AssertionError: "the step must have read the token the bootstrap action left on the node's local disk: expected 'bootstrap-token-<nonce>' at s3://<bucket>/steps/observed-bootstrap-token-<nonce>, got ''"
2. emr.emr-cancel-and-wait-on-a-failed-step-cancels-the-following-step — AssertionError: "step 1's command must have written its own nonce to s3://<bucket>/markers/step-1; expected '<nonce>', got ''"
3. emr.emr-cancel-steps-on-a-running-step-prevents-its-output — AssertionError: "the control step's command must have written its own nonce to s3://<bucket>/markers/step-B; expected '<nonce>', got ''"
4. emr.emr-step-concurrency-level-two-runs-two-steps-with-overlapping-intervals — AssertionError: "the conc2 step's command must have written its own nonce to s3://<bucket>/markers/conc2-1; expected '<nonce>', got ''"
5. emr.emrfs-write-through-hadoop-fs-is-read-by-the-next-step — AssertionError: "on emr-6.15.0 the cluster's fs.s3.impl must resolve to EMRFS: expected 'com.amazon.ws.emr.hadoop.fs.EmrFileSystem', the cluster reported ''"
6. emr.hdfs-write-survives-the-step-boundary — AssertionError: "step 2 must read back exactly what step 1 wrote into HDFS: expected '<payload>' at s3://<bucket>/step2/observed.txt, got ''"
7. emr.installed-application-versions-agree-between-describe-cluster-and-the-node — AssertionError: "probing a binary that does not exist (<name>) must report 'ABSENT' — if it reported a version, the step's shell is fabricating output and no agreement below means anything. Got ''" (the empty result proves the step's shell never ran)
8. emr.spark-terminate-cluster-action-rejected-at-step-concurrency-two — AssertionError: "the control step must have written exactly its nonce to s3://<bucket>/out/control"
9. emr.step-added-while-cluster-starting-queues-then-runs — AssertionError: "the queued step must actually have executed: expected '<nonce>' at s3://<bucket>/steps/<nonce>, read back b''"

Expected (AWS): the step's command runs on the cluster, exits 0, the step reaches StepState COMPLETED, and the object/HDFS path/shell output the command produced carries exactly the bytes the command wrote.
Actual (emulator): the step record advances, but the command is never invoked; every read of its output yields '' / b''.

No HTTP status or error code diverges here — the control-plane calls answer 200. The divergence is the absence of the step's effect.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite) against the built emulator.
2. Or reproduce one check by hand against the emulator's EMR endpoint:
   a. RunJobFlow a cluster (ReleaseLabel emr-6.15.0, one primary + one core instance group, a service role and EC2 instance profile with S3 access, LogUri set).
   b. AddJobFlowSteps one step using command-runner.jar whose Args write a known nonce into an S3 key, e.g. Args = ["bash","-c","echo <nonce> | hadoop fs -put - s3://<bucket>/markers/step-1"].
   c. Poll DescribeStep until Status.State is terminal; it reports COMPLETED.
   d. GetObject s3://<bucket>/markers/step-1.
3. AWS returns a body of exactly <nonce>. The emulator returns an empty body (or no object), showing the step command never ran.
4. The same procedure with a BootstrapActions ScriptBootstrapAction that writes a token to the node's local disk, plus a step that copies that file to S3, also yields an empty object.
