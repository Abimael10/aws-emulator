Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cfn-deploy-emr-step-s3-bootstrap-positive
Issue-Key: emr__glue__s3.cfn-deploy-emr-step-s3-bootstrap-positive|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Commit: 14f7a4850e696cd0bba053c19b13f3f2b3c04149

# emr__glue__s3: a CFN-deployed cluster's S3-sourced bootstrap action never runs and no bootstrap-action log is ever delivered under the cluster's LogUri, so the check reaches no verdict

> _Note: this issue was identified against the code base at commit `14f7a4850e696cd0bba053c19b13f3f2b3c04149`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
One net-new root cause, grounded in the single emr__glue__s3 failure the replay report does not already attribute to an open issue.

=== Cause 1 of 1: the cluster's ScriptBootstrapAction is recorded but never executed, and its stdout/stderr are never delivered to S3 under LogUri ===

AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce) models BootstrapActionConfig{Name (XmlStringMaxLen256, required), ScriptBootstrapAction (ScriptBootstrapActionConfig, required)} and ScriptBootstrapActionConfig{Path (XmlString, 0..10280, required) — documented as "Location in Amazon S3 of the script to run during a bootstrap action", Args (XmlStringList) — "A list of command line arguments to pass to the bootstrap action script"}; RunJobFlowInput.LogUri is documented as "The location in Amazon S3 to write the log files of the job flow. If a value is not provided, logs are not created." The model says nothing about the log key layout, so recorded behavior only for that: for a cluster launched with a LogUri of s3://<log-bucket>/logs/, EMR fetches the script named by ScriptBootstrapAction.Path from S3, runs it on every provisioned node during the BOOTSTRAPPING span, and archives that run's console output to the log bucket under the per-node prefix logs/<cluster-id>/node/<ec2-instance-id>/bootstrap-actions/<n>/, delivering a stdout object (plain or .gz) whose body contains whatever the script echoed. The cluster then becomes ready to run steps. The requirement is the general shape — a key under logs/<cluster-id>/node/ containing both "bootstrap-actions" and "stdout", whose decoded body contains the script's stdout; the bucket name, cluster id, instance id and marker text are values taken from the request and from the emulator's own state, not literals from one run.

Emulator: the CloudFormation stack (AWS::S3::Bucket + two AWS::IAM::Role + AWS::IAM::InstanceProfile + AWS::EMR::Cluster with BootstrapActions and LogUri) is deployed and the EMR cluster id and log-bucket name are returned as stack Outputs, but no bootstrap-action log object ever appears: ListObjectsV2(Bucket=<log bucket>, Prefix="logs/<cluster-id>/node/") keeps returning no key containing "bootstrap-actions"/"stdout", so the check never gets a verdict and ends as `Check timed out after 60.0s`. This is consistent with the bootstrap script never being fetched or executed at all (the same non-execution that open issue 20261116T232636Z-emr-3-net-new-causes-app-ui-presigned.md records for RunJobFlow-launched clusters in its Causes 2 and 3) — but that issue is filed against service `emr`, carries only the three `emr.*` replay keys, and says nothing about S3 delivery of bootstrap-action logs under LogUri or about the CloudFormation-declared BootstrapActions path, so it does not cover this check.

Why:      a client that bootstraps its nodes from an S3 script gets a cluster that silently never ran its customization: packages are not installed, config is not written, and nothing in the API or in S3 says so. Any operator runbook or test that reads the bootstrap-action stdout under logs/<cluster-id>/node/.../bootstrap-actions/ to confirm or debug the bootstrap waits forever on an object that is never written, and the "LogUri set => logs are created" contract is broken for the node logs. A CFN-driven EMR deployment therefore reports success for a cluster that is not actually provisioned as declared.

Fix:      the RunJobFlow/AWS::EMR::Cluster bootstrap-action execution path and the cluster's log-delivery path — the BootstrapActions handling of the RunJobFlow handler plus the cluster state machine in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs (the same engine named by the open emr issue above), and whatever writes the cluster's LogUri archive: during BOOTSTRAPPING each ScriptBootstrapAction must be fetched from its S3 Path and run per node, and its stdout and stderr archived into the LogUri bucket under logs/<cluster-id>/node/<instance-id>/bootstrap-actions/<index>/stdout(.gz) and .../stderr(.gz) before the cluster is reported ready.

Traces:   emr__glue__s3.cfn-deploy-emr-step-s3-bootstrap-positive

## Evidence
Behavioral replay against the booted emulator, service heading `### emr__glue__s3`.

Check emr__glue__s3.cfn-deploy-emr-step-s3-bootstrap-positive [mismatch]:
  setup:    CreateBucket <script bucket>; PutObject key "scripts/bootstrap.sh" body "#!/bin/bash\necho \"sim2real-bootstrap-ran-ok\"\n"; CreateStack of a template whose AWS::EMR::Cluster (ReleaseLabel emr-6.15.0, Applications [Hadoop], 1x m5.xlarge master, KeepJobFlowAliveWhenNoSteps true) carries BootstrapActions=[{Name:"s3-sourced-bootstrap", ScriptBootstrapAction:{Path:"s3://<script bucket>/scripts/bootstrap.sh"}}] and LogUri "s3://<stack-created LogBucket>/logs/"; stack Outputs give ClusterId and LogBucketName.
  expected: polling ListObjectsV2(Bucket=<LogBucket>, Prefix="logs/<cluster-id>/node/") yields at least one key containing both "bootstrap-actions" and "stdout", and GetObject of that key (gunzipped when the key ends .gz) returns a body containing the script's echoed marker text.
  actual:   no such key is ever listed; the check exhausts the harness budget and the suite reports `Error: Check timed out after 60.0s`. No bootstrap-action stdout or stderr object is delivered under the cluster's LogUri prefix at any point, i.e. the declared bootstrap script leaves no trace of having run.
  note:     the cluster id, bucket names and marker string are per-run values; the requirement is the key layout logs/<cluster-id>/node/<instance-id>/bootstrap-actions/<index>/stdout and that its body carries the script's stdout.

## Steps to Reproduce
1. Boot the emulator.
2. s3:CreateBucket <script-bucket> and PutObject key "scripts/bootstrap.sh" with body `#!/bin/bash\necho "sim2real-bootstrap-ran-ok"\n`.
3. cloudformation:CreateStack (Capabilities CAPABILITY_IAM, CAPABILITY_NAMED_IAM) with a template containing: an AWS::S3::Bucket LogBucket; an AWS::IAM::Role trusting elasticmapreduce.amazonaws.com with AmazonElasticMapReduceRole; an AWS::IAM::Role trusting ec2.amazonaws.com with AmazonElasticMapReduceforEC2Role plus an AWS::IAM::InstanceProfile for it; and an AWS::EMR::Cluster with ReleaseLabel emr-6.15.0, Applications [{Name: Hadoop}], Instances.MasterInstanceGroup 1x m5.xlarge, KeepJobFlowAliveWhenNoSteps true, JobFlowRole/ServiceRole refs, LogUri "s3://${LogBucket}/logs/" and BootstrapActions [{Name: s3-sourced-bootstrap, ScriptBootstrapAction: {Path: "s3://<script-bucket>/scripts/bootstrap.sh"}}]; Outputs ClusterId and LogBucketName.
4. Wait for StackStatus CREATE_COMPLETE and read the ClusterId and LogBucketName outputs.
5. Poll s3:ListObjectsV2(Bucket=<LogBucketName>, Prefix="logs/<ClusterId>/node/") for any key containing "bootstrap-actions" and "stdout": observe that none is ever returned, so the poll never terminates and the check times out.
6. After the fix, re-replay emr__glue__s3.cfn-deploy-emr-step-s3-bootstrap-positive and confirm the bootstrap stdout object exists under that prefix and contains the script's echoed output.
