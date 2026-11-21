Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite
Issue-Key: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite|_run_probe#11|AssertionError|assert inv["Status"] == "Success", (
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# ec2__sts: the instance-profile guest has no `aws` CLI, so an AWS-RunShellScript identity probe ends Failed with 'sh: aws: command not found' and the session's UserId can never be read

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
One net-new root cause under the suite's `### ec2__sts` heading.

Cause 1 of 1: the AWS CLI is not present in the guest the emulator presents for an instance-profile-bearing EC2 instance, so the in-guest identity probe never runs.
AWS: the Amazon Linux 2023 AMI resolved through /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 ships AWS CLI v2 on PATH as `aws`, and the SSM agent executes `AWS-RunShellScript` commands as root with that PATH, so a script line invoking `aws sts get-caller-identity` resolves and signs with the instance profile's IMDS credentials. Smithy (com.amazonaws.ssm): GetCommandInvocation output carries Status (CommandInvocationStatus), ResponseCode (integer), StandardOutputContent (@length 0..24000) and StandardErrorContent (@length 0..8000); a probe script that captures each sub-command's return code and itself exits 0 returns Status `Success`, ResponseCode 0 and a zero-length StandardErrorContent. The behaviour under test is that the resulting STS Arn/UserId is the role-id:instance-id composite — unreachable while the CLI is absent.
Emulator: GetCommandInvocation reports Status `Failed`, and StandardErrorContent is `sh: aws: command not found\nsh: ARC: parameter not set\n`. The fixed text is the shell's `sh: <name>: command not found` and `sh: <name>: parameter not set`; the variable parts are the program name `aws` and the variable name `ARC`, both taken from the submitted script — `ARC` is unset precisely because the `aws` invocation whose return code it was to hold never ran. AWS produces neither line: the probe exits 0 with its values on stdout.
Why: a client cannot observe the identity its instance profile actually projects — the STS Arn and the `<role-id>:<instance-id>` UserId composite that every IAM policy condition, CloudTrail attribution and bucket-policy principal match depends on. More broadly, the single most common guest pattern (using the instance's own credentials through the CLI, in user data, bootstrap scripts, Run Command runbooks and deployment hooks) fails on the emulator with a shell error, and the client cannot distinguish "the command is absent" from "the API denied me". The probe is also written so its own exit status is 0 even when a call is denied, so the Failed status itself is wrong.
Fix: the guest userland/image behind the EC2 instance data plane and the PATH the `AWS-RunShellScript` plugin executes with — provide the AWS CLI and have it sign with the instance profile's IMDS credentials. The SSM and STS API handlers are not at fault; they report the terminal status faithfully.
Traces: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite.

Context: this is the same family as the already-open 20261121T165349Z-ec2-ssm-3-net-new-causes-the-al2023.md (cause 1: no `aws` CLI, filed under service ec2__ssm) and 20261114T220532Z-ec2-sts-an-aws-runshellscript-probe-reports-success.md (a probe that reports Success but carries no values). Neither covers this check — the suite marked it neither ALREADY FILED nor OWNED — and the failure mode here is distinct: Status `Failed` with an explicit `sh: aws: command not found` on stderr. They are named only for context.

## Evidence
Suite: simparity-behavioral-aws-v1 — 1444/2191 passed (65.9%). Service heading `### ec2__sts`: 0/1 (0%), 1 failed, not marked ALREADY FILED or OWNED.

ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite [mismatch]:
  Expected: GetCommandInvocation Status 'Success' with ResponseCode 0 and empty StandardErrorContent for the probe script (the script captures each sub-command's rc itself, so even a denied PutObject leaves its own exit status 0), and its stdout carrying the STS caller identity whose UserId is the <role-id>:<instance-id> composite.
  Actual:   AssertionError: the probe script itself must run to completion (its own exit status is 0 even when a PutObject is denied, because the rc is captured); SSM reports 'Failed', stderr='sh: aws: command not found\nsh: ARC: parameter not set\n'

Message-wording note: the fixed text of both stderr lines is the shell's own `sh: ` ... `: command not found` / `: parameter not set`; the variable parts (`aws`, `ARC`) are names taken from the submitted script. The requirement is that neither line is produced at all.

## Steps to Reproduce
Create a VPC with an internet gateway, a default route and a public subnet with MapPublicIpOnLaunch; create an IAM role trusted by ec2.amazonaws.com with arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore attached (plus a narrow S3 policy for the probe's PutObject), and an instance profile holding it. Resolve ImageId from the SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 and RunInstances a t3.micro into the subnet with IamInstanceProfile set to that profile. Poll DescribeInstances to 'running' and ssm.DescribeInstanceInformation to PingStatus 'Online'. Then ssm.send_command(InstanceIds=[<id>], DocumentName='AWS-RunShellScript', Parameters={'commands': ['OUT=$(aws sts get-caller-identity 2>&1); ARC=$?', 'echo "MARK rc=$ARC out=$OUT"']}) and poll ssm.get_command_invocation(CommandId, InstanceId) to a terminal status. Observe Status 'Failed' with StandardErrorContent 'sh: aws: command not found\nsh: ARC: parameter not set\n'; AWS returns Status 'Success', ResponseCode 0, empty StandardErrorContent, and stdout carrying the identity whose UserId is '<role-id>:<instance-id>' and whose Arn is the assumed-role ARN ending in the instance id.
