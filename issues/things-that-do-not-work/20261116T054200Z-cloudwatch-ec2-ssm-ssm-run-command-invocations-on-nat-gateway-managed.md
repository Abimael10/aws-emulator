Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-and-instance-metrics-count-guest-bytes-unrouted-gateway-counts-none cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-carries-no-transfer-while-the-sink-ingress-is-blocked-then-counts-it-once-opened
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-and-instance-metrics-count-guest-bytes-unrouted-gateway-counts-none|_transfer_and_measure#2|AssertionError|assert sender_inv["ResponseCode"] == 0, (
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-carries-no-transfer-while-the-sink-ingress-is-blocked-then-counts-it-once-opened|_phase#2|AssertionError|assert sink_inv["ResponseCode"] == 0, (
Issue-Commit: 911c830f7d5d4e7f71004d0c38e62fd15ce28281

# SSM Run Command invocations on NAT-gateway managed nodes end Status 'Failed' with empty stdout/stderr, so no guest transfer ever runs and AWS/NATGateway metrics are never produced

> _Note: this issue was identified against the code base at commit `911c830f7d5d4e7f71004d0c38e62fd15ce28281`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
One net-new root cause.

Cause 1 of 1: an `AWS-RunShellScript` command sent to a registered, Online managed node never executes. The invocation reaches the terminal state `Failed` with an empty `StandardOutputContent`, an empty `StandardErrorContent` and a non-zero `ResponseCode`, so neither the sink listener nor the sender side of a guest-to-guest TCP transfer ever runs, and the NAT-gateway byte/packet metrics that the transfer is supposed to produce can never be observed. Failing checks: cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-and-instance-metrics-count-guest-bytes-unrouted-gateway-counts-none, cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-carries-no-transfer-while-the-sink-ingress-is-blocked-then-counts-it-once-opened.

AWS: Smithy (ssm) models SendCommand -> SendCommandResult{Command} and GetCommandInvocation -> GetCommandInvocationResult with `Status` (CommandInvocationStatus enum), `ResponseCode` (integer, default 0), `StandardOutputContent` (@length 0..24000) and `StandardErrorContent` (@length 0..8000). For a well-formed shell document delivered to a healthy, reachable managed node AWS drives the invocation to `Status` 'Success' with `ResponseCode` 0 and `StandardOutputContent` equal to the script's stdout; a script that exits non-zero yields `Status` 'Failed' WITH that script's output and that script's exit code in `ResponseCode`. Guest execution itself is recorded behavior only: the recording shows the script's own bytes in StandardOutputContent and ResponseCode 0 for a successful probe. A terminal 'Failed' with BOTH output streams empty is not a shape AWS produces for a valid document on an Online node — it is indistinguishable from "document not found" / "node unreachable".

Emulator: every in-guest leg of both checks terminates as `status='Failed' stdout='' stderr=''` with `ResponseCode != 0`. The listener leg reports "the sink failed: status='Failed' stdout='' stderr=''" and the transfer leg reports "the sender's transfer failed: status='Failed' stdout='' stderr=''". No script output is ever returned, so the 32 MiB payload the checks move across the NAT gateway is never sent and AWS/NATGateway `BytesOutToDestination`/`BytesInFromSource` and AWS/EC2 `NetworkOut`/`NetworkPacketsOut` have nothing to count. The emulator simultaneously reports the node Online through DescribeInstanceInformation.

Why: Run Command is the primary way a client configures and observes a fleet, and it is the only way these data-plane facts (a guest-originated transfer through a NAT gateway, a listening sink, an ingress rule actually blocking a connect) can be established. A terminal 'Failed' with no output and a non-zero ResponseCode gives automation nothing to branch on: it cannot tell "the node never ran the script" from "the script exited non-zero", and because the control plane says the node is Online the operator is told the fleet is healthy while nothing executes. Downstream, every metric assertion built on guest traffic silently sees a quiet gateway, so a client cannot distinguish a blocked path from an unexercised one — the exact distinction the negative check exists to make.

Fix: the SSM command-invocation execution path — delivery of the document to the managed node and collection of the guest process's exit status and output streams into GetCommandInvocation's Status/ResponseCode/StandardOutputContent/StandardErrorContent. The guest-side execution of `aws:RunShellScript` in the managed-node runtime is where the work belongs; the NAT-gateway/EC2 metric publication path is only observable once invocations actually run.

Related open issues describe the same family but do NOT carry these two checks' replay keys, so neither covers them: 20261115T003554Z-ec2-ssm-ssm-managed-nodes-execute-nothing-invocations-end-failed.md (service ec2__ssm), 20261114T222555Z-cloudwatch-ec2-ssm-3-net-new-causes-a.md and 20261114T143448Z-ec2-ssm-guest-execution-never-terminates-describeinstanceinformation-reports.md.

Traces: cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-and-instance-metrics-count-guest-bytes-unrouted-gateway-counts-none; cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-carries-no-transfer-while-the-sink-ingress-is-blocked-then-counts-it-once-opened.

## Evidence
Cause 1 — Expected (AWS): GetCommandInvocation for an AWS-RunShellScript invocation on an Online managed node returns Status 'Success', ResponseCode 0 and StandardOutputContent containing the script's own stdout; a non-zero exit returns Status 'Failed' with the script's stdout/stderr present and its exit code in ResponseCode.

Actual (emulator): both legs end with empty streams and a non-zero ResponseCode.
  - cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-and-instance-metrics-count-guest-bytes-unrouted-gateway-counts-none: AssertionError at `assert sender_inv["ResponseCode"] == 0` — "the sender's transfer failed: status='Failed' stdout='' stderr=''".
  - cloudwatch__ec2__ssm.obs-ec2-1-nat-gateway-carries-no-transfer-while-the-sink-ingress-is-blocked-then-counts-it-once-opened: AssertionError at `assert sink_inv["ResponseCode"] == 0` — "the sink failed: status='Failed' stdout='' stderr=''".

The empty-string stdout/stderr and the 'Failed' status are the diverging values; the instance ids, command ids, VPC/subnet CIDRs and the 32 MiB payload size are per-run inputs of these checks, not part of the requirement. The requirement is: a valid document on an Online node reaches 'Success' with ResponseCode 0 and the script's real stdout, and a failing script reports its own output together with its own exit code — never a 'Failed' with both streams empty.

## Steps to Reproduce
1. Create a VPC with a public and a private subnet, an internet gateway, a NAT gateway with an EIP in the public subnet, and route tables so the private subnet egresses through the NAT gateway.
2. Launch two t3.micro instances from /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 with an instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore (one sink in the public subnet, one sender in the private subnet).
3. Poll ssm.DescribeInstanceInformation until both nodes report Online.
4. ssm.SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[<sink-id>], Parameters={'commands': [<a shell script that opens a TCP listener on port 5201 and prints a marker>]}) and poll ssm.GetCommandInvocation(CommandId=..., InstanceId=<sink-id>) to a terminal state.
5. ssm.SendCommand the sender script that connects to the sink through the NAT gateway and writes 32 MiB, then poll ssm.GetCommandInvocation for it.
6. AWS: both invocations reach Status 'Success' with ResponseCode 0 and the scripts' stdout in StandardOutputContent, and the transfer is then visible in cloudwatch.GetMetricStatistics for AWS/NATGateway BytesOutToDestination / AWS/EC2 NetworkOut. Emulator: both invocations reach Status 'Failed' with ResponseCode != 0 and StandardOutputContent == '' and StandardErrorContent == '', so no bytes are ever transferred and no metric is produced.
