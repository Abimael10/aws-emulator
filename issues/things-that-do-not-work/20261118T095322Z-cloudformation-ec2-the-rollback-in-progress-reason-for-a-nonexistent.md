Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.parameter-ec2-typed-nonexistent-ids-roll-back
Issue-Key: cloudformation__ec2.parameter-ec2-typed-nonexistent-ids-roll-back|run#13|AssertionError|assert rollback_reasons == [expected_reason], f"nonexistent {key}: rollback reasons {rollback_reasons!r}"
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# The ROLLBACK_IN_PROGRESS reason for a nonexistent EC2-typed parameter value carries a doubled period

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the stack-level ROLLBACK_IN_PROGRESS event reason for an AWS-specific parameter type whose value does not exist is one character off AWS's text - the sentence-ending period of the validation sentence is emitted twice.

AWS: a stack whose parameter is typed AWS::EC2::VPC::Id / AWS::EC2::SecurityGroup::Id / List<AWS::EC2::Subnet::Id> and is given an id that does not exist rolls back, and the stack event for the stack resource itself (LogicalResourceId == stack name, ResourceStatus ROLLBACK_IN_PROGRESS) carries ResourceStatusReason 'Parameter validation failed: parameter value <value> for parameter name <name> does not exist. Rollback requested by user.' The fixed text is 'Parameter validation failed: parameter value ', ' for parameter name ', ' does not exist. Rollback requested by user.'; <value> and <name> are taken from the request. Recorded behavior only for the exact wording; the status (ROLLBACK_COMPLETE) matches already.

Emulator: it serves '... does not exist.. Rollback requested by user.' - the message is built by appending '. Rollback requested by user.' to a validation sentence that already ends in a period, so two periods run together. Everything else about the leg matches: the stack does reach ROLLBACK_COMPLETE and the Handle resource is never created.

Why: a client that matches or asserts on the documented rollback reason (deployment tooling that surfaces or classifies parameter-validation failures) does not match the emulator's text, and operators reading the event see malformed prose.

Fix: the CloudFormation parameter-validation path that composes the stack-level rollback reason - join the validation sentence and the 'Rollback requested by user.' suffix with a single space without re-adding the terminating period.

Traces: cloudformation__ec2.parameter-ec2-typed-nonexistent-ids-roll-back (the VpcId case is the first to fail; the GroupId and Subnets cases exercise the same composer).

## Evidence
Expected rollback reasons == ['Parameter validation failed: parameter value <vpc-id> for parameter name VpcId does not exist. Rollback requested by user.']

Actual (replay): AssertionError: nonexistent VpcId: rollback reasons ['Parameter validation failed: parameter value vpc-f6a1185a8a1a4760a for parameter name VpcId does not exist.. Rollback requested by user.'] - note the '..'. The vpc id shown is a per-run generated value; the doubled period is the defect.

## Steps to Reproduce
1. CreateStack with a template whose Parameters include VpcId of type AWS::EC2::VPC::Id, and pass a well-formed but nonexistent vpc id.
2. Poll DescribeStacks until ROLLBACK_COMPLETE.
3. DescribeStackEvents and read ResourceStatusReason of the event whose LogicalResourceId is the stack name and ResourceStatus is ROLLBACK_IN_PROGRESS: it contains 'does not exist.. Rollback requested by user.' instead of 'does not exist. Rollback requested by user.'
