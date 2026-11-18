Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__elasticache.tgwc-service-subnets-spanning-ecs-and-cache-vpcs-rejected
Issue-Key: ec2__ecs__elasticache.tgwc-service-subnets-spanning-ecs-and-cache-vpcs-rejected|run#15|AssertionError|assert msg == want, f"the refusal should name the VPC mismatch: expected {want!r}, got {msg!r}"
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# ECS CreateService: subnets spanning two VPCs are refused with a generic message instead of 'All input subnets must be from the same VPC.'

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: CreateService with an awsvpcConfiguration whose subnets come from two different VPCs is refused with the right code but the wrong message.

AWS: the call fails with InvalidParameterException and the fixed message 'All input subnets must be from the same VPC.' (no variable parts). Smithy: ecs CreateService declares InvalidParameterException (client, HTTP 400); the message text is recorded behaviour only. The refused call must create nothing - DescribeServices reports the service name under failures with reason MISSING - which the emulator already satisfies.

Emulator: answers InvalidParameterException with 'You have specified two resources that belong to different networks.' - the same generic sentence it uses for the foreign-VPC security-group validation (filed separately under ec2__ecs for ecsdeploy-service-security-group-from-other-vpc-rejected), so the two validations cannot be told apart.

Why: a client that matches AWS's message to report or repair the misconfiguration cannot learn that the SUBNETS are the problem; the generic text also hides which of the two cross-VPC checks fired.

Fix: the CreateService handler's awsvpcConfiguration subnet validation (its validation layer) - emit 'All input subnets must be from the same VPC.' for subnets drawn from more than one VPC.

Related open work that does NOT carry this check's replay key: 20261108T093128Z-ec2-ecs-elasticache-ecs-createservice-accepts-an-awsvpcconfiguration-whose-subnets-span.md reported this request being ACCEPTED; it is now refused, but with the wrong message.

Traces: ec2__ecs__elasticache.tgwc-service-subnets-spanning-ecs-and-cache-vpcs-rejected.

## Evidence
Expected: ('InvalidParameterException', 'All input subnets must be from the same VPC.')

Actual (replay): AssertionError: the refusal should name the VPC mismatch: expected 'All input subnets must be from the same VPC.', got 'You have specified two resources that belong to different networks.' (the code InvalidParameterException already matches).

## Steps to Reproduce
1. Build two VPCs, each with one private subnet in the same AZ (e.g. an ECS VPC and an ElastiCache VPC), plus a security group in the first VPC.
2. Create an ECS cluster and register a FARGATE task definition.
3. CreateService(launchType='FARGATE', desiredCount=0, networkConfiguration={'awsvpcConfiguration': {'subnets': [<subnet in VPC A>, <subnet in VPC B>], 'securityGroups': [<sg in VPC A>], 'assignPublicIp': 'DISABLED'}}) and read Error.Code/Message.
