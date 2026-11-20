Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative
Issue-Key: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative|_poll_until#26|Timeout|time.sleep(interval_s)
Issue-Commit: c5b9149d70e2a9aad6a1bceb8aa9795c91a9cab7

# CloudFormation SSM-on-EC2 flows never settle: the SSM Association deploy and the CLI update-stack SSM-block flow both run out of budget with no response (2 net-new causes)

> _Note: this issue was identified against the code base at commit `c5b9149d70e2a9aad6a1bceb8aa9795c91a9cab7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two checks in this service group produce no response at all: each is cut off by the harness after its 60 s budget, so no status, error code or message ever comes back to assert on. Neither failing check is marked OWNED or ALREADY FILED in this replay. The open issue 20261117T184012Z-cloudformation-ec2-ssm-ssm-on-ec2-cloudformation-deploys-never-settle-the.md describes the same stall shape for a sibling check (cfn-deploy-ssm-patch-baseline-associated-ec2-positive) and names the Association resource handler; it does not carry the replay keys of the two checks below, so they are filed here and that issue is named rather than duplicated — a fixer should treat them together.

Cause 1 of 2: a stack that wires an AWS::SSM::Document plus an AWS::SSM::Association to a tagged EC2 instance never reaches a terminal stack status.
AWS: recorded behavior only. The stack reaches CREATE_COMPLETE; Fn::GetAtt Association.AssociationId is readable from the stack outputs; the tagged instance appears in SSM DescribeInstanceInformation with PingStatus "Online"; and the association's AWS-RunShellScript invocation reaches Status "Success" with its own invocation record. Every one of those is a polled read that terminates.
Emulator: the check makes no observable progress and is killed at its budget with only "Check timed out after 60.0s" — no stack status, no error code, no message. Either the stack stays non-terminal or one of the SSM reads never settles, so nothing is returned to compare.
Why: a client that waits on stack_create_complete (or on PingStatus/invocation Status) for an SSM-on-EC2 deploy gets neither success nor failure, so retry, rollback and alarm paths never fire; the deploy is indistinguishable from a hang.
Fix: the AWS::SSM::Document / AWS::SSM::Association resource handlers' Create together with the stack's progress accounting (the stack must be driven to a terminal status), and the SSM association-execution path that must make the instance report Online and the invocation reach a terminal Status.
Traces: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive

Cause 2 of 2: the CLI create-stack / update-stack + Run Command negative flow never settles either.
AWS: recorded behavior only. The `aws cloudformation create-stack` call returns a StackId and the stack settles CREATE_COMPLETE; a following `update-stack` that drops outbound 443 from the target instance's security group settles UPDATE_COMPLETE; the restarted target then drops off SSM, so a Run Command to it is refused outright or ends DeliveryTimedOut with no output while the control instance succeeds; restoring the egress rule makes the same command succeed. Every step returns a terminal answer.
Emulator: the check is cut off at its 60 s budget with only "Check timed out after 60.0s" — no stack status and no Run Command status are returned, so neither the deploy nor the negative-delivery assertion can be evaluated.
Why: the whole class of "a security-group change withdraws SSM reachability" behavior is unverifiable, and any client polling an update-stack or a SendCommand invocation on this estate waits forever instead of seeing UPDATE_COMPLETE or a terminal invocation status.
Fix: the CloudFormation UpdateStack path for an in-place AWS::EC2::SecurityGroup egress change (it must reach a terminal stack status) and the SSM Run Command delivery path that must settle to a terminal invocation status for an unreachable managed node.
Traces: cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative

## Evidence
Both failing checks report exactly one line of detail: `Error: Check timed out after 60.0s`. No HTTP status, no error code and no error message were produced by the emulator, which is itself the divergence: AWS answers each polled read with a terminal value.

Expected (AWS):
- cfn-deploy-ssm-document-association-ec2-positive: DescribeStacks -> StackStatus "CREATE_COMPLETE"; outputs carry InstanceId and AssociationId; DescribeInstanceInformation(Filters=[{Key:"InstanceIds",Values:[<instance>]}]) -> one entry with PingStatus "Online"; the association's AWS-RunShellScript invocation -> Status "Success".
- cli-script-cfn-update-stack-security-group-blocks-ssm-negative: create-stack then update-stack settle CREATE_COMPLETE / UPDATE_COMPLETE; the Run Command to the egress-blocked target is refused or ends "DeliveryTimedOut" with empty output while the control ends "Success"; after the egress rule is restored the same command ends "Success".

Actual (emulator): no response at all for either check — the harness terminated each at its budget. Sibling checks in the same service group return concrete responses inside the same budget, so the budget is not the differentiator.

## Steps to Reproduce
Replay the behavioral suite and re-run:
1. cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive — create the stack (AWS::SSM::Document + AWS::SSM::Association targeting the instance by tag) and poll DescribeStacks: it must reach CREATE_COMPLETE, the AssociationId output must resolve, DescribeInstanceInformation must report PingStatus "Online" for the instance, and the association's invocation must reach Status "Success".
2. cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative — via the aws CLI, create-stack and wait for CREATE_COMPLETE, then update-stack with the target's outbound 443 removed and wait for UPDATE_COMPLETE; send a Run Command to the target and assert it is refused or ends DeliveryTimedOut with no output while the control succeeds, then restore the egress rule and assert the same command succeeds.
In both cases the requirement is that every polled read returns a terminal value rather than the call never settling.
