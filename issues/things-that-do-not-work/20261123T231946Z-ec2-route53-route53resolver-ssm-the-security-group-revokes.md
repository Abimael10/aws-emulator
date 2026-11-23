Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53
Issue-Key: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53|_poll#8|Timeout|return last
Issue-Commit: 3f6b3450ba424c03cec73d9d5f39cd4c56b1b263

# ec2__route53__route53resolver__ssm: the security-group-revokes-udp-53 resolver-endpoint scenario never settles — the launched DNS client never becomes an Online SSM managed node, so no query is ever made and the check is killed before its first assertion

> _Note: this issue was identified against the code base at commit `3f6b3450ba424c03cec73d9d5f39cd4c56b1b263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceInformation

## Description
One net-new root cause grounded in the failing check the suite lists under '### ec2__route53__route53resolver__ssm'. 1 CAUSE.

NOTE: open issue 20261107T081213Z-ec2-route53-route53resolver-ssm-the-resolver-endpoint-dns.md describes the same family of hang and even mentions this check in its prose, but this replay did NOT mark this check as OWNED by it (its recorded replay key no longer matches this check's failure, which now surfaces as a bare 'Check timed out after 60.0s' rather than the TimeoutError that issue keys on). Filing it here so the check is not lost; a fixer closing that issue should re-replay this check id too.

Cause 1 of 1: the EC2 instance this scenario launches as its in-VPC DNS client never registers as a Systems Manager managed node, so the in-guest wire-format DNS query that is the scenario's only observation point never runs, and the check is killed inside the agent-online wait before it reaches a single assertion. Failing check id: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53.
AWS: an EC2 instance launched with an instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore registers with Systems Manager shortly after boot, and DescribeInstanceInformation then reports it in InstanceInformationList. Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): InstanceInformation$PingStatus -> enum PingStatus with values Online | ConnectionLost | Inactive, alongside InstanceId (pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$), LastPingDateTime, AgentVersion, PlatformType, IamRole (length 0-64) and ResourceType. Once the node is Online, SendCommand('AWS-RunShellScript') plus GetCommandInvocation run the payload in the guest and return its stdout — which is how this scenario (a) resolves a private-hosted-zone A record through an INBOUND Route 53 Resolver endpoint's listener addresses over both udp/53 and tcp/53, (b) observes that revoking only the udp/53 ingress rule on the endpoint's security group makes the udp query time out while the tcp control still answers, and (c) observes that re-authorizing udp/53 restores the answer. Recorded behavior only for the DNS-listener enforcement itself: the recording shows the udp probe returning rcode TIMEOUT with zero answer records while the tcp probe to the same address returns NOERROR with the zone's A record.
Emulator: the check reaches no assertion at all. The suite reports only 'Error: Check timed out after 60.0s' with no response diff — it is killed in the same wait for PingStatus 'Online' that its sibling check (inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone) dies in with an explicit TimeoutError on the agent coming Online. Hypothesis: nothing in the emulator registers an EC2-launched instance as an SSM managed node, so DescribeInstanceInformation never returns it and the poll runs to the runner's deadline; consequently neither the resolver endpoint's DNS listener nor the security group's enforcement of udp/53 on it is ever exercised.
Why: a client gets a hang instead of a result — there is no error code to branch on and no bounded failure, so any automation that waits for a managed node to come Online before running an in-guest step stalls until its own timeout. The entire hybrid-DNS story is unobservable: a private hosted zone answered through an inbound resolver endpoint, and the security group that gates udp/53 versus tcp/53 to that endpoint's listener addresses, cannot be verified at all.
Fix: SSM managed-node registration in the ssm crate's state (so an instance launched with an SSM-capable instance profile is projected into DescribeInstanceInformation with PingStatus 'Online'), plus in-guest execution for SendCommand/GetCommandInvocation so the probe's stdout is returned. The behavior the scenario then asserts belongs in the route53resolver crate's endpoint listener (answering queries for an associated private hosted zone on the endpoint's IP addresses, over udp and tcp) and in the ec2 crate's security-group evaluation applied to that listener (a revoked rule drops, producing no response rather than an error).
Traces: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout; the check is listed under '### ec2__route53__route53resolver__ssm'.

[mismatch] ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53
  Error: Check timed out after 60.0s
  (no response diff printed; the check body performs _await_agent -> ssm DescribeInstanceInformation poll for PingStatus == 'Online' before its first assertion, and _poll_dns is never reached)

Sibling check in the same service group, same wait, explicit message: 'TimeoutError: the SSM agent on <instance-id> coming Online: the run budget ended first; not graded'. The instance id in that message is a per-run value and is not part of the requirement.

Expected (AWS): ssm DescribeInstanceInformation(Filters=[{'Key':'InstanceIds','Values':[<launched instance id>]}]) returns an InstanceInformation with PingStatus 'Online' within normal registration latency; SendCommand/GetCommandInvocation then return the guest's DNS probe output, showing rcode NOERROR and the zone record's address over both udp/53 and tcp/53; after revoke_security_group_ingress removes only the udp/53 rule, the udp probe reports rcode TIMEOUT with an empty answer list while the tcp probe still reports NOERROR with the same address; after authorize_security_group_ingress restores udp/53, the udp probe reports NOERROR with that address again.
Actual (emulator): the instance never appears as a managed node, the agent-online poll never satisfies its predicate, and the check is terminated at the 60 s budget with no assertion evaluated and no status/error code produced.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 and re-run the check id in trace_ids: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53.
3. By hand: create a VPC (DNS support and DNS hostnames enabled) with two public subnets in two AZs and one security group permitting udp/53 and tcp/53 from itself; create a Route 53 Resolver INBOUND endpoint in those subnets with that group and wait for status OPERATIONAL; create a private hosted zone associated with the VPC holding an A record; launch a t3.micro with an instance profile whose role has arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore.
4. Poll ssm DescribeInstanceInformation(Filters=[{'Key':'InstanceIds','Values':[<instance id>]}]) for PingStatus 'Online'. Expected: Online within registration latency. Actual: the list stays empty and the poll never completes.
5. (Blocked by step 4) ssm SendCommand('AWS-RunShellScript') running a udp and a tcp DNS query for the zone record against an address from route53resolver ListResolverEndpointIpAddresses. Expected: both NOERROR with the record's address. Then revoke only the udp/53 ingress rule: expected udp rcode TIMEOUT with no answers while tcp stays NOERROR with the same address; re-authorizing udp/53 restores the udp answer.
