Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kafka.deploy-cfn-msk-topic-round-trip-and-delete cloudformation__kafka.deploy-cfn-msk-topic-rejects-a-cluster-that-does-not-exist
Issue-Key: cloudformation__kafka|CreateStack|ValidationError
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# CloudFormation refuses AWS::MSK::Topic as an unrecognized resource type

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the AWS::MSK::Topic resource type is not registered with CloudFormation.
AWS: recorded behavior only. AWS::MSK::Topic is a supported resource type: a stack declaring one creates the topic on the named MSK cluster and reaches CREATE_COMPLETE, and a Topic naming a cluster that does not exist fails that resource and settles the stack at ROLLBACK_COMPLETE with the MSK handler's refusal in the resource's CREATE_FAILED ResourceStatusReason.
Emulator: CreateStack is refused synchronously with ValidationError and the message `Template format error: Unrecognized resource types: [AWS::MSK::Topic]`, so neither the positive nor the negative path is reachable and no stack is created at all.
Why: a client cannot manage MSK topics through CloudFormation; the template is rejected up front, which is a different failure class from the per-resource failure AWS reports for a bad cluster, so error handling keyed on a rolled-back stack never runs.
Fix: the CloudFormation resource-type registry (register AWS::MSK::Topic) and that type's resource handler.
Traces: cloudformation__kafka.deploy-cfn-msk-topic-round-trip-and-delete, cloudformation__kafka.deploy-cfn-msk-topic-rejects-a-cluster-that-does-not-exist

## Evidence
Both checks fail at CreateStack: `botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::MSK::Topic]`. Expected: the round-trip stack reaches CREATE_COMPLETE; the stack whose Topic names a nonexistent cluster reaches ROLLBACK_COMPLETE with the MSK handler reason on the MskTopic resource.

## Steps to Reproduce
Replay the behavioral suite and re-run:
1. cloudformation__kafka.deploy-cfn-msk-topic-round-trip-and-delete
2. cloudformation__kafka.deploy-cfn-msk-topic-rejects-a-cluster-that-does-not-exist
Each calls CreateStack with a template containing an AWS::MSK::Topic resource.
