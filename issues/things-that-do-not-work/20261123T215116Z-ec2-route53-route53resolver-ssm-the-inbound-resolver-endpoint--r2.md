Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53
Issue-Key: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53|_poll#8|Timeout|last = probe()
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__route53__route53resolver__ssm: the inbound Resolver endpoint data-plane flow never settles — the security-group-revoked UDP/53 query check reaches no verdict inside its budget

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolverEndpoint

## Description
Cause 1 of 1: the inbound Route 53 Resolver endpoint data-plane flow never advances to the state the replay polls for, so the check exhausts its whole budget without producing a verdict and the security-group gate on resolver queries is never exercised.
  AWS: recorded behavior only for timing; the sequence the recording drives is: Route 53 Resolver `CreateResolverEndpoint` (Direction INBOUND) reaches `ResolverEndpoint.Status` OPERATIONAL and publishes its `IpAddresses`; Route 53 `CreateHostedZone`/`ChangeResourceRecordSets` publishes an A record in a private hosted zone associated with the VPC; EC2 launches a guest whose SSM agent reaches `DescribeInstanceInformation.PingStatus` 'Online'; the guest then queries the endpoint's addresses and gets the private-zone answer, and after `RevokeSecurityGroupIngress` removes UDP/53 from the endpoint's security group the identical query is DENIED (dropped, i.e. it times out rather than being answered or refused).
  Emulator: no verdict at all — the replay reports "Error: Check timed out after 60.0s". One of the polled states (resolver endpoint OPERATIONAL with addresses, instance running, or agent Online) never reaches its expected value, so neither the permitted-query arm nor the revoked-UDP/53 arm is ever measured. Sibling checks on this emulator return inside the same budget, so the stall is the emulator's state machine not converging.
  Why: a client that creates an inbound resolver endpoint and waits for it to become usable waits forever, and the security-group control over resolver queries — the only thing that decides who may use an inbound endpoint — is unenforced and unobservable.
  Fix: whichever state transition the flow polls never completes — the Route 53 Resolver endpoint status/IpAddresses progression in the resolver endpoint handler and state, and the instance + SSM-agent readiness progression in the EC2 instance state machine.
  Traces: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53.

Note: the open issue 20261107T081213Z-ec2-route53-route53resolver-ssm-the-resolver-endpoint-dns.md covers the sibling check in this suite group; the replay report does not mark this check as owned by it.

## Evidence
ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53 [mismatch]
  Expected: the inbound resolver endpoint becomes OPERATIONAL and publishes its IpAddresses; a wire-format DNS query from the in-VPC guest to those addresses returns the private hosted zone's A record; after RevokeSecurityGroupIngress removes UDP/53 the identical query is denied (dropped/timed out) rather than answered.
  Actual: "Error: Check timed out after 60.0s" — no assertion is produced; the flow never gets past its build phase, so neither the answered nor the denied arm is measured.

## Steps to Reproduce
Create a VPC 10.91.0.0/16 with two subnets (10.91.1.0/24, 10.91.2.0/24) and an internet gateway; create a security group admitting UDP/53; CreateResolverEndpoint Direction=INBOUND across the two subnets with that group and poll DescribeResolverEndpoints for Status OPERATIONAL plus its IpAddresses; create a private hosted zone associated with the VPC and an A record pointing at 192.0.2.10; launch an AL2023 t3.micro with an SSM instance profile and poll DescribeInstanceInformation for PingStatus 'Online'; run a wire-format DNS query from the guest against the endpoint's addresses; then RevokeSecurityGroupIngress for UDP/53 and repeat the query. The sequence never completes — the replay aborts on the suite's time budget with no verdict.
