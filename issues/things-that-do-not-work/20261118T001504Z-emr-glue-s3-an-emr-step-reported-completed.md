Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.emr-cluster-step-reads-input-from-s3-positive
Issue-Key: emr__glue__s3.emr-cluster-step-reads-input-from-s3-positive|run#19|AssertionError|assert actual_md5 == expected_md5, (
Issue-Commit: 42c962a883373db882714838d134007e6c5153c3

# emr__glue__s3: an EMR step reported COMPLETED never executes its command — the S3 object the step pipes its result into is served empty, so the EMRFS read-back checksum is blank

> _Note: this issue was identified against the code base at commit `42c962a883373db882714838d134007e6c5153c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
1 net-new root cause in the EMR step data plane, reached through the emr__glue__s3 cross-service group.

Cause 1 of 1: a step that the control plane settles COMPLETED did not run its command, and the S3 key the command writes is materialized with an EMPTY body instead of the bytes the command would have produced.

AWS: Smithy (com.amazonaws.emr): `StepState` = PENDING | CANCEL_PENDING | RUNNING | COMPLETED | CANCELLED | FAILED | INTERRUPTED, and `StepTimeline.StartDateTime` is documented as "The date and time when the cluster step execution started" / `EndDateTime` "when the cluster step execution completed or failed" — COMPLETED is a statement that the step's command actually executed on the cluster and exited 0. The step payload itself is opaque to the model, so for the command's effect this is recorded behavior only: a `command-runner.jar` step whose Args are `["bash","-c", "hadoop fs -cat s3://<bucket>/<key> | md5sum | awk '{print $1}' | aws s3 cp - s3://<bucket>/verified/checksum.txt"]` runs on the cluster's nodes, reads the input object through EMRFS, and leaves the destination object holding exactly the pipeline's stdout — the 32-character lowercase MD5 hex digest of the input object's bytes (the digest value itself is a function of the input the check uploads, not a fixed string). A step cannot be COMPLETED and its command's side effects absent.

Emulator: the step does reach a terminal `COMPLETED` StepState (the check's state assertion passes), and `GetObject` on the destination key `verified/checksum.txt` SUCCEEDS — but the body is EMPTY. The check's comparison therefore reads `checksum computed on the cluster () does not match the content actually stored under the S3 input path (<md5 of the input object>)`: the first parenthesis, the value read back from S3, is the empty string. So the emulator both fabricates the COMPLETED verdict and fabricates the step's output object as a zero-byte placeholder, rather than running the command or leaving the key absent. Hypothesis: the step runner creates the destination key named in an `aws s3 cp` arg without executing the pipeline that should supply its bytes, so nothing is ever read through EMRFS from the input key.

Why: this is strictly worse for a client than a step that never settles. A caller that waits for COMPLETED and then reads the step's output gets a syntactically valid but semantically empty answer — a present, zero-byte object — and has no signal that the work did not happen: no FAILED state, no missing key, no error. Every downstream stage that parses that output (a checksum, a row count, a manifest) silently consumes empty input, and any check that proves "the step really read its S3 input through EMRFS" by comparing a digest computed on the cluster against one computed locally can never pass.

Fix: the EMR crate's step execution path and its S3 side-effect handling (services/service-com-amazonaws-emr-elasticmapreduce — the step runner behind run_job_flow.rs / add_job_flow_steps.rs and the cluster state that backs describe_step.rs): execute the dispatched step's command so its EMRFS read and its stdout-to-S3 write produce real bytes, and do not create an output object for a command that was not run — a step whose command did not execute must not be reported COMPLETED.

Note: several open issues cover the EMR step plane but none covers this behavior and none carries this check id. 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md, 20261105T074335Z-emr-glue-s3-emr-cluster-steps-never-reach.md and 20261108T104909Z-emr-glue-s3-2-net-new-causes-cluster.md all describe steps that never reach a terminal state (checks killed on a poll) or whose output object never appears at all; here the step DOES settle COMPLETED and the output object DOES exist, served empty, which is a different defect in the same path.

Traces: emr__glue__s3.emr-cluster-step-reads-input-from-s3-positive.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator (507/724 passed; emr__glue__s3 26/47).

Failing check: [mismatch] emr__glue__s3.emr-cluster-step-reads-input-from-s3-positive

Detail, from the suite report:
  File "check_emr__glue__s3_emr_cluster_step_reads_input_from_s3_positive.py", line 61, in run
    assert actual_md5 == expected_md5, (
  AssertionError: checksum computed on the cluster () does not match the content actually stored under the S3 input path (7592d1726258f0bbfa91d512beb12e41); the step did not really read the input dataset from S3

What that shows, step by step:
- The check PUTs a 50-line text object at `input/data.txt` in a fresh bucket and computes its MD5 locally (`expected_md5`; the hex value shown is this run's input, not a requirement).
- It RunJobFlows a transient cluster (EMR service role + EC2 instance profile created in IAM) with one `command-runner.jar` step: `hadoop fs -cat s3://<bucket>/input/data.txt | md5sum | awk '{print $1}' | aws s3 cp - s3://<bucket>/verified/checksum.txt`, ActionOnFailure=CONTINUE, KeepJobFlowAliveWhenNoSteps=false.
- `ListSteps` returns the step and the poll for a terminal StepState reaches `COMPLETED` — the preceding assertion (`expected read step to COMPLETE, got <state>`) did NOT fire, so the emulator settled the step COMPLETED.
- `GetObject(Bucket=<bucket>, Key="verified/checksum.txt")` SUCCEEDS — no NoSuchKey — and `Body.read().decode().strip()` is the EMPTY STRING (`actual_md5` renders as `()` in the message).

Expected (real AWS): the destination object holds the 32-char lowercase MD5 hex digest of the input object's bytes, equal to the digest the caller computes locally.
Actual (emulator): step COMPLETED, destination object present with a zero-length body.

## Steps to Reproduce
Boot the emulator and replay emr__glue__s3.emr-cluster-step-reads-input-from-s3-positive from suite simparity-behavioral-aws-v1, or by hand:
1. CreateBucket a fresh bucket; PutObject key `input/data.txt` with any multi-line body; compute that body's MD5 hex digest locally.
2. In IAM, create the EMR service role (trusts elasticmapreduce.amazonaws.com, AmazonElasticMapReduceRole attached) and an EC2 instance profile for the cluster.
3. RunJobFlow a transient cluster (KeepJobFlowAliveWhenNoSteps=false) with one step: Name=read-from-s3, ActionOnFailure=CONTINUE, HadoopJarStep={Jar: command-runner.jar, Args: ["bash","-c","hadoop fs -cat s3://<bucket>/input/data.txt | md5sum | awk '{print $1}' | aws s3 cp - s3://<bucket>/verified/checksum.txt"]}.
4. ListSteps(ClusterId) to get the step id, then poll DescribeStep until Step.Status.State is terminal. Observe COMPLETED.
5. GetObject(Bucket=<bucket>, Key="verified/checksum.txt") and read the body. Observed: the call succeeds and the body is empty. Expected: the body is the MD5 hex digest computed in step 1, proving the step read the input through EMRFS and its stdout was piped to S3.
