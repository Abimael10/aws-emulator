Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.sdk-ssm-send-command-executes-on-ec2-instance-positive cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress
Issue-Key: cloudformation__ec2__ssm.sdk-ssm-send-command-executes-on-ec2-instance-positive|run#12|AssertionError|assert result["Status"] == "Success", (
Issue-Key: cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive|_run_shell#9|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress|_read_boot#18|AssertionError|assert set(fields) == {"BOOTID", "BOOTTIME"}, f"unparseable boot read: {out!r}"
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# SSM run-command on a CloudFormation-provisioned EC2 instance never executes: every invocation ends Failed with empty stdout and stderr, and the instance's boot output is empty

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
One net-new root cause (three failing checks).

Cause 1 of 1: an EC2 instance created by a CloudFormation stack with an SSM-capable instance profile registers as a managed node, but every AWS-RunShellScript invocation sent to it terminates in status 'Failed' with StandardOutputContent and StandardErrorContent both empty, and the instance's console/boot output is empty too.

AWS: Smithy (ssm) models SendCommand -> Command and GetCommandInvocation -> GetCommandInvocationResult with Status (CommandInvocationStatus) and StandardOutputContent/StandardErrorContent. For a reachable managed node running a valid shell document, AWS drives the invocation to Status 'Success' with ResponseCode 0 and StandardOutputContent carrying exactly what the script printed. 'Failed' is reserved for a script that exited non-zero, and AWS then still reports the script's stdout/stderr and a non-zero ResponseCode. EC2 GetConsoleOutput returns the guest's boot log. The documents' execution semantics are recorded behavior only; the Status/stdout contract is from the model plus the recording.

Emulator: the invocation is accepted (SendCommand returns a CommandId and the node is listed by DescribeInstanceInformation) and then settles on Status 'Failed' / StatusDetails 'Failed' with stdout='' and stderr='' — nothing the script printed is ever reported, so no in-guest observation is possible. The same instance's boot read comes back as the two-newline string '\n\n', i.e. no console output at all. Instance ids, tag values and stack names in the recordings are per-run values; the requirement is that a healthy managed node's valid invocation reaches 'Success' and reports the script's stdout.

Why: every client that uses CloudFormation to stand up an instance and then SSM to configure, inspect or verify it gets a terminal 'Failed' with no diagnostic content. Automation cannot distinguish "the node is unreachable", "the document is wrong" and "the script exited 1", because all three collapse to 'Failed' with empty output; and tag-targeted automation cannot even resolve the node (DescribeInstanceInformation with the stack's instance tag filter returned an empty list in one of these checks).

Fix: the SSM command-invocation execution path — the component that delivers a document to a managed node and collects its exit status and output streams — and the EC2 instance's console/boot output state it reads from.

Traces: cloudformation__ec2__ssm.sdk-ssm-send-command-executes-on-ec2-instance-positive (command was not executed by the instance: status='Failed' details='Failed'); cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive (guest command failed with status 'Failed': stdout='' stderr=''); cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress (unparseable boot read: '\n\n').

## Evidence
Expected (AWS): GetCommandInvocation reaches Status 'Success' with ResponseCode 0 and StandardOutputContent equal to the script's stdout; a script that genuinely fails reports Status 'Failed' WITH a non-zero ResponseCode and its stdout/stderr. EC2 GetConsoleOutput returns the guest's boot log.

Actual (emulator):
- AssertionError: command was not executed by the instance: status='Failed' details='Failed'
- AssertionError: guest command failed with status 'Failed': stdout='' stderr=''
- AssertionError: unparseable boot read: '\n\n'
- in the same family, AssertionError: expected describe_instance_information(tag:<stack tag>='<value>') to resolve to ['<instance id>'], got []

## Steps to Reproduce
1. Create a CloudFormation stack that provisions a VPC/subnet and an AWS::EC2::Instance with an IAM instance profile carrying AmazonSSMManagedInstanceCore; wait for CREATE_COMPLETE.
2. Wait for ssm.DescribeInstanceInformation to list the instance (optionally filtering by the stack's instance tag).
3. Call ssm.SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[<id>], Parameters={'commands': ['echo hello']}) and poll ssm.GetCommandInvocation.
4. AWS reaches Status 'Success' with StandardOutputContent 'hello'. The emulator reaches Status 'Failed' with empty stdout and stderr.
5. Call ec2.GetConsoleOutput(InstanceId=<id>): AWS returns a boot log; the emulator returns empty output.
