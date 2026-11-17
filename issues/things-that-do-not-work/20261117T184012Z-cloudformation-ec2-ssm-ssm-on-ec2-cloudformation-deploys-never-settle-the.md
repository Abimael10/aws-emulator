Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive cloudformation__ec2__ssm.cfn-deploy-ssm-patch-baseline-associated-ec2-positive
Issue-Key: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive|_poll_until#14|Timeout|last = probe()
Issue-Key: cloudformation__ec2__ssm.cfn-deploy-ssm-patch-baseline-associated-ec2-positive|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# SSM-on-EC2 CloudFormation deploys never settle: the SSM Association and PatchBaseline checks do not complete

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: two CloudFormation stacks that wire SSM resources to an EC2 instance never reach a terminal state within the suite's budget.
AWS: recorded behavior only. A stack declaring an AWS::SSM::Document plus AWS::SSM::Association against a running instance, and a stack declaring an AWS::SSM::PatchBaseline associated with an instance, each reach CREATE_COMPLETE and the association/baseline is then readable through the SSM API (the Association check reads Fn::GetAtt Association.AssociationId out of the stack's outputs).
Emulator: neither check ever produces a response to assert on — each is cut off by the harness after its 60 s budget with no terminal stack status, so the deploy is still in progress (or stuck) when the budget runs out. No status, error code or message was returned to compare.
Why: a client that waits on stack_create_complete for an SSM-on-EC2 deploy never gets an answer; the deploy neither succeeds nor fails, so retry and rollback logic has nothing to act on. Sibling checks in the same suite that deploy comparable stacks do settle inside the same budget, so the stall is specific to this resource combination.
Fix: the AWS::SSM::Association and AWS::SSM::PatchBaseline resource handlers' Create (and the stack's progress accounting, which must drive the stack to a terminal status).
Traces: cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive, cloudformation__ec2__ssm.cfn-deploy-ssm-patch-baseline-associated-ec2-positive

## Evidence
Both checks report only `Error: Check timed out after 60.0s` — no stack status, no error code and no message were returned. The surrounding checks in the same service group fail fast with concrete responses, so the budget itself is not the differentiator.

## Steps to Reproduce
Replay the behavioral suite and re-run:
1. cloudformation__ec2__ssm.cfn-deploy-ssm-document-association-ec2-positive
2. cloudformation__ec2__ssm.cfn-deploy-ssm-patch-baseline-associated-ec2-positive
In each, create the stack and poll DescribeStacks: it must reach CREATE_COMPLETE rather than remain in progress.
