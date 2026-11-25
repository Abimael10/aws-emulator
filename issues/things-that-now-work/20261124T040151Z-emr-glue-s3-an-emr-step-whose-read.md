Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.emr-step-missing-s3-input-read-permission-negative
Issue-Key: emr__glue__s3.emr-step-missing-s3-input-read-permission-negative|run#22|AssertionError|assert denied_state == "FAILED", f"expected denied-path step to FAIL, got {denied_state}"
Issue-Commit: 34b956776d59299fbb546444fafe35aa6ddbd93a

# emr__glue__s3: an EMR step whose read of an S3 prefix the cluster's instance role is explicitly DENIED s3:GetObject on is reported COMPLETED instead of FAILED

> _Note: this issue was identified against the code base at commit `34b956776d59299fbb546444fafe35aa6ddbd93a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStep

## Description
Cause 1 of 1: the EMR step executor does not evaluate the cluster's EC2 instance-profile identity policy against the S3 requests a step makes, so a step that reads an S3 key covered by an explicit Deny on s3:GetObject runs as if it were permitted and is settled COMPLETED. Failing check: emr__glue__s3.emr-step-missing-s3-input-read-permission-negative.

AWS: Smithy com.amazonaws.emr#StepState enumerates PENDING, CANCEL_PENDING, RUNNING, COMPLETED, CANCELLED, FAILED, INTERRUPTED, and com.amazonaws.emr#StepStatus carries State plus StateChangeReason{Code,Message} and FailureDetails{Reason,Message,LogFile} ("the descriptive message including the error the Amazon EMR service has identified as the cause of step failure"; Reason is "Unknown Error" when the root cause cannot be determined); DescribeStep/ListSteps report that status. The IAM side is recorded behavior only (the EMR model says nothing about authorization): a command-runner.jar step runs on cluster nodes under the cluster's JobFlowRole/EC2 instance profile, S3 reads from that step are made with that role's credentials, an explicit Deny on s3:GetObject for the prefix makes the read fail with S3 403 AccessDenied, the shell command therefore exits non-zero, and the step settles StepStatus.State == FAILED. Because the step's `&&`-chained downstream command never runs, the proof object it would have written does not exist, so HeadObject on that key answers 404. The requirement is the pair of outcomes (denied-prefix step FAILED with no downstream effect, unrestricted-prefix step on the SAME cluster COMPLETED with its marker body written); the bucket name, cluster id, step ids and key prefixes are per-run values, not requirements.
Emulator: the denied step is reported COMPLETED. Replay detail: `AssertionError: expected denied-path step to FAIL, got COMPLETED` (check body line 77). The explicit Deny attached to the instance profile has no effect on the step's S3 access, so the negative half of the permission boundary is indistinguishable from the permitted control.
Why: a client that relies on EMR honouring least privilege gets the opposite of AWS's answer — a step that AWS stops with AccessDenied is reported successful, and any downstream effect it is chained to is treated as having been produced. Infrastructure tests that assert a Deny actually blocks a job's input read pass against the emulator and fail in AWS, and a reviewer cannot tell "the deny was enforced" from "the deny was ignored": both now read COMPLETED.
Fix: the EMR step execution path in the EMR service crate (the step runner behind RunJobFlow/AddJobFlowSteps and the step-status writer DescribeStep/ListSteps read). The step's process must obtain credentials for the cluster's EC2 instance profile and have every S3 request it issues authorized against that role's identity policies with explicit-Deny precedence, and a non-zero command exit must settle the step in StepState FAILED with StepStatus.FailureDetails populated, leaving no downstream object written.
Traces: emr__glue__s3.emr-step-missing-s3-input-read-permission-negative.

Related open issues that do NOT carry this check's replay key, and so do not cover it: 20261108T165124Z-emr-glue-s3-an-emr-step-is-reported.md (same class, but grounded in an instance role denied glue:* and the check emr-cluster-glue-catalog-missing-permission-negative) and 20261116T201047Z-emr-glue-s3-2-net-new-causes-an.md (an instance role explicitly denied s3:PutObject still gets cluster logs written under LogUri). This issue is the s3:GetObject read path for a step's own input and should be closed together with them.

## Evidence
Failing check (verdict: mismatch): emr__glue__s3.emr-step-missing-s3-input-read-permission-negative
Replay output:
  File "check_emr__glue__s3_emr_step_missing_s3_input_read_permission_negative.py", line 77, in run
    assert denied_state == "FAILED", f"expected denied-path step to FAIL, got {denied_state}"
  AssertionError: expected denied-path step to FAIL, got COMPLETED

Scenario replayed: one bucket holding input-denied/data.txt and input-allowed/data.txt with identical content; an EMR service role; an EC2 instance profile carrying an extra inline policy {Effect: Deny, Action: s3:GetObject, Resource: arn:aws:s3:::<bucket>/input-denied/*}; one RunJobFlow cluster (KeepJobFlowAliveWhenNoSteps=false) with two command-runner.jar steps, ActionOnFailure=CONTINUE:
  - "read-denied-path":  bash -c 'hadoop fs -cat s3://<bucket>/input-denied/data.txt > /tmp/x1 && echo ok | aws s3 cp - s3://<bucket>/proof/denied-ok.txt'
  - "read-allowed-path": bash -c 'hadoop fs -cat s3://<bucket>/input-allowed/data.txt > /tmp/x2 && echo ok | aws s3 cp - s3://<bucket>/proof/allowed-ok.txt'

Expected per AWS: polling DescribeStep for the two step ids settles the denied step at StepStatus.State == FAILED (with FailureDetails describing the AccessDenied) and the allowed step at COMPLETED; HeadObject(proof/denied-ok.txt) then answers 404 (the chained copy never ran) while GetObject(proof/allowed-ok.txt) returns the body "ok\n".
Actual from the emulator: the denied step's state is COMPLETED, so the explicit Deny was not applied to the step's S3 read; the assertion on the denied step's terminal state fails immediately and the two downstream effect checks (404 for the denied marker, "ok\n" for the allowed one) are never graded.

## Steps to Reproduce
Boot the emulator and replay emr__glue__s3.emr-step-missing-s3-input-read-permission-negative from suite simparity-behavioral-aws-v1, or by hand:
1. s3.create_bucket; put_object input-denied/data.txt and input-allowed/data.txt with the same small text body.
2. iam.create_role for the EMR service role (trusting elasticmapreduce.amazonaws.com) and an EC2 instance profile role (trusting ec2.amazonaws.com) with the usual EMR EC2 permissions, PLUS an inline policy {"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::<bucket>/input-denied/*"}; create and populate the instance profile.
3. emr.run_job_flow(ServiceRole=<service role>, JobFlowRole=<instance profile>, KeepJobFlowAliveWhenNoSteps=false) with two command-runner.jar steps (ActionOnFailure=CONTINUE): one running `hadoop fs -cat s3://<bucket>/input-denied/data.txt > /tmp/x1 && echo ok | aws s3 cp - s3://<bucket>/proof/denied-ok.txt`, one running the same against input-allowed/ writing proof/allowed-ok.txt.
4. emr.list_steps(ClusterId) to get both step ids, then poll emr.describe_step for each until Step.Status.State is terminal.
5. AWS: the denied-path step is FAILED (Status.FailureDetails set) and the allowed-path step is COMPLETED. Emulator: the denied-path step is COMPLETED.
6. s3.head_object(Bucket, 'proof/denied-ok.txt') — AWS: 404 (the chained copy never ran). s3.get_object(Bucket, 'proof/allowed-ok.txt') — AWS: body 'ok\n'.
