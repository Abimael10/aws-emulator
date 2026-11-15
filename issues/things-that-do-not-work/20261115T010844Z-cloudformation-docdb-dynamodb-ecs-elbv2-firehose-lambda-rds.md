Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints
Issue-Key: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints|_run_cli#14|AssertionError|assert result.returncode == 0, (
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds: a nine-stack CDK estate cannot deploy — AWS::EC2::Instance is refused InvalidRequest because the emulator does not apply its LaunchTemplate property

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation's AWS::EC2::Instance resource handler refuses any instance declared with a LaunchTemplate, which fails the whole stack and with it the nine-stack estate that would have exercised the Firehose delivery stream's buffering hints.

AWS: AWS::EC2::Instance accepts the LaunchTemplate property (a LaunchTemplateSpecification naming a launch template by id or name plus a version) and creates the instance from it, so the stack containing it reaches CREATE_COMPLETE. A CDK-synthesised EC2 instance emits exactly that shape. Smithy: CloudFormation's CreateStack models no per-resource-type error — a resource handler that cannot satisfy a declared property surfaces as a stack-level failure event, so AWS's contract here is that the property is APPLIED, not refused (recorded behavior: `cdk deploy --all` exits 0 and every stack reaches CREATE_COMPLETE).

Emulator: the ComputeEC2 stack fails to deploy with a resource event on `AWS::EC2::Instance TestInstance`: `Resource handler returned message: "this emulator does not apply the LaunchTemplate property of AWS::EC2::Instance" (HandlerErrorCode: InvalidRequest)`. The CDK CLI then exits 1 with DeploymentError, so no later stack in the estate deploys and nothing downstream is observable.

Why: every CDK or CloudFormation template that places an EC2 instance behind a launch template — the default shape for instance metadata options, IMDSv2 and block-device defaults — cannot be deployed at all. The failure is not confined to EC2: it aborts the multi-stack deploy, so unrelated resources in later stacks (here the Firehose delivery stream whose BufferingHints the check grades) are never created.

This is the same underlying gap as the open issue 20261109T142303Z-apigateway-cloudformation-dynamodb-lambd-aws-ec2-instance-ignores-the-launchtemplate-property-the.md, but that issue carries a different service's replay key and does not cover this check; it should be fixed together with this one.

Fix: the AWS::EC2::Instance resource provider in the CloudFormation handler — resolve the LaunchTemplate specification and apply its settings when creating the instance instead of returning InvalidRequest.

Traces: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints.

## Evidence
Expected: `cdk deploy --all` exits 0 and all nine stacks reach CREATE_COMPLETE, after which the Firehose delivery stream echoes its declared BufferingHints.

Actual: `cdk deploy --all` exited 1. Stack event tail:
```
❌  <prefix>-ComputeEC2 failed to deploy
‣ DeploymentError: Resource updates failed:
<prefix>-ComputeEC2/TestInstance  (AWS::EC2::Instance TestInstance)
  Resource handler returned message: "this emulator does not apply the LaunchTemplate property of AWS::EC2::Instance"
  (RequestToken: <token>, HandlerErrorCode: InvalidRequest)
```
The message text is emulator-authored — AWS has no such refusal for this property; the stack name and request token are per-run values.

## Steps to Reproduce
1. Synthesize a CDK app (or a plain CloudFormation template) containing an AWS::EC2::Instance whose LaunchTemplate property names a launch template plus version.
2. Deploy it against the emulator (`cdk deploy --all`, or CreateStack + DescribeStackEvents).
3. Observe the AWS::EC2::Instance resource event carrying HandlerErrorCode InvalidRequest and the message `this emulator does not apply the LaunchTemplate property of AWS::EC2::Instance`, and the stack entering a failed state.
Replay check cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints to confirm the fix.
