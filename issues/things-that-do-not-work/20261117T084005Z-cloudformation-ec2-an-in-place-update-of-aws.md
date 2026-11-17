Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.cfn-ec2-security-group-create-update-in-place-replace-retained-then-delete
Issue-Key: cloudformation__ec2.cfn-ec2-security-group-create-update-in-place-replace-retained-then-delete|_expect#20|AssertionError|raise AssertionError(
Issue-Commit: 1a7edcaa7636911df7e69d7fc1e45311aea1d7a5

# cloudformation__ec2: an in-place update of AWS::EC2::SecurityGroup answers NotImplemented, so the stack rolls back instead of rewriting the ingress rule

> _Note: this issue was identified against the code base at commit `1a7edcaa7636911df7e69d7fc1e45311aea1d7a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
One net-new root cause in the CloudFormation-provisioned EC2 security-group surface.

=== Cause 1 of 1: updating the properties of an existing AWS::EC2::SecurityGroup is not implemented, so the stack rolls back ===

AWS: recorded behavior only for the exact event payloads; the rule itself is the CloudFormation template reference for AWS::EC2::SecurityGroup, where SecurityGroupIngress is an updatable property ("Update requires: No interruption") while GroupDescription is create-only. An UpdateStack whose only change is the ingress port of an AWS::EC2::SecurityGroup is therefore applied in place: the resource keeps its physical id (the same sg-... GroupId, so Fn::GetAtt GroupId in the stack Outputs does not move), the group's IpPermissions and DescribeSecurityGroupRules rows carry the new FromPort/ToPort, the default allow-all egress rule is untouched, and the stack reaches UPDATE_COMPLETE with an UPDATE_IN_PROGRESS -> UPDATE_COMPLETE pair of events for that logical id and no UPDATE_FAILED. (Only after that does the check's next step change the create-only GroupDescription, which AWS turns into a replacement honouring UpdateReplacePolicy: Retain — that step is never reached here.)

Emulator: the update is refused at the resource handler. The stack ends UPDATE_ROLLBACK_COMPLETE with StackStatusReason "The following resource(s) failed to update: [SecurityGroup]. " and DescribeStackEvents carries ('SecurityGroup', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::EC2::SecurityGroup in place; the resource was left as it is.'). In that reason only the logical id (SecurityGroup, taken from the template) varies; the rest is emulator-authored text that no AWS stack event ever carries — AWS has no NotImplemented resource-status reason for this.

Why: a client cannot evolve a security group through its own stack at all. Widening or narrowing a rule by editing the template — the ordinary way security posture is changed under CloudFormation — rolls the whole update back, leaves the live rules at their old values, and reports a failure that tells the caller it is an emulator limitation rather than anything a template change can fix. Because the in-place step fails first, everything downstream of it in the same stack (the create-only GroupDescription replacement under UpdateReplacePolicy: Retain, the retained old group, the post-delete InvalidGroup.NotFound reads) is unreachable too.

Fix: the AWS::EC2::SecurityGroup resource provider in the cloudformation service must implement the update path — in place for the updatable rule/tag properties (re-authorizing/revoking the group's permissions against EC2) and replacement for the create-only ones — instead of returning a NotImplemented resource failure.

Related but NOT covering this: open issue 20261115T034057Z-cloudformation-ec2-3-net-new-causes-an-in.md Cause 1 reports the same NotImplemented shape for the separate AWS::EC2::SecurityGroupIngress resource type, and open issue 20261114T135329Z-cloudformation-ec2-ssm-6-net-new-causes-ssm.md Cause 5 reports an egress-rule stack update rolling back under the cloudformation__ec2__ssm heading. Neither names this check or the AWS::EC2::SecurityGroup provider's own update path, and the replay report does not mark this check as owned by either.

Traces: cloudformation__ec2.cfn-ec2-security-group-create-update-in-place-replace-retained-then-delete.

## Evidence
Failing check cloudformation__ec2.cfn-ec2-security-group-create-update-in-place-replace-retained-then-delete [mismatch]:

AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [SecurityGroup]. '), expected 'UPDATE_COMPLETE'; failed events: [('SecurityGroup', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::EC2::SecurityGroup in place; the resource was left as it is.')]

Expected (AWS): StackStatus UPDATE_COMPLETE; DescribeStackResources still reports the same physical sg- id for logical id SecurityGroup; DescribeSecurityGroups/DescribeSecurityGroupRules show the rule at the new port with the same group description and the default 0.0.0.0/0 egress rule intact.
Actual (emulator): StackStatus UPDATE_ROLLBACK_COMPLETE, StackStatusReason "The following resource(s) failed to update: [SecurityGroup]. ", resource event UPDATE_FAILED with the NotImplemented sentence above. Fixed text of that emulator sentence: "NotImplemented: this emulator does not change the properties of an existing AWS::EC2::SecurityGroup in place; the resource was left as it is."; the only request-derived part is the logical id quoted in the stack reason.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. CreateStack with an AWS::EC2::VPC and an AWS::EC2::SecurityGroup (logical id SecurityGroup, UpdateReplacePolicy: Retain) whose Properties carry a GroupDescription, VpcId: {Ref: Vpc} and a single SecurityGroupIngress entry (IpProtocol tcp, FromPort=ToPort=8443, CidrIp 198.51.100.0/24). Wait for CREATE_COMPLETE and note the group id from Fn::GetAtt SecurityGroup.GroupId.
3. UpdateStack with the identical template except FromPort=ToPort=9443, then poll DescribeStacks.
4. Emulator: the stack settles UPDATE_ROLLBACK_COMPLETE and DescribeStackEvents shows SecurityGroup UPDATE_FAILED with the NotImplemented reason. AWS: UPDATE_COMPLETE, the same group id, and DescribeSecurityGroupRules showing tcp 9443 from 198.51.100.0/24 plus the default egress rule.
