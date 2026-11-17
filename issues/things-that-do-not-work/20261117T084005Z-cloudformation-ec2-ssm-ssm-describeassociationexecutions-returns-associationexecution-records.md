Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-change-set-preview-ec2-ssm-association-update-positive
Issue-Key: cloudformation__ec2__ssm.cfn-change-set-preview-ec2-ssm-association-update-positive|<lambda>#12|KeyError|lambda e: len(e) >= 1 and e[0]["ResourceCountByStatus"] != "{}",
Issue-Commit: 1a7edcaa7636911df7e69d7fc1e45311aea1d7a5

# cloudformation__ec2__ssm: SSM DescribeAssociationExecutions returns AssociationExecution records with no ResourceCountByStatus member

> _Note: this issue was identified against the code base at commit `1a7edcaa7636911df7e69d7fc1e45311aea1d7a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAssociationExecutions

## Description
One net-new root cause in the SSM State Manager association-execution surface.

=== Cause 1 of 1: the AssociationExecution projection omits ResourceCountByStatus ===

AWS: Smithy model com.amazonaws.ssm#AssociationExecution declares the member ResourceCountByStatus (target ssm#ResourceCountByStatus, string), documented as "An aggregate status of the resources in the execution based on the status type", and it is part of every element of DescribeAssociationExecutionsResult.AssociationExecutions (member AssociationExecutions, target AssociationExecutionsList). AWS populates it on each execution record as a brace-wrapped per-status count string — the aggregate of the execution's resolved targets by status (the brace-wrapped per-status form is the requirement; the concrete statuses and counts come from the association's resolved target set and vary per run, and an execution that resolved no targets carries the empty aggregate "{}"). Presence of the member on every record is what the API guarantees.

Emulator: the records DescribeAssociationExecutions returns for an association created by CloudFormation (AWS::SSM::Association with a tag: target over an EC2 fleet) do carry at least one execution — the list is non-empty — but the record has no ResourceCountByStatus key at all. A client that reads the member off the response dict gets KeyError: 'ResourceCountByStatus' instead of an aggregate string.

Why: ResourceCountByStatus is the only thing in DescribeAssociationExecutions that tells a caller whether an association execution actually resolved any targets, and how those targets came out. Omitting it makes "the association ran and matched the fleet" indistinguishable from "the association ran and matched nothing", and any client or SDK-typed consumer that indexes the modeled member crashes on a response the service says always carries it. It also blocks the whole downstream contract this check exercises — comparing the target aggregate before and after a change set retargets the association from one EC2 fleet tag group to another.

Fix: the DescribeAssociationExecutions handler in the ssm service (the AssociationExecution projection/serializer that builds each list element) must populate ResourceCountByStatus from the execution's per-status resolved-target counts, emitting the empty aggregate when the execution resolved no targets, rather than leaving the member unset.

Related but NOT covering this: open issue 20261114T135329Z-cloudformation-ec2-ssm-6-net-new-causes-ssm.md Cause 2 says State Manager associations never execute and "DescribeAssociationExecutions records nothing". That is a different state: here an execution record IS returned and only the modeled member is missing, and the replay report does not mark this check as owned by that issue.

Traces: cloudformation__ec2__ssm.cfn-change-set-preview-ec2-ssm-association-update-positive.

## Evidence
Failing check cloudformation__ec2__ssm.cfn-change-set-preview-ec2-ssm-association-update-positive [error]:

Error: File "check_cloudformation__ec2__ssm_cfn_change_set_preview_ec2_ssm_association_update_positive.py", line 230, in _poll_until
  if predicate(last):
File "check_cloudformation__ec2__ssm_cfn_change_set_preview_ec2_ssm_association_update_positive.py", line 140, in <lambda>
  lambda e: len(e) >= 1 and e[0]["ResourceCountByStatus"] != "{}",
KeyError: 'ResourceCountByStatus'

The predicate short-circuits on `len(e) >= 1`, so the KeyError proves the emulator returned a non-empty AssociationExecutions list whose first element lacks the member.

Expected (AWS): ssm.describe_association_executions(AssociationId=<id>)["AssociationExecutions"][0] contains ResourceCountByStatus as a brace-wrapped per-status aggregate string (the empty aggregate "{}" only when the execution resolved no targets); the check then asserts it is not the empty aggregate because the association's tag: target matches the deployed fleet.
Actual (emulator): the same element has no ResourceCountByStatus member, so the key lookup raises.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. CreateStack with an IAM role + instance profile carrying AmazonSSMManagedInstanceCore, two AWS::EC2::Instance resources tagged Group=fleet-v1, and an AWS::SSM::Association whose Targets are [{Key: "tag:Group", Values: ["fleet-v1"]}]; wait for CREATE_COMPLETE and read the association id from the stack Outputs.
3. Wait until DescribeInstanceInformation reports both instances PingStatus Online, then call ssm.describe_association_executions(AssociationId=<association id>).
4. Emulator: the returned AssociationExecutions elements have no ResourceCountByStatus key (KeyError when read). AWS: every element carries ResourceCountByStatus, a brace-wrapped per-status aggregate of the execution's resolved targets.
