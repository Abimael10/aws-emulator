Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__codebuild__dynamodb__ec2__ecs__kinesis__rds__route53__s3__sqs__ssm.cdk-storage-estate-two-regions-serves-and-destroys
Issue-Key: athena__codebuild__dynamodb__ec2__ecs__kinesis__rds__route53__s3__sqs__ssm.cdk-storage-estate-two-regions-serves-and-destroys|_run_cli#13|AssertionError|assert result.returncode == 0, (
Issue-Commit: 792e2d56472eaa6a918bc292baf30b2d329971ab

# athena__codebuild__dynamodb__ec2__ecs__kinesis__rds__route53__s3__sqs__ssm: a Custom::AWS (AwsCustomResource) resource in a 30-stack CDK estate never gets a response delivered to CloudFormation, so `cdk deploy --all` fails with "CloudFormation did not receive a response from your Custom Resource"

> _Note: this issue was identified against the code base at commit `792e2d56472eaa6a918bc292baf30b2d329971ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause for this service heading.

Cause 1 of 1: a Lambda-backed CDK custom resource (Custom::AWS, emitted by aws-cdk-lib's AwsCustomResource) never completes, so a stack of the estate fails to deploy and `cdk deploy --all` exits non-zero.

AWS: recorded behavior only — the CloudFormation Smithy model describes nothing about the custom-resource response protocol (CreateStack's input is StackName/TemplateBody/... and its only modeled errors are AlreadyExistsException, InsufficientCapabilitiesException, LimitExceededException, TokenAlreadyExistsException, all 400; no error covers a provider that does not answer). On real AWS, a custom resource's provider function is invoked with the CloudFormation custom-resource request (RequestType Create/Update/Delete, ResponseURL, StackId, RequestId, LogicalResourceId, ResourceProperties) and signals completion by PUTting a SUCCESS or FAILED response document to the pre-signed ResponseURL; CloudFormation consumes that document and moves the resource to CREATE_COMPLETE or CREATE_FAILED. For this estate the providers do answer, so `cdk deploy --all` exits 0, every stack reaches CREATE_COMPLETE, and the DynamoDB tables the AwsCustomResource populates (its BatchWriteItem/PutItem calls) are readable afterwards. The failure sentence AWS emits only when no response arrives before the timeout is the fixed text "CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [<requestId>]. If you are using the Python cfn-response module, you may need to update your Lambda function code so that CloudFormation can attach the updated version.", in which the only variable part is the request id (the bracketed uuid in the recording is a per-run value, not part of the requirement); on real AWS this sentence is not produced for these stacks at all.

Emulator: the response never reaches CloudFormation. Of the thirty stacks the estate deploys, the StatusTable stack's PopulateItems resource (logical id PopulateItems<suffix>, type Custom::AWS, the CDK path <stack>/PopulateItems/Resource/Default) never reaches a terminal state; the deploy eventually fails and the CLI reports "❌ <stack> failed to deploy / DeploymentError: Resource updates failed: <stack>/PopulateItems/Resource/Default (Custom::AWS PopulateItems<suffix>) CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]", and `cdk deploy --all` exits 1 instead of 0. So either the provider Lambda is never invoked with the custom-resource request, or the ResponseURL it is handed cannot be PUT to / is never read back by the stack engine. Because the deploy aborts there, the rest of the estate's assertions (the seeded DynamoDB item scans on the ItemsTable/StatusTable outputs, and everything after them) never run.

Why: every CDK construct built on a custom resource is unusable against the emulator. A client cannot provision any estate containing an AwsCustomResource: the deploy neither succeeds nor fails cleanly, it hangs to a timeout and then exits 1 with a message that on AWS indicates a broken user function, sending the client to debug code that is correct. Data the custom resource was to seed (here the status/items rows) is absent, and the partially created stacks are left behind for the client to clean up.

Fix: CloudFormation's custom-resource provider dispatch and response-collection path — the invocation of the provider Lambda with the custom-resource request, the pre-signed ResponseURL endpoint that accepts the provider's SUCCESS/FAILED document, and the stack engine's consumption of it to transition the resource and the stack (and the same on Update/Delete, so a destroy also completes).

Related (does not cover this heading or this check): 20261117T115047Z-cloudformation-ecr-iam-logs-s3-ssm-sts-2.md and 20261117T115022Z-cloudformation-ecr-iam-logs-s3-ssm-sts-2.md report the same class of breakage for Custom::CloudWatchLogsResourcePolicy / Custom::CDKBucketDeployment under the cloudformation__ecr__iam__logs__s3__ssm__sts heading; 20261108T043811Z-cloudformation-s3-a-lambda-backed-cloudformation-custom-resource-never-completes.md and 20261105T204906Z-cloudformation-dynamodb-lambda-a-custom-resources-provider-custom.md report it for other constructs. None of them carries this check's replay key, and the suite did not mark this check as owned.

Traces: athena__codebuild__dynamodb__ec2__ecs__kinesis__rds__route53__s3__sqs__ssm.cdk-storage-estate-two-regions-serves-and-destroys.

## Evidence
Check athena__codebuild__dynamodb__ec2__ecs__kinesis__rds__route53__s3__sqs__ssm.cdk-storage-estate-two-regions-serves-and-destroys [mismatch]:

  AssertionError: `cdk deploy --all` exited 1, expected 0; output tail:
    "❌  <prefix>-StatusTable failed to deploy
     ‣ DeploymentError: Resource updates failed:
     <prefix>-StatusTable/PopulateItems/Resource/Default  (Custom::AWS PopulateItems8F5E621A)
       CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId
       [<uuid>]. If you are using the Python cfn-response module, you may need to update your Lambda function code
       so that CloudFormation can attach the updated version."

Expected: `cdk deploy --all` exits 0; the StatusTable stack reaches CREATE_COMPLETE with its Custom::AWS PopulateItems resource CREATE_COMPLETE, and the check then scans the seeded tables (e.g. StatusTable rows user-001/user-005 with status "active", user-003/user-004 with pending|suspended).
Actual: exit 1; the Custom::AWS resource never completes and CloudFormation reports the no-response sentence as the resource status reason, aborting the whole 30-stack deploy.

Message parts: the fixed text is "CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]. If you are using the Python cfn-response module, you may need to update your Lambda function code so that CloudFormation can attach the updated version."; the bracketed request id, the stack-name prefix (sim2real-ds-<account-suffix>), the logical-id suffix and the uuid are per-run values, not requirements. The requirement is that this sentence is NOT produced: the provider's response must be delivered and the resource must reach a terminal state.

## Steps to Reproduce
1. Boot the emulator and point the AWS CDK at it (the check's endpoint/credential wiring).
2. Synthesize the two-region, thirty-stack storage estate app (primary us-east-1, secondary us-west-2) in which the StatusTable stack seeds its DynamoDB table via an aws-cdk-lib custom_resources.AwsCustomResource (Custom::AWS, logical path <stack>/PopulateItems/Resource/Default).
3. Run `cdk deploy --all --ci --no-version-reporting --no-notices`.
4. Expected: exit 0, every stack CREATE_COMPLETE, and the seeded rows readable via DynamoDB Scan.
   Actual: exit 1 — "<stack>-StatusTable failed to deploy / DeploymentError: Resource updates failed: ... (Custom::AWS PopulateItems...) CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [...]".
5. Minimal repro: deploy a single stack containing one AwsCustomResource whose onCreate performs a DynamoDB PutItem, and watch DescribeStackEvents — the Custom::AWS resource never leaves CREATE_IN_PROGRESS and then fails with the no-response reason.
