Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-deploys-serves-and-destroys cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints
Issue-Key: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-deploys-serves-and-destroys|run#16|KeyError|outputs[part] = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Key: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints|run#16|KeyError|outputs[part] = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Commit: f2e6a4662e66854bb360a08423e8578f1603b4d0

# cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds: DescribeStacks serializes a settled stack's Outputs entries without OutputValue, so every CDK stack output is unreadable

> _Note: this issue was identified against the code base at commit `f2e6a4662e66854bb360a08423e8578f1603b4d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
One net-new root cause, reached by both nine-stack CDK estate checks of this service heading. Neither check is marked OWNED or ALREADY FILED in the replay report.

Cause 1 of 1: DescribeStacks returns Stack.Outputs entries that carry OutputKey but no OutputValue member at all, so a client that reads a declared stack output crashes.

AWS: Smithy services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json — DescribeStacks -> Stack$Outputs is list<com.amazonaws.cloudformation#Output>, and Output has members OutputKey ('The key associated with the output'), OutputValue ('The value associated with the output'), Description and ExportName. AWS resolves every Output declared in the template (Ref, Fn::GetAtt, Fn::ImportValue of another stack's export, Fn::Sub) and populates OutputValue with the resolved value; a stack whose declared output cannot be resolved never reaches CREATE_COMPLETE. Both the SDKs and the CLI (`--query 'Stacks[0].Outputs[?OutputKey==`X`].OutputValue'`) index the member unconditionally, and CDK's cross-stack wiring consumes exports/outputs the same way.

Emulator: for stacks that have settled, each entry of Stacks[0].Outputs is serialized with OutputKey present and OutputValue absent. Both checks build the per-stack output map `{o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}` for the nine estate stacks (AccessRoles, Networking, S3Storage, Monitoring, Database, LambdaMisc, ECS, ComputeEC2, DocDBAthenaNLB) and raise `KeyError: 'OutputValue'` on the first entry — i.e. the call answered 200 with Outputs present, only the value member is missing. The absent member is the defect; the key names (per template) and the values (per run) are not.

Why: a client relying on AWS's behavior crashes instead of degrading. Nothing a stack publishes can be addressed through the contract the template defines — no VPC/subnet ids a consumer stack imports, no bucket name, table name, cluster endpoint, delivery-stream name, ALB DNS name or role ARN — so every multi-stack deployment (CDK estates in particular, where Networking's outputs are imported by five consumer stacks) is unusable past the first stack.

Fix: the CloudFormation DescribeStacks response serializer plus the stack-output resolution in the CloudFormation stack state that builds Stack.Outputs — resolve each declared Output's Value and emit it as the OutputValue member.

Related open issue that describes the same symptom under a different service heading and does NOT carry these checks' replay keys, named for the fixer: 20261113T113502Z-apigateway-cloudformation-lambda-describestacks-serializes-a-create-complete.md. It is not resolved and does not cover these two checks.

Traces: cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-deploys-serves-and-destroys and .cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout; both failures are [error], not marked OWNED/ALREADY FILED.

[error] cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-deploys-serves-and-destroys
  File "check_..._cdk_nine_stack_estate_deploys_serves_and_destroys.py", line 105, in run
    outputs[part] = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
  KeyError: 'OutputValue'

[error] cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints
  File "check_..._cdk_nine_stack_estate_delivery_stream_echoes_buffering_hints.py", line 105, in run
    outputs[part] = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
  KeyError: 'OutputValue'

Expected (AWS): DescribeStacks on a settled stack answers 200 and each Stacks[0].Outputs entry is {"OutputKey": "<the template's Output name>", "OutputValue": "<the resolved value from the stack's own state>"} (plus Description/ExportName when declared). The key set is the template's; the values are per-run.
Actual (emulator): each entry carries OutputKey and omits OutputValue entirely, so indexing it raises KeyError.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack (or `cdk deploy`) a template that declares at least one Outputs entry whose Value is a Ref or Fn::GetAtt of a resource in the stack — e.g. Outputs: {VpcId: {Value: {Ref: Vpc}, Export: {Name: <stack>-VpcId}}}.
3. Poll DescribeStacks until the stack settles (CREATE_COMPLETE).
4. Read Stacks[0].Outputs: each entry has OutputKey but no OutputValue member, where AWS returns the resolved value; indexing "OutputValue" raises KeyError and the AWS CLI's `--query ...OutputValue` yields null.
5. Re-replay cloudformation__docdb__dynamodb__ecs__elbv2__firehose__lambda__rds.cdk-nine-stack-estate-deploys-serves-and-destroys and .cdk-nine-stack-estate-delivery-stream-echoes-buffering-hints.
