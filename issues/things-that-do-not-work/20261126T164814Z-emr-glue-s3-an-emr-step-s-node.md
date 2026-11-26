Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative
Issue-Key: emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative|run#19|AssertionError|assert "AccessDenied" in err_text, (
Issue-Commit: 127e90fa938572074a2c08d828ebb8de7f0d4317

# emr__glue__s3: an EMR step's node-side `aws glue get-tables` denial is unobservable — the step's rc/stderr objects are materialized as zero-byte placeholders, so no AccessDenied ever reaches S3

> _Note: this issue was identified against the code base at commit `127e90fa938572074a2c08d828ebb8de7f0d4317`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
1 net-new root cause in the EMR step data plane, reached through the emr__glue__s3 cross-service group.

Cause 1 of 1: a command-runner.jar step whose bash script runs `aws glue get-tables` on the cluster and uploads its return code and its stderr to S3 produces both destination objects, but both are served EMPTY — the node-side command is never executed, so the explicit-Deny refusal the step exists to observe (glue:GetTables denied on the EC2 instance role) never appears anywhere. Failing check: emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative.

AWS: Smithy (com.amazonaws.emr): RunJobFlow/AddJobFlowSteps accept a step whose HadoopJarStep is `command-runner.jar` with Args ["bash","-c",<script>]; the step payload itself is opaque to the model, so for the script's effect this is recorded behavior only — the script RUNS on the cluster's nodes under the EC2 instance profile, and its redirections and `aws s3 cp` commands leave the bytes the commands actually produced. Smithy (com.amazonaws.glue): GetTables models EntityNotFoundException, FederationSourceException, FederationSourceRetryableException, GlueEncryptionException, InvalidInputException, OperationTimeoutException (400) and InternalServiceException (500) — an authorization refusal is NOT a modeled Glue error but the common IAM denial: HTTP 400 with error code AccessDeniedException and the documented explicit-deny message, whose fixed text is "User: <principal ARN> is not authorized to perform: <action> on resource: <resource ARN> with an explicit deny in an identity-based policy" and whose variable parts are taken from the request and from IAM state (here the assumed-role ARN of the EMR EC2 instance role, `glue:GetTables`, and the Glue table/database ARN). Reference: https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_evaluation-logic.html. So on AWS the step leaves `glue-check/rc.txt` holding the CLI's non-zero exit status (255) and `glue-check/err.txt` holding that AccessDeniedException text; neither is empty.

Emulator: the step is accepted and the two keys DO appear under `glue-check/` in the stack's output bucket, but their bodies are the EMPTY STRING. `rc.txt` reads as "" — which is why the check's `rc != "0"` assertion passes only vacuously — and `err.txt` reads as "", so the check fails on `assert "AccessDenied" in err_text` with `expected an AccessDenied error, got: ''`. Nothing on the node ran: no Glue call was made under the denied instance role, no refusal was generated, and the destination keys named in the script's `aws s3 cp` arguments were created as zero-byte placeholders instead.

Why: this is the worst shape for a caller. A present, zero-byte artifact is indistinguishable from "the command ran and printed nothing", so a client that waits for the step's output and parses it silently consumes empty input — there is no FAILED step, no missing key and no error to react to. Worse, for this scenario class the emulator cannot show a refusal at all: an estate whose instance role explicitly denies glue:* behaves exactly like one that is fully permitted, so a user cannot verify least-privilege wiring, and the companion restore leg (delete the explicit Deny and demand the identical call now succeeds) can never distinguish the two states either.

Fix: the EMR crate's step runner and its S3 side-effect handling (services/service-com-amazonaws-emr-elasticmapreduce — the step execution path behind run_job_flow.rs / add_job_flow_steps.rs and the cluster state behind describe_step.rs): execute the dispatched step's command so its own stdout/stderr and exit status are what the script's `aws s3 cp` uploads, route the node's AWS calls through the cluster's EC2 instance profile so an identity-based Deny is evaluated and refused with the IAM AccessDeniedException message above, and never fabricate a destination object for a command that was not run.

Traces: emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative. Related but not covering it: 20261118T001504Z-emr-glue-s3-an-emr-step-reported-completed.md describes the same zero-byte materialization of a step's output object for a DIFFERENT check and does not carry this check's replay key; 20261105T074335Z-emr-glue-s3-emr-cluster-steps-never-reach.md, 20261108T104909Z-emr-glue-s3-2-net-new-causes-cluster.md and 20261125T181022Z-emr-glue-s3-an-emr-cluster-s-step.md describe steps that never settle or logs that never land, which is not what this check observes (here the objects DO appear, empty).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator (2351/2681 passed; emr__glue__s3 shows 10 failures).

Failing check: [mismatch] emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative

Detail, from the suite report:
  File "check_emr__glue__s3_cfn_deploy_emr_cluster_glue_catalog_permission_missing_negative.py", line 194, in run
    assert "AccessDenied" in err_text, (
  AssertionError: expected an AccessDenied error, got: ''

What that shows, step by step:
- The check creates a Glue database and proves the positive control first: GetTables(DatabaseName=<db>) with the harness's own credentials returns TableList == [] (this assertion passes).
- It deploys a CloudFormation stack with an S3 output bucket, an EMR service role, an EMR EC2 instance role carrying an inline policy named `explicit-deny-glue`, an instance profile, and an AWS::EMR::Cluster (emr-6.15.0, Hadoop, one m5.xlarge master) with one CONTINUE step: command-runner.jar, Args ["bash","-c", "aws glue get-tables --database-name <db> --region <region> >/tmp/out.txt 2>/tmp/err.txt; echo $? >/tmp/rc.txt; aws s3 cp /tmp/rc.txt s3://<bucket>/glue-check/rc.txt; aws s3 cp /tmp/err.txt s3://<bucket>/glue-check/err.txt"].
- It polls GetObject for `glue-check/rc.txt` until present. The object DOES appear, so the first assertion (`rc != "0"`) does not fire — but only because the body is the empty string.
- GetObject for `glue-check/err.txt` returns an EMPTY body, so `err_text` is ''.

Expected (real AWS): rc.txt holds the AWS CLI's non-zero exit status (255) and err.txt holds the CLI's stderr, which contains the IAM explicit-deny refusal — "An error occurred (AccessDeniedException) when calling the GetTables operation: User: arn:<partition>:sts::<account>:assumed-role/<emr-ec2-role>/<session> is not authorized to perform: glue:GetTables on resource: arn:<partition>:glue:<region>:<account>:table/<db>/* with an explicit deny in an identity-based policy" (the role/session/db/region/account values come from the request and IAM state; the quoted sentence frame and the error code are the requirement).
Actual (emulator): both objects present, both zero bytes; no Glue denial produced or recorded anywhere.

## Steps to Reproduce
Boot the emulator and replay emr__glue__s3.cfn-deploy-emr-cluster-glue-catalog-permission-missing-negative from suite simparity-behavioral-aws-v1, or by hand:
1. glue CreateDatabase(DatabaseInput={Name: <db>}); confirm GetTables(DatabaseName=<db>) returns TableList == [].
2. CreateStack with: an AWS::S3::Bucket; an EMR service role trusting elasticmapreduce.amazonaws.com with AmazonElasticMapReduceRole; an EMR EC2 role trusting ec2.amazonaws.com with AmazonElasticMapReduceforEC2Role plus an INLINE policy named `explicit-deny-glue` = {Effect: Deny, Action: "glue:*", Resource: "*"}; an instance profile for it; and an AWS::EMR::Cluster (ReleaseLabel emr-6.15.0, Applications [Hadoop], one m5.xlarge master in a private subnet, KeepJobFlowAliveWhenNoSteps=true) whose single step is command-runner.jar with Args ["bash","-c","aws glue get-tables --database-name <db> --region <region> >/tmp/out.txt 2>/tmp/err.txt; echo $? >/tmp/rc.txt; aws s3 cp /tmp/rc.txt s3://<bucket>/glue-check/rc.txt; aws s3 cp /tmp/err.txt s3://<bucket>/glue-check/err.txt"].
3. Poll s3 GetObject(Bucket=<bucket>, Key="glue-check/rc.txt") until it exists, then read its body. Observed: empty. Expected: the CLI's non-zero exit status (255).
4. s3 GetObject(Bucket=<bucket>, Key="glue-check/err.txt") and read its body. Observed: empty. Expected: the CLI's stderr carrying error code AccessDeniedException and the "... is not authorized to perform: glue:GetTables on resource: ... with an explicit deny in an identity-based policy" message for the cluster's assumed instance role.
5. (Restore leg, also unreachable today) iam DeleteRolePolicy(RoleName=<EMR EC2 role>, PolicyName="explicit-deny-glue"), AddJobFlowSteps with the identical script writing rc2.txt/err2.txt, and expect rc2.txt == "0" with empty stderr — on the emulator both objects are again served empty, so the allowed and denied states are indistinguishable.
