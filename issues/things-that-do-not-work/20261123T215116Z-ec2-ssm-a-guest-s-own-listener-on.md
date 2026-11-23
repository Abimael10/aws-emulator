Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer
Issue-Key: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer|_build#5|AssertionError|assert local["local"] == st["nonce"], (
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__ssm: a guest's own listener on 127.0.0.1 answers nothing through Run Command, so the security-group / prefix-list / Reachability Analyzer data-plane flow never starts

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
Cause 1 of 1: a server guest cannot be observed answering its own loopback listener — the in-guest probe through SSM Run Command returns an empty string — so the whole security-group-resolution data plane (prefix-list source rules, deny-by-default drops, and the Reachability Analyzer explanation that must agree with them) is never exercised.
  AWS: recorded behavior only — in-guest process state and loopback HTTP are not Smithy-modelled. Smithy grounding: SSM `SendCommand` with DocumentName `AWS-RunShellScript` runs the `commands` list in the guest and `GetCommandInvocation` returns `Status` (terminal values Success/Cancelled/TimedOut/Failed) with `StandardOutputContent` carrying the command's real stdout and `ResponseCode` its exit status. AWS's behavior the recording depends on: a listener the guest itself started is reachable from that same guest over 127.0.0.1 and returns the nonce it was configured to serve, independently of any security group — loopback traffic never crosses the VPC. EC2 grounding for the arms that follow: `CreateManagedPrefixList` + `AuthorizeSecurityGroupIngress` with `PrefixListId` admits exactly the listed CIDRs; traffic no group admits is SILENTLY DROPPED (curl exit 28, not 7); and `StartNetworkInsightsAnalysis` on a path whose destination ENI's groups admit nothing on the port returns one Explanation with ExplanationCode `ENI_SG_RULES_MISMATCH` and DirectionIn=true. The nonce value is the check's own; the requirement is that the guest's stdout is its real output.
  Emulator: the loopback probe's stdout is '' — the guest does not answer its own listener (or Run Command does not return the output). The check fails its first precondition: "the server must answer its own listener on 127.0.0.1 before any part runs; it returned ''". Every downstream arm — prefix-list admission, the deny-by-default drop, and the Reachability Analyzer explanation — is therefore unmeasured.
  Why: a client cannot bring up any in-guest service at all, so no security-group behavior is testable or trustworthy: an open rule and a missing rule are indistinguishable when nothing ever answers. Run Command is also unusable as an operational tool, since a successful-looking invocation carries no output.
  Fix: the EC2 instance guest runtime (process execution and loopback networking inside the guest) together with the SSM Run Command invocation path that captures and returns StandardOutputContent.
  Traces: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer.

Note: the open issues 20261115T003554Z-ec2-ssm-ssm-managed-nodes-execute-nothing-invocations-end-failed.md and 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md describe neighbouring guest-execution defects; the replay report does not mark this check as owned by either.

## Evidence
ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer [mismatch]
  Expected: after the server guest starts its listener on port 8080, an in-guest probe to 127.0.0.1:8080 through AWS-RunShellScript returns the nonce the listener serves (the check's own 'sim2real-sgres-<id>' value), proving the guest is serving before any security-group arm runs; the client guest's probe is then DROPPED (curl exit 28) until a managed prefix list naming its subnet CIDR is authorized, and Reachability Analyzer reports exactly one Explanation with ExplanationCode 'ENI_SG_RULES_MISMATCH' (DirectionIn=true) while the group admits nothing.
  Actual: AssertionError: "the server must answer its own listener on 127.0.0.1 before any part runs; it returned '', expected 'sim2real-sgres-<id>'" — the in-guest loopback probe produced no output, so the check aborts in _build and none of the security-group or Reachability Analyzer arms are measured.

## Steps to Reproduce
Create a VPC 10.72.0.0/16 with client (10.72.10.0/24), server (10.72.11.0/24) and endpoint (10.72.20.0/24) subnets; launch an AL2023 t3.micro server at 10.72.11.10 and a client, both with an SSM instance profile; wait for DescribeInstanceInformation PingStatus 'Online'. Use SSM SendCommand with AWS-RunShellScript to start a listener on port 8080 in the server guest, then SendCommand a curl to http://127.0.0.1:8080 in that same guest and read GetCommandInvocation StandardOutputContent: the emulator returns an empty string instead of the nonce the listener serves.
