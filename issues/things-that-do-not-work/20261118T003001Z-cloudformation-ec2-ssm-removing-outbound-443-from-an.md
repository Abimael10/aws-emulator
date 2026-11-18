Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative
Issue-Key: cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative|run#27|AssertionError|assert ping_after_restart != "Online", (
Issue-Commit: 0129204ced37f63133d2cc688ac6df5b5ea216be

# cloudformation/ec2/ssm: removing outbound 443 from an instance's security group never disconnects it from SSM — the restarted node reports PingStatus Online

> _Note: this issue was identified against the code base at commit `0129204ced37f63133d2cc688ac6df5b5ea216be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceInformation

## Description
One net-new cause.

Cause 1 of 1: an SSM managed node's registration/ping state is independent of its security group's egress rules — after a CloudFormation stack update rewrites the node's security group to drop outbound 443 and the instance is stop/started so its agent must open a FRESH connection, the emulator still reports the node Online and Run Command still reaches it.
- AWS: recorded behavior only (no Smithy member models reachability; SSM DescribeInstanceInformation output carries PingStatus of InstanceInformation, and GetCommandInvocation carries Status/StatusDetails/StandardOutputContent). The SSM Agent reaches Systems Manager over outbound TCP 443. With the instance's only security group permitting egress on port 22 alone, a freshly started agent cannot establish that connection, so DescribeInstanceInformation never reports PingStatus 'Online' again for that instance (it reports 'ConnectionLost' or the node drops out of the managed-node list), and a command addressed to it either is refused at SendCommand with error code InvalidInstanceId and the fixed message "Instances not in a valid state for account", or is accepted and ends in a terminal non-Success invocation with StatusDetails 'DeliveryTimedOut' and an empty StandardOutputContent. A second instance in the same stack whose security group was untouched stays Online and runs the same AWS-RunShellScript document to Success with its stdout returned. Restoring unrestricted egress makes the identical command succeed again, which is what proves the egress rule — not an unrelated fluke — was the cause.
- Emulator: the stack update completes (UPDATE_COMPLETE) and the target is stop/started, yet DescribeInstanceInformation reports PingStatus 'Online' for the target within the poll window. The egress rules of the attached security group are not consulted when deciding managed-node connectivity, so the emulator is strictly more permissive than AWS and the disconnected-target branch of the check (InvalidInstanceId refusal / DeliveryTimedOut invocation) is never reachable.
- Why: this is the control that operators rely on to prove a node is network-isolated. A client that removes egress 443 to quarantine an instance, then verifies the quarantine through PingStatus and a probe command, is told the isolation succeeded in AWS and is told the node is still fully reachable here — or, worse for test suites, a negative test asserting that the blocked node CANNOT be commanded passes against AWS and fails here, while any code path that assumes an Online node is genuinely reachable silently diverges. Egress-based break/fix runbooks and incident-containment automation cannot be exercised at all.
- Fix: the managed-node connectivity model shared by the EC2 security-group state and SSM's node registration — PingStatus and command delivery for an instance must be derived from whether the instance's current security-group egress rules permit the agent's outbound 443 flow (re-evaluated when the instance restarts and the agent reconnects), with SendCommand/GetCommandInvocation taking the refusal (InvalidInstanceId, "Instances not in a valid state for account") or the DeliveryTimedOut path for a node that cannot be reached.
- Traces: cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative.

## Evidence
Behavioral suite replay of this checkout: 352/639 checks passed (55.1%); 193 mismatched, 94 errored, bootstrap_signal=false. Service heading 'cloudformation__ec2__ssm': 26/37 passed, 11 failed; this check was marked neither 'ALREADY FILED' nor 'OWNED by'.

cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative [mismatch]:
  File "check_cloudformation__ec2__ssm_cli_script_cfn_update_stack_security_group_blocks_ssm_negative.py", line 131, in run
    assert ping_after_restart != "Online", (
  AssertionError: target unexpectedly reconnected to SSM after outbound 443 was removed: Online

Sequence actually replayed: `aws cloudformation create-stack` with two SSM-managed EC2 instances (control + target) → both reach PingStatus Online → `aws cloudformation update-stack` with a template whose target security group allows egress on port 22 only → UPDATE_COMPLETE → StopInstances/StartInstances on the target → DescribeInstanceInformation polled for up to 240 s. Expected: the target never returns to 'Online' (ConnectionLost, or absent), the untouched control still runs AWS-RunShellScript to Success with its marker in StandardOutputContent, and the target's command is either refused InvalidInstanceId "Instances not in a valid state for account" or ends non-Success with StatusDetails='DeliveryTimedOut' and StandardOutputContent=''. Observed: PingStatus 'Online' for the target, so the run stopped at that assertion and the delivery-failure legs were never evaluated. Stack/instance ids and the stack name are per-run values.

## Steps to Reproduce
Boot the emulator from this checkout and replay cloudformation__ec2__ssm.cli-script-cfn-update-stack-security-group-blocks-ssm-negative, or reproduce by hand:
1. create-stack a template with two EC2 instances that each carry an instance profile holding AmazonSSMManagedInstanceCore, each in its own security group with unrestricted egress; wait for CREATE_COMPLETE and for both instances to show PingStatus 'Online' in ssm DescribeInstanceInformation.
2. update-stack the same stack with a template identical except that the TARGET instance's security group egress is rewritten to allow TCP 22 only (no outbound 443); wait for UPDATE_COMPLETE.
3. StopInstances then StartInstances the target and wait for state 'running', so its agent must open a fresh outbound connection.
4. Poll ssm DescribeInstanceInformation for the target for ~240 s — expect it never to reach 'Online' again; observe PingStatus 'Online'.
5. SendCommand AWS-RunShellScript to the control (expect Success with its stdout) and to the target (expect either InvalidInstanceId "Instances not in a valid state for account", or a terminal non-Success invocation with StatusDetails 'DeliveryTimedOut' and empty StandardOutputContent).
6. update-stack back to unrestricted egress and re-send the identical command to the target — it must succeed, proving the egress rule was the cause.
