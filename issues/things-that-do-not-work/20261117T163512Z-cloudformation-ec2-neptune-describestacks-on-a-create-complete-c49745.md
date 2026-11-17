Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags
Issue-Key: cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags|stack_outputs#16|KeyError|return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# cloudformation__ec2__neptune: DescribeStacks on a CREATE_COMPLETE Neptune two-instance stack serves Outputs entries with an OutputKey but no OutputValue

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
THIS ISSUE HAS 1 CAUSE.

TRIAGE NOTE (read first): the open issue 20261117T150412Z-cloudformation-ec2-neptune-describestacks-on-a-create-complete.md describes this same symptom and already names this exact failing check in its Traces section, but the replay report does NOT mark this check as OWNED by it (five of the six cloudformation__ec2__neptune failures are marked OWNED, this one is not), so the check is still unowned by the replay-key bookkeeping and is filed here to keep the divergence attached to a replayable key. If that issue is still open, dedupe these two — they are one cause, not two.

Cause 1 of 1: a stack that reached CREATE_COMPLETE is described with an Outputs list whose entries carry OutputKey but no OutputValue, so every declared template output is unreadable.
AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) com.amazonaws.cloudformation#Output has members OutputKey ("The key associated with the output"), OutputValue ("The value associated with the output"), Description and ExportName; Stack.Outputs is a list of Output. For a stack in CREATE_COMPLETE, DescribeStacks returns one Output entry per declared template output and each entry carries BOTH OutputKey and the RESOLVED OutputValue — the value of that output's Value expression. Here the expressions are !Ref of an AWS::Neptune::DBCluster and of two AWS::Neptune::DBInstance resources (each resolving to the resource's physical id, i.e. the DB cluster / DB instance identifier the template supplied) and !GetAtt Cluster.Port / !GetAtt Cluster.ClusterResourceId. The requirement is that OutputValue is PRESENT for every declared key and equals the resolved expression; the particular per-run cluster/instance identifiers, stack name and stack id are values from the request and state, not part of the requirement.
Emulator: DescribeStacks by StackId on the CREATE_COMPLETE stack returns Outputs entries with no OutputValue member at all — reading o["OutputValue"] raises KeyError: 'OutputValue'. The preceding status assertion (status == "CREATE_COMPLETE") passed, so the create and the Outputs list itself were served; the divergence is confined to the serialized Output entries: either the output expressions are never resolved at CREATE_COMPLETE, or the resolved value is dropped by the Output serializer.
Why: stack Outputs are the documented contract between a stack and its consumers. A client, a nested or dependent stack, or `aws cloudformation describe-stacks --query 'Stacks[0].Outputs'` that reads an output to learn the physical id, endpoint or port of a resource the stack created receives a key with no value and cannot proceed — every post-deploy step keyed off an output fails even though the deploy itself succeeded. Ref/GetAtt outputs are the only supported way to learn a CloudFormation-generated identifier.
Fix: the DescribeStacks handler's Stack.Outputs projection, together with the stack state that records resolved output values (output-expression resolution for Ref/Fn::GetAtt at CREATE_COMPLETE) and the Output structure serializer.
Traces: cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Per-service: cloudformation__ec2__neptune 0/6 (0%), 6 failed; five of those six are marked OWNED by existing issues, this one is not.

[error] cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags:
  File "check_cloudformation__ec2__neptune_cfn_deploy_neptune_cluster_two_instances_writer_reader_and_tags.py", line 252, in run
    outputs = stack_outputs(cfn, stack_id)
  File "...", line 102, in stack_outputs
    return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
  KeyError: 'OutputValue'

The check's template declares five outputs: ClusterRef (!Ref Cluster), ClusterArn (!GetAtt Cluster.ClusterResourceId), ClusterPort (!GetAtt Cluster.Port), InstanceARef (!Ref InstanceA), InstanceBRef (!Ref InstanceB). The assertion immediately before the failure (`assert status == "CREATE_COMPLETE"`) passed, so the stack was created and described successfully and the Outputs list was served with entries lacking OutputValue.

Model evidence (smithy_query query=shape shape=Output on services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json): Output = { OutputKey, OutputValue, Description, ExportName }.

Expected: Stacks[0].Outputs contains one entry per declared output, each with OutputKey and an OutputValue equal to the resolved expression (the !Ref outputs equal the DB cluster / DB instance identifiers passed as template parameters).
Actual: entries present with OutputKey, OutputValue member absent.

Not claimed as requirements: the generated stack name/id and the per-run Neptune cluster/instance identifiers the outputs resolve to; only the presence of OutputValue for every declared output and its equality with the resolved Ref/GetAtt expression are.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this checkout, then replay the simparity-behavioral-aws-v1 behavioral suite.
2. Replay cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags.
3. Or directly: CreateStack with a template that creates a VPC, two subnets, a security group, an AWS::Neptune::DBSubnetGroup, an AWS::Neptune::DBCluster and two AWS::Neptune::DBInstance resources, and declares Outputs whose Values are !Ref Cluster, !Ref InstanceA, !Ref InstanceB, !GetAtt Cluster.Port and !GetAtt Cluster.ClusterResourceId. Poll DescribeStacks(StackName=<StackId>) until CREATE_COMPLETE.
4. Inspect Stacks[0].Outputs: every entry must carry an OutputValue alongside OutputKey, each equal to the resolved expression (the !Ref outputs equal the DB cluster / DB instance identifiers supplied in the template parameters). Today reading OutputValue raises KeyError because the member is absent.
