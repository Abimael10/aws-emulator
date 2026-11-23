Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone
Issue-Key: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone|_poll#8|Timeout|time.sleep(interval_s)
Issue-Commit: 6eb032b97e018d1f5cd705ca7e0cd0c3b41d57c9

# ec2__route53__route53resolver__ssm: the inbound Resolver endpoint happy-path data plane never settles — the endpoint addresses are never observed answering the associated private hosted zone

> _Note: this issue was identified against the code base at commit `6eb032b97e018d1f5cd705ca7e0cd0c3b41d57c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResolverEndpointIpAddresses

## Description
One net-new root cause grounded in the failing check the suite lists under '### ec2__route53__route53resolver__ssm'. 1 CAUSE.

Cause 1 of 1: the inbound Route 53 Resolver endpoint happy-path flow never advances to a state the replay can assert on, so the check burns its whole budget and reaches no verdict — the endpoint's published IP addresses are never seen answering (or NXDOMAIN-ing) names of the private hosted zone associated with the VPC. Failing check: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone.
  AWS: the sequence the recording drives is modelled end to end. Smithy (services/service-com-amazonaws-route53resolver-route53resolver): CreateResolverEndpoint(CreatorRequestId, SecurityGroupIds, Direction=INBOUND, IpAddresses @length min 2 max 20) returns a ResolverEndpoint whose Status is a ResolverEndpointStatus enum CREATING | OPERATIONAL | UPDATING | AUTO_RECOVERING | ACTION_NEEDED | DELETING, documented as 'CREATING: Resolver is creating and configuring one or more Amazon VPC network interfaces for this endpoint' then 'OPERATIONAL: the Amazon VPC network interfaces for this endpoint are correctly configured and able to pass inbound or outbound DNS queries'; ListResolverEndpointIpAddresses(ResolverEndpointId) then returns one IpAddressResponse per endpoint ENI (so one per subnet the endpoint was built over), paginated by NextToken/MaxResults. Smithy (services/service-com-amazonaws-ssm-amazonssm): DescribeInstanceInformation reports a managed node with PingStatus 'Online' after an instance launched with an AmazonSSMManagedInstanceCore instance profile registers, and SendCommand/GetCommandInvocation then run the in-guest DNS client whose stdout is this check's only observation point. Beyond that, recorded behavior only: each of the endpoint's addresses answers 'db.<zone>' A with rcode NOERROR, the matching transaction id, exactly the one A record the check wrote, TTL >0 and <= the record set's 60 s, and answers a name the zone does not hold with rcode NXDOMAIN and no answer records.
  Emulator: no verdict at all — the replay reports 'Error: Check timed out after 60.0s' with no response diff. One of the states the flow polls (instance running, SSM agent Online, resolver endpoint OPERATIONAL with its IpAddresses published, or the in-guest query converging) never reaches its expected value, so none of the assertions above is ever evaluated. Sibling checks in this suite answer inside the same budget, so the stall is the emulator's state machines not converging rather than a slow runner.
  Why: a client that creates an inbound resolver endpoint and waits for it to become usable waits forever with no error code to branch on, and the whole hybrid-DNS story this emulator is meant to reproduce — a private hosted zone reachable from outside the VPC through an inbound endpoint's addresses, authoritative for what the zone holds and NXDOMAIN for everything else — is unobservable.
  Fix: whichever polled transition never completes: the resolver endpoint Status/IpAddresses progression in the route53resolver endpoint handler and state (CreateResolverEndpoint, GetResolverEndpoint, ListResolverEndpointIpAddresses), the instance-running and SSM managed-node registration progression in the ec2/ssm state, and the DNS data plane that must answer on the endpoint's addresses out of the VPC-associated private hosted zone.
  Related open issues that do NOT carry this failure's replay key (the report does not mark this check as owned by any of them): 20261107T081213Z-ec2-route53-route53resolver-ssm-the-resolver-endpoint-dns.md (same check id, but keyed on a TimeoutError raised inside _poll, not on the 60 s check-level timeout seen here), 20261123T215116Z-ec2-route53-route53resolver-ssm-the-inbound-resolver-endpoint--r2.md and 20261123T231946Z-ec2-route53-route53resolver-ssm-the-security-group-revokes.md (both the security-group-revoked sibling check).
  Traces: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout; the check is listed under '### ec2__route53__route53resolver__ssm' as [mismatch].

ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone
  Expected (AWS): CreateResolverEndpoint(Direction=INBOUND) over two subnets reaches ResolverEndpoint.Status 'OPERATIONAL'; ListResolverEndpointIpAddresses returns one address per subnet; the launched in-VPC guest becomes an Online SSM managed node; a wire-format A query for 'db.<zone>' sent to each endpoint address returns rcode NOERROR, matching transaction id, exactly the single A record written by the check, name echoed as asked, type A, and TTL >0 and <= 60; a query for 'nosuch.<zone>' to the first address returns rcode NXDOMAIN with no answer records.
  Actual (emulator): 'Error: Check timed out after 60.0s' — the flow never reaches the first assertion, so neither the answered arm nor the NXDOMAIN arm is measured. No status, error code or DNS rcode is produced at all.

(The instance ids, endpoint id, zone name and addresses are this run's values and are not part of the requirement; the requirement is that each state converges and each address answers as described.)

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay simparity-behavioral-aws-v1, re-running the check id in trace_ids.
2. Create a VPC 10.91.0.0/16 with EnableDnsSupport and EnableDnsHostnames, two subnets (10.91.1.0/24, 10.91.2.0/24) in two AZs, an internet gateway with a default route, and a security group admitting udp/53 and tcp/53 from itself.
3. CreateResolverEndpoint with Direction=INBOUND, that security group and one IpAddresses entry per subnet; poll GetResolverEndpoint/DescribeResolverEndpoints until Status is OPERATIONAL, then ListResolverEndpointIpAddresses and expect one address per subnet.
4. CreateHostedZone (private, associated with the VPC) and ChangeResourceRecordSets to publish 'db.<zone>' A 192.0.2.10 with TTL 60.
5. RunInstances an AL2023 t3.micro in the first subnet with an instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore; poll DescribeInstanceInformation for PingStatus 'Online'.
6. Via SendCommand('AWS-RunShellScript')/GetCommandInvocation, send wire-format A queries from the guest to each endpoint address for 'db.<zone>' and one for 'nosuch.<zone>'.
Expected: every endpoint address answers NOERROR with the single written A record (TTL >0 and <=60) and the absent name answers NXDOMAIN with no answers. Actual: the scenario never converges and the check is killed on its time budget with no verdict.
