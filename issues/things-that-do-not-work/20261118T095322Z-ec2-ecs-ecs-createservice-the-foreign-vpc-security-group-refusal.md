Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs.ecsdeploy-service-security-group-from-other-vpc-rejected
Issue-Key: ec2__ecs.ecsdeploy-service-security-group-from-other-vpc-rejected|run#14|AssertionError|assert got == ("InvalidParameterException",
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# ECS CreateService: the foreign-VPC security-group refusal carries a generic message instead of AWS's 'Security group ... does not appear to belong to the same VPC as the input subnets.'

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: CreateService with an awsvpcConfiguration whose securityGroups belong to a different VPC than its subnets is refused with the right code but the wrong message.

AWS: the call fails with InvalidParameterException and message 'Security group <the security group id from the request> does not appear to belong to the same VPC as the input subnets.' - fixed text 'Security group ' / ' does not appear to belong to the same VPC as the input subnets.', with the group id taken from the request. Smithy: ecs CreateService declares InvalidParameterException (client, HTTP 400) among its errors; the message text is recorded behaviour only. The refused call creates nothing (DescribeServices reports the name under failures with reason MISSING), which the emulator already satisfies.

Emulator: answers InvalidParameterException with 'You have specified two resources that belong to different networks.' - a generic sentence that names neither the security group nor the subnets. The same generic sentence is served for the subnets-span-two-VPCs validation (see the sibling issue filed under ec2__ecs__elasticache for tgwc-service-subnets-spanning-ecs-and-cache-vpcs-rejected), so the two distinct validations are indistinguishable.

Why: a client or operator cannot tell which resource was wrong - the group or the subnets - and tooling that matches AWS's message to explain the failure or to pick a remediation gets no usable information.

Fix: the CreateService handler's awsvpcConfiguration cross-VPC validation (its validation layer) - emit AWS's per-case message naming the offending security group, and keep the subnets case distinct.

Related: the resolved issue 20261104T062603Z-ec2-ecs-createservice-accepts-an-empty-subnets-list.md added this refusal; the refusal now happens but with emulator-authored wording, which is what this issue reports.

Traces: ec2__ecs.ecsdeploy-service-security-group-from-other-vpc-rejected.

## Evidence
Expected: ('InvalidParameterException', 'Security group <sg-id> does not appear to belong to the same VPC as the input subnets.')

Actual (replay): AssertionError: a security group from VPC 'vpc-...096' beside a subnet of 'vpc-...08d' must be refused; got ('InvalidParameterException', 'You have specified two resources that belong to different networks.'). The vpc/sg ids are per-run values; the fixed message text is the requirement.

## Steps to Reproduce
1. Create a VPC with one subnet and a security group in it; take a second security group from a different VPC.
2. Create an ECS cluster and register a FARGATE task definition.
3. CreateService(launchType='FARGATE', networkConfiguration={'awsvpcConfiguration': {'subnets': [<subnet of VPC A>], 'securityGroups': [<sg of VPC B>], 'assignPublicIp': 'DISABLED'}}) and read Error.Code/Message.
