Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__logs__rds.cfn-aurora-cluster-and-instance-properties-round-trip
Issue-Key: cloudformation__logs__rds.cfn-aurora-cluster-and-instance-properties-round-trip|run#18|KeyError|assert instance["PromotionTier"] == _PROMOTION_TIER, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# RDS DescribeDBInstances omits PromotionTier for an Aurora DB instance created with it

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstances

## Description
Cause 1 of 1: a modeled DBInstance member is absent from the read path.
AWS: Smithy rds DBInstance$PromotionTier is documented as 'The order of priority in which an Aurora Replica is promoted to the primary instance after a failure of the existing primary instance'; it is part of every DBInstance AWS returns (defaulting to 1 when the request did not set it), so DescribeDBInstances on an Aurora instance created by CloudFormation with PromotionTier always reports it.
Emulator: the DBInstance in the DescribeDBInstances response has no PromotionTier key at all — reading it raises KeyError — so the stack's declared value cannot be round-tripped or verified.
Why: a client that reads back failover priority (CDK/CFN drift reads, any Aurora failover-ordering tooling) crashes on the missing key rather than seeing its configured tier, and a template property is silently unobservable.
Fix: the DBInstance serializer / instance state in the RDS read path (DescribeDBInstances and the matching Create/ModifyDBInstance state that must retain the member and its default).
Failing check: cloudformation__logs__rds.cfn-aurora-cluster-and-instance-properties-round-trip

## Evidence
[error] cloudformation__logs__rds.cfn-aurora-cluster-and-instance-properties-round-trip — KeyError: 'PromotionTier' at `assert instance["PromotionTier"] == _PROMOTION_TIER`; the DescribeDBInstances DBInstance carries no PromotionTier member, expected the value the stack declared (an integer priority; the recording's own value is just the template's input).

## Steps to Reproduce
1. Deploy a stack with an AWS::RDS::DBCluster (Aurora) plus an AWS::RDS::DBInstance declaring PromotionTier.
2. Call DescribeDBInstances for the instance and inspect the DBInstance for a PromotionTier member.
