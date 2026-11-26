Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.emr-cluster-instance-role-trust-mismatch-negative
Issue-Key: emr__glue__s3.emr-cluster-instance-role-trust-mismatch-negative|run#19|AssertionError|assert good_state == "COMPLETED", f"positive control step (correctly-trusted role) did not complete: {good_state}"
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# emr__glue__s3: a RunJobFlow step that writes to S3 with the cluster's correctly-trusted instance profile ends FAILED and writes nothing, so the instance-profile trust boundary is unmeasurable (1 cause)

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
Cause 1 of 1 — the positive control of the instance-profile trust boundary fails: a `command-runner.jar` step that pipes data into `aws s3 cp - s3://<bucket>/<key>` on a cluster whose EC2 instance profile role DOES trust ec2.amazonaws.com settles with Status.State = FAILED and the object is never created, so the negative control (a role trusting glue.amazonaws.com instead) cannot be distinguished from the positive one. Failing check: emr__glue__s3.emr-cluster-instance-role-trust-mismatch-negative.

AWS: a step submitted with the job flow runs on the cluster's nodes under the instance profile named by JobFlowInstancesConfig.Ec2InstanceProfile. When that role's trust policy allows ec2.amazonaws.com to sts:AssumeRole and the role carries AmazonElasticMapReduceforEC2Role, the instance metadata service hands the step valid credentials, the `aws s3 cp` writes the object, the shell exits 0 and the step settles COMPLETED (Smithy: StepStatus.State -> StepState enum PENDING|CANCEL_PENDING|RUNNING|COMPLETED|CANCELLED|FAILED|INTERRUPTED, read back through DescribeStep/ListSteps; a real failure additionally carries StepStatus.FailureDetails{Reason,Message,LogFile}). A role whose trust policy names some other service principal (here glue.amazonaws.com) cannot be assumed by EC2, the step gets no usable credentials, the copy is refused and the step settles FAILED with no object written. The two outcomes must differ. Recorded behavior only for the exact failure text; the modelled requirement is the terminal StepState and the presence/absence of the object.

Emulator: BOTH clusters' steps settle FAILED. The correctly-trusted cluster's step reports Status.State = FAILED (replay: `AssertionError: positive control step (correctly-trusted role) did not complete: FAILED`) and s3://<bucket>/proof/good-ok.txt is never created, so the step's work was never performed as the instance-profile role at all — the state does not follow the exit status of any executed command, and the trust policy attached to the instance profile role has no influence on the outcome.

Why: a client using EMR to validate that a cluster's instance profile is correctly configured — a least-privilege review, a guardrail test asserting that a mis-trusted role cannot write to S3, or any job that simply expects its step to upload its result — is told the correctly-configured cluster failed, and gets the same FAILED verdict for the mis-configured one. Both a false failure on a healthy configuration and the loss of every trust-boundary signal follow; no step side effect ever appears in S3, so downstream stages that read a step's output break.

Fix: the EMR step executor and its state machine in the EMR service (the component that takes a job flow's Steps / AddJobFlowSteps steps to a terminal state) — the step's command must actually be run under the cluster's EC2 instance profile, with credentials derived from that role (so the role's trust policy decides whether they can be obtained), and StepStatus.State must be derived from the command's exit status (COMPLETED on 0, FAILED with StepStatus.FailureDetails populated otherwise) rather than being set unconditionally. Related open issues that do NOT carry this check's replay key: 20261108T104909Z-emr-glue-s3-2-net-new-causes-cluster.md (steps always end FAILED because no application is run), 20261105T074335Z-emr-glue-s3-emr-cluster-steps-never-reach.md and 20261108T165124Z-emr-glue-s3-an-emr-step-is-reported.md.

Traces: emr__glue__s3.emr-cluster-instance-role-trust-mismatch-negative.

## Evidence
Expected (AWS): the step `bash -c "echo ok | aws s3 cp - s3://<bucket>/proof/good-ok.txt"` on the cluster whose instance profile role trusts ec2.amazonaws.com settles DescribeStep -> Step.Status.State = "COMPLETED" and GetObject on proof/good-ok.txt returns the body "ok\n"; the sibling cluster whose instance profile role trusts glue.amazonaws.com instead settles in a non-COMPLETED state (FAILED) and HeadObject on proof/bad-ok.txt answers 404.
Actual (emulator): the correctly-trusted cluster's step settles Step.Status.State = "FAILED" and proof/good-ok.txt does not exist. Replay detail: `AssertionError: positive control step (correctly-trusted role) did not complete: FAILED` (check_emr__glue__s3_emr_cluster_instance_role_trust_mismatch_negative.py line 84). Fixed text of the assertion is the check's own; the varying parts are the bucket/key names and cluster ids, which come from the request. Both clusters report the same terminal state, so the trust-policy difference produces no observable difference.

## Steps to Reproduce
1. CreateBucket a test bucket.
2. IAM: create an EMR service role trusting elasticmapreduce.amazonaws.com with AmazonElasticMapReduceRole attached.
3. IAM: create TWO roles, each with AmazonElasticMapReduceforEC2Role attached and each in its own instance profile — role A's trust policy names Service ec2.amazonaws.com, role B's names Service glue.amazonaws.com.
4. RunJobFlow twice (KeepJobFlowAliveWhenNoSteps=false), one cluster per instance profile, each with a single step: HadoopJarStep.Jar = command-runner.jar, Args = ["bash","-c","echo ok | aws s3 cp - s3://<bucket>/proof/<good|bad>-ok.txt"], ActionOnFailure=CONTINUE.
5. Poll DescribeCluster until each cluster is TERMINATED/TERMINATED_WITH_ERRORS, then ListSteps + DescribeStep on each.
6. Expect the ec2.amazonaws.com-trusted cluster's step COMPLETED with proof/good-ok.txt containing "ok\n", and the glue.amazonaws.com-trusted cluster's step non-COMPLETED with proof/bad-ok.txt absent (HeadObject 404). Observe: the correctly-trusted step is FAILED and neither object exists.
7. Replay: emr__glue__s3.emr-cluster-instance-role-trust-mismatch-negative.
