Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips|_cdk#17|AssertionError|assert result.returncode == expect_rc, (
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected|run#37|AssertionError|assert status == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention|_retention_failure_reasons#15|AssertionError|assert (len(created), len(deleted)) == (1, 1), (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# cloudformation__ecr__iam__logs__s3__ssm__sts: 2 net-new causes — a Lambda-backed CDK custom resource's response never reaches CloudFormation (deploys and rollbacks hang, then fail with "CloudFormation did not receive a response from your Custom Resource"), and a rollback emits no DELETE_FAILED event for the custom resource it could not delete

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two distinct root causes in CloudFormation's custom-resource lifecycle, both reached through `cdk deploy` against the emulator.

Cause 1 of 2: a Lambda-backed custom resource's response is never delivered to CloudFormation, so the resource never completes and the stack ends in a non-terminal-then-failed state.
AWS: a custom resource provider (an AWS::Lambda::Function invoked with the CloudFormation custom-resource request, as emitted by CDK's AwsCustomResource / BucketDeployment / LogRetention constructs) signals completion by PUTting a SUCCESS or FAILED response document to the pre-signed ResponseURL in the request. CloudFormation polls for that response and transitions the resource to CREATE_COMPLETE / CREATE_FAILED (or UPDATE_*/DELETE_* for the matching request type) accordingly. The failure sentence CloudFormation emits when no response arrives within the timeout is the fixed text `CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [<requestId>]. If you are using the Python cfn-response module, you may need to update your Lambda function code so that CloudFormation can attach the updated version.`, with requestId the only variable part — and on real AWS it is NOT emitted for these stacks, because the provider does respond: `cdk deploy --require-approval never` exits 0 and the stack reaches CREATE_COMPLETE / UPDATE_COMPLETE, and a stack whose custom resource legitimately fails reaches ROLLBACK_COMPLETE. (Recorded behavior; the CloudFormation model does not describe the response protocol.)
Emulator: no response ever arrives. The Custom::CloudWatchLogsResourcePolicy resource of an AwsCustomResource lifecycle deploy times out and `cdk deploy` exits 1 with DeploymentError "Resource updates failed: ... CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]", and the Custom::CDKBucketDeployment resource of a denied-put deploy leaves the stack in ROLLBACK_FAILED — not ROLLBACK_COMPLETE — with that same sentence as the status reason. So either the provider function is never invoked with the custom-resource request, or the ResponseURL it is handed cannot be PUT to / is not read back by the stack engine.
Why: every CDK construct built on a custom resource is unusable. A deploy neither succeeds nor fails cleanly: it hangs to a timeout and then exits non-zero, and a stack that should roll back is left stuck in ROLLBACK_FAILED, which on AWS requires operator intervention and blocks any further update or delete of that stack. No client can provision or tear down a stack containing AwsCustomResource, BucketDeployment or LogRetention.
Fix: CloudFormation's custom-resource provider dispatch and response-collection path — the invocation of the provider Lambda with the custom-resource request, the pre-signed ResponseURL endpoint that accepts the provider's SUCCESS/FAILED document, and the stack engine's consumption of it to transition the resource and the stack. Must also carry the resource to a terminal state on the Delete request so rollback can reach ROLLBACK_COMPLETE.
Traces: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips, cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected.
Related: many open issues report custom-resource breakage under OTHER service headings (e.g. 20261108T043811Z-cloudformation-s3-a-lambda-backed-cloudformation-custom-resource-never-completes.md, 20261107T081525Z-cloudformation-lambda-sqs-a-custom-resource-that-answers.md, 20261108T003043Z-cloudformation-lambda-ssm-a-cdk-custom-resource-whose.md); none covers this service heading or these checks.

Cause 2 of 2: rollback of a failed custom resource records only the CREATE_FAILED event, never the DELETE_FAILED one.
AWS: when a Custom::LogRetention resource fails to create (here because the log group name is invalid) and the stack rolls back, DescribeStackEvents for that stack shows TWO rows for that logical resource: a CREATE_FAILED from the failed create, and then a DELETE_FAILED from the rollback's attempt to delete the resource that was never successfully created — each carrying its own ResourceStatusReason. Both rows are part of the event history a client reads to learn why a rollback could not complete (recorded behavior).
Emulator: DescribeStackEvents reports only [('Retention8BFB5E80', 'CREATE_FAILED')] — one row, no DELETE_FAILED row for the same logical resource, so the rollback half of the history is missing.
Why: the rollback's own failure is invisible. A client that reads stack events to diagnose why a stack is stuck sees the create failure but no record that the rollback delete was attempted or failed, so it cannot distinguish "rollback deleted the resource" from "rollback could not delete it" — exactly the distinction that determines whether the stack needs manual recovery.
Fix: CloudFormation's rollback path and its stack-event recorder — the rollback must issue the Delete for a resource whose create failed and record the resulting DELETE_FAILED stack event (with its reason) alongside the CREATE_FAILED one.
Traces: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention.

## Evidence
Cause 1 — cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips:
  AssertionError: `cdk deploy --require-approval never` exited 1, expected 0; output tail: "... ❌ <stack> failed to deploy / DeploymentError: Resource updates failed: <stack>/ResourcePolicy/Resource/Default (Custom::CloudWatchLogsResourcePolicy ResourcePolicy7446AF09) CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]"
Cause 1 — cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-bucket-deployment-prefix-noprune-metadata-denied-put-rejected:
  AssertionError: stack reached 'ROLLBACK_FAILED', expected ROLLBACK_COMPLETE after the denied create. Reason: 'CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]'
  Expected: the Custom::CDKBucketDeployment create is refused by the denying bucket policy, the provider reports FAILED, and the stack reaches ROLLBACK_COMPLETE.
Cause 2 — cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-log-retention-invalid-name-rollback-failed-then-valid-sets-retention:
  AssertionError: expected one CREATE_FAILED and one DELETE_FAILED Custom::LogRetention row, got [('Retention8BFB5E80', 'CREATE_FAILED')]
  Expected: DescribeStackEvents shows both a CREATE_FAILED and a DELETE_FAILED row for that logical id, each with its own ResourceStatusReason. Actual: only the CREATE_FAILED row exists.
(The requestIds and logical/stack names above are per-run values; the requirement is the status/terminal state and the presence of both event rows, not these literals.)

## Steps to Reproduce
Cause 1a: synth and `cdk deploy --require-approval never` a stack using AwsCustomResource to put and then update a CloudWatch Logs resource policy (Custom::CloudWatchLogsResourcePolicy). Expected exit 0 and CREATE_COMPLETE/UPDATE_COMPLETE; actual exit 1 with "CloudFormation did not receive a response from your Custom Resource".
Cause 1b: deploy a stack with s3deploy.BucketDeployment (destinationKeyPrefix, prune=false, metadata) into a bucket whose policy DENIES the deployment role's s3:PutObject. Expected the stack to reach ROLLBACK_COMPLETE; actual ROLLBACK_FAILED with the same no-response reason.
Cause 2: deploy a stack with a LogRetention custom resource whose log group name is invalid, then call cloudformation.describe_stack_events on it. Expected both a CREATE_FAILED and a DELETE_FAILED row for the Custom::LogRetention logical id; actual only CREATE_FAILED.
