Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__vpc-lattice.deploy-vpclattice-gateway-configuration-and-associations-round-trip-and-delete
Issue-Key: cloudformation__ec2__vpc-lattice.deploy-vpclattice-gateway-configuration-and-associations-round-trip-and-delete|stack_outputs#10|KeyError|return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Commit: 86a81857af943b6387678f8429ed876a9a758ecc

# cloudformation__ec2__vpc-lattice: DescribeStacks serializes a CREATE_COMPLETE stack's Outputs entries without OutputValue, so every declared stack output is unreadable

> _Note: this issue was identified against the code base at commit `86a81857af943b6387678f8429ed876a9a758ecc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
One net-new root cause for this service heading. The only failing check under `### cloudformation__ec2__vpc-lattice` is not marked OWNED or ALREADY FILED in the replay report (the four `cloudformation__vpc-lattice` failures and the one `lambda__vpc-lattice` failure are OWNED and are skipped).

Cause 1 of 1: DescribeStacks returns Stack.Outputs entries that carry OutputKey but no OutputValue member at all, so a client that reads a declared stack output of a settled stack crashes instead of receiving the resolved value.

AWS: Smithy services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json — DescribeStacks -> Stack$Outputs is list<com.amazonaws.cloudformation#Output>, and Output declares OutputKey ('The key associated with the output'), OutputValue ('The value associated with the output'), Description (@length 1..1024) and ExportName. AWS resolves every entry of the template's Outputs section (Ref, Fn::GetAtt, Fn::Sub, Fn::ImportValue) while the stack settles and populates OutputValue with the resolved value; a stack whose declared Output cannot be resolved never reaches CREATE_COMPLETE, it fails the create. Both the SDKs and the CLI (`--query 'Stacks[0].Outputs[?OutputKey==`X`].OutputValue'`) index that member unconditionally. There is no success response in which an Outputs entry exists with a key and no value.

Emulator: the stack reaches CREATE_COMPLETE (the check's `wait_stack` assertion on CREATE_COMPLETE passed), DescribeStacks answers 200 with a non-empty Stacks[0].Outputs, and each entry is serialized with OutputKey present and OutputValue absent. Building the output map `{o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}` raises `KeyError: 'OutputValue'` on the first entry. The defect is the missing member; the key set (whatever the template declares) and the resolved values (per run) are not part of the requirement. In this check the template declares 17 Outputs over EC2 (Ref Vpc, Ref Subnet, Fn::GetAtt Sg.GroupId) and VpcLattice resources (Ref/Fn::GetAtt Id and Arn of ServiceNetwork, ResourceGateway, ResourceConfiguration, ServiceNetworkResourceAssociation, ServiceNetworkVpcAssociation, plus Fn::GetAtt SnVpcAssoc.Status), and none of them can be read.

Why: a client relying on AWS's behavior crashes rather than degrading. Everything a stack publishes through the contract its template defines is unreachable — the ids and ARNs a consumer reads back, anything a sibling stack imports — so any deployment that hands ids out through Outputs (the normal multi-stack and CDK pattern) is unusable past the first stack, even though the stack itself created successfully.

Fix: the CloudFormation DescribeStacks response serializer together with the stack-output resolution in the CloudFormation stack state that builds Stack.Outputs — resolve each declared Output's Value against the settled stack's resource ids/attributes and emit it as the OutputValue member (and Description/ExportName when the template declares them).

Related open issue, named for the fixer, that reports the same symptom under a DIFFERENT service heading and does not carry this check's replay key: 20261122T232606Z-cloudformation-docdb-dynamodb-ecs-elbv2-firehose-lambda-rds-f1eb40.md (and the issue it in turn names, 20261113T113502Z-apigateway-cloudformation-lambda-describestacks-serializes-a-create-complete.md). Neither is resolved and neither covers this check; it may share a fix, but it must be re-replayed here.

Traces: cloudformation__ec2__vpc-lattice.deploy-vpclattice-gateway-configuration-and-associations-round-trip-and-delete.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout. Suite total 15/21; this failure is [error] and is NOT marked OWNED or ALREADY FILED.

[error] cloudformation__ec2__vpc-lattice.deploy-vpclattice-gateway-configuration-and-associations-round-trip-and-delete
  File "check_cloudformation__ec2__vpc_lattice_deploy_vpclattice_gateway_configuration_and_associations_round_trip_and_delete.py", line 280, in run
    outputs = stack_outputs(cfn, stack_id)
  File "...", line 127, in stack_outputs
    return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
  KeyError: 'OutputValue'

The preceding assertion `status == "CREATE_COMPLETE"` passed, so the stack settled successfully and DescribeStacks answered 200 with Outputs present — only the value member is missing.

Expected (AWS): DescribeStacks on a settled stack answers 200 and every Stacks[0].Outputs entry is {"OutputKey": "<the name declared in the template's Outputs section>", "OutputValue": "<the value that Output's Ref/Fn::GetAtt resolves to in this stack's state>"}, plus Description/ExportName where declared. Fixed part of the contract: the member is always present on a settled stack. Variable part: the key names come from the request's template, the values from the emulator's own state (vpc-…, subnet-…, sg-…, the VpcLattice ids/ARNs and the association Status).
Actual (emulator): each entry carries OutputKey and omits OutputValue entirely, so indexing "OutputValue" raises KeyError and `aws cloudformation describe-stacks --query '...OutputValue'` yields null.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack with a template that declares an Outputs section whose Values are Ref / Fn::GetAtt of resources in the stack — e.g. Resources {Vpc: AWS::EC2::VPC, Subnet: AWS::EC2::Subnet, Sg: AWS::EC2::SecurityGroup, ServiceNetwork: AWS::VpcLattice::ServiceNetwork, ResourceGateway: AWS::VpcLattice::ResourceGateway, ResourceConfiguration: AWS::VpcLattice::ResourceConfiguration, SnResourceAssoc: AWS::VpcLattice::ServiceNetworkResourceAssociation, SnVpcAssoc: AWS::VpcLattice::ServiceNetworkVpcAssociation} and Outputs {VpcId: {Value: {Ref: Vpc}}, SgId: {Value: {Fn::GetAtt: [Sg, GroupId]}}, ServiceNetworkArn: {Value: {Fn::GetAtt: [ServiceNetwork, Arn]}}, SnVpcAssocStatus: {Value: {Fn::GetAtt: [SnVpcAssoc, Status]}}, ...}.
3. Poll DescribeStacks(StackName=<StackId>) until StackStatus is CREATE_COMPLETE.
4. Inspect Stacks[0].Outputs: each entry has OutputKey but no OutputValue member, where AWS returns the resolved value. `{o["OutputKey"]: o["OutputValue"] for o in Outputs}` raises KeyError: 'OutputValue'.
5. Re-replay cloudformation__ec2__vpc-lattice.deploy-vpclattice-gateway-configuration-and-associations-round-trip-and-delete.
