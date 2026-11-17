Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips|_cdk#17|AssertionError|assert result.returncode == expect_rc, (
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected|run#37|AssertionError|assert status == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention|_retention_failure_reasons#15|AssertionError|assert (len(created), len(deleted)) == (1, 1), (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation__ecr__iam__logs__s3__ssm__sts: 2 causes — Lambda-backed CDK custom resources never deliver a response to CloudFormation, and a failed Custom::LogRetention replacement emits no DELETE_FAILED row

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two net-new root causes in the CloudFormation custom-resource path exercised by CDK apps.

Cause 1 of 2: a Lambda-backed custom resource's response never reaches CloudFormation.
AWS: recorded behavior only — a Custom::<Type> resource invokes its provider Lambda with the RequestType/ResponseURL envelope and CloudFormation waits for the provider to PUT a SUCCESS/FAILED response document to that pre-signed ResponseURL. For a succeeding provider the resource reaches CREATE_COMPLETE; for a failing one it reaches CREATE_FAILED with the provider's Reason and the stack then rolls back to ROLLBACK_COMPLETE. The message 'CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [<id>]. If you are using the Python cfn-response module, ...' is AWS's TIMEOUT text (the request id is a per-run value) and must only appear when a provider genuinely never answers.
Emulator: that timeout text is what every custom resource produces. `cdk deploy` of the aws-custom-resource log-group ResourcePolicy app exits 1 with DeploymentError naming Custom::CloudWatchLogsResourcePolicy and that message, and the bucket-deployment negative ends ROLLBACK_FAILED (not ROLLBACK_COMPLETE) with the same reason for its Custom::CDKBucketDeployment — the rollback cannot complete because the delete half never answers either.
Why: CDK's own constructs (AwsCustomResource, BucketDeployment, LogRetention, auto-delete-objects) are all custom resources; none can be deployed or cleanly rolled back, and a stack is left in ROLLBACK_FAILED, a state a client can only escape by hand.
Fix: the custom-resource provider invocation path — the ResponseURL the resource hands the provider Lambda and the receiver that correlates the provider's SUCCESS/FAILED document back to the waiting resource (both for Create and for Delete during rollback).
Failing checks: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips, cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected
Related open issue that looks similar but does not carry these replay keys: 20261113T041707Z-cloudformation-lambda-logs-a-lambda-backed-custom-resource.md.

Cause 2 of 2: a failed Custom::LogRetention create produces no matching DELETE_FAILED event.
AWS: recorded behavior only — when a Custom::LogRetention resource is created with an invalid log-group name the resource records CREATE_FAILED with the provider's reason, and the ensuing rollback's delete of that same logical resource ALSO fails, recording a second row, DELETE_FAILED, for it; the stack ends UPDATE/ROLLBACK_FAILED and both reasons are readable from DescribeStackEvents.
Emulator: only one row exists for the logical resource ('Retention8BFB5E80', 'CREATE_FAILED'); no DELETE_FAILED row for it is ever emitted.
Why: the rollback's failure is invisible, so a client polling events cannot see that the stack needs manual cleanup, and the documented 'rollback failed, then a valid name sets retention' recovery cannot be followed.
Fix: the rollback path's event emission for a resource whose delete fails (a DELETE_FAILED event per logical resource), in the custom-resource handler's rollback bookkeeping.
Failing check: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention

## Evidence
Cause 1: [mismatch] ...deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips — AssertionError: `cdk deploy --require-approval never` exited 1, expected 0; tail: '❌ <stack> failed to deploy / DeploymentError: Resource updates failed: <stack>/ResourcePolicy/Resource/Default (Custom::CloudWatchLogsResourcePolicy ResourcePolicy7446AF09) CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]'. [mismatch] ...deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected — AssertionError: stack reached 'ROLLBACK_FAILED', expected ROLLBACK_COMPLETE after the denied create. Reason: 'CloudFormation did not receive a response from your Custom Resource. ...'.

Cause 2: [mismatch] ...deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention — AssertionError: expected one CREATE_FAILED and one DELETE_FAILED Custom::LogRetention row, got [('Retention8BFB5E80', 'CREATE_FAILED')].

## Steps to Reproduce
1. `cdk deploy` an app whose only resource is an AwsCustomResource calling logs:PutResourcePolicy (Custom::CloudWatchLogsResourcePolicy) and observe the deploy fail with CloudFormation's 'did not receive a response from your Custom Resource' text.
2. Deploy a CDK BucketDeployment with a prefix/metadata/noPrune configuration whose PutObject is denied and poll the stack: it settles ROLLBACK_FAILED instead of ROLLBACK_COMPLETE.
3. Deploy a CDK LogRetention with an invalid log-group name and read DescribeStackEvents for the Custom::LogRetention logical id: only CREATE_FAILED is present, no DELETE_FAILED.
