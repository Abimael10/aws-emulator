Issue-Severity: HIGH
Issue-Commit: 238fa933eaa755325de1be4f75aa8cc4e9812c10

# cloudformation__ec2__neptune: DescribeStacks on a CREATE_COMPLETE Neptune stack returns Outputs entries with an OutputKey but no OutputValue

> _Note: this issue was identified against the code base at commit `238fa933eaa755325de1be4f75aa8cc4e9812c10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
THIS ISSUE HAS 1 CAUSE.

Cause 1 of 1: a stack that reached CREATE_COMPLETE is described with an Outputs list whose entries carry OutputKey but no OutputValue, so every template output is unreadable.
AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) Stack.Outputs -> list of com.amazonaws.cloudformation#Output with members OutputKey ("The key associated with the output"), OutputValue ("The value associated with the output"), Description and ExportName. For a stack in CREATE_COMPLETE, DescribeStacks returns one Output entry per declared template output, and each entry carries BOTH OutputKey and the RESOLVED OutputValue — the value of the output's Value expression (here `!Ref` of an AWS::Neptune::DBCluster / AWS::Neptune::DBInstance, which resolves to the resource's physical id, i.e. the DB cluster / DB instance identifier, and `!GetAtt Cluster.Port` / `!GetAtt Cluster.ClusterResourceId`). The requirement is that OutputValue is present for every key and equals the resolved expression; the particular identifiers are per-run values from the request, not part of the requirement.
Emulator: DescribeStacks on the CREATE_COMPLETE stack returns Outputs entries without the OutputValue member at all — reading `o["OutputValue"]` raises KeyError: 'OutputValue'. The stack's status assertion passed, so the divergence is purely in the serialized Output entries (either the output expressions are never resolved, or the resolved value is dropped by the Output serializer).
Why: Outputs are the documented contract between a stack and its consumers. A client (or a nested/dependent stack, or `aws cloudformation describe-stacks --query Stacks[0].Outputs`) that reads an output to learn the physical id, endpoint or port of a resource the stack created gets a key with no value and cannot proceed — every post-deploy step keyed off an output fails, even though the deploy itself succeeded.
Fix: the DescribeStacks handler's Stack.Outputs projection and the stack state that records resolved output values (output-expression resolution at CREATE_COMPLETE, and the Output structure serializer).
Similar OPEN issues name the same symptom for other stacks but do not carry this check's key and so do not cover it: 20261116T174247Z-cloudformation-cloudfront-3-net-new-causes-handler-reason.md ("stack Outputs carry no OutputValue"), 20261117T134552Z-cloudformation-lexv2-models-a-create-complete-aws-lex.md (Outputs with an OutputKey but no value), and 20261109T134315Z-cloudformation-ec2-neptune-3-net-new-causes-aws.md (Neptune GetAtt outputs) — this failing check is not marked owned by any of them.
Traces: cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: per-service cloudformation__ec2__neptune 0/6 (6 failed); five of those six are marked OWNED by existing issues, this one is not.

[error] cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags:
  File "...two_instances_writer_reader_and_tags.py", line 252, in run -> outputs = stack_outputs(cfn, stack_id)
  File "...", line 102, in stack_outputs -> return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
  KeyError: 'OutputValue'

The stack reached CREATE_COMPLETE (the preceding `assert status == "CREATE_COMPLETE"` passed), so the Outputs list was served and its entries carried OutputKey while OutputValue was absent. The template declares five outputs: ClusterRef (!Ref Cluster), ClusterArn (!GetAtt Cluster.ClusterResourceId), ClusterPort (!GetAtt Cluster.Port), InstanceARef (!Ref InstanceA), InstanceBRef (!Ref InstanceB).

Model evidence (smithy_query, query=shape, shape=Output,Stack on services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json): Output has members OutputKey, OutputValue, Description, ExportName; Stack.Outputs is a list of Output.

Not claimed as requirements: the generated stack name/id and the per-run Neptune cluster/instance identifiers that the outputs resolve to — only the presence of OutputValue for every declared output, and its equality with the resolved Ref/GetAtt, are.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this checkout, and replay the simparity-behavioral-aws-v1 behavioral suite.
2. Replay cloudformation__ec2__neptune.cfn-deploy-neptune-cluster-two-instances-writer-reader-and-tags. Or directly: CreateStack with a template declaring Outputs whose Values are !Ref of an AWS::Neptune::DBCluster and AWS::Neptune::DBInstance plus !GetAtt Cluster.Port / Cluster.ClusterResourceId, wait for CREATE_COMPLETE, then DescribeStacks by StackId.
3. Inspect Stacks[0].Outputs: every entry must carry an OutputValue member alongside OutputKey, and each value must equal the resolved expression (the !Ref outputs must equal the DB cluster / DB instance identifiers the template supplied). Today reading OutputValue raises KeyError because the member is absent.
